//! Phase helpers — thin delegation to the canonical [`SingleFlowPhase`] state machine.
//!
//! These functions exist only for backward compatibility with callers that
//! reference `workflow_phases::get_phase()` etc. The real phase logic lives
//! in [`super::phase`].

use super::engine::WorkflowEngine;
use super::phase::{self, SingleFlowPhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPhase {
    Act,
    Perceive,
    Think,
}

/// Map canonical [`SingleFlowPhase`] → legacy [`WorkflowPhase`].
///
/// - `Implement` → `Act` (engaged in code changes)
/// - `Complete` → `Think` (wrapping up)
/// - Everything else → `Perceive` (exploring / discussing)
pub fn get_phase(engine: &WorkflowEngine) -> WorkflowPhase {
    match phase::get(engine) {
        SingleFlowPhase::Implement => WorkflowPhase::Act,
        SingleFlowPhase::Complete => WorkflowPhase::Think,
        SingleFlowPhase::Receive | SingleFlowPhase::Review | SingleFlowPhase::AwaitUser => {
            WorkflowPhase::Perceive
        }
    }
}

/// No-op — transitions are handled by [`phase::transition`].
pub fn set_phase(_: &WorkflowEngine, _: WorkflowPhase) {}

/// No-op — [`phase::transition`] drives all state changes.
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
