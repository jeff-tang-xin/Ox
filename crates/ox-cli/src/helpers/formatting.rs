//! Formatting helper functions.

use std::path::{Component, Path};

use ox_core::message::Message;

/// Short label for an embedding model id (last path segment).
pub fn short_model_id(model_id: &str) -> String {
    model_id.rsplit('/').next().unwrap_or(model_id).to_string()
}

/// Compact path for status bar: `…/parent/name` or truncated.
pub fn short_display_path(path: &str, max_chars: usize) -> String {
    if path.chars().count() <= max_chars {
        return path.to_string();
    }
    let p = Path::new(path);
    let file_name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = p
        .parent()
        .and_then(|pp| pp.file_name())
        .map(|s| s.to_string_lossy().to_string());
    let compact = match parent {
        Some(parent) => format!("{parent}/{file_name}"),
        None => file_name.clone(),
    };
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut parts: Vec<String> = p
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    while parts.len() > 1 {
        let tail = parts.split_off(parts.len().saturating_sub(2)).join("/");
        let candidate = format!("…/{tail}");
        if candidate.chars().count() <= max_chars {
            return candidate;
        }
        parts.pop();
    }
    if file_name.chars().count() <= max_chars {
        file_name
    } else {
        let truncated: String = file_name
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect();
        format!("{truncated}…")
    }
}

/// Summarize tool result for display — outputs compact Markdown-like text.
pub fn summarize_tool_result(name: &str, output: &str) -> String {
    match name {
        "file_write" | "edit_file" | "delete_range" => {
            let first_line = output.lines().next().unwrap_or(output);
            let path = first_line
                .trim_start_matches("✅ Successfully written")
                .trim_start_matches("✅ Successfully applied patch to")
                .trim_start_matches("✅ Successfully applied edit to")
                .trim_start_matches("✅ Successfully deleted")
                .trim()
                .trim_start_matches(" bytes to ")
                .trim();
            let ast_note = if output.contains("⚠️ AST Syntax Check") {
                " `⚠️ AST errors`"
            } else {
                ""
            };
            let short_path = short_display_path(path, 60);
            let truncated: String = short_path.chars().take(100).collect();
            format!("`{truncated}`{ast_note}")
        }
        "file_read" => {
            let line_count = output.lines().count();
            let path = output
                .lines()
                .next()
                .and_then(|l| {
                    l.split_whitespace()
                        .find(|w| w.contains(".java") || w.contains(".kt") || w.contains(".ts") || w.contains(".rs") || w.contains(".py") || w.contains('.')
                            && l.trim_start().chars().all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '.' || c == '/' || c == '\\'))
                })
                .unwrap_or("");
            if path.is_empty() {
                format!("`{line_count} lines`")
            } else {
                let short = short_display_path(path, 50);
                format!("`{short}` · {line_count} lines")
            }
        }
        "code_search" => {
            let match_count = output.lines().take(101).count();
            if output.contains("truncated") {
                "`100+ matches`".into()
            } else if match_count == 0 {
                "`no matches`".into()
            } else {
                format!("`{match_count} matches`")
            }
        }
        "code_graph" => {
            if output.contains("Error") || output.contains("错误") {
                let first_line = output.lines().next().unwrap_or("Error");
                let msg: String = first_line.chars().take(100).collect();
                format!("`❌ {msg}`")
            } else {
                let line_count = output.lines().count();
                format!("`✅ code_graph: {line_count} lines`")
            }
        }
        "shell_exec" => {
            if let Some(line) = output.lines().find(|l| l.starts_with("[exit code:")) {
                format!("`{line}`")
            } else {
                let count = output.lines().count();
                format!("`{count} lines`")
            }
        }
        "file_list" | "file_search" => {
            let count = output.lines().count();
            format!("`{count} entries`")
        }
        "project_detect" => {
            let first_line = output.lines().next().unwrap_or(output);
            let truncated: String = first_line.chars().take(120).collect();
            truncated
        }
        "git_status" | "git_diff" | "git_commit" => {
            let count = output.lines().count();
            format!("`{count} lines`")
        }
        "web_fetch" => {
            let len = output.len();
            format!("`{len} chars`")
        }
        "complete_and_check" => {
            let first = output.lines().next().unwrap_or("?");
            if first.starts_with("✓")
                || first.starts_with("✗")
            {
                let rest: String = output.lines().skip(1).take(2).collect::<Vec<_>>().join(" ");
                let rest: String = rest.chars().take(80).collect();
                format!("{} {}", first, rest)
            } else {
                first.chars().take(120).collect()
            }
        }
        _ => {
            let truncated: String = output.chars().take(120).collect();
            if output.len() > 120 {
                format!("{truncated}…")
            } else {
                truncated
            }
        }
    }
}

/// Extract file path from file_write output.
pub fn extract_file_path_from_output(output: &str) -> Option<String> {
    if let Some(pos) = output.find("to ") {
        // 使用安全的字符边界检查
        if let Some(after_to) = output.get(pos + 3..) {
            if let Some(end_pos) = after_to.find('\n') {
                // 安全地获取子字符串
                after_to.get(..end_pos).map(|s| s.trim().to_string())
            } else {
                Some(after_to.trim().to_string())
            }
        } else {
            None
        }
    } else {
        None
    }
}

/// Extract content from last file_write tool call in messages.
pub fn extract_last_file_write_content(messages: &[Message]) -> Option<String> {
    for msg in messages.iter().rev() {
        if let Message::Assistant { tool_calls, .. } = msg {
            for tc in tool_calls {
                if tc.name == "file_write" {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        return args
                            .get("content")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Calculate tool success rate from session messages.
pub fn calculate_tool_success_rate(messages: &[Message]) -> f64 {
    let mut total_tools = 0u32;
    let mut successful_tools = 0u32;

    for msg in messages {
        if let Message::ToolResult { content, .. } = msg {
            total_tools += 1;
            if !content.starts_with("Error:") && !content.starts_with("Unknown tool") {
                successful_tools += 1;
            }
        }
    }

    if total_tools == 0 {
        1.0
    } else {
        successful_tools as f64 / total_tools as f64
    }
}
