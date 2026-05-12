use serde_json::{Value, json};
use std::fs;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileListTool;

#[async_trait::async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List files and directories. Supports glob patterns. Use to explore project structure."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path or glob pattern (e.g., 'src/**/*.rs'). Default: list all indexed files."
                }
            },
            "examples": [
                {},
                {"path": "src/"},
                {"path": "**/*.rs"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // If no path provided, use hierarchical directory listing
        let path_str = args.get("path").and_then(|p| p.as_str());

        match path_str {
            None => {
                // Use hierarchical directory listing (root level)
                match ctx.file_index.list_directory("", false) {
                    Ok(listing) => {
                        let mut output = format!(
                            "📁 Project Root\nTotal files: {}\n\n",
                            listing.total_file_count
                        );

                        // Show subdirectories
                        if !listing.subdirs.is_empty() {
                            output.push_str("📂 Subdirectories:\n");
                            for dir in &listing.subdirs {
                                output.push_str(&format!(
                                    "  - {}/ ({} files)\n",
                                    dir.name, dir.file_count
                                ));
                            }
                            output.push('\n');
                        }

                        // Show files directly under root
                        if !listing.files.is_empty() {
                            output.push_str("📄 Files:\n");
                            for file in &listing.files {
                                let type_info = file
                                    .file_type
                                    .as_ref()
                                    .map(|t| format!(" (.{})", t))
                                    .unwrap_or_default();
                                output.push_str(&format!(
                                    "  [{}] {}{}\n",
                                    file.id, file.filename, type_info
                                ));
                            }
                        }

                        output.push_str("\n💡 Tip: Use 'path' parameter to explore specific directories (e.g., 'src/', 'docs/')");

                        ToolOutput::success(output)
                    }
                    Err(e) => ToolOutput::error(format!("Failed to list directory: {}", e)),
                }
            }
            Some(path) => {
                // Use hierarchical directory listing for specified path
                match ctx.file_index.list_directory(path, false) {
                    Ok(listing) => {
                        let display_path = if listing.path.is_empty() {
                            "(root)".to_string()
                        } else {
                            listing.path.clone()
                        };

                        let mut output = format!(
                            "📁 Directory: {}\nTotal files: {}\n\n",
                            display_path, listing.total_file_count
                        );

                        // Show subdirectories
                        if !listing.subdirs.is_empty() {
                            output.push_str("📂 Subdirectories:\n");
                            for dir in &listing.subdirs {
                                output.push_str(&format!(
                                    "  - {}/ ({} files)\n",
                                    dir.name, dir.file_count
                                ));
                            }
                            output.push('\n');
                        }

                        // Show files
                        if !listing.files.is_empty() {
                            output.push_str("📄 Files:\n");
                            for file in &listing.files {
                                let type_info = file
                                    .file_type
                                    .as_ref()
                                    .map(|t| format!(" (.{})", t))
                                    .unwrap_or_default();
                                output.push_str(&format!(
                                    "  [{}] {}{}\n",
                                    file.id, file.filename, type_info
                                ));
                            }
                        }

                        if listing.subdirs.is_empty() && listing.files.is_empty() {
                            output.push_str("(empty directory)");
                        } else {
                            output.push_str(&format!(
                                "\n💡 Tip: Use 'path' parameter to explore subdirectories (e.g., '{}/{}/')",
                                if listing.path.is_empty() { "" } else { &listing.path },
                                listing.subdirs.first().map(|d| d.name.as_str()).unwrap_or("")
                            ));
                        }

                        ToolOutput::success(output)
                    }
                    Err(e) => ToolOutput::error(format!("Failed to list directory '{}': {}", path, e)),
                }
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

        let validated_path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
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
                                let relative = path.strip_prefix(&ctx.working_dir).unwrap_or(&path);
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
                Err(e) => {
                    ToolOutput::error(format!("Failed to list {}: {e}", display_path.display()))
                }
            }
        }
    }
}
