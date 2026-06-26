//! Session-related helper functions.

use crate::terminal::app::App;
use crate::terminal::output_pane::OutputLine;
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;

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
                    // For old sessions: detect internal step JSON and format it for display
                    let display = maybe_format_internal_step_output(content);
                    app.output.push_line(OutputLine::Markdown(display));
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

    app.output
        .push_line(OutputLine::System("--- end ---".to_string()));

    refresh_header_info(app, rt_env, has_provider);
    app.message_count = messages.len();
}

/// Detect and format internal workflow step JSON output for display.
/// Used during session replay to prettify raw JSON stored in older sessions.
fn maybe_format_internal_step_output(content: &str) -> String {
    let json_str = if let (Some(s), Some(e)) = (content.find('{'), content.rfind('}')) {
        &content[s..=e]
    } else {
        return content.to_string();
    };

    let parsed: Option<serde_json::Value> = serde_json::from_str(json_str).ok();
    let v = match parsed {
        Some(v) => v,
        None => return content.to_string(),
    };

    // Detect step type by JSON fields and format accordingly
    if v.get("intent").and_then(|s| s.as_str()).is_some() {
        // Step 0: Intent Classification
        let intent = v.get("intent").and_then(|s| s.as_str()).unwrap_or("?");
        let complexity = v.get("complexity").and_then(|s| s.as_str()).unwrap_or("");
        let topic = v.get("topic").and_then(|s| s.as_str()).unwrap_or("");
        let emoji = match intent {
            "coding" => "💻",
            "exploring" => "🔍",
            "chat" => "💬",
            _ => "🤔",
        };
        if topic.is_empty() {
            format!("{} {}({})", emoji, intent, complexity)
        } else {
            format!("{} {}({}) — {}", emoji, intent, complexity, topic)
        }
    } else if v.get("plan").and_then(|p| p.as_array()).is_some() {
        // Step 1: Task Planning (supports both old string format and new object format)
        let mut lines = vec!["📋 **执行计划**".to_string()];
        if let Some(plan) = v.get("plan").and_then(|p| p.as_array()) {
            for step in plan {
                if let Some(obj) = step.as_object() {
                    let num = obj.get("step").and_then(|s| s.as_u64()).unwrap_or(0);
                    let file = obj.get("file").and_then(|s| s.as_str()).unwrap_or("");
                    let action = obj.get("action").and_then(|s| s.as_str()).unwrap_or("");
                    let target = obj.get("target").and_then(|s| s.as_str()).unwrap_or("");
                    let desc = obj.get("desc").and_then(|s| s.as_str()).unwrap_or("");
                    let verify = obj.get("verify").and_then(|s| s.as_str()).unwrap_or("");
                    let action_icon = match action {
                        "add" | "create" => "➕",
                        "modify" => "✏️",
                        "delete" => "🗑️",
                        _ => "→",
                    };
                    let target_str = if target.is_empty() {
                        String::new()
                    } else {
                        format!(" `{}`", target)
                    };
                    let file_str = if file.is_empty() {
                        String::new()
                    } else {
                        format!(" 📄`{}`", file)
                    };
                    let verify_str = if verify.is_empty() {
                        String::new()
                    } else {
                        format!(" 🔍{}", verify)
                    };
                    lines.push(format!(
                        "  {}. {}{}{} — {}{}",
                        num, action_icon, target_str, file_str, desc, verify_str
                    ));
                } else if let Some(s) = step.as_str() {
                    lines.push(format!("  - {}", s));
                }
            }
        }
        if let Some(skills) = v.get("skills").and_then(|s| s.as_array()) {
            let names: Vec<&str> = skills.iter().filter_map(|s| s.as_str()).collect();
            if !names.is_empty() {
                lines.push(format!("\n🧠 Skills: {}", names.join(", ")));
            }
        }
        if let Some(files) = v.get("key_files").and_then(|f| f.as_array()) {
            let names: Vec<&str> = files.iter().filter_map(|f| f.as_str()).collect();
            if !names.is_empty() {
                lines.push(format!("📁 关键文件: {}", names.join(", ")));
            }
        }
        lines.join("\n")
    } else if v.get("safe").and_then(|s| s.as_bool()).is_some()
        && v.get("complete").and_then(|c| c.as_bool()).is_some()
    {
        // Step 2: Review (new format with safe + complete)
        let safe = v.get("safe").and_then(|s| s.as_bool()).unwrap_or(true);
        let complete = v.get("complete").and_then(|c| c.as_bool()).unwrap_or(true);
        let issues = v
            .get("issues")
            .and_then(|i| i.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        let warnings = v
            .get("warnings")
            .and_then(|w| w.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut lines = Vec::new();
        if safe && complete && issues.is_empty() {
            lines.push("✅ 计划通过审阅".to_string());
        } else {
            if !safe {
                lines.push("⚠️ 安全问题".to_string());
            }
            if !complete {
                lines.push("⚠️ 计划不完整".to_string());
            }
            for issue in &issues {
                lines.push(format!("  ❌ {}", issue));
            }
        }
        for warning in &warnings {
            lines.push(format!("  💡 {}", warning));
        }
        lines.join("\n")
    } else if v.get("safe").and_then(|s| s.as_bool()).is_some() && v.get("complete").is_none() {
        // Old Safety Check (backward compat: safe but no "complete" field)
        let safe = v.get("safe").and_then(|s| s.as_bool()).unwrap_or(true);
        if safe {
            "✅ 安全".to_string()
        } else {
            let reason = v.get("reason").and_then(|s| s.as_str()).unwrap_or("");
            format!("⚠️ 不安全 — {}", reason)
        }
    } else {
        // Not an internal step output — leave as-is
        content.to_string()
    }
}

/// Refresh header_info from current runtime state.
pub fn refresh_header_info(app: &mut App, rt_env: &RuntimeEnvironment, has_provider: bool) {
    use crate::terminal::app::PlanItemStatus;
    app.header_info.clear();
    app.header_info.push(rt_env.banner_summary());
    if has_provider {
        app.header_info
            .push("Type a message or /help. /exit to quit.".into());
    } else {
        app.header_info
            .push("No API key. Running in echo mode.".into());
    }
    // Show active plan items
    if !app.plan_items.is_empty() {
        let pending: Vec<_> = app
            .plan_items
            .iter()
            .filter(|p| p.status == PlanItemStatus::Pending)
            .collect();
        let done: Vec<_> = app
            .plan_items
            .iter()
            .filter(|p| p.status == PlanItemStatus::Done)
            .collect();
        if !pending.is_empty() {
            let files: Vec<_> = pending.iter().map(|p| p.file.as_str()).collect();
            app.header_info
                .push(format!("📋 Plan: {}", files.join(", ")));
        }
        if !done.is_empty() {
            let files: Vec<_> = done.iter().map(|p| p.file.as_str()).collect();
            app.header_info
                .push(format!("✅ Done: {}", files.join(", ")));
        }
        if pending.is_empty() && !done.is_empty() {
            app.header_info
                .push("✅ All planned tasks complete.".into());
        }
    }
    app.working_dir = rt_env
        .working_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rt_env.working_dir.display().to_string());
}
