//! Context injector — iterative memory for workflow steps.
//!
//! Each LLM iteration injects a compact, high-priority context block that tells
//! the LLM what it has already done and what it must do next. Without this, the
//! LLM treats every iteration as a fresh start because the system prompt
//! (which says "explore" or "execute") dominates its attention.

use crate::agent::engine::WorkflowEngine;
use crate::message::Message;
use crate::tools::ToolContext;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Marker prefix — prior step-memory injections are stripped before each iteration.
pub const STEP_MEMORY_TAG: &str = "[STEP_MEMORY]";

/// Inject context at the start of each LLM iteration.
///
/// The injected message is placed LAST so it gets the LLM's strongest attention.
pub fn inject_context(
    messages: &mut Vec<Message>,
    user_task: &Option<String>,
    iteration: u32,
    tool_ctx: &Arc<ToolContext>,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
) {
    strip_prior_step_memory(messages);
    strip_prior_discipline(messages);

    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if engine.is_workflow_active() && !engine.is_workflow_complete() {
                // Per-iteration refresh: full body on iter 0, one-liner after.
                messages.push(Message::system(
                    &crate::agent::idle_narrative::discipline_for_iteration(iteration),
                ));
                crate::agent::workspace::inject_workspace(messages, &engine);
                inject_task_step_memory(messages, iteration, &engine);
                return;
            }
        }
    }

    // ── Generic fallback (non-workflow) ──
    if iteration == 0 {
        return;
    }

    if let Some(task) = user_task {
        let anchor: String = task.chars().take(200).collect();
        let mut reminder = format!(
            "{STEP_MEMORY_TAG}\n📋 Task: {}",
            if anchor.len() < task.len() {
                format!("{anchor}…")
            } else {
                anchor
            }
        );
        if iteration % 3 == 0 {
            if let Ok(engine) = tool_ctx.knowledge.try_read() {
                if let Ok(hits) = engine.retrieve_for_context(task, "", 3) {
                    if !hits.is_empty() {
                        reminder.push_str("\n\n📚 Memory:");
                        for hit in hits.iter().take(3) {
                            let preview: String = hit.entity.content.chars().take(100).collect();
                            reminder.push_str(&format!(
                                "\n- [{}] {}",
                                hit.entity.kind.as_str(),
                                preview
                            ));
                        }
                    }
                }
            }
        }
        messages.push(Message::system(&reminder));
    }
}

fn strip_prior_discipline(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            Message::System { content }
                if content.starts_with("【输出纪律】")
        )
    });
}

fn strip_prior_step_memory(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(m, Message::System { content } if content.starts_with(STEP_MEMORY_TAG))
    });
}

/// Single-step task memory: tool progress + plan tracker + user guidance.
fn inject_task_step_memory(
    messages: &mut Vec<Message>,
    iteration: u32,
    engine: &WorkflowEngine,
) {
    if crate::agent::workspace::uses_workspace_memory(engine) {
        let progress = build_tool_progress(messages, true);
        if !progress.is_empty() {
            messages.push(Message::system(&format!(
                "{STEP_MEMORY_TAG}\n【本轮工具（勿重复）】\n{progress}"
            )));
        }
        return;
    }

    let progress = build_tool_progress(messages, true);
    let mut parts = vec![format!(
        "{STEP_MEMORY_TAG}\n⚡ 第{}轮 — 调工具或交产物（## Done / findings / 直接答），禁止空转。",
        iteration + 1
    )];

    if iteration == 0 {
        if let Some(req) = engine.get_variable("_current_user_request") {
            if !req.trim().is_empty() {
                let snippet: String = req.chars().take(800).collect();
                parts.push(format!("【用户请求】\n{snippet}"));
            }
        }
        let guidance = engine.workflow_guidance_block();
        if !guidance.is_empty() {
            parts.push(guidance);
        }
    }

    if !progress.is_empty() {
        parts.push(format!("【本轮已完成（勿重复）】\n{progress}"));
    }

    let plan_progress = engine.plan_progress_summary();
    if !plan_progress.is_empty() {
        parts.push(plan_progress);
    }

    if let Some(report) = engine.execute_review_report_block(2000) {
        parts.push(report);
    }

    messages.push(Message::system(&parts.join("\n\n")));
}

pub fn build_tool_progress(messages: &[Message], include_writes: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (i, msg) in messages.iter().enumerate() {
        let Message::Assistant { tool_calls, .. } = msg else {
            continue;
        };
        for tc in tool_calls {
            let Some(key) = tool_key(&tc.name, &tc.arguments, include_writes) else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            let outcome = messages
                .iter()
                .skip(i + 1)
                .find_map(|m| {
                    if let Message::ToolResult {
                        tool_call_id,
                        content,
                    } = m
                    {
                        if tool_call_id == &tc.id {
                            Some(if content.contains('❌') {
                                "失败"
                            } else {
                                "成功"
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .unwrap_or("已调用");
            parts.push(format_tool_line(&tc.name, &key, outcome));
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    }
}

fn tool_key(name: &str, arguments: &str, include_writes: bool) -> Option<String> {
    match name {
        "file_list" => {
            let path = parse_tool_path_arg(arguments).unwrap_or_else(|| ".".into());
            Some(format!("file_list:{path}"))
        }
        "file_read" => {
            let path = parse_tool_path_arg(arguments).unwrap_or_else(|| "?".into());
            let (offset, limit) = parse_read_range(arguments);
            Some(format!("file_read:{path}@{offset}+{limit}"))
        }
        "file_write" | "edit_file" | "delete_range" if include_writes => {
            let path = parse_tool_path_arg(arguments).unwrap_or_else(|| "?".into());
            Some(format!("{name}:{path}"))
        }
        "shell_exec" if include_writes => {
            let cmd = parse_shell_command(arguments).unwrap_or_else(|| "?".into());
            Some(format!("shell_exec:{cmd}"))
        }
        "project_detect" => Some("project_detect".into()),
        "find_symbol" | "code_search" | "file_search" => Some(format!(
            "{name}:{}",
            arguments.chars().take(60).collect::<String>()
        )),
        _ => None,
    }
}

fn format_tool_line(name: &str, key: &str, outcome: &str) -> String {
    match name {
        "file_list" => format!(
            "  file_list({}) → {outcome}",
            key.strip_prefix("file_list:").unwrap_or("?")
        ),
        "file_read" => format!(
            "  file_read({}) → {outcome}",
            key.strip_prefix("file_read:").unwrap_or("?")
        ),
        "file_write" => format!(
            "  file_write({}) → {outcome}",
            key.strip_prefix("file_write:").unwrap_or("?")
        ),
        "edit_file" => format!(
            "  edit_file({}) → {outcome}",
            key.strip_prefix("edit_file:").unwrap_or("?")
        ),
        "delete_range" => format!(
            "  delete_range({}) → {outcome}",
            key.strip_prefix("delete_range:").unwrap_or("?")
        ),
        "shell_exec" => format!(
            "  shell_exec({}) → {outcome}",
            key.strip_prefix("shell_exec:").unwrap_or("?")
        ),
        "project_detect" => format!("  project_detect → {outcome}"),
        other => format!("  {other} → {outcome}"),
    }
}

fn parse_tool_path_arg(arguments: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
}

fn parse_read_range(arguments: &str) -> (u64, u64) {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .map(|v| {
            let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
            let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200);
            (offset, limit)
        })
        .unwrap_or((0, 200))
}

fn parse_shell_command(arguments: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| {
            v.get("command")
                .or_else(|| v.get("cmd"))
                .and_then(|p| p.as_str())
                .map(|s| s.chars().take(80).collect())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;

    #[test]
    fn test_empty_messages() {
        let msgs: Vec<Message> = vec![];
        assert_eq!(build_tool_progress(&msgs, false), "");
    }

    #[test]
    fn test_execute_progress_includes_writes() {
        let msgs = vec![Message::Assistant {
            content: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "t1".into(),
                    name: "file_read".into(),
                    arguments: r#"{"path":"src/a.rs"}"#.into(),
                },
                ToolCall {
                    id: "t2".into(),
                    name: "edit_file".into(),
                    arguments: r#"{"path":"src/a.rs"}"#.into(),
                },
            ],
            reasoning_content: None,
        }];
        let p = build_tool_progress(&msgs, true);
        assert!(p.contains("file_read(src/a.rs)"));
        assert!(p.contains("edit_file(src/a.rs)"));
    }

    #[test]
    fn test_strip_prior_step_memory() {
        let mut msgs = vec![
            Message::system("[STEP_MEMORY]\nold"),
            Message::user("hi"),
        ];
        strip_prior_step_memory(&mut msgs);
        assert_eq!(msgs.len(), 1);
    }
}
