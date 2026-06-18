//! Clarification / park menus — disabled in single-step + gatekeeper model.

use super::engine::WorkflowEngine;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentParseResult {
    pub routing: super::engine::IntentRouting,
    pub needs_clarification: bool,
    pub clarification_questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkDisambiguationResolution {
    ContinuePrevious { follow_up: String },
    Feedback { text: String },
    NewTask { task: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkFollowUpOutcome {
    Resolved(ParkDisambiguationResolution),
    NeedDetail { hint: String },
}

pub fn extract_clarification(v: &serde_json::Value) -> (bool, Vec<String>) {
    let needs = v
        .get("needs_clarification")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let questions: Vec<String> = v
        .get("clarification_questions")
        .and_then(|q| q.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    (needs && !questions.is_empty(), questions)
}

pub fn is_awaiting(_: &WorkflowEngine) -> bool {
    false
}

pub fn is_park_disambiguation(_: &WorkflowEngine) -> bool {
    false
}

pub fn is_intent_clarification(_: &WorkflowEngine) -> bool {
    false
}

pub fn pending_advance_step(_: &WorkflowEngine) -> usize {
    0
}

pub fn park_pending_input(_: &WorkflowEngine) -> String {
    String::new()
}

pub fn arm_gate(_: &WorkflowEngine, _: &[String], _: usize) {}

pub fn advance_step_after_intent_clarification(_: &str, _: bool) -> usize {
    0
}

pub fn clear_gate(_: &WorkflowEngine) {}

pub fn arm_park_follow_up_menu(_: &WorkflowEngine) {}

pub fn arm_park_disambiguation_gate(_: &WorkflowEngine, _: &str) {}

pub fn questions(_: &WorkflowEngine) -> Vec<String> {
    vec![]
}

pub fn format_markdown(_: &WorkflowEngine) -> String {
    String::new()
}

pub fn apply_answer(_: &WorkflowEngine, _: &str) {}

pub fn resolve_park_follow_up(
    _: &WorkflowEngine,
    _: &str,
) -> Result<ParkFollowUpOutcome, String> {
    Err("单步模式：直接输入你的需求即可，无需菜单选择。".into())
}

pub fn is_explicit_parked_continue(_: &str) -> bool {
    false
}

pub fn is_explicit_parked_new_task(user_text: &str) -> bool {
    let t = user_text.trim();
    t.starts_with("/new") || t.starts_with("新任务")
}

pub fn validate_intent_clarification_answer(answer: &str) -> Result<(), String> {
    if answer.trim().is_empty() {
        Err("请提供澄清说明。".into())
    } else {
        Ok(())
    }
}
