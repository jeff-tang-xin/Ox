//! Tool routing graph — phase-aware tool availability and recommended paths.
//!
//! Injected each LLM iteration as `[TOOL_ROUTE]` so the model knows which tools
//! to call without scanning a flat tool list.

use super::engine::WorkflowEngine;
use super::phase::{self, SingleFlowPhase};
use super::workspace::WorkspaceMode;

pub const TOOL_ROUTE_TAG: &str = "[TOOL_ROUTE]";

struct ToolRouteSpec {
    recommended: Vec<&'static str>,
    allowed: Vec<&'static str>,
    blocked: Vec<&'static str>,
    note: &'static str,
}

fn route_spec(engine: &WorkflowEngine) -> ToolRouteSpec {
    let phase = phase::get(engine);
    let report_done = engine.execute_report_already_delivered();
    let has_findings =
        super::findings::load_or_migrate(engine).is_some_and(|s| !s.findings.is_empty());

    let (recommended, allowed, blocked, note) = match phase {
        SingleFlowPhase::Complete => (
            // Not a lock: the turn was handed back to the user. Keep read-only
            // tools available so a re-invocation never strands with no tools.
            vec![],
            vec![
                "file_read",
                "find_symbol",
                "code_search",
                "file_list",
                "file_search",
                "load_skill",
                "git_status",
                "git_diff",
            ],
            vec!["edit_file", "file_write", "delete_range", "shell_exec"],
            "本轮已收尾 — 等用户新输入；如需继续可只读探索。",
        ),
        SingleFlowPhase::AwaitUser => {
            if super::gate::business_gate::scope_implementation_unlocked(engine) {
                (
                    vec!["file_read", "edit_file", "shell_exec"],
                    vec![
                        "file_read",
                        "edit_file",
                        "file_write",
                        "delete_range",
                        "find_symbol",
                        "shell_exec",
                        "git_status",
                        "git_diff",
                        "load_skill",
                    ],
                    vec!["code_search", "file_search", "file_list"],
                    "实施：业务流程门禁已确认；edit 前走安全门禁（Allow/Deny）。",
                )
            } else {
                (
                    vec![],
                    vec![],
                    vec!["*"],
                    "业务流程门禁：待确认 findings 范围 — 面板选 finding 后 c /confirm。",
                )
            }
        }
        SingleFlowPhase::Implement => (
            vec!["file_read", "edit_file", "shell_exec"],
            vec![
                "file_read",
                "edit_file",
                "file_write",
                "delete_range",
                "find_symbol",
                "recall",
                "memory_search",
                "shell_exec",
                "git_status",
                "git_diff",
                "load_skill",
            ],
            vec!["code_search", "file_search", "file_list"],
            "实施：先 file_read 再 edit_file；定位符号用 find_symbol。",
        ),
        SingleFlowPhase::Review | SingleFlowPhase::Receive => {
            if report_done && has_findings {
                (
                    vec!["## Done"],
                    vec!["memory_search", "recall", "load_skill"],
                    vec![
                        "find_symbol",
                        "code_search",
                        "file_search",
                        "file_list",
                        "file_read",
                        "edit_file",
                        "file_write",
                    ],
                    "审查报告已提交 — 补全 ## Done / findings，或等待用户 /fix。",
                )
            } else {
                (
                    vec!["project_detect", "file_list", "file_read", "find_symbol"],
                    vec![
                        "project_detect",
                        "file_list",
                        "file_search",
                        "file_read",
                        "find_symbol",
                        "code_search",
                        "memory_search",
                        "recall",
                        "load_skill",
                        "git_status",
                        "git_diff",
                    ],
                    vec!["edit_file", "file_write", "delete_range"],
                    "审查(只读)：探索后输出 findings + ## Done；禁止改代码。",
                )
            }
        }
    };

    ToolRouteSpec {
        recommended,
        allowed,
        blocked,
        note,
    }
}

/// Tool names exposed to the LLM API for the current single-flow phase.
pub fn allowed_tool_names(engine: &WorkflowEngine) -> Vec<&'static str> {
    if !engine.is_single_step() {
        return vec![];
    }
    route_spec(engine).allowed
}

/// Filter registry schemas to phase-allowed tools.
pub fn filter_tool_schemas(
    all: &[crate::llm::ToolSchema],
    allowed: &[&'static str],
) -> Vec<crate::llm::ToolSchema> {
    if allowed.is_empty() {
        return Vec::new();
    }
    all.iter()
        .filter(|s| allowed.contains(&s.name.as_str()))
        .cloned()
        .collect()
}

/// Build a compact tool-route block for the current workflow state.
pub fn build_tool_route(engine: &WorkflowEngine) -> String {
    let phase = phase::get(engine);
    let mode = phase::workspace_mode(engine);
    let intent = engine.get_task_intent();
    let spec = route_spec(engine);

    let mode_note = match mode {
        WorkspaceMode::ScopeConfirm => "模式: 确认范围",
        WorkspaceMode::FeedbackDiscuss => "模式: 讨论反馈",
        WorkspaceMode::ExecuteImpl => "模式: 实施",
        WorkspaceMode::ExecuteReview => "模式: 审查",
    };

    let mut out = format!(
        "{TOOL_ROUTE_TAG}\nphase={} | intent={} | {mode_note}\n",
        phase.as_str(),
        intent.as_str()
    );
    if !spec.recommended.is_empty() {
        out.push_str(&format!("推荐路径: {}\n", spec.recommended.join(" → ")));
    }
    if !spec.allowed.is_empty() {
        out.push_str(&format!("可用: {}\n", spec.allowed.join(", ")));
    }
    if !spec.blocked.is_empty() {
        out.push_str(&format!("禁用: {}\n", spec.blocked.join(", ")));
    }
    out.push_str(&format!("💡 {}", spec.note));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::findings::{self, Finding, FindingStatus, FindingsStore, Severity};
    use crate::agent::phase::{self};
    use crate::agent::session::SessionState;
    use crate::agent::task_intent::TaskIntent;
    use crate::agent::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn test_engine_in_implement() -> WorkflowEngine {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        phase::on_round_started(&engine, TaskIntent::Review);
        let store = FindingsStore {
            summary: "1 issue".into(),
            findings: vec![Finding {
                index: 1,
                severity: Severity::High,
                file: "src/Foo.java".into(),
                symbol: "bar".into(),
                issue: "bug".into(),
                recommendation: "fix".into(),
                fix_plan: String::new(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![1],
        };
        findings::save(&engine, &store);
        phase::pivot_to_fix_mode(&engine, "/fix");
        engine
    }

    #[test]
    fn implement_allowed_tools_exclude_roam_search() {
        let engine = test_engine_in_implement();
        let allowed = allowed_tool_names(&engine);
        assert!(allowed.contains(&"file_read"));
        assert!(allowed.contains(&"edit_file"));
        assert!(!allowed.contains(&"code_search"));
    }

    #[test]
    fn await_user_exposes_no_tools() {
        use crate::agent::phase::{self, PhaseEvent};
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        phase::on_round_started(&engine, TaskIntent::Review);
        let store = FindingsStore {
            summary: "1".into(),
            findings: vec![Finding {
                index: 1,
                severity: Severity::High,
                file: "a.java".into(),
                symbol: "m".into(),
                issue: "x".into(),
                recommendation: "y".into(),
                fix_plan: String::new(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![1],
        };
        findings::save(&engine, &store);
        phase::transition(&engine, PhaseEvent::FindingsStored);
        assert_eq!(phase::get(&engine), SingleFlowPhase::AwaitUser);
        assert!(allowed_tool_names(&engine).is_empty());
    }
}
