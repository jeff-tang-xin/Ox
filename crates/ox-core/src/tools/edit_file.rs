/// edit_file — precise string replacement in existing files.
///
/// Primary tool for editing files. Only three parameters: path + old_string + new_string.
/// The old_string must match exactly (including whitespace). If it doesn't match once,
/// the tool fails with actionable diagnostics — force the LLM to file_read first.

use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct EditFileTool;

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file with another. old_string must occur exactly once; \
         add surrounding context to disambiguate. Use for targeted edits instead of rewriting the whole file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace root)."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace — must match exactly once in the file. \
                                   Include 2-5 lines of surrounding context for uniqueness."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text. May be empty to delete."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // ── Resolve path ──
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p.trim().replace('\\', "/"),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'path'.\n\
                 Usage: {\"path\": \"<relative-path>\", \"old_string\": \"<exact text>\", \"new_string\": \"<replacement>\"}",
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

        // ── Extract required parameters ──
        let old_string = match args.get("old_string").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'old_string'. Must be the EXACT text to find in the file.",
            ),
        };

        let new_string = match args.get("new_string").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'new_string'. Use empty string \"\" to delete.",
            ),
        };

        // Validate new content
        if let Err(e) = content_validation::validate_content(&new_string) {
            return ToolOutput::error(e);
        }

        // ── Read file ──
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!(
                "❌ Cannot read {}: {e}\n💡 Check the path with file_list or file_read.",
                path.display()
            )),
        };

        // ── Match: exact match only ──
        let count = content.matches(&old_string).count();
        if count == 0 {
            // Show a snippet of the file around where we searched
            let search_line = old_string.lines().next().unwrap_or(&old_string);
            let mut hint = String::new();
            for (i, line) in content.lines().enumerate() {
                if line.contains(search_line.trim()) {
                    hint.push_str(&format!("  Line {}: {}\n", i + 1, line));
                    if hint.lines().count() >= 5 { break; }
                }
            }
            return ToolOutput::error(format!(
                "❌ old_string not found in {}.\n\n\
                 🔍 Searched for {} bytes; file is {} lines / {} bytes.\n\
                 {}\
                 💡 Fix: use file_read to get the EXACT current content, then retry.",
                path.display(),
                old_string.len(),
                content.lines().count(),
                content.len(),
                if hint.is_empty() {
                    "No similar lines found — the content may have changed.\n".to_string()
                } else {
                    format!("🔍 Lines containing similar text:\n{hint}\n")
                },
            ));
        }
        if count > 1 {
            // Show each occurrence with line numbers
            let mut locations = Vec::new();
            let mut pos = 0usize;
            for _ in 0..count {
                if let Some(found) = content[pos..].find(&old_string) {
                    let abs = pos + found;
                    let line_num = content[..abs].lines().count() + 1; // 1-based
                    let preview: String = content.lines().nth(line_num - 1).unwrap_or("").chars().take(80).collect();
                    locations.push((line_num, preview));
                    pos = abs + 1;
                }
            }
            let loc_str: Vec<String> = locations.iter()
                .map(|(n, p)| format!("  Line {}: …{}…", n, p))
                .collect();
            return ToolOutput::error(format!(
                "❌ old_string matched {} times in {}.\n\n\
                 {}\n\n\
                 💡 Fix: add more surrounding context to old_string so it matches exactly once.",
                count,
                path.display(),
                loc_str.join("\n"),
            ));
        }

        // ── Apply replacement ──
        let new_content = content.replacen(&old_string, &new_string, 1);
        match std::fs::write(&path, &new_content) {
            Ok(()) => {
                // Update file index
                if let Ok(relative_path) = path.strip_prefix(&ctx.working_dir) {
                    let rel_str = relative_path.to_string_lossy();
                    if let Err(e) = ctx.file_index.add_file(&rel_str) {
                        tracing::warn!("Failed to update file index after edit_file: {e}");
                    }
                }
                let old_lines = old_string.lines().count();
                let new_lines = new_string.lines().count();
                ToolOutput::success(format!(
                    "✅ Patched {} ({} → {} lines)",
                    path.display(),
                    old_lines,
                    new_lines,
                ))
            }
            Err(e) => ToolOutput::error(format!(
                "❌ Failed to write {}: {e}",
                path.display()
            )),
        }
    }
}
