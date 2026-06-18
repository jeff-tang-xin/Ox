//! Gate primitives — the heart of the simplified agent model.
//!
//! Instead of a multi-step workflow state machine, the agent runs ONE LLM
//! loop. When the model emits `## Done`, a fixed pipeline of pure-function
//! **gates** validates the result. Each gate either passes, rejects with
//! machine-generated feedback (the model retries), or escalates to the user.
//!
//! State is reduced to `messages + a global failure counter`, which removes
//! the combinatorial state explosion that caused the previous bug class.

use crate::agent::engine::WorkflowEngine;

/// Read-only context handed to each gate at `## Done` time.
pub struct GateCtx<'a> {
    /// Engine, used only as a key-value store for verify status / findings.
    pub engine: &'a WorkflowEngine,
    /// The assistant message that contained `## Done`.
    pub assistant_text: &'a str,
    /// Source files modified during this turn (relative paths).
    pub touched_files: &'a [String],
    /// Whether any code-modifying tool ran this turn.
    pub had_code_changes: bool,
}

/// Outcome of a single gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// Requirement satisfied.
    Pass,
    /// Rejected — inject `feedback` and let the model try again.
    Fail { feedback: String },
    /// Needs a human decision — stop the loop and prompt the user.
    NeedsUser { prompt: String },
}

/// A pure validation rule evaluated after `## Done`.
pub trait Gate: Send + Sync {
    fn id(&self) -> &'static str;
    fn check(&self, ctx: &GateCtx) -> GateOutcome;
}

/// Aggregate result of running the whole pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateReport {
    /// Every gate passed — the turn is genuinely complete.
    Pass,
    /// A gate rejected; `gate` identifies which, `feedback` is for the model.
    Fail { gate: String, feedback: String },
    /// A gate needs the user; stop and surface `prompt`.
    NeedsUser { gate: String, prompt: String },
}

const GATE_FAILURE_KEY: &str = "_gate_failure_count";

/// Ordered gate pipeline with a global per-turn failure budget.
pub struct GateRunner {
    gates: Vec<Box<dyn Gate>>,
    /// Max total gate failures in one turn before escalating to the user.
    budget: u32,
}

impl GateRunner {
    pub fn new(gates: Vec<Box<dyn Gate>>, budget: u32) -> Self {
        Self { gates, budget }
    }

    /// Run gates in order; first non-pass wins. Respects the global budget:
    /// once cumulative failures reach `budget`, a `Fail` is upgraded to
    /// `NeedsUser` so the loop stops instead of spinning.
    pub fn run(&self, ctx: &GateCtx) -> GateReport {
        for gate in &self.gates {
            match gate.check(ctx) {
                GateOutcome::Pass => continue,
                GateOutcome::NeedsUser { prompt } => {
                    return GateReport::NeedsUser {
                        gate: gate.id().to_string(),
                        prompt,
                    };
                }
                GateOutcome::Fail { feedback } => {
                    let count = bump_failure(ctx.engine);
                    if count >= self.budget {
                        return GateReport::NeedsUser {
                            gate: gate.id().to_string(),
                            prompt: format!(
                                "已连续 {count} 次未通过门禁「{}」，停止自动修复，交给你判断：\n\n{feedback}",
                                gate.id()
                            ),
                        };
                    }
                    return GateReport::Fail {
                        gate: gate.id().to_string(),
                        feedback,
                    };
                }
            }
        }
        reset_failures(ctx.engine);
        GateReport::Pass
    }
}

/// Increment and return the cumulative gate-failure counter for this turn.
fn bump_failure(engine: &WorkflowEngine) -> u32 {
    let next = current_failures(engine) + 1;
    engine.set_variable(GATE_FAILURE_KEY, next.to_string());
    next
}

pub fn current_failures(engine: &WorkflowEngine) -> u32 {
    engine
        .get_variable(GATE_FAILURE_KEY)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Clear the counter (call at the start of every fresh user turn).
pub fn reset_failures(engine: &WorkflowEngine) {
    engine.set_variable(GATE_FAILURE_KEY, String::new());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionState;
    use std::sync::Arc;

    fn engine() -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        WorkflowEngine::new(session)
    }

    struct Always(GateOutcome);
    impl Gate for Always {
        fn id(&self) -> &'static str {
            "always"
        }
        fn check(&self, _: &GateCtx) -> GateOutcome {
            self.0.clone()
        }
    }

    fn ctx<'a>(e: &'a WorkflowEngine) -> GateCtx<'a> {
        GateCtx {
            engine: e,
            assistant_text: "## Done",
            touched_files: &[],
            had_code_changes: false,
        }
    }

    #[test]
    fn all_pass_resets_counter() {
        let e = engine();
        e.set_variable(GATE_FAILURE_KEY, "2".into());
        let runner = GateRunner::new(vec![Box::new(Always(GateOutcome::Pass))], 8);
        assert_eq!(runner.run(&ctx(&e)), GateReport::Pass);
        assert_eq!(current_failures(&e), 0);
    }

    #[test]
    fn first_failure_returns_fail() {
        let e = engine();
        let runner = GateRunner::new(
            vec![Box::new(Always(GateOutcome::Fail {
                feedback: "boom".into(),
            }))],
            8,
        );
        match runner.run(&ctx(&e)) {
            GateReport::Fail { feedback, .. } => assert_eq!(feedback, "boom"),
            other => panic!("expected Fail, got {other:?}"),
        }
        assert_eq!(current_failures(&e), 1);
    }

    #[test]
    fn budget_exhaustion_escalates_to_user() {
        let e = engine();
        e.set_variable(GATE_FAILURE_KEY, "2".into());
        let runner = GateRunner::new(
            vec![Box::new(Always(GateOutcome::Fail {
                feedback: "still broken".into(),
            }))],
            3,
        );
        match runner.run(&ctx(&e)) {
            GateReport::NeedsUser { prompt, .. } => assert!(prompt.contains("still broken")),
            other => panic!("expected NeedsUser, got {other:?}"),
        }
    }

    #[test]
    fn needs_user_short_circuits() {
        let e = engine();
        let runner = GateRunner::new(
            vec![Box::new(Always(GateOutcome::NeedsUser {
                prompt: "approve?".into(),
            }))],
            8,
        );
        match runner.run(&ctx(&e)) {
            GateReport::NeedsUser { prompt, .. } => assert_eq!(prompt, "approve?"),
            other => panic!("expected NeedsUser, got {other:?}"),
        }
    }
}
