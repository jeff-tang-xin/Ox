pub mod compressed_store;
mod effort;
mod spec;
mod system_prompt;

pub use effort::{EffortLevel, estimate_effort};
pub use spec::{TASK_TYPE_PROMPT, load_spec, save_spec, spec_exists};
pub use system_prompt::build_system_prompt;

use crate::llm::tokenizer::estimate_tokens;
use crate::message::Message;

/// User intent detection for smart context assembly.
#[derive(Debug, Clone, PartialEq)]
pub enum UserIntent {
    /// User is exploring project structure (e.g., "show me the project", "what files are there")
    Exploration,
    /// User is asking about specific code logic (e.g., "how does auth work", "explain this function")
    CodeUnderstanding,
    /// User wants to modify code (e.g., "add a feature", "fix this bug")
    CodeModification,
    /// General conversation or unclear intent
    General,
}

/// Detect user intent from their query.
pub fn detect_intent(query: &str) -> UserIntent {
    let query_lower = query.to_lowercase();
    
    // Exploration keywords
    let exploration_keywords = [
        "show me", "list", "what files", "project structure", "directory",
        "explore", "browse", "overview", "structure"
    ];
    if exploration_keywords.iter().any(|k| query_lower.contains(k)) {
        return UserIntent::Exploration;
    }
    
    // Code understanding keywords
    let understanding_keywords = [
        "how does", "explain", "what is", "understand", "logic",
        "implementation", "work", "function", "method"
    ];
    if understanding_keywords.iter().any(|k| query_lower.contains(k)) {
        return UserIntent::CodeUnderstanding;
    }
    
    // Code modification keywords
    let modification_keywords = [
        "add", "create", "modify", "change", "update", "fix",
        "implement", "refactor", "delete", "remove"
    ];
    if modification_keywords.iter().any(|k| query_lower.contains(k)) {
        return UserIntent::CodeModification;
    }
    
    UserIntent::General
}

/// Extract recently accessed file paths from message history.
/// This helps identify which files the user is currently working on.
pub fn extract_recent_files(history: &[Message], max_files: usize) -> Vec<String> {
    use regex::Regex;
    
    let mut files = Vec::new();
    let file_pattern = Regex::new(r"[\w./-]+\.(rs|toml|json|md|py|js|ts|go|css|html)").unwrap();
    
    // Scan recent messages (last 10) for file paths
    let recent_messages = history.iter().rev().take(10);
    
    for msg in recent_messages {
        let content = match msg {
            Message::User { content } | Message::Assistant { content, .. } => content,
            Message::ToolResult { content, .. } => content,
            _ => continue,
        };
        
        for mat in file_pattern.find_iter(content) {
            let file_path = mat.as_str().to_string();
            if !files.contains(&file_path) && files.len() < max_files {
                files.push(file_path);
            }
        }
    }
    
    files
}

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
#[derive(Clone)]
pub struct ContextBuilder {
    system_prompt_ratio: f32,
    memory_ratio: f32,
    history_ratio: f32,
    reply_reserve_ratio: f32,
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextBuilder {
    /// Create a new ContextBuilder with default ratios.
    pub fn new() -> Self {
        Self {
            system_prompt_ratio: 0.02,
            memory_ratio: 0.02,
            history_ratio: 0.10, // 10% for history
            reply_reserve_ratio: 0.85,
        }
    }

    /// Create a ContextBuilder from ContextConfig ratios.
    pub fn from_config(config: &crate::config::ContextConfig) -> Self {
        let user_ratio_sum =
            config.history_ratio + config.memory_ratio + config.system_prompt_ratio;
        let reply_reserve = if user_ratio_sum >= 1.0 {
            0.0 // Fallback if ratios are invalid
        } else {
            1.0 - user_ratio_sum
        };

        Self {
            system_prompt_ratio: config.system_prompt_ratio,
            memory_ratio: config.memory_ratio,
            history_ratio: config.history_ratio,
            reply_reserve_ratio: reply_reserve,
        }
    }

    /// Get the history ratio (0.10 = 10% of context window).
    pub fn history_ratio(&self) -> f32 {
        self.history_ratio
    }

    /// Calculate token budgets based on model context window.
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

    /// Adjust budgets based on user intent for smarter context allocation.
    pub fn budgets_for_intent(&self, context_window: u32, intent: UserIntent) -> TokenBudgets {
        match intent {
            UserIntent::Exploration => {
                // Exploration mode: more history for context, less memory
                TokenBudgets {
                    system_prompt: (context_window as f32 * self.system_prompt_ratio) as u32,
                    memory: (context_window as f32 * 0.01) as u32,  // Reduce to 1%
                    history: (context_window as f32 * 0.15) as u32,  // Increase to 15%
                    reply_reserve: (context_window as f32 * 0.83) as u32,
                    total: context_window,
                }
            }
            UserIntent::CodeUnderstanding | UserIntent::CodeModification => {
                // Development mode: more memory for relevant code knowledge
                TokenBudgets {
                    system_prompt: (context_window as f32 * self.system_prompt_ratio) as u32,
                    memory: (context_window as f32 * 0.05) as u32,  // Increase to 5%
                    history: (context_window as f32 * 0.08) as u32,  // Slightly reduce to 8%
                    reply_reserve: (context_window as f32 * 0.86) as u32,
                    total: context_window,
                }
            }
            UserIntent::General => {
                // General mode: use default ratios
                self.budgets(context_window)
            }
        }
    }

    /// Assemble the final message list for an LLM call.
    ///
    /// Strategy: system prompt first, then memory context, then fill history
    /// from newest to oldest within budget.
    pub fn build(
        &self,
        system_prompt: &str,
        memory_context: &str,
        history: &[Message],
        context_window: u32,
    ) -> Vec<Message> {
        let budgets = self.budgets(context_window);

        let mut result = Vec::new();

        // 1. System prompt + Memory context (merged into ONE system message for MiniMax compatibility).
        // 🚨 FIX: Check if history already starts with a system message (e.g., compression notice)
        let has_leading_system = matches!(history.first(), Some(Message::System { .. }));
        
        let combined_system = if !memory_context.is_empty() {
            format!(
                "{}\n\n{}",
                system_prompt.trim_end_matches('\n'),
                memory_context.trim_start_matches('\n')
            )
        } else {
            system_prompt.to_string()
        };
        
        // If history already has a system message, merge it with our system prompt
        if has_leading_system {
            if let Some(Message::System { content }) = history.first() {
                // Merge: existing system message + our system prompt
                let merged = format!("{}\n\n{}", content, combined_system);
                result.push(Message::system(&merged));
                // Skip the first message in history when copying later
            } else {
                result.push(Message::system(&combined_system));
            }
        } else {
            result.push(Message::system(&combined_system));
        }

        // 2. Fill history from newest to oldest within budget.
        let history_budget = budgets.history as usize;
        let mut used_tokens: usize = 0;
        let mut selected_indices: Vec<usize> = Vec::new();

        // Skip the first message if it's a system message (already merged above)
        let start_idx = if has_leading_system { 1 } else { 0 };

        for (i, msg) in history.iter().enumerate().skip(start_idx).rev() {
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

        // Sanitize: remove orphaned ToolResults and strip orphaned tool_calls
        // caused by budget truncation breaking tool interaction sequences.
        sanitize_tool_pairs(&mut result);

        result
    }
}

/// Sanitize tool interaction pairs in a message list.
///
/// After budget truncation, the message list may contain:
/// - ToolResult without a preceding Assistant that has the matching tool_call
/// - Assistant tool_calls without a following ToolResult
///
/// This function removes orphaned ToolResults and strips orphaned tool_calls
/// to produce a valid message sequence for the LLM API.
///
/// IMPORTANT: OpenAI API requires strict matching between tool_calls and tool_results.
/// If we send a ToolResult with an ID that doesn't exist in any Assistant's tool_calls,
/// the API will return error 400 "tool result's tool id not found".
///
/// CRITICAL FIX: We must ensure tool_call and tool_result are kept together as a pair.
/// If either is missing due to truncation, BOTH should be removed to maintain consistency.
pub fn sanitize_tool_pairs(messages: &mut Vec<Message>) {
    // Collect all tool_call_ids from Assistant messages in a single pass
    let mut assistant_call_ids = std::collections::HashSet::with_capacity(messages.len());
    let mut result_call_ids = std::collections::HashSet::with_capacity(messages.len());

    for msg in messages.iter() {
        match msg {
            Message::Assistant { tool_calls, .. } => {
                for tc in tool_calls {
                    assistant_call_ids.insert(tc.id.clone());
                }
            }
            Message::ToolResult { tool_call_id, .. } => {
                result_call_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }

    // Step 1: Remove orphaned ToolResults (no matching tool_call in any Assistant)
    // This is safe - if there's no tool_call, the result is meaningless
    messages.retain(|m| {
        if let Message::ToolResult { tool_call_id, .. } = m {
            assistant_call_ids.contains(tool_call_id)
        } else {
            true
        }
    });

    // ✅ CRITICAL FIX: Remove orphaned tool_calls (no matching ToolResult)
    // We MUST remove the entire Assistant message if it has tool_calls but no results,
    // because sending incomplete tool_call/tool_result pairs will cause API errors.
    // 
    // Reason: If we keep an Assistant with tool_calls but remove the ToolResult,
    // then when we try to send a new ToolResult later, the API will complain
    // that the tool_call ID doesn't exist (because it was truncated).
    messages.retain(|m| {
        if let Message::Assistant { tool_calls, content, .. } = m {
            // Keep if:
            // 1. Has text content (even without tool_calls)
            // 2. OR has tool_calls AND all of them have matching ToolResults
            if !content.trim().is_empty() {
                true  // Has content, keep it
            } else if tool_calls.is_empty() {
                true  // No tool_calls, keep it (might be pure text response)
            } else {
                // Has tool_calls - check if ALL of them have matching ToolResults
                tool_calls.iter().all(|tc| result_call_ids.contains(&tc.id))
            }
        } else {
            true  // Not an Assistant message, keep it
        }
    });
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
            history.push(Message::user(format!(
                "Message number {i} with some extra text to consume tokens"
            )));
            history.push(Message::assistant(format!(
                "Response {i} with additional content"
            )));
        }
        let result = cb.build("System", "", &history, 128_000);
        // Should have fewer than all 200 history messages.
        assert!(result.len() < 201);
        // But should have at least system + a few recent messages.
        assert!(result.len() >= 3);
    }

    #[test]
    fn sanitize_removes_orphaned_tool_results() {
        use crate::message::ToolCall;

        let mut messages = vec![
            Message::Assistant {
                content: "Let me check that".to_string(),
                tool_calls: vec![ToolCall {
                    id: "call_abc".to_string(),
                    name: "file_read".to_string(),
                    arguments: "{\"path\": \"test.txt\"}".to_string(),
                }],
            },
            Message::ToolResult {
                tool_call_id: "call_xyz".to_string(), // Orphaned - no matching tool_call
                content: "Some result".to_string(),
            },
        ];

        sanitize_tool_pairs(&mut messages);

        // Orphaned ToolResult should be removed
        assert_eq!(messages.len(), 1);
        assert!(matches!(&messages[0], Message::Assistant { .. }));
    }

    #[test]
    fn sanitize_removes_empty_assistant_messages() {
        use crate::message::ToolCall;

        let mut messages = vec![
            Message::Assistant {
                content: "".to_string(),
                tool_calls: vec![ToolCall {
                    id: "call_abc".to_string(),
                    name: "file_read".to_string(),
                    arguments: "{\"path\": \"test.txt\"}".to_string(),
                }],
            },
            // No ToolResult for call_abc - orphaned tool_call
        ];

        sanitize_tool_pairs(&mut messages);

        // Empty Assistant message should be removed
        assert_eq!(messages.len(), 0);
    }

    #[test]
    fn sanitize_keeps_assistant_with_content_even_if_tool_calls_removed() {
        use crate::message::ToolCall;

        let mut messages = vec![
            Message::Assistant {
                content: "I'll read the file for you.".to_string(),
                tool_calls: vec![ToolCall {
                    id: "call_abc".to_string(),
                    name: "file_read".to_string(),
                    arguments: "{\"path\": \"test.txt\"}".to_string(),
                }],
            },
            // No ToolResult for call_abc - orphaned tool_call
        ];

        sanitize_tool_pairs(&mut messages);

        // Assistant message should be kept (has content), but tool_calls removed
        assert_eq!(messages.len(), 1);
        if let Message::Assistant {
            content,
            tool_calls,
        } = &messages[0]
        {
            assert_eq!(content, "I'll read the file for you.");
            assert!(tool_calls.is_empty());
        } else {
            panic!("Expected Assistant message");
        }
    }
}
