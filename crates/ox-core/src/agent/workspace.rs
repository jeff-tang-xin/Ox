//! Structured workflow workspace — single LLM context block per iteration.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;
use super::findings::{self, FindingStatus, FindingsStore};

pub const WORKSPACE_TAG: &str = "[WORKSPACE]";

/// When true, skip legacy DURABLE_MEMORY / heavy STEP_MEMORY (workspace is canonical).
pub fn uses_workspace_memory(engine: &WorkflowEngine) -> bool {
    if !engine.is_workflow_active() || engine.is_workflow_complete() {
        return false;
    }
    let step = engine.get_current_step_index();
    // Single-step (0) and legacy execute step (3).
    step == 0 || step == 3
}

/// Minimal addon when workspace mode is active (skills only).
pub fn minimal_durable_addon(engine: &WorkflowEngine) -> String {
    let guidance = engine.workflow_guidance_block();
    if guidance.is_empty() {
        String::new()
    } else {
        format!("{}\n\n{}", super::memory_bridge::DURABLE_MEMORY_TAG, guidance)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    ExecuteReview,
    Parked,
    FeedbackDiscuss,
    ScopeConfirm,
    ExecuteImpl,
    Paused,
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
        let mode = infer_mode(engine);
        let user_request = engine
            .get_variable("_current_user_request")
            .unwrap_or_default();
        let store = findings::load_or_migrate(engine);
        let (findings_summary, findings, active_indices) = if let Some(ref s) = store {
            (
                s.summary.clone(),
                s.findings
                    .iter()
                    .map(workspace_finding_from)
                    .collect(),
                s.active_indices.clone(),
            )
        } else {
            (String::new(), Vec::new(), Vec::new())
        };

        let files_read: Vec<ReadSlot> = engine
            .get_variable("_impl_files_read")
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|slot| parse_read_slot(&slot))
            .collect();

        let file_digests: Vec<crate::agent::tool_digest::FileDigest> =
            crate::agent::tool_digest::all_digests(engine);

        let files_edited = engine
            .get_variable("_impl_files_edited")
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default();

        let required_action = compute_required_action(engine, mode, &store, &files_read, &files_edited);
        let forbidden = forbidden_for_mode(mode);
        let phase_notes = phase_notes_for_mode(mode);

        Some(Self {
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
        })
    }

    pub fn format_for_llm(&self) -> String {
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        format!(
            "{WORKSPACE_TAG}\n\
             ## 当前工作区（唯一真相 — 按 mode 与 required_action 执行）\n\
             ```json\n{json}\n```"
        )
    }
}

fn infer_mode(engine: &WorkflowEngine) -> WorkspaceMode {
    if crate::agent::workflow_session::is_paused(engine) {
        return WorkspaceMode::Paused;
    }
    if crate::agent::workflow_session::is_scope_confirm(engine) {
        return WorkspaceMode::ScopeConfirm;
    }
    if crate::agent::workflow_session::is_feedback_discuss(engine) {
        return WorkspaceMode::FeedbackDiscuss;
    }
    if crate::agent::workflow_session::is_implementation_phase(engine) {
        return WorkspaceMode::ExecuteImpl;
    }
    if crate::agent::workflow_session::is_parked(engine) {
        return WorkspaceMode::Parked;
    }
    if engine.get_current_step_index() == 3 && engine.is_perceive_execute() {
        if crate::agent::workflow_session::is_parked(engine)
            || engine.execute_report_already_delivered()
        {
            return WorkspaceMode::Parked;
        }
        return WorkspaceMode::ExecuteReview;
    }
    // Single-step: review/explain by default; implementation only when user asks to fix.
    if engine.is_single_step() {
        let user = engine
            .get_variable("_current_user_request")
            .unwrap_or_default();
        if crate::agent::workflow_session::looks_like_implementation_request(&user) {
            return WorkspaceMode::ExecuteImpl;
        }
        return WorkspaceMode::ExecuteReview;
    }
    // Legacy default
    WorkspaceMode::ExecuteImpl
}

fn compute_required_action(
    engine: &WorkflowEngine,
    mode: WorkspaceMode,
    store: &Option<FindingsStore>,
    files_read: &[ReadSlot],
    files_edited: &[String],
) -> RequiredAction {
    match mode {
        WorkspaceMode::ExecuteReview => {
            if store
                .as_ref()
                .is_some_and(|s| !s.findings.is_empty())
            {
                return RequiredAction::AwaitUser;
            }
            if engine.execute_report_already_delivered() {
                return RequiredAction::AwaitUser;
            }
            RequiredAction::EmitFindingsAndDone
        }
        WorkspaceMode::Parked | WorkspaceMode::ScopeConfirm => RequiredAction::AwaitUser,
        WorkspaceMode::FeedbackDiscuss => RequiredAction::DiscussOnly,
        WorkspaceMode::Paused => RequiredAction::AwaitUser,
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
                let has_read = !f.file.is_empty()
                    && files_read.iter().any(|r| plan_tracker_normalize(&r.path) == norm_path);
                let has_edit = !f.file.is_empty()
                    && files_edited.iter().any(|p| plan_tracker_normalize(p) == norm_path);
                if !f.file.is_empty() && !has_read {
                    return RequiredAction::ReadFile {
                        path: f.file.clone(),
                        offset: 0,
                        limit: 200,
                        finding_index: idx,
                    };
                }
                if !has_edit {
                    return RequiredAction::EditFile {
                        path: f.file.clone(),
                        finding_index: idx,
                    };
                }
                if f.status == FindingStatus::AwaitingVerify {
                    if let Some(tracker) = engine.get_plan_tracker() {
                        if let Some(step) = tracker.steps.iter().find(|s| s.index == idx) {
                            if !step.verify.is_empty() {
                                return RequiredAction::Verify {
                                    command: step.verify.clone(),
                                    finding_index: idx,
                                };
                            }
                        }
                    }
                }
            }
            RequiredAction::EmitCompletionReceipt
        }
    }
}

fn forbidden_for_mode(mode: WorkspaceMode) -> Vec<String> {
    match mode {
        WorkspaceMode::ExecuteReview => vec![
            "edit_file".into(),
            "file_write".into(),
            "delete_range".into(),
            "复述已完成探索".into(),
        ],
        WorkspaceMode::FeedbackDiscuss => vec![
            "edit_file".into(),
            "file_write".into(),
            "shell_exec".into(),
        ],
        WorkspaceMode::ExecuteImpl => vec![
            "code_search".into(),
            "file_list".into(),
            "file_search".into(),
            "find_symbol".into(),
            "shell_exec cat".into(),
            "复述审查报告".into(),
        ],
        WorkspaceMode::Parked | WorkspaceMode::ScopeConfirm | WorkspaceMode::Paused => {
            vec!["工具调用".into()]
        }
    }
}

fn phase_notes_for_mode(mode: WorkspaceMode) -> String {
    match mode {
        WorkspaceMode::ExecuteReview => {
            "只读审查：输出报告 + findings JSON + ## Done（报告已出则等待用户 /fix /discuss）".into()
        }
        WorkspaceMode::Parked => "已 Park — 用户将选择范围或讨论".into(),
        WorkspaceMode::ScopeConfirm => "等待用户确认实施范围".into(),
        WorkspaceMode::FeedbackDiscuss => {
            "讨论模式：直接回应用户；禁止重出审查报告 / findings JSON / ## Done".into()
        }
        WorkspaceMode::ExecuteImpl => "实施：一次推进一个 finding，遵守 required_action".into(),
        WorkspaceMode::Paused => "已暂停 — /resume 继续".into(),
    }
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

fn parse_read_slot(slot: &str) -> Option<ReadSlot> {
    let (path, rest) = slot.split_once('@')?;
    let (offset, limit) = rest.split_once('+')?;
    Some(ReadSlot {
        path: path.to_string(),
        offset: offset.parse().ok()?,
        limit: limit.parse().ok()?,
    })
}

fn plan_tracker_normalize(path: &str) -> String {
    crate::agent::plan_tracker::normalize_path(path)
}

pub fn inject_workspace(messages: &mut Vec<crate::message::Message>, engine: &WorkflowEngine) {
    let Some(ws) = WorkflowWorkspace::build(engine) else {
        return;
    };
    let step = engine.get_current_step_index();
    let inject = step == 0
        || step == 3
        || matches!(
            ws.mode,
            WorkspaceMode::Parked
                | WorkspaceMode::ScopeConfirm
                | WorkspaceMode::FeedbackDiscuss
                | WorkspaceMode::Paused
        );
    if inject {
        strip_workspace(messages);
        messages.push(crate::message::Message::system(&ws.format_for_llm()));
    }
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
    fn build_review_mode() {
        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        engine.set_variable("_current_user_request", "review".into());
        // step 0 default — no workspace for non-step3
        assert!(WorkflowWorkspace::build(&engine).is_some());
    }
}
