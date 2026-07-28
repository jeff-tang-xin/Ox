//! Context injector — iterative memory for workflow steps.
//!
//! Only live utilities used elsewhere:
//! - [`STEP_MEMORY_TAG`] — marker constant for step-memory injection blocks
//! - [`build_tool_progress`] — compact tool-history log for turn-memory / slim context
//!
//! The legacy `inject_context` function (which did all stripping + workspace injection)
//! has been removed — it was dead code never called from anywhere. The actual
//! per-iteration injection now lives in `mod.rs:inject_slim_context()`.

use crate::message::Message;

/// Marker prefix — prior step-memory injections are stripped before each iteration.
pub const STEP_MEMORY_TAG: &str = "[STEP_MEMORY]";

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
    if name == crate::agent::unified_action::TOOL_NAME {
        return unified_tool_key(arguments, include_writes);
    }
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

fn unified_tool_key(arguments: &str, include_writes: bool) -> Option<String> {
    let req = crate::agent::unified_action::parse_request(arguments).ok()?;
    let inner = crate::agent::unified_action::action_to_tool_name(&req.action)
        .unwrap_or(req.action.as_str());
    if inner == "finish" {
        let kind = if crate::agent::unified_action::finding_json(&req.params).is_some() {
            "finish:findings"
        } else {
            "finish"
        };
        return Some(format!("complete_and_check:{kind}"));
    }
    tool_key(inner, &req.params.to_string(), include_writes)
        .map(|k| format!("complete_and_check:{k}"))
}

fn format_tool_line(name: &str, key: &str, outcome: &str) -> String {
    if name == crate::agent::unified_action::TOOL_NAME {
        return format!("  complete_and_check({key}) → {outcome}");
    }
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
        .and_then(|v| {
            v.get("path")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
        })
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
        let mut msgs = vec![Message::system("[STEP_MEMORY]\nold"), Message::user("hi")];
        let before = msgs.len();
        // strip_prior_step_memory was removed with dead code; the actual stripping
        // happens in strip_all_injection_blocks in mod.rs.
        // This test validates the constant still exists.
        assert_eq!(STEP_MEMORY_TAG, "[STEP_MEMORY]");
        // Strip by hand to verify the tag format
        msgs.retain(
            |m| !matches!(m, Message::System { content } if content.starts_with(STEP_MEMORY_TAG)),
        );
        assert_eq!(msgs.len(), 1);
        assert_eq!(before, 2);
    }
}
