/// Context refinement module - creates condensed, structured conversation summaries.
///
/// Transforms raw message history into精炼 format:
/// "User message: Model response (refined) [tools used]"
use crate::message::Message;

/// A refined conversation turn
#[derive(Debug, Clone)]
pub struct RefinedTurn {
    /// User's original message (kept as-is for context)
    pub user_message: String,
    /// Assistant's refined response (summary of key points, without <think> tags)
    pub assistant_summary: String,
    /// Tools successfully used in this turn
    pub tools_used: Vec<String>,
    /// Whether this turn resulted in successful code changes
    pub has_code_changes: bool,
}

impl RefinedTurn {
    /// Format as compact string for context injection
    pub fn format_compact(&self) -> String {
        let tools_str = if self.tools_used.is_empty() {
            String::new()
        } else {
            format!(" [{}]", self.tools_used.join(", "))
        };

        let change_marker = if self.has_code_changes { " ✏️" } else { "" };

        format!(
            "User: {}\nAssistant: {}{}{}",
            self.user_message, self.assistant_summary, tools_str, change_marker
        )
    }
}

/// Refine assistant response by removing <think> tags and extracting key conclusions
pub fn refine_assistant_response(content: &str) -> String {
    // Remove all <think>...</think> blocks
    let without_thinks = remove_think_blocks(content);

    // Extract key sentences (first meaningful sentence or summary)
    extract_key_points(&without_thinks)
}

/// Remove all <think>...</think> blocks from content
fn remove_think_blocks(content: &str) -> String {
    use regex::Regex;
    let re = Regex::new(r"(?s)<think>.*?</think>").unwrap();
    let cleaned = re.replace_all(content, "").to_string();

    // Clean up extra whitespace
    cleaned
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract key points from assistant response
fn extract_key_points(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() {
        return String::from("(no response)");
    }

    // Strategy: Take first 2-3 meaningful lines or summarize
    let mut key_lines = Vec::new();
    let mut char_count = 0;
    const MAX_CHARS: usize = 200; // Limit summary length

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("---") {
            continue;
        }

        // Skip tool result indicators
        if trimmed.starts_with("📁")
            || trimmed.starts_with("📄")
            || trimmed.starts_with("✅")
            || trimmed.starts_with("❌")
        {
            continue;
        }

        key_lines.push(trimmed);
        char_count += trimmed.len();

        if char_count >= MAX_CHARS {
            break;
        }
    }

    if key_lines.is_empty() {
        // Fallback: take first non-empty line
        lines
            .iter()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().chars().take(100).collect())
            .unwrap_or_else(|| "(response too long)".to_string())
    } else {
        let result = key_lines.join(" ");
        if result.len() > MAX_CHARS {
            format!("{}...", result.chars().take(MAX_CHARS).collect::<String>())
        } else {
            result
        }
    }
}

/// Refine entire conversation history into structured turns
pub fn refine_conversation(messages: &[Message]) -> Vec<RefinedTurn> {
    let mut turns = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        // Look for user message
        if let Message::User { content: user_msg } = &messages[i] {
            let mut assistant_summary = String::new();
            let mut tools_used = Vec::new();
            let mut has_code_changes = false;

            // Look ahead for assistant response and tool results
            let mut j = i + 1;
            while j < messages.len() {
                match &messages[j] {
                    Message::Assistant {
                        content,
                        tool_calls,
                        ..
                    } => {
                        // Only capture the FIRST assistant response (initial answer)
                        if assistant_summary.is_empty() {
                            assistant_summary = refine_assistant_response(content);
                        }

                        // Collect tool calls
                        for tc in tool_calls {
                            if !tools_used.contains(&tc.name) {
                                tools_used.push(tc.name.clone());
                            }
                        }
                        break; // Stop at first assistant message
                    }
                    Message::ToolResult { content, .. } => {
                        // Check if this was a successful code change
                        if content.contains("✅ Successfully")
                            || content.contains("Successfully patched")
                        {
                            has_code_changes = true;
                        }
                        j += 1;
                        continue;
                    }
                    _ => break,
                }
            }

            // Only add turn if there's meaningful content
            if !user_msg.trim().is_empty() {
                turns.push(RefinedTurn {
                    user_message: user_msg.clone(),
                    assistant_summary,
                    tools_used,
                    has_code_changes,
                });
            }

            i = j + 1;
        } else {
            i += 1;
        }
    }

    turns
}

/// Build refined context string from recent conversation turns
pub fn build_refined_context(messages: &[Message], max_turns: usize) -> String {
    let refined_turns = refine_conversation(messages);

    // Take most recent N turns
    let recent_turns: Vec<&RefinedTurn> = refined_turns.iter().rev().take(max_turns).collect();

    if recent_turns.is_empty() {
        return String::new();
    }

    // Format in reverse chronological order (most recent first)
    let mut parts = Vec::new();
    for turn in recent_turns.iter().rev() {
        parts.push(turn.format_compact());
    }

    parts.join("\n\n---\n\n")
}

/// Generate a memory-worthy summary from a completed task
pub fn generate_memory_summary(messages: &[Message]) -> Option<MemorySummary> {
    let turns = refine_conversation(messages);

    if turns.is_empty() {
        return None;
    }

    // Extract topic from first user message (keep it short)
    let topic = turns.first()?.user_message.chars().take(100).collect();

    // Collect all tools used
    let all_tools: Vec<String> = turns
        .iter()
        .flat_map(|t| t.tools_used.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Check for code changes
    let has_changes = turns.iter().any(|t| t.has_code_changes);

    // 🔍 Extract key insights: look for ## Done block in ALL assistant messages
    let mut key_insights = String::new();
    for msg in messages.iter().rev() {
        if let Message::Assistant { content, .. } = msg {
            // Prefer Done block content
            if let Some(done_pos) = content.rfind("## Done") {
                let done_content = &content[done_pos..];
                let first_line = done_content
                    .lines()
                    .find(|l| !l.trim().is_empty() && !l.trim().starts_with("##"))
                    .unwrap_or("");
                if !first_line.trim().is_empty() {
                    key_insights = first_line.trim().chars().take(200).collect();
                    break;
                }
            }
        }
        // Look for successful patch in tool results
        if let Message::ToolResult { content, .. } = msg
            && (content.contains("Successfully patched") || content.contains("Successfully written"))
            {
                let line = content
                    .lines()
                    .find(|l| l.contains("Successfully"))
                    .unwrap_or("");
                key_insights = line.trim().chars().take(200).collect();
                break;
            }
    }

    // Fallback to first assistant summary if no Done block found
    if key_insights.is_empty() {
        // Look for the LAST assistant message with real content (not just Plan)
        for msg in messages.iter().rev() {
            if let Message::Assistant { content, .. } = msg {
                let trimmed = content.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with("## Plan")
                    && !trimmed.starts_with("▎ Plan")
                {
                    key_insights = trimmed.chars().take(200).collect();
                    break;
                }
            }
        }
    }
    if key_insights.is_empty() {
        key_insights = turns.last()?.assistant_summary.clone();
    }

    Some(MemorySummary {
        topic,
        key_insights,
        tools_used: all_tools,
        has_code_changes: has_changes,
        turn_count: turns.len(),
    })
}

/// A memory-worthy summary of a completed task
#[derive(Debug, Clone)]
pub struct MemorySummary {
    pub topic: String,
    pub key_insights: String,
    pub tools_used: Vec<String>,
    pub has_code_changes: bool,
    pub turn_count: usize,
}

impl MemorySummary {
    /// Format for storage in memory system
    pub fn format_for_storage(&self) -> String {
        format!(
            "Topic: {}\nKey Insights: {}\nTools: {}\nCode Changes: {}\nTurns: {}",
            self.topic,
            self.key_insights,
            self.tools_used.join(", "),
            if self.has_code_changes { "Yes" } else { "No" },
            self.turn_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_think_blocks() {
        let input = "<think>Let me analyze this</think>\n\nThe answer is 42.";
        let output = remove_think_blocks(input);
        assert_eq!(output, "The answer is 42.");
    }

    #[test]
    fn test_refine_simple_conversation() {
        let messages = vec![
            Message::user("What is Rust?"),
            Message::assistant(
                "<think>Thinking...</think>\n\nRust is a systems programming language.",
            ),
        ];

        let turns = refine_conversation(&messages);
        assert_eq!(turns.len(), 1);
        assert!(!turns[0].assistant_summary.contains("<think>"));
        assert!(turns[0].assistant_summary.contains("Rust"));
    }

    #[test]
    fn test_refine_with_tools() {
        use crate::message::ToolCall;

        let messages = vec![
            Message::user("Read the file"),
            Message::Assistant {
                content: "<think>I'll read it</think>\nReading now.".to_string(),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "file_read".to_string(),
                    arguments: "{}".to_string(),
                }],
                reasoning_content: None,
            },
            Message::ToolResult {
                tool_call_id: "call_1".to_string(),
                content: "File contents".to_string(),
            },
        ];

        let turns = refine_conversation(&messages);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tools_used, vec!["file_read"]);
    }
}
