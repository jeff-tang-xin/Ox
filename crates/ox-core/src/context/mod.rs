mod effort;
mod system_prompt;

pub use effort::{estimate_effort, EffortLevel};
pub use system_prompt::build_system_prompt;

use crate::llm::tokenizer::estimate_tokens;
use crate::message::Message;

/// Token budget allocation for a given model context window.
#[derive(Debug, Clone)]
pub struct TokenBudgets {
    pub system_prompt: u32,
    pub memory: u32,
    pub history: u32,
    pub reply_reserve: u32,
    pub total: u32,
}

/// Builds the final message list for an LLM call, fitting within token budgets.
pub struct ContextBuilder {
    system_prompt_ratio: f32,
    memory_ratio: f32,
    history_ratio: f32,
    reply_reserve_ratio: f32,
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self {
            system_prompt_ratio: 0.02,
            memory_ratio: 0.02,
            history_ratio: 0.36,
            reply_reserve_ratio: 0.59,
        }
    }
}

impl ContextBuilder {
    /// Calculate token budgets based on model context window.
    pub fn budgets(&self, context_window: u32) -> TokenBudgets {
        TokenBudgets {
            system_prompt: (context_window as f32 * self.system_prompt_ratio) as u32,
            memory: (context_window as f32 * self.memory_ratio) as u32,
            history: (context_window as f32 * self.history_ratio) as u32,
            reply_reserve: (context_window as f32 * self.reply_reserve_ratio) as u32,
            total: context_window,
        }
    }

    /// Assemble the final message list for an LLM call.
    ///
    /// Strategy: system prompt first, then memory context, then fill history
    /// from newest to oldest until the history budget is exhausted.
    pub fn build(
        &self,
        system_prompt: &str,
        memory_context: &str,
        history: &[Message],
        context_window: u32,
    ) -> Vec<Message> {
        let budgets = self.budgets(context_window);

        let mut result = Vec::new();

        // 1. System prompt (always included).
        result.push(Message::system(system_prompt));

        // 2. Memory context (if non-empty).
        if !memory_context.is_empty() {
            result.push(Message::system(memory_context));
        }

        // 3. Fill history from newest to oldest within budget.
        let history_budget = budgets.history as usize;
        let mut used_tokens: usize = 0;
        let mut selected_indices: Vec<usize> = Vec::new();

        for (i, msg) in history.iter().enumerate().rev() {
            let msg_tokens = estimate_message_tokens(msg);
            if used_tokens + msg_tokens > history_budget {
                break;
            }
            used_tokens += msg_tokens;
            selected_indices.push(i);
        }

        // Reverse to maintain chronological order.
        selected_indices.reverse();
        for i in selected_indices {
            result.push(history[i].clone());
        }

        result
    }
}

/// Estimate tokens for a single message.
fn estimate_message_tokens(msg: &Message) -> usize {
    let content_len = match msg {
        Message::System { content }
        | Message::User { content }
        | Message::ToolResult { content, .. } => estimate_tokens(content),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut tokens = estimate_tokens(content);
            for tc in tool_calls {
                tokens += estimate_tokens(&tc.name);
                tokens += estimate_tokens(&tc.arguments);
                tokens += 10; // overhead for tool call structure
            }
            tokens
        }
    };
    content_len as usize + 4 // message framing overhead
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_sum_is_reasonable() {
        let cb = ContextBuilder::default();
        let b = cb.budgets(128_000);
        // Sum of ratios is 0.99 (1% for user input), so total allocated < context_window.
        let sum = b.system_prompt + b.memory + b.history + b.reply_reserve;
        assert!(sum <= 128_000);
        assert!(sum > 120_000); // At least 94% allocated.
    }

    #[test]
    fn build_includes_system_and_recent_history() {
        let cb = ContextBuilder::default();
        let history = vec![
            Message::user("hello"),
            Message::assistant("hi there"),
            Message::user("how are you"),
            Message::assistant("I'm good"),
        ];
        let result = cb.build("You are helpful.", "", &history, 128_000);
        // Should have system + all 4 history messages (they're tiny).
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn build_truncates_old_history_when_budget_exceeded() {
        let cb = ContextBuilder {
            system_prompt_ratio: 0.02,
            memory_ratio: 0.02,
            history_ratio: 0.01, // Very tight budget.
            reply_reserve_ratio: 0.95,
        };
        // Create many messages to exceed the tiny budget.
        let mut history = Vec::new();
        for i in 0..100 {
            history.push(Message::user(format!("Message number {i} with some extra text to consume tokens")));
            history.push(Message::assistant(format!("Response {i} with additional content")));
        }
        let result = cb.build("System", "", &history, 128_000);
        // Should have fewer than all 200 history messages.
        assert!(result.len() < 201);
        // But should have at least system + a few recent messages.
        assert!(result.len() >= 3);
    }
}
