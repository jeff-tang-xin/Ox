//! Structured workflow workspace — single LLM context block per iteration.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;
use super::findings::{self, FindingStatus, FindingsStore};
use super::task_intent::TaskIntent;

pub const WORKSPACE_TAG: &str = "[WORKSPACE]";

/// When true, skip legacy DURABLE_MEMORY / heavy STEP_MEMORY (workspace is canonical).
pub fn uses_workspace_memory(engine: &WorkflowEngine) -> bool {
    if !crate::agent::phase::should_inject_workspace(engine) {
        return false;
    }
    let step = engine.get_current_step_index();
    step == 0
        || step == 3
        || crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::Implement
}

/// Minimal addon when workspace mode is active (skills only).
pub fn minimal_durable_addon(engine: &WorkflowEngine) -> String {
    let guidance = engine.workflow_guidance_block();
    if guidance.is_empty() {
        String::new()
    } else {
        format!(
            "{}\n\n{}",
            super::memory_bridge::DURABLE_MEMORY_TAG,
            guidance
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    ExecuteReview,
    FeedbackDiscuss,
    ScopeConfirm,
    ExecuteImpl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequiredAction {
    Explore {
        hint: String,
    },
    ReadFile {
        path: String,
        offset: u32,
        limit: u32,
        finding_index: u32,
    },
    EditFile {
        path: String,
        finding_index: u32,
    },
    Verify {
        command: String,
        finding_index: u32,
    },
    EmitFindingsAndDone,
    EmitCompletionReceipt,
    AwaitUser,
    DiscussOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ReadSlot {
    pub path: String,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowWorkspace {
    pub task_intent: TaskIntent,
    pub tool_hints: String,
    pub authority_note: String,
    pub mode: WorkspaceMode,
    pub user_request: String,
    pub findings_summary: String,
    pub findings: Vec<WorkspaceFinding>,
    pub active_indices: Vec<u32>,
    pub files_read: Vec<ReadSlot>,
    #[serde(default)]
    pub file_digests: Vec<crate::agent::tool_digest::FileDigest>,
    pub files_edited: Vec<String>,
    pub required_action: RequiredAction,
    pub forbidden: Vec<String>,
    pub phase_notes: String,
    /// Canonical single-flow phase (from phase state machine).
    pub single_flow_phase: String,
    /// Recent user directives (from workflow guidance — no separate block in Implement).
    #[serde(default)]
    pub user_directives: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceFinding {
    pub index: u32,
    pub severity: String,
    pub file: String,
    pub symbol: String,
    pub issue: String,
    pub recommendation: String,
    pub status: FindingStatus,
}

impl WorkflowWorkspace {
    pub fn build(engine: &WorkflowEngine) -> Option<Self> {
        let task_intent = engine.get_task_intent();
        let mode = crate::agent::phase::workspace_mode(engine);
        let user_request = engine
            .get_variable("_current_user_request")
            .unwrap_or_default();
        let store = findings::load_or_migrate(engine);
        let (findings_summary, findings, active_indices) = if let Some(ref s) = store {
            (
                s.summary.clone(),
                s.findings.iter().map(workspace_finding_from).collect(),
                s.active_indices.clone(),
            )
        } else {
            (String::new(), Vec::new(), Vec::new())
        };

        let files_read: Vec<ReadSlot> = build_files_read(engine);

        let file_digests: Vec<crate::agent::tool_digest::FileDigest> =
            crate::agent::tool_digest::all_digests(engine);

        let files_edited = engine
            .get_variable("_impl_files_edited")
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default();

        let required_action =
            compute_required_action(engine, mode, &store, &files_read, &files_edited);
        let forbidden = forbidden_for_mode(mode);
        let phase_notes = phase_notes_for_mode(mode);
        let user_directives = recent_user_directives(engine);

        Some(Self {
            task_intent,
            tool_hints: task_intent.tool_hint().to_string(),
            authority_note: task_intent.authority_note().to_string(),
            mode,
            user_request,
            findings_summary,
            findings,
            active_indices,
            files_read,
            file_digests,
            files_edited,
            required_action,
            forbidden,
            phase_notes,
            single_flow_phase: crate::agent::phase::get(engine).as_str().to_string(),
            user_directives,
        })
    }

    pub fn format_for_llm(&self) -> String {
        self.format_body(false)
    }

    pub fn format_for_llm_unified(&self) -> String {
        self.format_body(true)
    }

    fn format_body(&self, unified: bool) -> String {
        let mut out = format!(
            "{WORKSPACE_TAG}\n\
             ## 当前任务\n\n\
             **任务:** {}\n",
            truncate_line(&self.user_request, 400),
        );
        if unified {
            out.push_str(&format!(
                "**状态:** {} · {}\n",
                self.single_flow_phase,
                phase_mode_label(&self.single_flow_phase, self.mode),
            ));
        } else {
            out.push_str(&format!(
                "**阶段:** {} · **模式:** {}\n",
                self.single_flow_phase,
                phase_mode_label(&self.single_flow_phase, self.mode),
            ));
        }
        if !self.findings_summary.is_empty() {
            out.push_str(&format!("\n**摘要:** {}\n", self.findings_summary));
        }
        out.push_str("\n### 下一步\n");
        out.push_str(&format_required_action(&self.required_action, unified));
        if let RequiredAction::EditFile {
            path,
            finding_index,
        } = &self.required_action
        {
            let norm = plan_tracker_normalize(path);
            if let Some(d) = self
                .file_digests
                .iter()
                .find(|d| plan_tracker_normalize(&d.path) == norm)
            {
                out.push_str(&format!(
                    "\n**edit 参考 digest (#{finding_index}):** {}\n",
                    truncate_line(&d.summary, 600)
                ));
            } else {
                out.push_str(if unified {
                    "\n**edit 提示:** 尚无文件内容 — 先 `complete_and_check(action=file_read, ...)`，再 `action=edit_file`。\n"
                } else {
                    "\n**edit 提示:** 尚无文件内容 — 先 `file_read` 该文件，再 `edit_file`。\n"
                });
            }
        }
        let forbidden = if unified {
            forbidden_for_mode_unified(self.mode)
        } else {
            self.forbidden.clone()
        };
        if !forbidden.is_empty() {
            out.push_str("\n**禁止:** ");
            out.push_str(&forbidden.join(" · "));
            out.push('\n');
        }
        let phase_notes = if unified {
            phase_notes_for_mode_unified(self.mode)
        } else {
            self.phase_notes.clone()
        };
        if !phase_notes.is_empty() {
            out.push_str(&format!("\n_{}_\n", phase_notes));
        }
        if !self.findings.is_empty() {
            use std::collections::BTreeMap;
            let mut by_file: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for f in &self.findings {
                let key = if f.file.is_empty() {
                    "(无文件)".to_string()
                } else {
                    f.file.clone()
                };
                by_file.entry(key).or_default().push(f.index);
            }
            if by_file.len() > 1 || by_file.values().any(|v| v.len() > 1) {
                out.push_str("\n### 问题关联（同文件）\n");
                for (file, indices) in &by_file {
                    let ids: Vec<String> = indices.iter().map(|i| format!("#{i}")).collect();
                    out.push_str(&format!("- `{file}` → {}\n", ids.join(", ")));
                }
            }
            out.push_str("\n### Findings\n");
            let mut last_file: Option<String> = None;
            for f in &self.findings {
                let file_key = if f.file.is_empty() {
                    "(无文件)".to_string()
                } else {
                    f.file.clone()
                };
                if last_file.as_ref() != Some(&file_key) {
                    if f.file.is_empty() {
                        out.push_str(&format!("\n#### {}\n", file_key));
                    } else {
                        out.push_str(&format!("\n#### `{}`\n", f.file));
                    }
                    last_file = Some(file_key);
                }
                let loc = if f.file.is_empty() {
                    f.symbol.clone()
                } else if f.symbol.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", f.symbol)
                };
                let in_scope =
                    if self.active_indices.is_empty() || self.active_indices.contains(&f.index) {
                        ""
                    } else {
                        " (范围外)"
                    };
                out.push_str(&format!(
                    "- **#{}** [{}]{loc}{in_scope}\n  问题: {}\n",
                    f.index, f.severity, f.issue
                ));
                if !f.recommendation.is_empty() {
                    out.push_str(&format!("  建议: {}\n", f.recommendation));
                }
            }
        }
        if !self.file_digests.is_empty() {
            let header = if self.mode == WorkspaceMode::ExecuteImpl {
                if matches!(self.required_action, RequiredAction::ReadFile { .. }) {
                    "### 审查期 digest（**不等于实施已读** — 须先执行上方 ReadFile）"
                } else {
                    "### 审查期 digest（参考行号；实施已读后请 edit_file）"
                }
            } else {
                "### 已读文件 digest"
            };
            out.push_str(&format!("\n{header}\n"));
            for d in &self.file_digests {
                let syms = if d.symbols.is_empty() {
                    String::new()
                } else {
                    d.symbols
                        .iter()
                        .take(4)
                        .map(|s| format!("{}@L{}-{}", s.name, s.line_start, s.line_end))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!("- `{}`", d.path));
                if !syms.is_empty() {
                    out.push_str(&format!(" — {syms}"));
                }
                out.push('\n');
            }
        }
        if !self.user_directives.is_empty() {
            out.push_str("\n### 用户补充\n");
            for d in &self.user_directives {
                out.push_str(&format!("- {d}\n"));
            }
        }
        out.trim_end().to_string()
    }
}

fn mode_label(mode: WorkspaceMode) -> &'static str {
    match mode {
        WorkspaceMode::ExecuteReview => "审查(只读)",
        WorkspaceMode::ExecuteImpl => "实施(可改代码)",
        WorkspaceMode::ScopeConfirm => "确认范围",
        WorkspaceMode::FeedbackDiscuss => "讨论",
    }
}

fn phase_mode_label(phase: &str, mode: WorkspaceMode) -> String {
    match phase {
        "await_user" => "待用户确认".to_string(),
        "implement" => "实施".to_string(),
        "complete" => "已完成".to_string(),
        _ => mode_label(mode).to_string(),
    }
}

fn truncate_line(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

fn format_required_action(action: &RequiredAction, unified: bool) -> String {
    if unified {
        return format_required_action_unified(action);
    }
    match action {
        RequiredAction::Explore { hint } => {
            format!("🔍 **探索** — 调工具收集信息\n→ {hint}")
        }
        RequiredAction::ReadFile {
            path,
            finding_index,
            offset,
            limit,
            ..
        } => {
            if *offset > 0 {
                format!(
                    "📖 **读取** finding #{finding_index}\n\
                     → 工具: `file_read` path=`{path}` offset={offset} limit={limit}\n\
                     → 下一 tool **必须**是 `edit_file`（从返回内容复制 old_string）"
                )
            } else {
                format!(
                    "📖 **读取** finding #{finding_index}\n\
                     → 工具: `file_read` path=`{path}` offset={offset} limit={limit}\n\
                     → 下一 tool **必须**是 `edit_file`（同一文件）"
                )
            }
        }
        RequiredAction::EditFile {
            path,
            finding_index,
        } => {
            format!(
                "✏️ **编辑** finding #{finding_index}\n\
                 → 工具: `edit_file` path=`{path}`\n\
                 → 按上方 findings 的「建议」修改；不确定关联代码时可 `find_symbol`"
            )
        }
        RequiredAction::Verify {
            command,
            finding_index,
        } => {
            format!(
                "✅ **验证** finding #{finding_index}\n\
                 → 工具: `shell_exec` 运行: `{command}`\n\
                 → exit 0 后继续下一项"
            )
        }
        RequiredAction::EmitFindingsAndDone => "📋 **审查交付**\n\
             → 1) 用 prose 写审查结论\n\
             → 2) 附 findings JSON 代码块（机器解析，UI 不展示 JSON）\n\
             → 3) 输出 `## Done`"
            .to_string(),
        RequiredAction::EmitCompletionReceipt => "🏁 **修复完成**\n\
             → 全部 in-scope finding 已 edit + verify\n\
             → 输出 completion_receipt JSON + `## Done`"
            .to_string(),
        RequiredAction::AwaitUser => "⏸️ **讨论暂停**（同一会话，非新对话）\n\
             → findings 已入库\n\
             → 用户在面板选范围并按 c /confirm 后自动切入执行\n\
             → 若用户输入讨论：仅文字回应，勿重出 findings / ## Done"
            .to_string(),
        RequiredAction::DiscussOnly => {
            "💬 **讨论** — `complete_and_check(action=finish, params={content:\"...\"})` 回应用户"
                .to_string()
        }
    }
}

fn format_required_action_unified(action: &RequiredAction) -> String {
    match action {
        RequiredAction::Explore { hint } => {
            format!(
                "🔍 **探索** — `complete_and_check` 调 read/list/search 类 action\n→ {hint}"
            )
        }
        RequiredAction::ReadFile {
            path,
            finding_index,
            offset,
            limit,
            ..
        } => {
            format!(
                "📖 **读取** finding #{finding_index}\n\
                 → `complete_and_check(action=\"file_read\", params={{\"path\":\"{path}\",\"offset\":{offset},\"limit\":{limit}}})`\n\
                 → 下一 action **必须**是 `edit_file`（从 observation 复制 old_string）"
            )
        }
        RequiredAction::EditFile {
            path,
            finding_index,
        } => {
            format!(
                "✏️ **编辑** finding #{finding_index}\n\
                 → `complete_and_check(action=\"edit_file\", params={{\"path\":\"{path}\", ...}})`\n\
                 → 按 findings 建议修改；可 `find_symbol` / `recall`"
            )
        }
        RequiredAction::Verify {
            command,
            finding_index,
        } => {
            format!(
                "✅ **验证** finding #{finding_index}\n\
                 → `complete_and_check(action=\"shell_exec\", params={{\"command\":\"{command}\"}})`\n\
                 → exit 0 后继续下一项"
            )
        }
        RequiredAction::EmitFindingsAndDone => {
            "📋 **提交计划/结论**\n\
             → `complete_and_check(action=\"finish\", params={{\"finding_json\":{{\"findings_summary\":\"…\",\"findings\":[…]}}}})`\n\
             → 需用户审核的 plan/bug/将改动放 finding_json；纯分析放 content"
                .to_string()
        }
        RequiredAction::EmitCompletionReceipt => {
            "🏁 **修复完成**\n\
             → 全部 in-scope finding 已 edit + verify\n\
             → `complete_and_check(action=\"finish\", params={{\"content\":\"…\"}})`（无 finding_json → 结束本轮）"
                .to_string()
        }
        RequiredAction::AwaitUser => {
            "⏸️ **门禁暂停**（同一会话）\n\
             → finding_json 已提交 — **禁止**一切 complete_and_check\n\
             → 用户 c /confirm 后切入实施；讨论用 UI 介入"
                .to_string()
        }
        RequiredAction::DiscussOnly => {
            "💬 **讨论** — `finish(params.content=...)` 回应；禁止 read/write".to_string()
        }
    }
}

fn forbidden_for_mode_unified(_mode: WorkspaceMode) -> Vec<String> {
    vec![]
}

fn phase_notes_for_mode_unified(_mode: WorkspaceMode) -> String {
    String::new()
}

fn build_files_read(engine: &WorkflowEngine) -> Vec<ReadSlot> {
    let mut slots: Vec<ReadSlot> = crate::agent::read_guard::paths_read(engine)
        .into_iter()
        .map(|path| ReadSlot {
            path,
            offset: 0,
            limit: 200,
        })
        .collect();
    for d in crate::agent::tool_digest::all_digests(engine) {
        if !slots
            .iter()
            .any(|s| plan_tracker_normalize(&s.path) == plan_tracker_normalize(&d.path))
        {
            slots.push(ReadSlot {
                path: d.path.clone(),
                offset: 0,
                limit: d.line_count.min(200) as u32,
            });
        }
    }
    slots
}

fn compute_required_action(
    engine: &WorkflowEngine,
    mode: WorkspaceMode,
    store: &Option<FindingsStore>,
    _files_read: &[ReadSlot],
    files_edited: &[String],
) -> RequiredAction {
    match mode {
        WorkspaceMode::ExecuteReview => {
            if matches!(engine.get_task_intent(), TaskIntent::Qa) {
                return RequiredAction::Explore {
                    hint: "问答模式：直接回答；需核对代码时单次 file_read/find_symbol".to_string(),
                };
            }
            if crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::AwaitUser {
                return RequiredAction::AwaitUser;
            }
            // 还没 findings → 先用 code_graph 了解结构，再深入文件
            if store.as_ref().is_none_or(|s| s.findings.is_empty()) {
                return RequiredAction::Explore {
                    hint: "先用 code_graph 了解项目/模块结构（code_graph query + find_symbol），\
                           再 file_read 深入具体文件"
                        .to_string(),
                };
            }
            RequiredAction::EmitFindingsAndDone
        }
        WorkspaceMode::ScopeConfirm => RequiredAction::AwaitUser,
        WorkspaceMode::FeedbackDiscuss => RequiredAction::DiscussOnly,
        WorkspaceMode::ExecuteImpl => {
            let Some(store) = store else {
                return RequiredAction::Explore {
                    hint: "无 findings — 按 plan 执行".to_string(),
                };
            };
            let indices = if store.active_indices.is_empty() {
                store
                    .findings
                    .iter()
                    .filter(|f| {
                        !matches!(
                            f.status,
                            FindingStatus::Done
                                | FindingStatus::Skipped
                                | FindingStatus::Disputed
                                | FindingStatus::WontFix
                        )
                    })
                    .map(|f| f.index)
                    .collect::<Vec<_>>()
            } else {
                store.active_indices.clone()
            };
            for idx in indices {
                let Some(f) = store.get(idx) else {
                    continue;
                };
                if matches!(
                    f.status,
                    FindingStatus::Done | FindingStatus::Skipped | FindingStatus::Disputed
                ) {
                    continue;
                }
                let norm_path = plan_tracker_normalize(&f.file);
                let has_edit = !f.file.is_empty()
                    && files_edited
                        .iter()
                        .any(|p| plan_tracker_normalize(p) == norm_path);
                let has_impl_read = !f.file.is_empty() && engine.impl_file_already_read(&f.file);

                if f.status == FindingStatus::AwaitingVerify
                    && let Some(command) = verify_command_for_finding(engine, idx)
                {
                    return RequiredAction::Verify {
                        command,
                        finding_index: idx,
                    };
                }
                // Before reading/editing: suggest code_graph impact analysis
                // so the LLM understands the blast radius of its changes.
                if !f.file.is_empty() && !engine.impl_impact_done(idx) {
                    let hint = format!(
                        "code_graph impact — 分析 {file} 的调用链影响范围：\
                         complete_and_check(action=\"code_graph\", \
                         params={{\"op\":\"impact\",\"target\":\"{file}\",\"direction\":\"downstream\"}})",
                        file = f.file
                    );
                    return RequiredAction::Explore { hint };
                }
                if !f.file.is_empty() && !has_edit && !has_impl_read {
                    let (offset, limit) = read_offset_for_finding(engine, f);
                    return RequiredAction::ReadFile {
                        path: f.file.clone(),
                        offset,
                        limit,
                        finding_index: idx,
                    };
                }
                if !has_edit {
                    return RequiredAction::EditFile {
                        path: f.file.clone(),
                        finding_index: idx,
                    };
                }
            }
            RequiredAction::EmitCompletionReceipt
        }
    }
}

fn verify_command_for_finding(engine: &WorkflowEngine, finding_index: u32) -> Option<String> {
    if let Some(tracker) = engine.get_plan_tracker()
        && let Some(step) = tracker.steps.iter().find(|s| s.index == finding_index)
        && !step.verify.trim().is_empty()
    {
        return Some(step.verify.clone());
    }
    let cmd = crate::agent::post_edit_verification::verify_command(engine);
    if cmd.trim().is_empty() {
        None
    } else {
        Some(cmd)
    }
}
fn forbidden_for_mode(_mode: WorkspaceMode) -> Vec<String> {
    vec![]
}

fn phase_notes_for_mode(_mode: WorkspaceMode) -> String {
    "全阶段可用所有工具：find_symbol, file_read, edit_file, code_search, shell_exec 等".into()
}

fn workspace_finding_from(f: &findings::Finding) -> WorkspaceFinding {
    WorkspaceFinding {
        index: f.index,
        severity: f.severity.label().to_string(),
        file: f.file.clone(),
        symbol: f.symbol.clone(),
        issue: f.issue.clone(),
        recommendation: f.recommendation.clone(),
        status: f.status,
    }
}

fn plan_tracker_normalize(path: &str) -> String {
    crate::agent::plan_tracker::normalize_path(path)
}

/// Suggest file_read offset near the finding's symbol (from digest or name).
fn read_offset_for_finding(engine: &WorkflowEngine, f: &findings::Finding) -> (u32, u32) {
    const LIMIT: u32 = 200;
    let Some(digest) = crate::agent::tool_digest::get_digest(engine, &f.file) else {
        return (0, LIMIT);
    };
    let sym = f.symbol.trim();
    if sym.is_empty() {
        return (0, LIMIT);
    }
    let sym_lower = sym.to_lowercase();
    if let Some(s) = digest.symbols.iter().find(|s| {
        s.name.eq_ignore_ascii_case(sym)
            || sym_lower.contains(&s.name.to_lowercase())
            || s.name.to_lowercase().contains(&sym_lower)
    }) {
        let offset = s.line_start.saturating_sub(8);
        return (offset, LIMIT);
    }
    (0, LIMIT)
}

fn recent_user_directives(engine: &WorkflowEngine) -> Vec<String> {
    crate::agent::workflow_guidance::load(engine)
        .into_iter()
        .rev()
        .take(4)
        .map(|e| {
            let snip: String = e.text.chars().take(200).collect();
            format!("[{}] {snip}", e.step_name)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn inject_workspace(
    messages: &mut Vec<crate::message::Message>,
    engine: &WorkflowEngine,
    unified_tool_mode: bool,
) {
    if !crate::agent::phase::should_inject_workspace(engine) {
        return;
    }
    let Some(ws) = WorkflowWorkspace::build(engine) else {
        return;
    };
    strip_workspace(messages);
    let mut text = if unified_tool_mode {
        ws.format_for_llm_unified()
    } else {
        ws.format_for_llm()
    };
    if unified_tool_mode {
        text.push_str("\n\n");
        text.push_str(&crate::agent::unified_action::build_unified_route_compact(
            engine,
        ));
    }
    messages.push(crate::message::Message::system(&text));
}

pub fn strip_workspace(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        !matches!(m, crate::message::Message::System { content } if content.starts_with(WORKSPACE_TAG))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use crate::agent::session::SessionState;

    #[test]
    fn format_includes_action_card() {
        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        engine.set_variable("_current_user_request", "review".into());
        let ws = WorkflowWorkspace::build(&engine).unwrap();
        let text = ws.format_for_llm();
        assert!(text.contains("下一步"));
        assert!(!text.contains("```json"));
    }

    #[test]
    fn implement_requires_fresh_read_not_review_digest() {
        use crate::agent::findings::{Finding, FindingStatus, FindingsStore, Severity};
        use crate::agent::phase::{self, SingleFlowPhase};

        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        engine.set_variable(
            phase::PHASE_STATE_KEY,
            SingleFlowPhase::Implement.as_str().to_string(),
        );
        let store = FindingsStore {
            summary: "s".into(),
            findings: vec![Finding {
                index: 1,
                severity: Severity::High,
                file: "src/Foo.java".into(),
                symbol: "doHandle".into(),
                issue: "bug".into(),
                recommendation: "fix".into(),
                fix_plan: String::new(),
                status: FindingStatus::Scoped,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![1],
        };
        findings::save(&engine, &store);
        crate::agent::tool_digest::record_read(&engine, "src/Foo.java", "class Foo {}", 0, Some(1));
        // Mark impact as done so the gate doesn't block ReadFile
        engine.record_impl_impact(1);
        let ws = WorkflowWorkspace::build(&engine).unwrap();
        assert!(matches!(
            ws.required_action,
            RequiredAction::ReadFile {
                finding_index: 1,
                ..
            }
        ));
    }

    #[test]
    fn implement_moves_to_edit_after_impl_read() {
        use crate::agent::findings::{Finding, FindingStatus, FindingsStore, Severity};
        use crate::agent::phase::{self, SingleFlowPhase};

        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        engine.set_variable(
            phase::PHASE_STATE_KEY,
            SingleFlowPhase::Implement.as_str().to_string(),
        );
        let store = FindingsStore {
            summary: "s".into(),
            findings: vec![Finding {
                index: 1,
                severity: Severity::High,
                file: "src/Foo.java".into(),
                symbol: "doHandle".into(),
                issue: "bug".into(),
                recommendation: "fix".into(),
                fix_plan: String::new(),
                status: FindingStatus::Scoped,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![1],
        };
        findings::save(&engine, &store);
        engine.record_impl_file_read("src/Foo.java", "{}");
        // Mark impact as done so the gate allows EditFile
        engine.record_impl_impact(1);

        let ws = WorkflowWorkspace::build(&engine).unwrap();
        assert!(matches!(
            ws.required_action,
            RequiredAction::EditFile {
                finding_index: 1,
                ..
            }
        ));
    }
}
