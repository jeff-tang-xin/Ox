//! Session flags for discuss / scope-confirm overlays on the single-step model.

use super::engine::WorkflowEngine;

const FEEDBACK_DISCUSS_KEY: &str = "_feedback_discuss";

pub fn is_parked(_: &WorkflowEngine) -> bool {
    false
}
pub fn park(_: &WorkflowEngine) {}
pub fn unpark(_: &WorkflowEngine) {}
pub fn clear_session_flags(engine: &WorkflowEngine) {
    clear_feedback_discuss(engine);
}
pub fn is_feedback_discuss(engine: &WorkflowEngine) -> bool {
    engine.get_variable(FEEDBACK_DISCUSS_KEY).as_deref() == Some("1")
}
pub fn enter_feedback_discuss(engine: &WorkflowEngine) {
    engine.set_variable(FEEDBACK_DISCUSS_KEY, "1".to_string());
}
pub fn clear_feedback_discuss(engine: &WorkflowEngine) {
    engine.set_variable(FEEDBACK_DISCUSS_KEY, String::new());
}
pub fn validate_feedback_discuss_tool(
    engine: &WorkflowEngine,
    tool_name: &str,
) -> Result<(), String> {
    if !is_feedback_discuss(engine) {
        return Ok(());
    }
    const BLOCKED: &[&str] = &[
        "edit_file",
        "file_write",
        "delete_range",
        "run_command",
        "shell_exec",
    ];
    if BLOCKED.contains(&tool_name) {
        return Err(format!(
            "❌ 讨论模式（只读）禁止 `{tool_name}` — 直接回应用户即可。"
        ));
    }
    Ok(())
}
pub fn compact_discuss_session(_: &mut Vec<crate::message::Message>) {}
pub fn is_implementation_phase(engine: &WorkflowEngine) -> bool {
    matches!(
        crate::agent::phase::get(engine),
        crate::agent::phase::SingleFlowPhase::Implement
    )
}
pub fn enter_implementation_phase(_: &WorkflowEngine) {}
pub fn mark_execute_approved(_: &WorkflowEngine) {}
pub fn is_execute_user_approved(engine: &WorkflowEngine) -> bool {
    crate::agent::business_gate::scope_implementation_unlocked(engine)
}
pub fn is_scope_confirm(engine: &WorkflowEngine) -> bool {
    crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::AwaitUser
        && !is_feedback_discuss(engine)
        && crate::agent::findings::load_or_migrate(engine).is_some_and(|s| !s.findings.is_empty())
}
pub fn enter_scope_confirm(_: &WorkflowEngine) {}
pub fn leave_scope_confirm(_: &WorkflowEngine) {}
pub fn is_paused(_: &WorkflowEngine) -> bool {
    false
}
pub fn enter_paused(_: &WorkflowEngine) {}
pub fn leave_paused(_: &WorkflowEngine) {}
pub fn validate_session_tool(_: &WorkflowEngine, _: &str) -> Result<(), String> {
    Ok(())
}
pub fn looks_like_workflow_continuation(user_text: &str) -> bool {
    looks_like_fix_continuation(user_text)
}
pub fn looks_like_post_failure_fix(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    [
        "还有报错",
        "仍有错误",
        "编译失败",
        "构建失败",
        "没通过",
        "build failed",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
        && ["修复", "fix", "改", "解决", "处理"]
            .iter()
            .any(|k| t.contains(k) || lower.contains(k))
}
pub fn looks_like_fix_continuation(user_text: &str) -> bool {
    looks_like_implementation_request(user_text) || looks_like_post_failure_fix(user_text)
}

pub fn looks_like_implementation_request(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    [
        "修复",
        "fix",
        "改",
        "解决",
        "处理",
        "执行",
        "实施",
        "implement",
        "/fix",
        "apply fix",
        "resolve finding",
        "按你说的",
        "动手",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
}

pub fn looks_like_review_follow_up(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    if looks_like_fix_continuation(user_text) || looks_like_new_task(user_text) {
        return false;
    }
    let lower = t.to_lowercase();
    [
        "finding",
        "第",
        "条",
        "什么意思",
        "为何",
        "为什么",
        "explain",
        "clarify",
        "不对",
        "不同意",
        "漏了",
        "补充",
        "详细",
        "展开",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
}
pub fn looks_like_new_task(user_text: &str) -> bool {
    let t = user_text.trim();
    t.starts_with("/new") || t.starts_with("/reset")
}
pub fn implementation_phase_system_note() -> &'static str {
    ""
}
pub fn resume_message(user_text: &str, _: Option<&str>) -> String {
    user_text.to_string()
}
pub fn looks_like_workflow_continuation_alias(user_text: &str) -> bool {
    looks_like_workflow_continuation(user_text)
}
