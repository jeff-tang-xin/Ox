/// Manages satisfaction tracking for implicit feedback system.

/// Decision made by the rollback evaluation
#[derive(Debug)]
pub enum RollbackDecision {
    /// No rollback needed, state saved
    NoRollback {
        current_score: f64,
    },
    /// Rollback is needed but no snapshot available
    NeedsRollback {
        current_score: f64,
        baseline_score: f64,
        degradation: f64,
    },
}

/// Manages rollback decisions based on satisfaction scores.
pub struct RollbackManager;

impl RollbackManager {
    pub fn new() -> Self {
        Self
    }

    /// Evaluate satisfaction and decide if rollback is needed.
    pub fn evaluate_and_maybe_rollback(
        &mut self,
        current_satisfaction: f64,
        baseline_satisfaction: f64,
    ) -> RollbackDecision {
        let degradation = baseline_satisfaction - current_satisfaction;
        
        if degradation > 0.2 {
            RollbackDecision::NeedsRollback {
                current_score: current_satisfaction,
                baseline_score: baseline_satisfaction,
                degradation,
            }
        } else {
            RollbackDecision::NoRollback {
                current_score: current_satisfaction,
            }
        }
    }

    /// Calculate composite satisfaction score from multiple signals
    pub fn calculate_satisfaction_score(
        &self,
        explicit_feedback_rate: f64,  // good / total feedback
        tool_success_rate: f64,        // successful tool calls / total
        code_accept_rate: f64,         // accepted writes / total writes
        has_explicit_feedback: bool,
    ) -> f64 {
        if has_explicit_feedback {
            // With explicit feedback: weight it higher
            explicit_feedback_rate * 0.4 + tool_success_rate * 0.3 + code_accept_rate * 0.3
        } else {
            // Without explicit feedback: rely on implicit signals
            explicit_feedback_rate * 0.1 + tool_success_rate * 0.3 + code_accept_rate * 0.6
        }
    }
}

impl Default for RollbackManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_satisfaction_with_explicit() {
        let manager = RollbackManager::new();
        let score = manager.calculate_satisfaction_score(0.9, 0.8, 0.7, true);
        // Should weight explicit feedback heavily
        assert!(score > 0.7);
    }

    #[test]
    fn test_calculate_satisfaction_without_explicit() {
        let manager = RollbackManager::new();
        let score = manager.calculate_satisfaction_score(0.5, 0.8, 0.7, false);
        // Should weight code accept rate heavily
        assert!(score > 0.6);
    }

    #[test]
    fn test_rollback_decision_no_degradation() {
        let mut manager = RollbackManager::new();
        
        let decision = manager.evaluate_and_maybe_rollback(
            0.85,  // current
            0.80,  // baseline
        );
        
        matches!(decision, RollbackDecision::NoRollback { .. });
    }

    #[test]
    fn test_rollback_decision_with_degradation() {
        let mut manager = RollbackManager::new();
        
        let decision = manager.evaluate_and_maybe_rollback(
            0.50,  // current (degraded)
            0.80,  // baseline
        );
        
        match decision {
            RollbackDecision::NeedsRollback { degradation, .. } => {
                assert!(degradation > 0.2);
            }
            _ => panic!("Expected NeedsRollback"),
        }
    }
}
