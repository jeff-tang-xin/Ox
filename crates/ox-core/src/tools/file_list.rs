use serde_json::{json, Value};
use std::fs;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileListTool;

#[async_trait::async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List files and directories at a path. Supports glob patterns."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path or glob pattern (e.g. 'src/**/*.rs'). Optional - if not provided, lists all indexed files."
                }
            }
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // If no path provided, list all files from index
        let path_str = args.get("path").and_then(|p| p.as_str());
        
        match path_str {
            None => {
                // List all files from database index
                match ctx.file_index.list_all_files() {
                    Ok(entries) => {
                        if entries.is_empty() {
                            return ToolOutput::success("No files found in index.");
                        }
                        
                        // Format: [ID] path (file_type)
                        let lines: Vec<String> = entries
                            .iter()
                            .map(|e| {
                                let type_info = e.file_type
                                    .as_ref()
                                    .map(|t| format!(" (.{})", t))
                                    .unwrap_or_default();
                                format!("[{}] {}{}", e.id, e.full_path, type_info)
                            })
                            .collect();
                        
                        ToolOutput::success(format!(
                            "Found {} files:\n{}",
                            entries.len(),
                            lines.join("\n")
                        ))
                    }
                    Err(e) => ToolOutput::error(format!("Failed to query file index: {}", e)),
                }
            }
            Some(path) => {
                // Use traditional filesystem listing with path parameter
                self.list_from_filesystem(path, ctx)
            }
        }
    }
}

impl FileListTool {
    /// Traditional filesystem listing (when path is provided)
    fn list_from_filesystem(&self, path_str: &str, ctx: &ToolContext) -> ToolOutput {
        use std::fs;
        
        // Normalize path: trim whitespace and standardize separators
        let normalized_path = path_str.trim().replace('\\', "/");
        
        // Path traversal protection.
        let resolved_path = ctx.working_dir.join(&normalized_path);
        
        // Keep user-friendly path for error messages
        let display_path = resolved_path.clone();
        
        let validated_path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };

        // Check if it's a glob pattern.
        if path_str.contains('*') || path_str.contains('?') {
            let full_pattern = validated_path;
            match glob::glob(&full_pattern.to_string_lossy()) {
                Ok(entries) => {
                    let mut results = Vec::new();
                    for entry in entries {
                        match entry {
                            Ok(path) => {
                                let relative = path
                                    .strip_prefix(&ctx.working_dir)
                                    .unwrap_or(&path);
                                results.push(relative.display().to_string());
                            }
                            Err(e) => {
                                results.push(format!("(error: {e})"));
                            }
                        }
                    }
                    if results.is_empty() {
                        ToolOutput::success("No matching files found.")
                    } else {
                        ToolOutput::success(results.join("\n"))
                    }
                }
                Err(e) => ToolOutput::error(format!("Invalid glob pattern: {e}")),
            }
        } else {
            let full_path = validated_path;
            match fs::read_dir(&full_path) {
                Ok(entries) => {
                    let mut items: Vec<String> = Vec::new();
                    for entry in entries {
                        let entry = match entry {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        if is_dir {
                            items.push(format!("{name}/"));
                        } else {
                            items.push(name);
                        }
                    }
                    items.sort();
                    ToolOutput::success(items.join("\n"))
                }
                Err(e) => ToolOutput::error(format!(
                    "Failed to list {}: {e}",
                    display_path.display()
                )),
            }
        }
    }
}
