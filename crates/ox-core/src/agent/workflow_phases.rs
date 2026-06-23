//! Legacy phase state machine — stubbed for single-step + gatekeeper model.

use super::engine::WorkflowEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPhase {
    Act,
    Perceive,
    Think,
}

pub fn set_phase(_: &WorkflowEngine, _: WorkflowPhase) {}
pub fn get_phase(engine: &WorkflowEngine) -> WorkflowPhase {
    if engine.is_single_step() {
        WorkflowPhase::Perceive
    } else {
        WorkflowPhase::Act
    }
}
pub fn sync_phase(_: &WorkflowEngine) {}
pub fn infer_phase(engine: &WorkflowEngine) -> WorkflowPhase {
    get_phase(engine)
}

pub fn allows_midflight_interjection(_: &WorkflowEngine) -> bool {
    true
}

pub fn accepts_user_round_input(_: &WorkflowEngine, _: &str) -> bool {
    true
}

pub fn act_interjection_blocked_message() -> &'static str {
    ""
}

pub fn validate_act_tool(_: &WorkflowEngine, _: &str) -> Result<(), String> {
    Ok(())
}

pub fn clear_phase(_: &WorkflowEngine) {}

pub const FINDINGS_JSON_SCHEMA: &str = "";

pub fn phase_prompt_addon(_: &WorkflowEngine) -> &'static str {
    ""
}

pub fn phase_banner(_: &WorkflowEngine) -> &'static str {
    ""
}

pub fn phase_context_block(_: &WorkflowEngine) -> String {
    String::new()
}
