//! Context injector — iterative memory for workflow steps.
//!
//! Each LLM iteration injects a compact, high-priority context block that tells
//! the LLM what it has already done and what it must do next. Without this, the
//! LLM treats every iteration as a fresh start because the system prompt
//! (which says "explore" or "execute") dominates its attention.

use crate::agent::engine::WorkflowEngine;
use crate::message::Message;
use crate::tools::{ToolContext, ToolRegistry};
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
    tool_registry: &ToolRegistry,
) {
    strip_prior_step_memory(messages);
    strip_prior_discipline(messages);
    strip_prior_phase(messages);
    strip_prior_phase_switch(messages);
    strip_prior_scope_gate(messages);
    strip_prior_tool_route(messages);
    strip_prior_skill_route(messages);

    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if let Some(notice) = crate::agent::phase::consume_transition_notice(&engine) {
                messages.push(Message::system(&notice));
            }
        }
    }

    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if crate::agent::phase::should_inject_workspace(&engine) {
                let slim = crate::agent::context_slim::is_slim_phase(&engine);
                if !slim {
                    if iteration == 0 {
                        messages.push(Message::system(
                            &crate::agent::idle_narrative::discipline_for_iteration(iteration),
                        ));
                    } else if engine.get_task_intent() != crate::agent::task_intent::TaskIntent::Fix
                    {
                        messages.push(Message::system(
                            &crate::agent::idle_narrative::discipline_for_iteration(iteration),
                        ));
                    }
                } else if iteration == 0 {
                    messages.push(Message::system(
                        crate::agent::idle_narrative::RESPONSE_DISCIPLINE,
                    ));
                }
                crate::agent::workspace::inject_workspace(messages, &engine);
                if let Some(gate) = crate::agent::phase::format_scope_gate_directive(&engine) {
                    messages.push(Message::system(&gate));
                }
                inject_task_step_memory(messages, iteration, &engine);
                inject_atlas_routes(messages, &engine, tool_registry);
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

fn strip_prior_phase(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            Message::System { content }
                if content.starts_with(crate::agent::phase::PHASE_TAG)
        )
    });
}

fn strip_prior_phase_switch(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            Message::System { content }
                if content.starts_with(crate::agent::phase::PHASE_SWITCH_TAG)
        )
    });
}

fn strip_prior_scope_gate(messages: &mut Vec<Message>) {
    crate::agent::phase::strip_scope_gate(messages);
}

fn strip_prior_tool_route(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            Message::System { content }
                if content.starts_with(crate::agent::tool_graph::TOOL_ROUTE_TAG)
        )
    });
}

fn strip_prior_skill_route(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            Message::System { content }
                if content.starts_with(crate::skill::policy::SKILL_ROUTE_TAG)
        )
    });
}

fn inject_atlas_routes(messages: &mut Vec<Message>, engine: &WorkflowEngine, tool_registry: &ToolRegistry) {
    messages.push(Message::system(&crate::agent::tool_graph::build_tool_route(engine)));
    let phase = crate::agent::phase::get(engine).as_str().to_string();
    let skills = tool_registry.get_skills_list();
    if let Some(block) = crate::skill::policy::build_skill_route(&skills, &phase) {
        messages.push(Message::system(&block));
    }
}

/// Single-step task memory: tool progress + digest paths in fix mode.
fn inject_task_step_memory(messages: &mut Vec<Message>, _iteration: u32, engine: &WorkflowEngine) {
    let slim = crate::agent::context_slim::is_slim_phase(engine);
    let progress = if slim {
        crate::agent::context_slim::build_recent_tool_progress(messages, true, 10)
    } else {
        build_tool_progress(messages, true)
    };
    let mut body = String::new();
    if !progress.is_empty() {
        body.push_str(&format!("【本轮工具（勿重复）】\n{progress}"));
    }
    if engine.get_task_intent() == crate::agent::task_intent::TaskIntent::Fix
        || crate::agent::context_slim::is_slim_phase(engine)
    {
        let needs_impl_read = crate::agent::workspace::WorkflowWorkspace::build(engine)
            .is_some_and(|ws| matches!(ws.required_action, crate::agent::workspace::RequiredAction::ReadFile { .. }));
        let digests: Vec<String> = crate::agent::tool_digest::all_digests(engine)
            .into_iter()
            .map(|d| format!("  digest: {}", d.path))
            .collect();
        if !digests.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            if needs_impl_read {
                body.push_str("【审查期 digest — 实施须再 file_read 一次】\n");
            } else {
                body.push_str("【已读文件 — 实施阶段直接 edit_file】\n");
            }
            body.push_str(&digests.join("\n"));
        }
    }
    if body.is_empty() {
        return;
    }
    messages.push(Message::system(&format!(
        "{STEP_MEMORY_TAG}\n{body}"
    )));
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
        assert!(p.contains("file_read(src/a.rs@"));
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
