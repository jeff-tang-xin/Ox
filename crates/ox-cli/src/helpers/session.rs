//! Session-related helper functions.

use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use crate::terminal::app::App;
use crate::terminal::output_pane::OutputLine;

const REPLAY_HISTORY_DEPTH: usize = 100;

/// Get display name for a session based on first user message.
pub fn session_display_name(session: &Session) -> String {
    session
        .messages
        .iter()
        .find_map(|m| match m {
            ox_core::message::Message::User { content } => {
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    let first_line = trimmed.lines().next().unwrap_or(trimmed);
                    let display = if first_line.chars().count() > 6 {
                        format!("{}..", first_line.chars().take(6).collect::<String>())
                    } else {
                        first_line.to_string()
                    };
                    Some(display)
                }
            }
            _ => None,
        })
        .unwrap_or_else(|| "new session".to_string())
}

/// Replay the last N messages from a session into the OutputPane.
pub fn replay_session_history(
    app: &mut App,
    messages: &[Message],
    rt_env: &RuntimeEnvironment,
    has_provider: bool,
) {
    app.output.clear();

    let start = messages.len().saturating_sub(REPLAY_HISTORY_DEPTH);
    // 使用安全的切片方法
    let slice = if start < messages.len() {
        &messages[start..]
    } else {
        &[]
    };
    if slice.is_empty() {
        refresh_header_info(app, rt_env, has_provider);
        app.message_count = messages.len();
        return;
    }

    app.output.push_line(OutputLine::System(format!(
        "--- {} messages ---",
        slice.len()
    )));

    let mut tc_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for msg in slice {
        match msg {
            Message::System { .. } => {} // Skip system prompts in display
            Message::User { content } => {
                app.output.push_line(OutputLine::User(content.clone()));
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if !content.is_empty() {
                    app.output.push_line(OutputLine::Markdown(content.clone()));
                }
                for tc in tool_calls {
                    tc_map.insert(tc.id.clone(), tc.name.clone());
                    app.output.push_line(OutputLine::Tool {
                        name: tc.name.clone(),
                        detail: None,
                    });
                }
            }
            Message::ToolResult {
                tool_call_id,
                content,
            } => {
                let name = tc_map
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
                let summary = super::formatting::summarize_tool_result(&name, content);
                let is_error = content.starts_with("Error:") || content.starts_with("Unknown tool");
                app.output.push_line(OutputLine::ToolResult {
                    name,
                    summary,
                    is_error,
                });
            }
        }
    }

    app.output.push_line(OutputLine::System("--- end ---".to_string()));

    refresh_header_info(app, rt_env, has_provider);
    app.message_count = messages.len();
}

/// Refresh header_info from current runtime state.
pub fn refresh_header_info(app: &mut App, rt_env: &RuntimeEnvironment, has_provider: bool) {
    use crate::terminal::app::PlanItemStatus;
    app.header_info.clear();
    app.header_info.push(rt_env.banner_summary());
    if has_provider {
        app.header_info.push("Type a message or /help. /exit to quit.".into());
    } else {
        app.header_info.push("No API key. Running in echo mode.".into());
    }
    // Show active plan items
    if !app.plan_items.is_empty() {
        let pending: Vec<_> = app.plan_items.iter().filter(|p| p.status == PlanItemStatus::Pending).collect();
        let done: Vec<_> = app.plan_items.iter().filter(|p| p.status == PlanItemStatus::Done).collect();
        if !pending.is_empty() {
            let files: Vec<_> = pending.iter().map(|p| p.file.as_str()).collect();
            app.header_info.push(format!("📋 Plan: {}", files.join(", ")));
        }
        if !done.is_empty() {
            let files: Vec<_> = done.iter().map(|p| p.file.as_str()).collect();
            app.header_info.push(format!("✅ Done: {}", files.join(", ")));
        }
        if pending.is_empty() && !done.is_empty() {
            app.header_info.push("✅ All planned tasks complete.".into());
        }
    }
    app.working_dir = rt_env
        .working_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rt_env.working_dir.display().to_string());
}
