//! Tool execution helpers — extracted from the agent turn loop in `mod.rs`.
//!
//! Provides pure helper functions for tool detail display, tool lookup,
//! and error message formatting. The main execution loop remains in `run_agent_turn`
//! due to its tight coupling with iteration state (messages, new_messages, etc.).

use serde_json::Value;

/// Extract a human-readable detail string from tool call arguments for UI display.
pub fn extract_tool_detail(tool_name: &str, arguments: &str) -> Option<String> {
    let args: Option<Value> = serde_json::from_str(arguments).ok();

    match tool_name {
        "shell_exec" => args
            .as_ref()
            .and_then(|v| v.get("command"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string()),
        "file_read" => args.as_ref().map(|v| {
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let limit = v
                .get("limit")
                .and_then(|l| l.as_u64())
                .map(|l| format!(" (limit:{})", l))
                .unwrap_or_default();
            format!("{} {}", path, limit)
        }),
        "file_write" => args.as_ref().map(|v| {
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let size = v
                .get("content")
                .and_then(|c| c.as_str())
                .map(|c| c.len())
                .unwrap_or(0);
            format!("{} ({} bytes)", path, size)
        }),
        "edit_file" => args.as_ref().map(|v| {
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let old = v
                .get("old_string")
                .and_then(|s| s.as_str())
                .map(|s| {
                    let one_line = s.lines().next().unwrap_or(s);
                    if one_line.len() > 60 {
                        let boundary = one_line
                            .char_indices()
                            .take_while(|(i, _)| *i < 60)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(one_line.len());
                        &one_line[..boundary]
                    } else {
                        one_line
                    }
                })
                .unwrap_or("");
            format!("{} | {}", path, old)
        }),
        "delete_range" => args.as_ref().map(|v| {
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let start = v.get("start_line").and_then(|l| l.as_u64()).unwrap_or(0);
            let end = v.get("end_line").and_then(|l| l.as_u64()).unwrap_or(0);
            format!("{} L{}-L{}", path, start, end)
        }),
        "code_search" => args
            .as_ref()
            .and_then(|v| v.get("pattern"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string()),
        "find_symbol" => args.as_ref().map(|v| {
            let query = v.get("name").or_else(|| v.get("query")).and_then(|q| q.as_str()).unwrap_or("?");
            let kind = v
                .get("kind")
                .and_then(|k| k.as_str())
                .map(|k| format!(" ({})", k))
                .unwrap_or_default();
            format!("{}{}", query, kind)
        }),
        "file_search" => args
            .as_ref()
            .and_then(|v| v.get("pattern"))
            .and_then(|p| p.as_str())
            .map(|s| format!("glob: {}", s)),
        "file_list" => args
            .as_ref()
            .and_then(|v| v.get("path"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string())
            .or_else(|| Some("(root)".to_string())),
        "git_status" => Some(String::new()),
        "git_diff" => args.as_ref().map(|v| {
            let staged = v.get("staged").and_then(|s| s.as_bool()).unwrap_or(false);
            if staged { "--staged" } else { "" }.to_string()
        }),
        "memory_search" => args
            .as_ref()
            .and_then(|v| v.get("query"))
            .and_then(|q| q.as_str())
            .map(|s| s.to_string()),
        "recall" => args
            .as_ref()
            .and_then(|v| v.get("node_id"))
            .and_then(|q| q.as_str())
            .map(|s| s.to_string()),
        "web_fetch" => args
            .as_ref()
            .and_then(|v| v.get("url"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Build a helpful error message for an unknown tool, including suggestions.
pub fn build_unknown_tool_error(tool_name: &str, available_tools: &[String]) -> String {
    let available = available_tools.join(", ");

    // Find similar tool names (prefix-based matching)
    let suggestions: Vec<&str> = available_tools
        .iter()
        .filter(|name| {
            let tc_prefix = tool_name.get(..tool_name.len().min(3)).unwrap_or(tool_name);
            let name_prefix = name.get(..name.len().min(3)).unwrap_or(name);
            name.starts_with(tc_prefix) || tool_name.starts_with(name_prefix)
        })
        .map(|s| s.as_str())
        .collect();

    let suggestion_text = if !suggestions.is_empty() {
        format!("\n\n💡 Did you mean: {}?", suggestions.join(", "))
    } else {
        String::new()
    };

    format!(
        "❌ Unknown tool: '{}'\n\n\
         Available tools: {}{}\n\n\
         💡 Tips:\n\
         • Check the tool name spelling\n\
         • Use /help to see all available tools\n\
         • Tool names are case-sensitive",
        tool_name, available, suggestion_text
    )
}

/// Build a helpful error message for JSON parse failures in tool arguments.
pub fn build_json_parse_error(tool_name: &str, parse_err: &str) -> String {
    let example = match tool_name {
        "file_read" => "{\"path\": \"src/main.rs\", \"limit\": 100}",
        "file_write" => "{\"path\": \"output.txt\", \"content\": \"Hello World\"}",
        "edit_file" => {
            "{\"path\": \"src/lib.rs\", \"old_string\": \"...\", \"new_string\": \"...\"}"
        }
        "shell_exec" => "{\"command\": \"ls -la\", \"timeout_ms\": 5000}",
        "file_search" => "{\"pattern\": \"*.rs\", \"path\": \"src/\"}",
        "code_search" => "{\"pattern\": \"fn main\", \"path\": \"src/\"}",
        "code_graph" => "{\"op\": \"impact\", \"target\": \"funcName\", \"direction\": \"upstream\"}",
        _ => "{ /* check tool documentation */ }",
    };

    format!(
        "❌ JSON Parse Error for tool '{}':\n{}\n\n\
         💡 How to fix:\n\
         • Ensure valid JSON syntax (no trailing commas)\n\
         • Quote all keys and string values with double quotes\n\
         • Escape special characters in strings\n\
         • Check for missing brackets or braces\n\n\
         📝 Correct format example:\n\
         {}\n\n\
         Please retry with corrected arguments.",
        tool_name, parse_err, example
    )
}

/// Get a progress message for a tool that's about to execute.
pub fn tool_progress_message(tool_name: &str) -> &'static str {
    match tool_name {
        "file_write" => "Starting file write...",
        "file_read" => "Reading file...",
        "shell_exec" => "Executing command...",
        "code_search" => "Searching code...",
        "edit_file" => "Editing file...",
        "delete_range" => "Deleting range...",
        "find_symbol" => "Finding symbols...",
        _ => "Executing...",
    }
}

/// Check if a tool call references a path outside the working directory.
pub fn is_path_outside_workdir(args_json: &str, working_dir: &std::path::Path) -> bool {
    if let Ok(args_val) = serde_json::from_str::<Value>(args_json)
        && let Some(path_str) = args_val.get("path").and_then(|v| v.as_str())
    {
        let resolved = working_dir.join(path_str);
        return !crate::safety::is_path_within_workdir(&resolved, working_dir);
    }
    false
}
