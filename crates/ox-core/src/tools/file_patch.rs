use serde_json::{json, Value};
use std::fs;

use super::{content_validation, SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FilePatchTool;

#[async_trait::async_trait]
impl Tool for FilePatchTool {
    fn name(&self) -> &str {
        "file_patch"
    }

    fn description(&self) -> &str {
        "Apply a targeted edit to an existing file by replacing specific text. \
         Use this for small changes (<50% of file). The search string must match EXACTLY ONCE in the file. \
         For creating new files or rewriting entire files, use file_write instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file (relative to working directory). Optional if using file_id or filename."
                },
                "filename": {
                    "type": "string",
                    "description": "Filename to search for in index. Must be unique."
                },
                "file_id": {
                    "type": "integer",
                    "description": "File ID from index for precise matching."
                },
                "search": {
                    "type": "string",
                    "description": "The EXACT text to find and replace. Must match exactly once in the file. Include enough context to be unique."
                },
                "replace": {
                    "type": "string",
                    "description": "The replacement text. Use \\n for newlines. Escape special characters properly in JSON."
                }
            },
            "required": ["search", "replace"],
            "examples": [
                {
                    "file_id": 123,
                    "search": "fn old_function() {\n    println!(\"old\");\n}",
                    "replace": "fn new_function() {\n    println!(\"new\");\n}"
                }
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // Determine file path from parameters (priority: file_id > filename > path)
        let resolved_path = if let Some(file_id) = args.get("file_id").and_then(|id| id.as_i64()) {
            // Method 1: Use file_id for precise matching
            match ctx.file_index.find_by_id(file_id) {
                Ok(Some(entry)) => ctx.working_dir.join(&entry.full_path),
                Ok(None) => return ToolOutput::error(
                    format!("❌ File Not Found: No file with ID {}\n\n\
                             💡 How to fix:\n\
                             • Use file_list tool to see available files and their IDs\n\
                             • Or use 'filename' or 'path' parameter instead", file_id)
                ),
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(filename) = args.get("filename").and_then(|f| f.as_str()) {
            // Method 2: Use filename (may have multiple matches)
            match ctx.file_index.find_by_filename(filename) {
                Ok(matches) if matches.len() == 1 => {
                    ctx.working_dir.join(&matches[0].full_path)
                }
                Ok(matches) if matches.len() > 1 => {
                    // Multiple matches - return options for LLM to choose
                    let options: Vec<String> = matches
                        .iter()
                        .map(|e| format!("  [ID: {}] {}", e.id, e.full_path))
                        .collect();
                    
                    return ToolOutput::error(
                        format!("❌ Multiple Files Matched '{}':\n{}\n\n\
                                 💡 How to fix:\n\
                                 • Retry with 'file_id' parameter for precise matching\n\
                                 • Example: {{\"file_id\": {}}}", 
                                filename,
                                options.join("\n"),
                                matches[0].id)
                    );
                }
                Ok(_) => {
                    return ToolOutput::error(
                        format!("❌ File Not Found: '{}' not in index\n\n\
                                 💡 How to fix:\n\
                                 • Check filename spelling\n\
                                 • Use file_list tool to see all indexed files\n\
                                 • Or use 'path' parameter with full relative path", filename)
                    );
                }
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(path_str) = args.get("path").and_then(|p| p.as_str()) {
            // Method 3: Traditional path-based approach (backward compatible)
            // Normalize path: trim whitespace and standardize separators
            let normalized_path = path_str.trim().replace('\\', "/");
            
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
                 {\"file_id\": 123, \"search\": \"...\", \"replace\": \"...\"} - Patch by ID\n\
                 {\"filename\": \"main.rs\", \"search\": \"...\", \"replace\": \"...\"} - Patch by filename\n\
                 {\"path\": \"src/main.rs\", \"search\": \"...\", \"replace\": \"...\"} - Patch by path"
            );
        };
        
        // Keep the user-friendly path for error messages
        let display_path = resolved_path.clone();
        
        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };
        let search = match args.get("search").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => return ToolOutput::error("Missing required parameter: search. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}"),
        };
        let replace = match args.get("replace").and_then(|r| r.as_str()) {
            Some(r) => r,
            None => return ToolOutput::error("Missing required parameter: replace. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}"),
        };

        // Validate replacement content using shared validation logic
        if let Err(e) = content_validation::validate_content(replace) {
            return ToolOutput::error(e);
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to read {}: {e}", path.display())),
        };

        let count = content.matches(search).count();
        match count {
            0 => ToolOutput::error(format!(
                "Search string not found in {}",
                display_path.display()
            )),
            1 => {
                let new_content = content.replacen(search, replace, 1);
                match fs::write(&path, &new_content) {
                    Ok(()) => {
                        // Update file index immediately for real-time availability
                        if let Ok(relative_path) = path.strip_prefix(&ctx.working_dir) {
                            let rel_str = relative_path.to_string_lossy();
                            if let Err(e) = ctx.file_index.add_file(&rel_str) {
                                tracing::warn!("Failed to update file index: {}", e);
                            }
                        }
                        
                        ToolOutput::success(format!(
                            "✅ Successfully patched {} (replaced 1 occurrence)\n\
                             💡 Tip: Use 'file_read' to verify the changes",
                            display_path.display()
                        ))
                    }
                    Err(e) => {
                        ToolOutput::error(format!("❌ Failed to write {}: {}\n\n\
                                                     💡 The search was found but writing failed.\n\
                                                     🔍 Possible causes:\n\
                                                     • Insufficient permissions\n\
                                                     • Disk is full\n\
                                                     • File is locked by another process",
                                                    display_path.display(), e))
                    }
                }
            }
            n => ToolOutput::error(format!(
                "Search string found {n} times in {} (must match exactly once). Provide more context to make it unique.",
                display_path.display()
            )),
        }
    }
}
