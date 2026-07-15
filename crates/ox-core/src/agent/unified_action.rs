//! Single exit tool: `complete_and_check` — all LLM actions route through here.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::llm::ToolSchema;
use crate::tools::SafetyLevel;

use super::engine::WorkflowEngine;
use super::phase::{self, SingleFlowPhase};
use super::workspace::WorkspaceMode;

pub const TOOL_NAME: &str = "complete_and_check";
pub const UNIFIED_ROUTE_TAG: &str = "[UNIFIED_ROUTE]";

/// Example call shape for injection blocks.
pub const UNIFIED_CALL_EXAMPLE: &str =
    r#"complete_and_check({"action":"file_read","params":{"path":"src/main.rs"}})"#;

/// Parsed LLM request body for `complete_and_check`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedActionRequest {
    pub action: String,
    #[serde(default)]
    pub params: Value,
}

/// Structured session summary returned by the LLM on finish.
/// Tells us what the LLM read, modified, and learned this session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionSummary {
    #[serde(default)]
    pub learnings: String,
    #[serde(default)]
    pub key_facts: Vec<KeyFact>,
    #[serde(default)]
    pub files_read: Vec<FileReadRecord>,
    #[serde(default)]
    pub files_modified: Vec<FileModifiedRecord>,
    #[serde(default)]
    pub skills: Vec<SessionSkill>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFact {
    pub fact: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadRecord {
    pub path: String,
    #[serde(default)]
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileModifiedRecord {
    pub path: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSkill {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub description: String,
}

/// Extract SessionSummary from finish params (if present).
pub fn parse_session_summary(params: &Value) -> Option<SessionSummary> {
    let v = params.get("session_summary")?;
    serde_json::from_value(v.clone()).ok()
}

/// Gate classification for sparse human blocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionGate {
    None,
    Safety,
    Business,
    Finish,
}

/// Outcome of handling one unified action (before raw tool execution details).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedRoute {
    DelegateTool,
    /// Single terminal action: yields the turn. Gate decided by presence of `finding_json`.
    Finish,
    /// Retrieve offloaded content: memory-graph node replay (`#N`) or offloader node_id.
    Recall,
    Unknown,
}

pub fn parse_request(arguments: &str) -> Result<UnifiedActionRequest, String> {
    let candidate = super::tool_args_repair::repair_unified_arguments(arguments)
        .unwrap_or_else(|| arguments.trim().to_string());
    if candidate.is_empty() {
        return Err(
            "empty arguments — 必须发送合法 JSON：{\"action\":\"file_read\",\"params\":{\"path\":\"…\"}}"
                .into(),
        );
    }
    let mut value: Value = serde_json::from_str(&candidate).map_err(|e| {
        format!("invalid JSON: {e} — 用 JSON：{{\"action\":\"…\",\"params\":{{…}}}}")
    })?;
    super::tool_args_repair::normalize_unified_value(&mut value)?;
    serde_json::from_value(value).map_err(|e| format!("invalid unified request: {e}"))
}

pub fn route(req: &UnifiedActionRequest) -> UnifiedRoute {
    match req.action.as_str() {
        // `finish` is the single terminal action. `deliver`/`report`/`done` alias to it
        // for backwards compatibility — behavior is decided solely by `finding_json`.
        "finish" | "deliver" | "report" | "done" | "complete" => UnifiedRoute::Finish,
        "recall" => UnifiedRoute::Recall,
        a if action_to_tool_name(a).is_some() => UnifiedRoute::DelegateTool,
        _ => UnifiedRoute::Unknown,
    }
}

pub fn action_to_tool_name(action: &str) -> Option<&'static str> {
    match action {
        "file_read" => Some("file_read"),
        "file_write" => Some("file_write"),
        "edit_file" => Some("edit_file"),
        "file_list" => Some("file_list"),
        "file_search" => Some("file_search"),
        "code_search" => Some("code_search"),
        "delete_range" => Some("delete_range"),
        "find_symbol" => Some("find_symbol"),
        "load_skill" => Some("load_skill"),
        "shell_exec" => Some("shell_exec"),
        "project_detect" => Some("project_detect"),
        "web_fetch" => Some("web_fetch"),
        "git_status" => Some("git_status"),
        "git_diff" => Some("git_diff"),
        "code_graph" => Some("code_graph"),
        "read" => Some("file_read"),
        "write" => Some("file_write"),
        "edit" => Some("edit_file"),
        "git" => Some("git_status"),
        _ => None,
    }
}

pub fn gate_for_action(action: &str, safety: SafetyLevel) -> ActionGate {
    match action {
        "finish" => ActionGate::Finish,
        "shell_exec" => ActionGate::Safety,
        "file_write" | "edit_file" | "edit" | "write" | "delete_range" => ActionGate::Safety,
        _ => match safety {
            SafetyLevel::Safe => ActionGate::None,
            SafetyLevel::RequiresConfirmation | SafetyLevel::Dangerous => ActionGate::Safety,
        },
    }
}

/// Free-text shown in chat (analysis / answer / summary). Accepts `content` or `summary`.
pub fn finish_content(params: &Value) -> String {
    params
        .get("content")
        .or_else(|| params.get("summary"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract review items as a findings-JSON string suitable for `ensure_from_review_output`.
///
/// Returns `Some(json)` when the call carries reviewable content (`finding_json`/`findings`
/// param, or a findings JSON block embedded in `content`); `None` when there is nothing to
/// review (turn should simply end and wait for the user).
pub fn finding_json(params: &Value) -> Option<String> {
    // Structured param first: finding_json | findings (object or array).
    for key in ["finding_json", "findings"] {
        if let Some(v) = params.get(key) {
            if v.is_null() {
                continue;
            }
            // Normalize: array → {findings:[...]}; object passed through.
            let normalized = if v.is_array() {
                json!({ "findings": v })
            } else if v.is_object() {
                // Already has findings? keep; else wrap single object as one finding.
                if v.get("findings").is_some() {
                    v.clone()
                } else {
                    json!({ "findings": [v] })
                }
            } else {
                continue;
            };
            let has_items = normalized
                .get("findings")
                .and_then(|f| f.as_array())
                .is_some_and(|a| !a.is_empty());
            if has_items {
                if let Some(summary) = params.get("findings_summary").and_then(|s| s.as_str()) {
                    let mut obj = normalized;
                    if obj.get("findings_summary").is_none() {
                        obj["findings_summary"] = json!(summary);
                    }
                    return serde_json::to_string(&obj).ok();
                }
                return serde_json::to_string(&normalized).ok();
            }
        }
    }
    // Fallback: a findings JSON block embedded in free-text content.
    // MUST verify the content actually contains a parseable JSON findings
    // block — a simple `"findings"` string match is too aggressive and
    // causes discussion replies mentioning findings to enter the scope gate,
    // leading to a 300s timeout that kills the agent turn.
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(extracted) = crate::agent::perception::extract_json_block(content)
        && serde_json::from_str::<serde_json::Value>(&extracted).is_ok()
    {
        return Some(content.to_string());
    }
    None
}

pub fn tool_schema_with_actions(actions: &[&str]) -> ToolSchema {
    ToolSchema {
        name: TOOL_NAME.to_string(),
        description:
            "唯一工具：所有读取/写入/结束都通过它，由 action 决定。结束本轮用 action=finish。"
                .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "动作类型",
                    "enum": actions
                },
                "params": {
                    "type": "object",
                    "description": "动作参数。file_read:{path}; find_symbol:{name}; edit_file:{path,old_string,new_string}; code_graph:{op,...}(代码图谱: op=query/context/impact/detect_changes/api_impact/cypher/rename… 改前查影响面); finish:{content?, finding_json?}。finish 时：有 finding_json(需用户审核的plan/bug/改动)→门禁确认；无→结束等用户。禁止 symbol/key 代替 name/node_id。"
                }
            },
            "required": ["action", "params"]
        }),
    }
}

pub fn tool_schema() -> ToolSchema {
    tool_schema_with_actions(ALL_ACTIONS)
}

const ALL_ACTIONS: &[&str] = &[
    "file_read",
    "file_write",
    "edit_file",
    "file_list",
    "file_search",
    "code_search",
    "delete_range",
    "find_symbol",
    "code_graph",
    "load_skill",
    "shell_exec",
    "project_detect",
    "web_fetch",
    "git_status",
    "git_diff",
    "finish",
];

pub fn unified_tool_schemas() -> Vec<ToolSchema> {
    vec![tool_schema()]
}

pub fn unified_tool_schemas_for_engine(engine: &WorkflowEngine) -> Vec<ToolSchema> {
    let allowed = allowed_actions_for_engine(engine);
    if allowed.is_empty() {
        return Vec::new();
    }
    vec![tool_schema_with_actions(&allowed)]
}

struct UnifiedRouteSpec {
    recommended: Vec<&'static str>,
    allowed: Vec<&'static str>,
    blocked: Vec<&'static str>,
    note: &'static str,
}

fn unified_route_spec(engine: &WorkflowEngine) -> UnifiedRouteSpec {
    let phase = phase::get(engine);
    let report_done = engine.execute_report_already_delivered();
    let has_findings =
        super::findings::load_or_migrate(engine).is_some_and(|s| !s.findings.is_empty());

    let (recommended, allowed, blocked, note) = match phase {
        SingleFlowPhase::Complete => (
            // Not a lock: finish handed the turn back to the user. If the model is
            // somehow re-invoked before a new round resets state, keep read-only
            // exploration + finish available so it can never strand with no tools.
            vec!["finish"],
            vec![
                "file_read",
                "find_symbol",
                "code_search",
                "code_graph",
                "file_list",
                "file_search",
                "load_skill",
                "git_status",
                "git_diff",
                "finish",
            ],
            vec!["edit_file", "file_write", "delete_range", "shell_exec"],
            "本轮已收尾 — 等用户新输入；如需继续可只读探索或 finish。",
        ),
        SingleFlowPhase::AwaitUser => {
            if super::business_gate::scope_implementation_unlocked(engine) {
                (
                    vec!["code_graph", "file_read", "edit_file", "shell_exec"],
                    vec![
                        "file_read",
                        "edit_file",
                        "file_write",
                        "delete_range",
                        "find_symbol",
                        "code_graph",
                        "shell_exec",
                        "git_status",
                        "git_diff",
                        "load_skill",
                        "finish",
                    ],
                    vec!["code_search", "file_search", "file_list"],
                    "实施：改前可 code_graph(op=impact) 看爆炸半径 → file_read → edit_file；完成后 action=finish（无 finding_json → 结束）。",
                )
            } else {
                // Discussion mode: read-only + finish to respond. Writes blocked individually.
                (
                    vec!["finish", "file_read", "find_symbol", "code_graph"],
                    vec![
                        "file_read",
                        "find_symbol",
                        "code_search",
                        "code_graph",
                        "file_list",
                        "file_search",
                        "load_skill",
                        "git_status",
                        "git_diff",
                        "finish",
                    ],
                    vec!["edit_file", "file_write", "delete_range", "shell_exec"],
                    "讨论：finish(content=...) 回应用户；只读探索(含 code_graph)；禁止 edit/write/shell。",
                )
            }
        }
        SingleFlowPhase::Implement => (
            vec!["code_graph", "file_read", "edit_file", "shell_exec"],
            vec![
                "file_read",
                "edit_file",
                "file_write",
                "delete_range",
                "find_symbol",
                "code_graph",
                "shell_exec",
                "git_status",
                "git_diff",
                "load_skill",
                "finish",
                "code_search",
            ],
            vec!["file_search", "file_list"],
            "实施：改前可 code_graph(op=impact/context) 核对影响面 → file_read → edit_file；验证用 shell_exec；完成后 action=finish（无 finding_json → 结束）。",
        ),
        SingleFlowPhase::Review | SingleFlowPhase::Receive => {
            if report_done && has_findings {
                (
                    vec!["finish"],
                    vec!["load_skill", "finish"],
                    vec![
                        "find_symbol",
                        "code_search",
                        "file_search",
                        "file_list",
                        "file_read",
                        "edit_file",
                        "file_write",
                    ],
                    "已分析完 — action=finish 提交 finding_json（需用户审核的 plan/bug/改动）。",
                )
            } else {
                (
                    vec![
                        "code_graph",
                        "project_detect",
                        "file_list",
                        "file_read",
                        "find_symbol",
                    ],
                    vec![
                        "project_detect",
                        "file_list",
                        "file_search",
                        "file_read",
                        "find_symbol",
                        "code_search",
                        "code_graph",
                        "load_skill",
                        "git_status",
                        "git_diff",
                        "finish",
                    ],
                    vec!["edit_file", "file_write", "delete_range", "shell_exec"],
                    "探索(只读)：先用 code_graph op=list_repos 看有哪些仓库，再用 code_graph op=query/context/impact 建关系模型+影响面，配合 file_read 核证 → finish 提交 finding_json 确认一次 → 实施 → finish 结束。禁止未确认前改代码。",
                )
            }
        }
    };

    UnifiedRouteSpec {
        recommended,
        allowed,
        blocked,
        note,
    }
}

/// Phase-filtered action names for schema enum + routing hints.
pub fn allowed_actions_for_engine(engine: &WorkflowEngine) -> Vec<&'static str> {
    unified_route_spec(engine).allowed
}

/// Build per-iteration `[UNIFIED_ROUTE]` injection (replaces legacy `[TOOL_ROUTE]`).
pub fn build_unified_route(engine: &WorkflowEngine) -> String {
    let phase = phase::get(engine);
    let mode = phase::workspace_mode(engine);
    let intent = engine.get_task_intent();
    let spec = unified_route_spec(engine);

    let mode_note = match mode {
        WorkspaceMode::ScopeConfirm => "模式: 确认范围",
        WorkspaceMode::FeedbackDiscuss => "模式: 讨论反馈",
        WorkspaceMode::ExecuteImpl => "模式: 实施",
        WorkspaceMode::ExecuteReview => "模式: 审查",
    };

    let mut out = format!(
        "{UNIFIED_ROUTE_TAG}\n\
         ALL-TOOLING：唯一出口 `complete_and_check` — 禁止 assistant 纯文本交付。\n\
         调用形态: {UNIFIED_CALL_EXAMPLE}\n\
         phase={} | intent={} | {mode_note}\n",
        phase.as_str(),
        intent.as_str()
    );
    if !spec.recommended.is_empty() {
        out.push_str(&format!(
            "推荐: {}\n",
            spec.recommended
                .iter()
                .map(|a| format!("action={a}"))
                .collect::<Vec<_>>()
                .join(" → ")
        ));
    }
    if !spec.allowed.is_empty() {
        out.push_str(&format!("允许 action: {}\n", spec.allowed.join(", ")));
    }
    if !spec.blocked.is_empty() {
        out.push_str(&format!("禁止 action: {}\n", spec.blocked.join(", ")));
    }
    if spec.allowed.contains(&"code_graph") {
        out.push_str(
            "🕸 **code_graph (GitNexus 代码图谱) — 优先使用**：\n\
             \n\
             **理解代码时必用**：\n\
             • op=query → 根据概念找执行流程（如 主流程/auth流程）\n\
             • op=context → 查单个符号的 360° 视图（谁调谁、读写关系）\n\
             \n\
             **改动前必用**：\n\
             • op=impact → 改动爆炸半径分析（改 X 会影响哪些地方）\n\
             • op=detect_changes → 未提交改动的影响面\n\
             • op=api_impact → API 路由改动分析\n\
             \n\
             **比 grep/file_read 更强**：理解调用关系、执行流程、模块边界。\n\
             **默认策略**：先 code_graph query，再 file_read 深入。\n",
        );
    }
    out.push_str(
        "结束本轮 = 你主动调 `finish`（深思后的收尾，结束本轮、交还用户；不锁后续）：\n\
         • 有需用户审核的内容(plan/bug/将改动) → finish(params.finding_json=[...]) → 门禁仅校验，等 c 确认后继续\n\
         • 已完成/纯分析/回答 → finish(params.content=…) 收尾\n\
         • **用户明确拒绝修复（说 不修复/不改/算了）→ 直接 finish(params.content=…) 结束，勿再生成 finding_json**\n\
         • 即使 finding_json 确认并改完代码，也由你**自己** finish 收尾；门禁/工具永不替你结束\n\
         • 中间想说明但还要继续 → 文字随下一个工具动作一起输出，勿用 finish 投递中间内容\n\
         finding_json 形态: {\"findings_summary\":\"…\",\"findings\":[{\"index\":1,\"severity\":\"high\",\"file\":\"…\",\"issue\":\"…\",\"recommendation\":\"…\",\"fix_plan\":\"第几行+怎么改+代码草图\"}]}\n\
         用户 c 确认后，本轮所有 edit/write/shell 自动执行，不再逐个确认；禁止改计划外文件。\n",
    );
    out.push_str(&format!("💡 {}", spec.note));
    out
}

/// Compact route block embedded inside `[WORKSPACE]` (unified mode — no separate injection).
pub fn build_unified_route_compact(engine: &WorkflowEngine) -> String {
    let spec = unified_route_spec(engine);
    let lock = if super::business_gate::scope_implementation_unlocked(engine) {
        "🔓 写权限已解锁 — edit/write/shell 自动执行（硬安全例外仍拦截）"
    } else if super::business_gate::is_pending_scope(engine) {
        "⏸ 等待用户 c 确认 — 禁止一切 action"
    } else {
        "🔒 只读 — 动手前先 finish(finding_json=[...]) 提交计划，用户 c 确认后解锁"
    };
    let mut out = format!("### 工具路由\n{lock}\n");
    if !spec.allowed.is_empty() {
        out.push_str(&format!("允许: {}\n", spec.allowed.join(", ")));
    }
    if !spec.blocked.is_empty() && spec.blocked != ["*"] {
        out.push_str(&format!("禁止: {}\n", spec.blocked.join(", ")));
    }
    if !spec.recommended.is_empty() {
        out.push_str(&format!("推荐: {}\n", spec.recommended.join(" → ")));
    }
    out.push_str(&format!("💡 {}", spec.note));
    out
}

/// Non-workflow sessions — minimal unified route block.
pub fn build_unified_route_fallback() -> String {
    format!(
        "[WORKSPACE]\n\
         ## 当前任务（非 workflow 会话）\n\n\
         **主流程:** 探索 → finish 提交 finding_json 确认一次 → 实施 → finish 结束\n\n\
         ### 工具路由\n\
         🔒 动手前先 finish(finding_json=[...]) 提交计划，用户 c 确认后解锁写权限\n\
         唯一出口: `complete_and_check` — {UNIFIED_CALL_EXAMPLE}\n\
         禁止 assistant 纯文本交付与 `## Done` prose。"
    )
}

pub fn allowed_actions_for_phase(phase: &str) -> &'static [&'static str] {
    match phase {
        "implement" => &[
            "file_read",
            "edit_file",
            "file_write",
            "find_symbol",
            "code_graph",
            "shell_exec",
            "finish",
        ],
        "await_user" => &["finish"],
        "review" => &[
            "file_read",
            "code_search",
            "find_symbol",
            "code_graph",
            "file_list",
            "project_detect",
            "finish",
        ],
        _ => &[
            "file_read",
            "file_write",
            "edit_file",
            "file_list",
            "file_search",
            "code_search",
            "delete_range",
            "find_symbol",
            "code_graph",
            "load_skill",
            "shell_exec",
            "project_detect",
            "web_fetch",
            "git_status",
            "git_diff",
            "finish",
        ],
    }
}

/// Loop-detection key for unified `complete_and_check` calls.
pub fn tool_loop_key(arguments: &str) -> String {
    let Ok(req) = parse_request(arguments) else {
        return format!("{TOOL_NAME}:invalid");
    };
    match req.action.as_str() {
        "finish" | "deliver" | "report" | "done" | "complete" => format!("{TOOL_NAME}:finish"),
        a if action_to_tool_name(a).is_some() => {
            let inner = action_to_tool_name(a).unwrap();
            delegate_tool_loop_key(inner, &req.params)
        }
        _ => format!("{TOOL_NAME}:unknown"),
    }
}

fn delegate_tool_loop_key(inner: &str, params: &Value) -> String {
    match inner {
        "file_list" => {
            let path = params.get("path").and_then(|p| p.as_str()).unwrap_or(".");
            format!("file_list:{}", WorkflowEngine::normalize_explore_path(path))
        }
        "file_read" => {
            let path = params.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let offset = params.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
            let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(200);
            format!(
                "file_read:{}@{}+{}",
                WorkflowEngine::normalize_explore_path(path),
                offset,
                limit
            )
        }
        other => {
            if let Some(path) = params.get("path").and_then(|p| p.as_str()) {
                format!("{other}:{}", WorkflowEngine::normalize_explore_path(path))
            } else {
                other.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_with_finding_json_routes_finish() {
        let req = parse_request(
            r#"{"action":"finish","params":{"finding_json":{"findings":[{"index":1,"issue":"x","recommendation":"y"}]}}}"#,
        )
        .unwrap();
        assert_eq!(req.action, "finish");
        assert_eq!(route(&req), UnifiedRoute::Finish);
        assert!(finding_json(&req.params).is_some());
    }

    #[test]
    fn finish_without_finding_json_has_none() {
        let req = parse_request(r#"{"action":"finish","params":{"content":"分析结果"}}"#).unwrap();
        assert_eq!(route(&req), UnifiedRoute::Finish);
        assert!(finding_json(&req.params).is_none());
        assert_eq!(finish_content(&req.params), "分析结果");
    }

    #[test]
    fn finish_with_empty_findings_array_is_none() {
        // Empty array yields None — handle_finish must catch this (key present,
        // parse None) and error instead of silently ending the turn.
        let req = parse_request(r#"{"action":"finish","params":{"finding_json":[]}}"#).unwrap();
        assert!(finding_json(&req.params).is_none());
        // The key is nonetheless present and non-null → "attempted findings".
        assert!(
            req.params
                .get("finding_json")
                .map(|v| !v.is_null())
                .unwrap_or(false)
        );
    }

    #[test]
    fn finish_with_null_finding_json_is_not_attempted() {
        // Explicit null must NOT count as an attempted-findings submission — it's
        // a normal end-of-turn.
        let req =
            parse_request(r#"{"action":"finish","params":{"finding_json":null,"content":"done"}}"#)
                .unwrap();
        assert!(finding_json(&req.params).is_none());
        assert!(
            !req.params
                .get("finding_json")
                .map(|v| !v.is_null())
                .unwrap_or(false)
        );
    }

    #[test]
    fn fallback_route_tag() {
        let b = build_unified_route_fallback();
        assert!(b.contains(crate::agent::workspace::WORKSPACE_TAG));
        assert!(b.contains("complete_and_check"));
    }

    #[test]
    fn maps_read_alias() {
        assert_eq!(action_to_tool_name("read"), Some("file_read"));
    }

    #[test]
    fn tool_loop_key_file_read() {
        let key = tool_loop_key(
            r#"{"action":"file_read","params":{"path":"src/a.rs","offset":0,"limit":200}}"#,
        );
        assert!(key.contains("file_read"));
        assert!(key.contains("src/a.rs"));
    }

    #[test]
    fn tool_loop_key_invalid() {
        assert_eq!(tool_loop_key(""), format!("{TOOL_NAME}:invalid"));
    }
}
