use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileReadTool;

#[async_trait::async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read file contents. Use for viewing code, configs, or docs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative). Required unless using file_id or filename."
                },
                "filename": {
                    "type": "string",
                    "description": "Filename to search in index. Errors if multiple matches."
                },
                "file_id": {
                    "type": "integer",
                    "description": "File ID from index (most reliable). Use file_list to get IDs."
                },
                "offset": {
                    "type": "integer",
                    "description": "Start line (1-based). Default: 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to read. Default: unlimited. Recommended: 100-500 for large files."
                }
            },
            "examples": [
                {"path": "src/main.rs"},
                {"path": "config.toml", "limit": 100},
                {"file_id": 123}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // Determine file path from parameters (priority: file_id > filename > path)
        let resolved_path = if let Some(file_id) = args.get("file_id").and_then(|id| id.as_i64()) {
            // Method 1: Use file_id for precise matching
            match ctx.file_index.find_by_id(file_id) {
                Ok(Some(entry)) => ctx.working_dir.join(&entry.full_path),
                Ok(None) => {
                    return ToolOutput::error(format!(
                        "❌ File Not Found: No file with ID {}\n\n\
                             💡 How to fix:\n\
                             • Use file_list tool to see available files and their IDs\n\
                             • Or use 'filename' or 'path' parameter instead",
                        file_id
                    ));
                }
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(filename) = args.get("filename").and_then(|f| f.as_str()) {
            // Method 2: Use filename (may have multiple matches)
            match ctx.file_index.find_by_filename(filename) {
                Ok(matches) if matches.len() == 1 => ctx.working_dir.join(&matches[0].full_path),
                Ok(matches) if matches.len() > 1 => {
                    // Multiple matches - return options for LLM to choose
                    let options: Vec<String> = matches
                        .iter()
                        .map(|e| format!("  [ID: {}] {}", e.id, e.full_path))
                        .collect();

                    return ToolOutput::error(format!(
                        "❌ Multiple Files Matched '{}':\n{}\n\n\
                                 💡 How to fix:\n\
                                 • Retry with 'file_id' parameter for precise matching\n\
                                 • Example: {{\"file_id\": {}}}",
                        filename,
                        options.join("\n"),
                        matches[0].id
                    ));
                }
                Ok(_) => {
                    return ToolOutput::error(format!(
                        "❌ File Not Found: '{}' not in index\n\n\
                                 💡 How to fix:\n\
                                 • Check filename spelling\n\
                                 • Use file_list tool to see all indexed files\n\
                                 • Or use 'path' parameter with full relative path",
                        filename
                    ));
                }
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(raw_path) = args.get("path").and_then(|p| p.as_str()) {
            // Method 3: Traditional path-based approach (backward compatible)
            if raw_path.is_empty() {
                return ToolOutput::error(
                    "❌ Parameter Error: 'path' cannot be empty\n\n\
                     💡 Example usage:\n\
                     {\"path\": \"src/main.rs\", \"limit\": 100}\n\n\
                     Please provide a valid file path.",
                );
            }

            // Normalize path: trim whitespace and standardize separators
            let normalized_path = raw_path.trim().replace('\\', "/");

            // Handle absolute vs relative paths
            if std::path::Path::new(&normalized_path).is_absolute() {
                std::path::PathBuf::from(&normalized_path)
            } else {
                ctx.working_dir.join(&normalized_path)
            }
        } else {
            return ToolOutput::error(
                "❌ Missing Required Parameter\n\n\
                 💡 How to fix - provide ONE of:\n\
                 • 'file_id': Precise file ID from index (recommended)\n\
                 • 'filename': Filename to search (must be unique)\n\
                 • 'path': Full relative path (traditional method)\n\n\
                 📝 Examples:\n\
                 {\"file_id\": 123} - Read by ID\n\
                 {\"filename\": \"main.rs\"} - Read by filename\n\
                 {\"path\": \"src/main.rs\"} - Read by path",
            );
        };

        // Keep user-friendly path for error messages
        let display_path = resolved_path.clone();

        // Path traversal protection.
        let path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            };

        let offset = args
            .get("offset")
            .and_then(|o| o.as_u64())
            .map(|o| o.saturating_sub(1) as usize); // 1-based to 0-based
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize);

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.unwrap_or(0).min(lines.len());
                let end = limit
                    .map(|l| (start + l).min(lines.len()))
                    .unwrap_or(lines.len());

                let selected: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4}\t{line}", start + i + 1))
                    .collect();

                ToolOutput::success(selected.join("\n"))
            }
            Err(e) => ToolOutput::error(format!("Failed to read {}: {e}", display_path.display())),
        }
    }
}
