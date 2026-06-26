//! Implement-phase context diet — fold review exploration, minimal injections.

use super::engine::WorkflowEngine;
use super::phase::{self, SingleFlowPhase};
use crate::message::Message;

const EXPLORE_TOOL_MARKERS: &[&str] = &[
    "file_read",
    "find_symbol",
    "code_search",
    "file_list",
    "file_search",
    "project_detect",
];

/// True when the single-flow state machine is in implementation (slim injections).
pub fn is_slim_phase(engine: &WorkflowEngine) -> bool {
    phase::get(engine) == SingleFlowPhase::Implement
}

/// Compact keep-tail for in-turn message compression during Implement.
pub fn compact_keep_tail() -> usize {
    28
}

/// Fold review-era bulk before each LLM call in Implement phase.
pub fn fold_review_exploration(messages: &mut [Message], engine: &WorkflowEngine) {
    let preserve_impl_reads = is_slim_phase(engine);
    for msg in messages.iter_mut() {
        match msg {
            Message::Assistant {
                content,
                tool_calls,
                ..
            } if tool_calls.is_empty() => {
                if WorkflowEngine::looks_like_review_report(content) {
                    *content = "（审查报告已归档 — 细节见 [WORKSPACE].findings）".into();
                } else if crate::agent::idle_narrative::is_idle_narrative(content)
                    && content.chars().count() > 40
                {
                    *content = "（已折叠空转叙述）".into();
                }
            }
            Message::ToolResult { content, .. } => {
                if preserve_impl_reads && content.contains("── DATA (file_read) ──") {
                    continue;
                }
                if is_explore_tool_result(content) && content.chars().count() > 600 {
                    *content = fold_explore_tool_result(content);
                }
            }
            _ => {}
        }
    }
}

fn is_explore_tool_result(content: &str) -> bool {
    EXPLORE_TOOL_MARKERS
        .iter()
        .any(|m| content.contains(m) || content.starts_with("── DATA"))
}

fn fold_explore_tool_result(content: &str) -> String {
    let preview: String = content.chars().take(200).collect();
    format!("（审查期工具结果已折叠 — 见 [WORKSPACE].file_digests / STEP_MEMORY）\n{preview}…")
}

/// Recent tool lines only (Implement phase STEP_MEMORY).
pub fn build_recent_tool_progress(
    messages: &[Message],
    include_writes: bool,
    max_lines: usize,
) -> String {
    let full = crate::agent::context_injector::build_tool_progress(messages, include_writes);
    if full.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = full.lines().collect();
    if lines.len() <= max_lines {
        return full;
    }
    let skipped = lines.len() - max_lines;
    let tail = lines[lines.len() - max_lines..].join("\n");
    format!("（省略较早 {skipped} 条工具记录）\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_long_review_report() {
        use crate::agent::engine::WorkflowEngine;
        use crate::agent::session::SessionState;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        let body = "## 审查报告\n\n优先级：高\n".to_string() + &"问题描述。".repeat(40);
        let mut msgs = vec![Message::Assistant {
            content: body,
            tool_calls: vec![],
            reasoning_content: None,
        }];
        fold_review_exploration(&mut msgs, &engine);
        if let Message::Assistant { content, .. } = &msgs[0] {
            assert!(content.contains("WORKSPACE"));
        }
    }
}
