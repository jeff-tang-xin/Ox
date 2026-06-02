use serde_json::{Value, json};
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


