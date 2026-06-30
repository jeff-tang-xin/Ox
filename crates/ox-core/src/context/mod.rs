pub mod compressed_store;
mod effort;
pub mod refinement;
pub mod skill_prompts;
mod spec;
mod system_prompt;

pub use effort::{EffortLevel, estimate_effort};
pub use refinement::{
    MemorySummary, RefinedTurn, build_refined_context, generate_memory_summary,
    refine_assistant_response, refine_conversation,
};
pub use skill_prompts::SKILL_CREATION_PROMPT;
pub use spec::{TASK_TYPE_PROMPT, load_spec, save_spec, spec_exists};
pub use system_prompt::{
    TurnContext, build_system_prompt, build_system_prompt_with_context,
    build_system_prompt_with_step, gather_diff_context, gather_dir_context, gather_git_context,
};

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
        "show me",
        "list",
        "what files",
        "project structure",
        "directory",
        "explore",
        "browse",
        "overview",
        "structure",
    ];
    if exploration_keywords.iter().any(|k| query_lower.contains(k)) {
        return UserIntent::Exploration;
    }

    // Code understanding keywords
    let understanding_keywords = [
        "how does",
        "explain",
        "what is",
        "understand",
        "logic",
        "implementation",
        "work",
        "function",
        "method",
    ];
    if understanding_keywords
        .iter()
        .any(|k| query_lower.contains(k))
    {
        return UserIntent::CodeUnderstanding;
    }

    // Code modification keywords
    let modification_keywords = [
        "add",
        "create",
        "modify",
        "change",
        "update",
        "fix",
        "implement",
        "refactor",
        "delete",
        "remove",
    ];
    if modification_keywords
        .iter()
        .any(|k| query_lower.contains(k))
    {
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
    /// Ratios must sum to 1.0 for correct budget allocation.
    pub fn new() -> Self {
        Self {
            system_prompt_ratio: 0.02,
            memory_ratio: 0.03,
            history_ratio: 0.18, // 18% for history - more room for conversation
            reply_reserve_ratio: 0.77,
        }
    }

    /// Validate that ratios sum to 1.0 (with epsilon tolerance)
    fn validate_ratios(&self) -> bool {
        let sum = self.system_prompt_ratio
            + self.memory_ratio
            + self.history_ratio
            + self.reply_reserve_ratio;
        (sum - 1.0).abs() < 0.001
    }

    /// Create a ContextBuilder from ContextConfig ratios.
    /// FIX: Ensure ratios sum to 1.0 by normalizing if needed.
    pub fn from_config(config: &crate::config::ContextConfig) -> Self {
        let user_ratio_sum =
            config.history_ratio + config.memory_ratio + config.system_prompt_ratio;

        // Clamp reply_reserve to [0.0, 1.0] and normalize if ratios > 1.0
        let reply_reserve = if user_ratio_sum >= 1.0 {
            tracing::warn!("ContextConfig ratios sum to >= 1.0, clamping reply_reserve to 0");
            0.0
        } else {
            1.0 - user_ratio_sum
        };

        let builder = Self {
            system_prompt_ratio: config.system_prompt_ratio.clamp(0.0, 1.0),
            memory_ratio: config.memory_ratio.clamp(0.0, 1.0),
            history_ratio: config.history_ratio.clamp(0.0, 1.0),
            reply_reserve_ratio: reply_reserve.clamp(0.0, 1.0),
        };

        // Debug validate (doesn't enforce, but warns)
        if !builder.validate_ratios() {
            tracing::warn!(
                "ContextBuilder ratios don't sum to 1.0: sys={}, mem={}, hist={}, reply={}",
                builder.system_prompt_ratio,
                builder.memory_ratio,
                builder.history_ratio,
                builder.reply_reserve_ratio
            );
        }

        builder
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

    /// Adjust budgets based on user intent and conversation length.
    pub fn budgets_for_intent(
        &self,
        context_window: u32,
        intent: UserIntent,
        msg_count: usize,
    ) -> TokenBudgets {
        // Dynamic adjustment based on conversation length
        let (mem_r, hist_r) = if msg_count < 20 {
            // Short: give more to history, reply reserve is still large
            (0.02, 0.20)
        } else if msg_count < 100 {
            // Medium: default ratios
            (self.memory_ratio, self.history_ratio)
        } else {
            // Long: more memory, less history to avoid context bloat
            (0.05, 0.08)
        };
        let reply = (1.0 - self.system_prompt_ratio - mem_r - hist_r).max(0.5);

        match intent {
            UserIntent::Exploration => TokenBudgets {
                system_prompt: (context_window as f32 * self.system_prompt_ratio) as u32,
                memory: (context_window as f32 * 0.01) as u32,
                history: (context_window as f32 * 0.15) as u32,
                reply_reserve: (context_window as f32 * 0.83) as u32,
                total: context_window,
            },
            UserIntent::CodeUnderstanding | UserIntent::CodeModification => TokenBudgets {
                system_prompt: (context_window as f32 * self.system_prompt_ratio) as u32,
                memory: (context_window as f32 * mem_r) as u32,
                history: (context_window as f32 * hist_r) as u32,
                reply_reserve: (context_window as f32 * reply) as u32,
                total: context_window,
            },
            UserIntent::General => TokenBudgets {
                system_prompt: (context_window as f32 * self.system_prompt_ratio) as u32,
                memory: (context_window as f32 * mem_r) as u32,
                history: (context_window as f32 * hist_r) as u32,
                reply_reserve: (context_window as f32 * reply) as u32,
                total: context_window,
            },
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

        // 2. Compact oldest completed rounds first (far→near, token-budget driven).
        let mut history = history.to_vec();
        let history_budget = budgets.history as usize;
        compact_completed_rounds(&mut history, history_budget);
        // After compaction, start from index 0 — the system prompt is already in result[0].
        let start_idx: usize = 0;

        // 3. Fill history from newest to oldest within budget.
        let history_count = history.len().saturating_sub(start_idx);
        if history_count <= 8 {
            for i in start_idx..history.len() {
                result.push(history[i].clone());
            }
            sanitize_tool_pairs(&mut result);
            deduplicate_file_reads(&mut result);
            filter_noisy_messages(&mut result);
            return result;
        }

        let mut used_tokens: usize = 0;
        let mut selected_indices: Vec<usize> = Vec::new();
        let mut skipped_indices: Vec<usize> = Vec::new();

        // Phase 1: Fill with high-priority messages (user messages, assistant plans).
        // These are the conversation backbone and must not disappear just because
        // a recent tool result is huge. Large low-priority messages are skipped;
        // large high-priority messages are kept as anchors even if they exceed
        // the budget, because dropping the latest user request makes the model
        // look "forgetful".
        for (i, msg) in history.iter().enumerate().skip(start_idx).rev() {
            let msg_tokens = estimate_message_tokens(msg);
            let is_high_prio = matches!(msg, Message::User { .. })
                || matches!(msg, Message::Assistant { content, .. } if content.contains("## Plan") || content.contains("## Done"));

            if is_high_prio {
                selected_indices.push(i);
                used_tokens = used_tokens.saturating_add(msg_tokens);
                continue;
            }

            if used_tokens + msg_tokens > history_budget {
                skipped_indices.push(i);
                continue;
            }

            skipped_indices.push(i);
        }

        // Phase 2: Fill remaining budget with regular messages (tool results, etc.)
        // Only include messages between the selected range
        if !selected_indices.is_empty() {
            let min_idx = *selected_indices.iter().min().unwrap();
            let max_idx = *selected_indices.iter().max().unwrap();
            for i in skipped_indices {
                if i >= min_idx
                    && i <= max_idx
                    && used_tokens + estimate_message_tokens(&history[i]) <= history_budget
                {
                    used_tokens += estimate_message_tokens(&history[i]);
                    selected_indices.push(i);
                }
            }
        }

        // Sort by index to maintain chronological order
        selected_indices.sort();
        for i in selected_indices {
            result.push(history[i].clone());
        }

        // Sanitize: remove orphaned ToolResults and strip orphaned tool_calls
        sanitize_tool_pairs(&mut result);

        // 🚨 Deduplicate repeated file_read results — keep only most recent
        deduplicate_file_reads(&mut result);

        // Filter out noisy intermediate messages to reduce context bloat
        filter_noisy_messages(&mut result);

        result
    }

    /// Assemble the final message list using refined context format.
    ///
    /// This creates a more compact representation: "User message: Model response (refined) [tools used]"
    pub fn build_refined(
        &self,
        system_prompt: &str,
        memory_context: &str,
        history: &[Message],
        context_window: u32,
        max_turns: usize,
    ) -> Vec<Message> {
        let _budgets = self.budgets(context_window);

        let mut result = Vec::new();

        // 1. System prompt + Memory context (merged into ONE system message for MiniMax compatibility).
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
            } else {
                result.push(Message::system(&combined_system));
            }
        } else {
            result.push(Message::system(&combined_system));
        }

        // 2. Build refined context from recent turns
        let refined_context = build_refined_context(history, max_turns);

        // Add the refined context as a single user message
        if !refined_context.is_empty() {
            result.push(Message::user(format!(
                "Previous conversation summary:\n\n{}",
                refined_context
            )));
        }

        // 🚨 Sanitize tool pairs — same as build() to prevent API errors
        sanitize_tool_pairs(&mut result);
        filter_noisy_messages(&mut result);

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

    // 🔍 DIAGNOSTIC: Log validation results before sanitization
    let orphaned_results: Vec<_> = result_call_ids
        .iter()
        .filter(|id| !assistant_call_ids.contains(*id))
        .collect();

    if !orphaned_results.is_empty() {
        tracing::warn!(
            "[TOOL_PAIR_SANITIZATION] Found {} orphaned ToolResult(s) with IDs: {:?}",
            orphaned_results.len(),
            orphaned_results
        );
    }

    let orphaned_calls: Vec<_> = assistant_call_ids
        .iter()
        .filter(|id| !result_call_ids.contains(*id))
        .collect();

    if !orphaned_calls.is_empty() {
        tracing::warn!(
            "[TOOL_PAIR_SANITIZATION] Found {} orphaned tool_call(s) with IDs: {:?}",
            orphaned_calls.len(),
            orphaned_calls
        );
    }

    // Step 1: Remove orphaned ToolResults (no matching tool_call in any Assistant)
    // This is safe - if there's no tool_call, the result is meaningless
    let before_count = messages.len();
    messages.retain(|m| {
        if let Message::ToolResult { tool_call_id, .. } = m {
            assistant_call_ids.contains(tool_call_id)
        } else {
            true
        }
    });
    let removed_results = before_count - messages.len();
    if removed_results > 0 {
        tracing::info!(
            "[TOOL_PAIR_SANITIZATION] Removed {} orphaned ToolResult(s)",
            removed_results
        );
    }

    // ✅ CRITICAL FIX: Re-collect result_call_ids after removing orphaned ToolResults.
    // We must use the UPDATED set of result_call_ids to filter tool_calls,
    // otherwise we might keep tool_calls whose ToolResults were just deleted.
    let mut updated_result_call_ids = std::collections::HashSet::with_capacity(messages.len());
    for msg in messages.iter() {
        if let Message::ToolResult { tool_call_id, .. } = msg {
            updated_result_call_ids.insert(tool_call_id.clone());
        }
    }

    // ✅ CRITICAL FIX: Remove orphaned tool_calls from Assistant messages
    // OpenAI API requires: if an Assistant message has tool_calls, ALL of them must have
    // corresponding ToolResult messages. If some tool_calls are orphaned (no ToolResult),
    // we must remove those specific tool_calls from the Assistant message.
    //
    // IMPORTANT: We should NOT delete the entire Assistant message if it has text content.
    // Instead, we only remove the orphaned tool_calls from the tool_calls array.
    let mut removed_orphaned_calls = 0usize;

    for msg in messages.iter_mut() {
        if let Message::Assistant { tool_calls, .. } = msg {
            // Keep only tool_calls that have matching ToolResults (using UPDATED set)
            let original_count = tool_calls.len();
            tool_calls.retain(|tc| updated_result_call_ids.contains(&tc.id));
            let removed = original_count - tool_calls.len();
            removed_orphaned_calls += removed;

            if removed > 0 {
                tracing::debug!(
                    "[TOOL_PAIR_SANITIZATION] Removed {} orphaned tool_call(s) from Assistant message",
                    removed
                );
            }
        }
    }

    if removed_orphaned_calls > 0 {
        tracing::info!(
            "[TOOL_PAIR_SANITIZATION] Total removed {} orphaned tool_call(s) from Assistant messages",
            removed_orphaned_calls
        );
    }

    // 🚨 FINAL VALIDATION: Double-check after sanitization
    let mut final_assistant_ids = std::collections::HashSet::new();
    let mut final_result_ids = std::collections::HashSet::new();

    for msg in messages.iter() {
        match msg {
            Message::Assistant { tool_calls, .. } => {
                for tc in tool_calls {
                    final_assistant_ids.insert(tc.id.clone());
                }
            }
            Message::ToolResult { tool_call_id, .. } => {
                final_result_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }

    // Remove any remaining orphaned ToolResults (safety net)
    let before_final = messages.len();
    messages.retain(|m| {
        if let Message::ToolResult { tool_call_id, .. } = m {
            final_assistant_ids.contains(tool_call_id)
        } else {
            true
        }
    });
    if messages.len() < before_final {
        tracing::warn!(
            "[TOOL_PAIR_SANITIZATION] ⚠️ Removed {} additional orphaned ToolResult(s) in final pass",
            before_final - messages.len()
        );
    }

    // Remove empty Assistant messages (no content + no tool_calls after sanitization)
    messages.retain(|m| {
        if let Message::Assistant {
            content,
            tool_calls,
            ..
        } = m
        {
            !(content.is_empty() && tool_calls.is_empty())
        } else {
            true
        }
    });

    // Check for any remaining mismatches (diagnostic only)
    for result_id in &final_result_ids {
        if !final_assistant_ids.contains(result_id) {
            tracing::error!(
                "[TOOL_PAIR_SANITIZATION] ⚠️ CRITICAL: After sanitization, ToolResult ID '{}' still has no matching tool_call!",
                result_id
            );
        }
    }

    // 🔍 ENHANCED VALIDATION: Verify message order and fix if needed
    // OpenAI API requires strict ordering: Assistant with tool_calls must be immediately followed by ToolResults
    let mut i = 0;
    while i < messages.len() {
        if let Message::Assistant { tool_calls, .. } = &messages[i] {
            if !tool_calls.is_empty() {
                let expected_count = tool_calls.len();
                let expected_ids: Vec<_> = tool_calls.iter().map(|tc| tc.id.clone()).collect();

                // Check if the next N messages are ToolResults in the correct order
                let mut is_valid_sequence = true;
                let mut found_ids = Vec::new();

                for j in 1..=expected_count {
                    if i + j >= messages.len() {
                        is_valid_sequence = false;
                        break;
                    }

                    if let Message::ToolResult { tool_call_id, .. } = &messages[i + j] {
                        found_ids.push(tool_call_id.clone());
                    } else {
                        is_valid_sequence = false;
                        break;
                    }
                }

                // Verify IDs match
                if is_valid_sequence && found_ids != expected_ids {
                    is_valid_sequence = false;
                }

                if !is_valid_sequence {
                    tracing::warn!(
                        "[TOOL_PAIR_SANITIZATION] ⚠️ ORDER VIOLATION at index {}: Assistant has {} tool_calls but following messages are not valid ToolResults",
                        i,
                        expected_count
                    );
                    tracing::warn!("[TOOL_PAIR_SANITIZATION] Expected IDs: {:?}", expected_ids);

                    // FIX: Remove all tool_calls from this Assistant message since we can't guarantee proper ordering
                    if let Message::Assistant { tool_calls, .. } = &mut messages[i] {
                        let removed = tool_calls.len();
                        tool_calls.clear();
                        tracing::warn!(
                            "[TOOL_PAIR_SANITIZATION] Removed {} tool_calls from Assistant at index {} to fix ordering issue",
                            removed,
                            i
                        );
                    }

                    // Mark corresponding ToolResults for removal (they're now orphaned)
                    let indices_to_remove: Vec<usize> = (i + 1..i + 1 + expected_count)
                        .filter(|&idx| idx < messages.len())
                        .filter(|&idx| matches!(&messages[idx], Message::ToolResult { .. }))
                        .collect();

                    // Remove in reverse order to maintain indices
                    for idx in indices_to_remove.into_iter().rev() {
                        messages.remove(idx);
                        tracing::debug!(
                            "[TOOL_PAIR_SANITIZATION] Removed orphaned ToolResult at index {}",
                            idx
                        );
                    }
                } else {
                    // Valid sequence, skip past the ToolResults
                    i += expected_count + 1;
                    continue;
                }
            }
        }
        i += 1;
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
            reasoning_content: _,
        } => {
            // Think/reasoning is UI-only — never counts toward context budget.
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
    fn reasoning_content_not_counted_in_budget() {
        let msg = Message::Assistant {
            content: "hi".into(),
            tool_calls: vec![],
            reasoning_content: Some("x".repeat(10_000)),
        };
        let with = estimate_message_tokens(&msg);
        let without = estimate_message_tokens(&Message::Assistant {
            content: "hi".into(),
            tool_calls: vec![],
            reasoning_content: None,
        });
        assert_eq!(with, without);
    }

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
                reasoning_content: None,
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
                reasoning_content: None,
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
                reasoning_content: None,
            },
            // No ToolResult for call_abc - orphaned tool_call
        ];

        sanitize_tool_pairs(&mut messages);

        // Assistant message should be kept (has content), but tool_calls removed
        assert_eq!(messages.len(), 1);
        if let Message::Assistant {
            content,
            tool_calls,
            ..
        } = &messages[0]
        {
            assert_eq!(content, "I'll read the file for you.");
            assert!(tool_calls.is_empty());
        } else {
            panic!("Expected Assistant message");
        }
    }
}

/// Deduplicate repeated file_read results — keep only the most recent read per file.
/// Older reads of the same file are replaced with a compact summary.
fn deduplicate_file_reads(messages: &mut Vec<Message>) {
    use std::collections::HashMap;

    // Find all file_read tool_call IDs and their paths
    let mut file_reads: Vec<(usize, String)> = Vec::new(); // (index, path)
    for (i, msg) in messages.iter().enumerate() {
        if let Message::Assistant { tool_calls, .. } = msg {
            for tc in tool_calls {
                if tc.name == "file_read" {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                            file_reads.push((i, path.to_string()));
                        }
                    }
                }
            }
        }
    }

    if file_reads.len() < 2 {
        return;
    }

    // Find repeated files — keep last occurrence, summarize earlier ones
    let mut last_occurrence: HashMap<String, usize> = HashMap::new();
    for (idx, path) in &file_reads {
        last_occurrence.insert(path.clone(), *idx);
    }

    let mut replaced = 0;
    for &(idx, ref path) in &file_reads {
        let Some(last_idx) = last_occurrence.get(path).copied() else {
            continue;
        };
        if idx != last_idx {
            // Find the corresponding ToolResult for this older file_read. The
            // latest read must stay intact; otherwise the model reasons from
            // stale file contents after edits or re-reads.
            let msg = &messages[idx];
            let tool_call_id = if let Message::Assistant { tool_calls, .. } = msg {
                tool_calls
                    .iter()
                    .find(|tc| {
                        tc.name == "file_read"
                            && serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                .ok()
                                .and_then(|args| {
                                    args.get("path").and_then(|p| p.as_str()).map(str::to_owned)
                                })
                                .as_deref()
                                == Some(path.as_str())
                    })
                    .map(|tc| tc.id.clone())
            } else {
                None
            };

            if let Some(tc_id) = tool_call_id {
                // Replace the ToolResult with a compact summary
                for msg in messages.iter_mut() {
                    if let Message::ToolResult {
                        tool_call_id,
                        content,
                    } = msg
                    {
                        if tool_call_id == &tc_id {
                            let line_count = content.lines().count();
                            *content = format!(
                                "(previously read {} — {} lines, latest version kept in context)",
                                path, line_count
                            );
                            replaced += 1;
                            break;
                        }
                    }
                }
            }
        }
    }
    if replaced > 0 {
        tracing::info!("[DEDUP] Summarized {} repeated file_read results", replaced);
    }
}

/// 🚨 Filter out noisy intermediate messages that add little value.
pub fn filter_noisy_messages(messages: &mut Vec<Message>) {
    if messages.len() < 5 {
        return; // Only apply to longer conversations
    }

    let original_count = messages.len();
    let mut filtered = Vec::with_capacity(messages.len());
    let mut i = 0;

    while i < messages.len() {
        let current_msg = &messages[i];

        // Check if this is a ToolResult with "Infinite Loop Detected"
        if let Message::ToolResult { content, .. } = current_msg {
            if content.contains("Infinite Loop Detected") {
                // Look ahead to see if there are more infinite loop errors
                let mut consecutive_errors = 1;
                let mut j = i + 1;

                while j < messages.len() {
                    if let Message::ToolResult {
                        content: next_content,
                        ..
                    } = &messages[j]
                    {
                        if next_content.contains("Infinite Loop Detected") {
                            consecutive_errors += 1;
                            j += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }

                if consecutive_errors > 1 {
                    // Keep only the LAST infinite loop error (most relevant)
                    tracing::info!(
                        "[NOISE_FILTER] Consolidated {} consecutive 'Infinite Loop' errors into 1",
                        consecutive_errors
                    );
                    filtered.push(messages[j - 1].clone());
                    i = j;
                    continue;
                }
            }

            // Check for repeated "File Not Found" errors
            if content.contains("File Not Found") || content.contains("No file with ID") {
                // Look ahead for similar errors
                let mut consecutive_not_found = 1;
                let mut j = i + 1;

                while j < messages.len() {
                    if let Message::ToolResult {
                        content: next_content,
                        ..
                    } = &messages[j]
                    {
                        if next_content.contains("File Not Found")
                            || next_content.contains("No file with ID")
                        {
                            consecutive_not_found += 1;
                            j += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }

                if consecutive_not_found > 2 {
                    // Keep only the first and last "File Not Found" errors
                    tracing::info!(
                        "[NOISE_FILTER] Consolidated {} 'File Not Found' errors (kept first and last)",
                        consecutive_not_found
                    );
                    filtered.push(messages[i].clone()); // Keep first
                    if consecutive_not_found > 1 {
                        filtered.push(messages[j - 1].clone()); // Keep last
                    }
                    i = j;
                    continue;
                }
            }
        }

        // Keep this message as-is
        filtered.push(current_msg.clone());
        i += 1;
    }

    let removed_count = original_count - filtered.len();
    if removed_count > 0 {
        tracing::info!(
            "[NOISE_FILTER] Removed {} noisy messages ({} → {})",
            removed_count,
            original_count,
            filtered.len()
        );
        *messages = filtered;
    }
}

/// Compact oldest completed rounds first, until remaining history fits within
/// `history_budget` tokens. Process from earliest (index 0) → newest, replacing
/// each completed round (everything before a `[ROUND_COMPLETE]` boundary) with
/// a compact `[ROUND_HISTORY]` summary. The current incomplete round is never
/// touched.
///
/// This is the "far to near" strategy: old finished work gets condensed first,
/// respecting the token budget rather than a fixed round count.
pub fn compact_completed_rounds(messages: &mut Vec<Message>, history_budget: usize) {
    if messages.len() < 4 {
        return;
    }
    let total_tokens: usize = messages.iter().map(estimate_message_tokens).sum();
    if total_tokens <= history_budget {
        return;
    }

    let mut compressed_count = 0;
    let mut tokens_saved = 0;

    loop {
        // Re-collect boundaries each iteration (indices shift after compression)
        let boundaries: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                matches!(m, Message::System { content } if content.starts_with(
                    crate::agent::user_round::COMPLETE_BOUNDARY_TAG
                ))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(i, _)| i)
            .collect();

        // Need at least 2 boundaries to safely compress the oldest one
        // (keep the newest completed round + current round intact)
        if boundaries.len() < 2 {
            break;
        }

        // Compress the OLDEST completed round: from boundaries[0] to boundaries[1]
        let start = boundaries[0];
        let end = boundaries[1];

        let section_tokens: usize = messages[start..end]
            .iter()
            .map(estimate_message_tokens)
            .sum();

        let summary = Message::system(
            "[ROUND_HISTORY]\n✅ 已完成轮次（压缩摘要）".to_string(),
        );
        let summary_tokens = estimate_message_tokens(&summary);
        let saved = section_tokens.saturating_sub(summary_tokens);

        messages.splice(start..end, vec![summary]);
        compressed_count += 1;
        tokens_saved += saved;

        // Check budget
        let remaining: usize = messages.iter().map(estimate_message_tokens).sum();
        if remaining <= history_budget {
            break;
        }
    }

    if compressed_count > 0 {
        tracing::info!(
            "[CONTEXT_COMPACT] Compressed {} old round(s), saved ~{} tokens, {} msgs",
            compressed_count,
            tokens_saved,
            messages.len()
        );
    }
}
