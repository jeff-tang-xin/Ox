//! Gatekeeper — simplified single-step agent model.
//!
//! Replaces the multi-step workflow state machine (Intent→Plan→Review→Execute
//! plus Perceive/Think/Act phases and park/discuss/impl/scope flags) with:
//!
//! 1. ONE LLM loop with a single strong-format system prompt.
//! 2. A fixed pipeline of pure-function gates that runs after `## Done`.
//! 3. A global per-turn failure budget; on exhaustion the loop stops and asks
//!    the user instead of spinning.
//!
//! Gates reuse the existing AST / verify / findings / completion primitives, so
//! only the control flow is new — the verification logic is unchanged.

pub mod gate;
pub mod gates;

pub use gate::{
    Gate, GateCtx, GateOutcome, GateReport, GateRunner, current_failures, reset_failures,
};

/// Default per-turn global gate-failure budget before escalating to the user.
pub const DEFAULT_GATE_BUDGET: u32 = 8;

/// The standard `## Done` gate pipeline, in evaluation order.
pub fn standard_pipeline() -> GateRunner {
    GateRunner::new(
        vec![
            Box::new(gates::FormatGate),
            Box::new(gates::PlanGate),
            Box::new(gates::CitationGate),
            Box::new(gates::SyntaxGate),
            Box::new(gates::VerifyGate),
            Box::new(gates::ProvenanceGate),
            Box::new(gates::ScopeGate),
        ],
        DEFAULT_GATE_BUDGET,
    )
}
