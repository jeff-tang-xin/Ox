//! Context injector — iterative memory for workflow steps.
//!
//! Each LLM iteration injects a compact, high-priority context block that tells
//! the LLM what it has already done and what it must do next. Without this, the
//! LLM treats every iteration as a fresh start because the system prompt
//! (which says "explore") dominates its attention.

use crate::agent::engine::WorkflowEngine;
use crate::message::Message;
use crate::tools::ToolContext;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Inject context at the start of each LLM iteration (after iteration 0).
///
/// The injected message is placed LAST so it gets the LLM's strongest attention.
/// It uses directive language — no suggestions, no hedges.
pub fn inject_context(
    messages: &mut Vec<Message>,
    user_task: &Option<String>,
    iteration: u32,
    tool_ctx: &Arc<ToolContext>,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
) {
    if iteration == 0 {
        return;
    }

    // ── Step 1 (Plan): inject exploration progress with escalating urgency ──
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if engine.get_current_step_index() == 1 {
                let progress = build_exploration_progress(messages);
                if !progress.is_empty() {
                    let directive = if iteration >= 2 {
                        format!(
                            "⛔ 第{}轮 — 你已探索足够。立即输出计划 JSON，禁止再调任何工具。\n\n已探索:\n{}\n\n基于以上信息制定计划。只输出 JSON，不要其他内容。",
                            iteration + 1,
                            progress
                        )
                    } else {
                        format!(
                            "📋 第{}轮 — 基于已探索结果继续。project_detect 已从工具列表移除。\n\n已探索:\n{}",
                            iteration + 1,
                            progress
                        )
                    };
                    messages.push(Message::system(&directive));
                    return;
                }
            }
        }
    }

    // ── Generic fallback: task anchor ──
    if let Some(task) = user_task {
        let anchor: String = task.chars().take(200).collect();
        let mut reminder = format!(
            "📋 Task: {}",
            if anchor.len() < task.len() { format!("{}…", anchor) } else { anchor }
        );
        if iteration % 3 == 0 {
            if let Ok(engine) = tool_ctx.knowledge.try_read() {
                if let Ok(hits) = engine.retrieve_for_context(task, "", 3) {
                    if !hits.is_empty() {
                        reminder.push_str("\n\n📚 Memory:");
                        for hit in hits.iter().take(3) {
                            let preview: String = hit.entity.content.chars().take(100).collect();
                            reminder.push_str(&format!("\n- [{}] {}", hit.entity.kind.as_str(), preview));
                        }
                    }
                }
            }
        }
        messages.push(Message::system(&reminder));
    }
}

/// Scan recent ToolResult messages and build a concise summary of what was explored.
fn build_exploration_progress(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages.iter().rev() {
        if let Message::ToolResult { content, .. } = msg {
            if let (Some(s), Some(e)) = (content.find("── DATA ("), content.find(") ──")) {
                let tool = &content[s + 9..e];
                match tool {
                    "project_detect" => {
                        if content.contains("Project root:") {
                            if let Some(ps) = content.find("Project root:") {
                                let rest = &content[ps + 14..];
                                let root = rest.lines().next().unwrap_or("").trim();
                                parts.push(format!("  project_detect → {}", root));
                            }
                        }
                    }
                    "file_list" => {
                        // Extract first line as directory hint
                        let first_line = content.lines().nth(1).unwrap_or("");
                        let hint: String = first_line.chars().take(40).collect();
                        parts.push(format!("  file_list → {}", hint));
                    }
                    "file_read" => {
                        parts.push("  file_read → 已读取".to_string());
                    }
                    "code_search" | "find_symbol" => {
                        parts.push(format!("  {} → 搜索完成", tool));
                    }
                    _ => {}
                }
            }
            if parts.len() >= 8 {
                break;
            }
        }
    }

    if parts.is_empty() {
        return String::new();
    }
    parts.reverse();
    // Deduplicate
    let mut deduped: Vec<String> = Vec::new();
    for p in &parts {
        let key = &p[..p.len().min(20)];
        if !deduped.iter().any(|d: &String| d.starts_with(key)) {
            deduped.push(p.clone());
        }
    }
    deduped.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_messages() {
        let msgs: Vec<Message> = vec![];
        assert_eq!(build_exploration_progress(&msgs), "");
    }

    #[test]
    fn test_project_detect() {
        let msgs = vec![Message::ToolResult {
            tool_call_id: "t1".into(),
            content: "── DATA (project_detect) ──\nProject root: /home/user/proj\n── END DATA ──".into(),
        }];
        let result = build_exploration_progress(&msgs);
        assert!(result.contains("project_detect"));
        assert!(result.contains("/home/user/proj"));
    }
}
