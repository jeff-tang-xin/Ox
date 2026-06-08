/// delete_range — delete a contiguous range of lines using exact text anchors.
///
/// Each anchor must match exactly one line in the file. The range
/// (inclusive by default) is deleted. Use for large block deletions
/// where constructing a giant old_string for edit_file would be unwieldy.

use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct DeleteRangeTool;

#[async_trait::async_trait]
impl Tool for DeleteRangeTool {
    fn name(&self) -> &str {
        "delete_range"
    }

    fn description(&self) -> &str {
        "Delete a contiguous text range from a file using exact start/end text anchors. \
         Each anchor must match exactly one line. Use for large block deletions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace root)."
                },
                "start_anchor": {
                    "type": "string",
                    "description": "Exact text of the first line to delete (must be unique in the file)."
                },
                "end_anchor": {
                    "type": "string",
                    "description": "Exact text of the last line to delete (must be unique in the file)."
                },
                "inclusive": {
                    "type": "boolean",
                    "description": "Whether to include the anchor lines in the deletion. Default true."
                }
            },
            "required": ["path", "start_anchor", "end_anchor"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p.trim().replace('\\', "/"),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'path'.\n\
                 Usage: {\"path\": \"<file>\", \"start_anchor\": \"<first line>\", \"end_anchor\": \"<last line>\"}",
            ),
        };

        let resolved_path = if std::path::Path::new(&path_str).is_absolute() {
            std::path::PathBuf::from(&path_str)
        } else {
            ctx.working_dir.join(&path_str)
        };

        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };

        let start_anchor = match args.get("start_anchor").and_then(|s| s.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::error("❌ Missing required parameter: 'start_anchor'."),
        };
        let end_anchor = match args.get("end_anchor").and_then(|s| s.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::error("❌ Missing required parameter: 'end_anchor'."),
        };
        let inclusive = args.get("inclusive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let display_path = path.display().to_string();

        let result = tokio::task::spawn_blocking(move || {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => return Err(format!(
                    "❌ Cannot read {}: {e}\n💡 Check the path with file_list or file_read.",
                    path.display()
                )),
            };

            let lines: Vec<&str> = content.lines().collect();
            let n = lines.len();

            // Find start_anchor — must match exactly one line
            let mut start_idx = None;
            for (i, line) in lines.iter().enumerate() {
                if line == &start_anchor {
                    if start_idx.is_some() {
                        return Err(format!(
                            "❌ start_anchor matches multiple lines in {}.\n\
                             Lines {} and {} both match:\n  {}\n\
                             💡 Add more text to make the anchor unique.",
                            display_path,
                            start_idx.unwrap() + 1,
                            i + 1,
                            start_anchor
                        ));
                    }
                    start_idx = Some(i);
                }
            }
            let start_idx = match start_idx {
                Some(i) => i,
                None => {
                    // Build hint: show lines containing similar text
                    let mut hint = String::new();
                    for (i, line) in lines.iter().enumerate() {
                        if line.contains(start_anchor.trim()) || start_anchor.trim().contains(line) {
                            hint.push_str(&format!("  Line {}: {}\n", i + 1, line));
                            if hint.lines().count() >= 5 { break; }
                        }
                    }
                    return Err(format!(
                        "❌ start_anchor not found in {}.\n\n\
                         {}\
                         💡 Fix: use file_read to get the EXACT line text, then retry.",
                        display_path,
                        if hint.is_empty() {
                            "No similar lines found — the content may have changed.\n".to_string()
                        } else {
                            format!("🔍 Similar lines:\n{hint}\n")
                        }
                    ));
                }
            };

            // Find end_anchor — must match exactly one line, and be after start_anchor
            let mut end_idx = None;
            for (i, line) in lines.iter().enumerate() {
                if line == &end_anchor {
                    if end_idx.is_some() {
                        return Err(format!(
                            "❌ end_anchor matches multiple lines in {}.\n\
                             💡 Add more text to make the anchor unique.",
                            display_path
                        ));
                    }
                    end_idx = Some(i);
                }
            }
            let end_idx = match end_idx {
                Some(i) => i,
                None => {
                    // Show lines near and after start_anchor
                    let hint_start = start_idx.saturating_sub(2);
                    let hint_end = (start_idx + 15).min(n);
                    let mut hint = String::new();
                    for i in hint_start..hint_end {
                        let marker = if i == start_idx { "→ " } else { "  " };
                        hint.push_str(&format!("{}Line {}: {}\n", marker, i + 1, lines[i]));
                    }
                    return Err(format!(
                        "❌ end_anchor not found in {}.\n\n\
                         Lines near start_anchor:\n{}\n\
                         💡 Fix: use file_read to find the EXACT end line, then retry.",
                        display_path, hint
                    ));
                }
            };

            if end_idx < start_idx {
                return Err(format!(
                    "❌ end_anchor (line {}) is before start_anchor (line {}) in {}.\n\
                     💡 Swap start_anchor and end_anchor, or check the anchors.",
                    end_idx + 1, start_idx + 1, display_path
                ));
            }

            // Calculate the actual range to delete
            let (del_start, del_end) = if inclusive {
                (start_idx, end_idx + 1) // end is exclusive for slice
            } else {
                if end_idx <= start_idx + 1 {
                    return Err(format!(
                        "❌ Nothing to delete: start_anchor (line {}) and end_anchor (line {}) \
                         are adjacent with inclusive=false.\n\
                         💡 Set inclusive: true or use wider anchors.",
                        start_idx + 1, end_idx + 1
                    ));
                }
                (start_idx + 1, end_idx)
            };

            let deleted_lines = del_end - del_start;
            let mut out: Vec<String> = Vec::new();
            for i in 0..del_start {
                out.push(lines[i].to_string());
            }
            for i in del_end..n {
                out.push(lines[i].to_string());
            }
            let new_content = out.join("\n");

            match std::fs::write(&path, &new_content) {
                Ok(()) => {
                    let range_desc = if inclusive {
                        format!("lines {}-{}", start_idx + 1, end_idx + 1)
                    } else {
                        format!("lines {}-{} (exclusive anchors)", start_idx + 2, end_idx)
                    };
                    Ok(format!(
                        "✅ Deleted {} ({deleted_lines} lines) from {}",
                        range_desc, display_path
                    ))
                }
                Err(e) => Err(format!(
                    "❌ Failed to write {}: {e}",
                    display_path
                )),
            }
        }).await;

        match result {
            Ok(Ok(msg)) => ToolOutput::success(msg),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Delete task panicked: {e}")),
        }
    }
}
