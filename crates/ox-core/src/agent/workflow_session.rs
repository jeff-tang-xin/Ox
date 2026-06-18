//! Legacy session flags — stubbed out for single-step + gatekeeper model.

use super::engine::WorkflowEngine;

pub fn is_parked(_: &WorkflowEngine) -> bool {
    false
}
pub fn park(_: &WorkflowEngine) {}
pub fn unpark(_: &WorkflowEngine) {}
pub fn clear_session_flags(_: &WorkflowEngine) {}
pub fn is_feedback_discuss(_: &WorkflowEngine) -> bool {
    false
}
pub fn enter_feedback_discuss(_: &WorkflowEngine) {}
pub fn clear_feedback_discuss(_: &WorkflowEngine) {}
pub fn validate_feedback_discuss_tool(_: &WorkflowEngine, _: &str) -> Result<(), String> {
    Ok(())
}
pub fn compact_discuss_session(_: &mut Vec<crate::message::Message>) {}
pub fn is_implementation_phase(_: &WorkflowEngine) -> bool {
    true
}
pub fn enter_implementation_phase(_: &WorkflowEngine) {}
pub fn mark_execute_approved(_: &WorkflowEngine) {}
pub fn is_execute_user_approved(_: &WorkflowEngine) -> bool {
    true
}
pub fn is_scope_confirm(_: &WorkflowEngine) -> bool {
    false
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
    ["还有报错", "仍有错误", "编译失败", "构建失败", "没通过", "build failed"]
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
    ["修复", "fix", "改", "解决", "处理", "继续", "执行", "implement"]
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
pub fn looks_like_review_follow_up(_: &str) -> bool {
    false
}
pub fn looks_like_workflow_continuation_alias(user_text: &str) -> bool {
    looks_like_workflow_continuation(user_text)
}
