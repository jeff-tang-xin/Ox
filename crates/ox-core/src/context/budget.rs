/// Dynamic token budget allocation for context assembly.
///
/// Replaces the static `TokenBudgets` with an adaptive model that adjusts
/// allocation based on user intent and retrieval quality.
///
/// Per design doc §5.2 (深度优先 + Token预算):
/// - L0 (WorkingMemory): highest priority, always injected
/// - L1/L2 (Atomic/Episodic): injected by semantic similarity
/// - L3 (SemanticMemory): only if highly relevant (score ≥ 0.5)

use crate::context::UserIntent;

/// Dynamic budget model — adapts per-turn based on intent and feedback.
#[derive(Debug, Clone)]
pub struct DynamicBudget {
    pub intent: UserIntent,
    /// Average relevance score from the last retrieval (0.0-1.0)
    pub last_retrieval_relevance: f32,
    /// Whether the previous turn executed its tools successfully
    pub last_turn_success: bool,
    /// Session message count (for compression decisions)
    pub message_count: usize,
}

impl DynamicBudget {
    /// Create a budget for a new turn.
    pub fn new(intent: UserIntent, message_count: usize) -> Self {
        Self {
            intent,
            last_retrieval_relevance: 0.5, // Start with neutral
            last_turn_success: true,
            message_count,
        }
    }

    /// Allocate token budgets from a total context window.
    pub fn allocate(&self, context_window: u32) -> TokenBudgets {
        let total = context_window;

        // Base allocation by intent
        let (knowledge_pct, history_pct, reply_pct) = match self.intent {
            UserIntent::Exploration => {
                // Exploring project — more budget for directory/code structure
                (0.25, 0.15, 0.20)
            }
            UserIntent::CodeUnderstanding => {
                // Understanding code — more for relevant symbols + memories
                (0.28, 0.20, 0.20)
            }
            UserIntent::CodeModification => {
                // Modifying code — more for history continuity + memory
                (0.22, 0.30, 0.18)
            }
            UserIntent::General => {
                // General conversation — balanced
                (0.22, 0.25, 0.20)
            }
        };

        // Adjust by retrieval quality
        let knowledge_pct = if self.last_retrieval_relevance < 0.3 {
            // Poor retrieval: reduce knowledge, give more to history
            knowledge_pct * 0.6
        } else if self.last_retrieval_relevance > 0.7 {
            // Great retrieval: knowledge is valuable
            (knowledge_pct * 1.2).min(0.35)
        } else {
            knowledge_pct
        };

        // System prompt is fixed (use ~10% of window)
        let system_prompt = (total as f32 * 0.10) as u32;

        let knowledge = (total as f32 * knowledge_pct) as u32;
        let history = (total as f32 * history_pct) as u32;
        let reply_reserve = (total as f32 * reply_pct) as u32;

        TokenBudgets {
            system_prompt,
            knowledge,
            history,
            reply_reserve,
            total,
        }
    }

    /// Update retrieval relevance for the next turn.
    pub fn update_relevance(&mut self, relevance: f32) {
        // Exponential moving average
        self.last_retrieval_relevance = self.last_retrieval_relevance * 0.7 + relevance * 0.3;
    }

    /// Update turn success for the next turn.
    pub fn update_success(&mut self, success: bool) {
        self.last_turn_success = success;
    }
}

/// Token budget allocation for a given model context window.
#[derive(Debug, Clone)]
pub struct TokenBudgets {
    /// System prompt allocation
    pub system_prompt: u32,
    /// Knowledge context (retrieved entities)
    pub knowledge: u32,
    /// Conversation history
    pub history: u32,
    /// Reserve for LLM reply
    pub reply_reserve: u32,
    /// Total context window
    pub total: u32,
}

impl TokenBudgets {
    /// Create static defaults (used when DynamicBudget is not available).
    pub fn defaults(context_window: u32) -> Self {
        let total = context_window;
        Self {
            system_prompt: (total as f32 * 0.10) as u32,
            knowledge: (total as f32 * 0.22) as u32,
            history: (total as f32 * 0.25) as u32,
            reply_reserve: (total as f32 * 0.20) as u32,
            total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocation_sums_within_window() {
        let budget = DynamicBudget::new(UserIntent::General, 0);
        let alloc = budget.allocate(100_000);

        let used = alloc.system_prompt + alloc.knowledge + alloc.history + alloc.reply_reserve;
        assert!(used <= alloc.total, "used={} > total={}", used, alloc.total);
    }

    #[test]
    fn test_exploration_has_more_knowledge() {
        let explore = DynamicBudget::new(UserIntent::Exploration, 0);
        let modify = DynamicBudget::new(UserIntent::CodeModification, 0);

        let ae = explore.allocate(100_000);
        let am = modify.allocate(100_000);

        // Exploration should give more to knowledge than modification
        assert!(ae.knowledge > am.knowledge,
            "explore.knowledge={} vs modify.knowledge={}", ae.knowledge, am.knowledge);

        // Modification should give more to history than exploration
        assert!(am.history > ae.history,
            "modify.history={} vs explore.history={}", am.history, ae.history);
    }

    #[test]
    fn test_poor_relevance_reduces_knowledge() {
        let mut budget = DynamicBudget::new(UserIntent::CodeUnderstanding, 0);
        let before = budget.allocate(100_000);

        budget.update_relevance(0.2); // Poor relevance
        let after = budget.allocate(100_000);

        assert!(after.knowledge < before.knowledge,
            "after.knowledge={} should be < before.knowledge={}", after.knowledge, before.knowledge);
    }
}
