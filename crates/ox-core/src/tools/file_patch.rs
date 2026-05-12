use serde_json::{Value, json};
use std::fs;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct FilePatchTool;

#[async_trait::async_trait]
impl Tool for FilePatchTool {
    fn name(&self) -> &str {
        "file_patch"
    }

    fn description(&self) -> &str {
        "Apply small edits to existing files (<50% changed). Search text must match exactly once. For new files or large rewrites, use file_write."
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
                    "description": "Filename to search in index. Must be unique."
                },
                "file_id": {
                    "type": "integer",
                    "description": "File ID from index (most reliable)."
                },
                "search": {
                    "type": "string",
                    "description": "✅ REQUIRED: Exact text to find. Must match exactly once. Include enough context for uniqueness."
                },
                "replace": {
                    "type": "string",
                    "description": "✅ REQUIRED: Replacement text. Use \\n for newlines."
                }
            },
            "required": ["search", "replace"],
            "examples": [
                {
                    "path": "src/main.rs",
                    "search": "fn old_func() {\n    println!(\"old\");\n}",
                    "replace": "fn new_func() {\n    println!(\"new\");\n}"
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
                 {\"path\": \"src/main.rs\", \"search\": \"...\", \"replace\": \"...\"} - Patch by path",
            );
        };

        // Keep the user-friendly path for error messages
        let display_path = resolved_path.clone();

        let path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            };
        let search = match args.get("search").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutput::error(
                    "Missing required parameter: search. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}",
                );
            }
        };
        let replace = match args.get("replace").and_then(|r| r.as_str()) {
            Some(r) => r,
            None => {
                return ToolOutput::error(
                    "Missing required parameter: replace. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}",
                );
            }
        };

        // Validate replacement content using shared validation logic
        if let Err(e) = content_validation::validate_content(replace) {
            return ToolOutput::error(e);
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to read {}: {e}", path.display())),
        };

        // Try exact match first
        let count = content.matches(search).count();
        
        if count == 0 {
            // If exact match fails, try fuzzy matching with normalized whitespace
            tracing::warn!(
                "Exact match failed for file {}. Trying fuzzy matching...",
                display_path.display()
            );
            
            // Normalize both strings: collapse multiple whitespace into single space
            let normalize_whitespace = |s: &str| -> String {
                s.split_whitespace().collect::<Vec<&str>>().join(" ")
            };
            
            let normalized_search = normalize_whitespace(search);
            let normalized_content = normalize_whitespace(&content);
            
            // Check if normalized version matches
            if normalized_content.contains(&normalized_search) {
                return ToolOutput::error(format!(
                    "❌ Search string not found (exact match failed)\n\n\
                     🔍 Diagnosis: Your search string has whitespace differences from the file content.\n\n\
                     💡 Solutions:\n\
                     1. Use file_read to get the EXACT content including spaces/newlines\n\
                     2. Copy the exact text from file_read output\n\
                     3. Use fewer lines but more unique context\n\
                     4. Or use file_write to rewrite the entire file\n\n\
                     📝 Example of proper search string:\n\
                     \"search\": \"public void processOrder() {{\\n    Order order = validate(request);\\n}}\""
                ));
            }
            
            // Provide helpful diagnostic information
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len();
            
            // Try to find similar lines (first line of search string)
            let search_first_line = search.lines().next().unwrap_or("").trim();
            let mut similar_lines = Vec::new();
            
            for (i, line) in lines.iter().enumerate() {
                if line.trim().contains(search_first_line) || 
                   search_first_line.contains(line.trim()) && !line.trim().is_empty() {
                    similar_lines.push((i + 1, line.trim()));
                    if similar_lines.len() >= 5 {
                        break;
                    }
                }
            }
            
            let similar_info = if similar_lines.is_empty() {
                "No similar content found.".to_string()
            } else {
                let lines_str: Vec<String> = similar_lines
                    .iter()
                    .map(|(num, text)| format!("  Line {}: {}", num, text))
                    .collect();
                format!("🔍 Similar content found:\n{}\n\n", lines_str.join("\n"))
            };
            
            return ToolOutput::error(format!(
                "❌ Search string not found in {}\n\n\
                 🔍 File has {} lines\n\
                 {}\
                 💡 How to fix:\n\
                 - Use file_read to see the EXACT content first\n\
                 - Copy exact text from file_read output (including spaces/newlines)\n\
                 - Use shorter, more unique search strings (2-3 lines)\n\
                 - Include unique identifiers (method names, variable names)\n\
                 - Or use file_write to rewrite the entire file\n\n\
                 📝 Better approach:\n\
                 1. file_read(path=\"{}\")\n\
                 2. Copy exact code you want to change\n\
                 3. file_patch with that exact text",
                display_path.display(),
                total_lines,
                similar_info,
                display_path.display()
            ));
        }

        match count {
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
                    Err(e) => ToolOutput::error(format!(
                        "❌ Failed to write {}: {}\n\n\
                                                     💡 The search was found but writing failed.\n\
                                                     🔍 Possible causes:\n\
                                                     • Insufficient permissions\n\
                                                     • Disk is full\n\
                                                     • File is locked by another process",
                        display_path.display(),
                        e
                    )),
                }
            }
            n => ToolOutput::error(format!(
                "Search string found {n} times in {} (must match exactly once). Provide more context to make it unique.",
                display_path.display()
            )),
        }
    }
}
