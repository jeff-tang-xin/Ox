use serde_json::{Value, json};
use std::sync::Arc;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct FilePatchTool;

#[async_trait::async_trait]
impl Tool for FilePatchTool {
    fn name(&self) -> &str {
        "file_patch"
    }

    fn description(&self) -> &str {
        "Apply small edits to existing files (<50% changed). For new files or large rewrites, use file_write.\n\n\
         💡 Two modes:\n\
         1. Text-based: Provide 'search' + 'replace' for search/replace\n\
         2. Line-based: Provide 'start_line' + 'end_line' + 'new_content' to replace specific lines\n\n\
         ⚠️ For text-based mode: search must match exactly once. Use file_read first to get exact content."
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
                    "description": "Text to find (for text-based mode). Keep it SHORT - 2-3 unique lines max. Use distinctive identifiers."
                },
                "replace": {
                    "type": "string",
                    "description": "Replacement text (for text-based mode). Use \\n for newlines."
                },
                "start_line": {
                    "type": "integer",
                    "description": "Start line number (1-based, for line-based mode). Get this from file_read output."
                },
                "end_line": {
                    "type": "integer",
                    "description": "End line number (inclusive, 1-based, for line-based mode)."
                },
                "new_content": {
                    "type": "string",
                    "description": "New content to replace lines start_line..end_line (for line-based mode)."
                },
                "edits": {
                    "type": "array",
                    "description": "Multiple edits in one call. Each edit: {\"old\": \"...\", \"new\": \"...\"} OR {\"start_line\": N, \"end_line\": M, \"new_content\": \"...\"}"
                }
            },
            "examples": [
                {
                    "path": "src/main.rs",
                    "search": "fn calculate() {\n    let result = a + b;",
                    "replace": "fn calculate() {\n    let result = a * b;"
                },
                {
                    "path": "src/main.rs",
                    "start_line": 42,
                    "end_line": 44,
                    "new_content": "fn calculate() {\n    let result = a * b;\n}"
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

        // Determine edit mode: line-based OR text-based OR multiple edits
        let edit_mode = if args.get("edits").is_some() {
            "multiple".to_string()
        } else if args.get("start_line").is_some() {
            "line_based".to_string()
        } else {
            "text_based".to_string()
        };

        // Extract parameters based on mode
        let search = if edit_mode == "text_based" {
            match args.get("search").and_then(|s| s.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    return ToolOutput::error(
                        "Missing required parameter: search (for text-based mode). Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}",
                    );
                }
            }
        } else {
            String::new() // Not used in line-based mode
        };

        let replace = if edit_mode == "text_based" {
            match args.get("replace").and_then(|r| r.as_str()) {
                Some(r) => r.to_string(),
                None => {
                    return ToolOutput::error(
                        "Missing required parameter: replace (for text-based mode). Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}",
                    );
                }
            }
        } else {
            String::new() // Not used in line-based mode
        };

        // Validate replacement content based on mode
        if edit_mode == "text_based" {
            if let Err(e) = content_validation::validate_content(&replace) {
                return ToolOutput::error(e);
            }
        } else if edit_mode == "line_based" {
            if let Some(new_content) = args.get("new_content").and_then(|c| c.as_str()) {
                if let Err(e) = content_validation::validate_content(new_content) {
                    return ToolOutput::error(e);
                }
            }
        }

        // Report progress before blocking I/O
        tracing::info!("[FILE_PATCH] Starting patch operation for: {:?}", display_path);
        ctx.report_progress("Reading file...".to_string(), Some(10));
        
        // Run blocking file I/O on a dedicated thread to avoid blocking the Tokio runtime.
        let path_clone = path.clone();
        let display_path_clone = display_path.clone();
        let search_clone = search.to_string();
        let replace_clone = replace.to_string();
        let working_dir = ctx.working_dir.clone();
        let file_index = Arc::clone(&ctx.file_index);
        let edit_mode_clone = edit_mode.clone();
        let start_line = args.get("start_line").and_then(|v| v.as_i64());
        let end_line = args.get("end_line").and_then(|v| v.as_i64());
        let new_content = args.get("new_content").and_then(|v| v.as_str()).map(|s| s.to_string());
        
        tracing::info!("[FILE_PATCH] Spawning blocking task for: {:?}", display_path_clone);
        let result = tokio::task::spawn_blocking(move || {
            tracing::info!("[FILE_PATCH] Blocking task started, reading file: {:?}", path_clone);
            
            // Phase 1: Read file
            let content = match std::fs::read_to_string(&path_clone) {
                Ok(c) => {
                    tracing::info!("[FILE_PATCH] File read successfully, size: {} bytes", c.len());
                    c
                }
                Err(e) => {
                    tracing::error!("[FILE_PATCH] Failed to read file: {:?}, error: {}", path_clone, e);
                    return Err(format!("Failed to read {}: {e}", path_clone.display()));
                }
            };

            // Phase 2: Apply patch based on mode
            let new_file_content = if edit_mode_clone == "line_based" {
                // Line-based mode: replace specific lines by number
                let start = match start_line {
                    Some(s) if s >= 1 => s as usize,
                    _ => return Err("Missing or invalid 'start_line' parameter (must be >= 1)".to_string()),
                };
                let end = match end_line {
                    Some(e) if e >= start as i64 => e as usize,
                    _ => return Err("Missing or invalid 'end_line' parameter (must be >= start_line)".to_string()),
                };
                let replacement = match new_content {
                    Some(c) => c,
                    None => return Err("Missing 'new_content' parameter for line-based mode".to_string()),
                };

                let lines: Vec<&str> = content.lines().collect();
                let total_lines = lines.len();

                if start > total_lines {
                    return Err(format!(
                        "❌ start_line {} exceeds file length ({})",
                        start, total_lines
                    ));
                }
                if end > total_lines {
                    return Err(format!(
                        "❌ end_line {} exceeds file length ({})",
                        end, total_lines
                    ));
                }

                // Build new content: lines before + replacement + lines after
                let mut result_lines: Vec<String> = Vec::new();
                
                // Lines before the range
                for i in 0..(start - 1) {
                    result_lines.push(lines[i].to_string());
                }
                
                // Replacement content (split by \n)
                for line in replacement.lines() {
                    result_lines.push(line.to_string());
                }
                
                // Lines after the range
                for i in end..total_lines {
                    result_lines.push(lines[i].to_string());
                }

                result_lines.join("\n")
            } else {
                // Text-based mode: search and replace
                let count = content.matches(&search_clone).count();
                
                if count == 0 {
                    // If exact match fails, try fuzzy matching with normalized whitespace
                    tracing::warn!(
                        "Exact match failed for file {}. Trying fuzzy matching...",
                        display_path_clone.display()
                    );
                    
                    // Normalize both strings: collapse multiple whitespace into single space
                    let normalize_whitespace = |s: &str| -> String {
                        s.split_whitespace().collect::<Vec<&str>>().join(" ")
                    };
                    
                    let normalized_search = normalize_whitespace(&search_clone);
                    let normalized_content = normalize_whitespace(&content);
                    
                    // Check if normalized version matches
                    if normalized_content.contains(&normalized_search) {
                        return Err(format!(
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
                    let search_first_line = search_clone.lines().next().unwrap_or("").trim();
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
                    
                    return Err(format!(
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
                        display_path_clone.display(),
                        total_lines,
                        similar_info,
                        display_path_clone.display()
                    ));
                }

                // Apply text-based replacement
                if count != 1 {
                    return Err(format!(
                        "Search string found {count} times in {} (must match exactly once). Provide more context to make it unique.",
                        display_path_clone.display()
                    ));
                }

                content.replacen(&search_clone, &replace_clone, 1)
            };

            // Phase 3: Write file
            tracing::info!("[FILE_PATCH] Writing file: {:?}, size: {} bytes", path_clone, new_file_content.len());
            match std::fs::write(&path_clone, &new_file_content) {
                Ok(()) => {
                    tracing::info!("[FILE_PATCH] File written successfully: {:?}", path_clone);
                    // Update file index immediately for real-time availability
                    if let Ok(relative_path) = path_clone.strip_prefix(&working_dir) {
                        let rel_str = relative_path.to_string_lossy();
                        if let Err(e) = file_index.add_file(&rel_str) {
                            tracing::warn!("Failed to update file index: {}", e);
                        }
                    }

                    let success_msg = if edit_mode_clone == "line_based" {
                        format!(
                            "✅ Successfully patched {} (replaced lines {}-{})\n\
                             💡 Tip: Use 'file_read' to verify the changes",
                            display_path_clone.display(),
                            start_line.unwrap_or(0),
                            end_line.unwrap_or(0)
                        )
                    } else {
                        format!(
                            "✅ Successfully patched {} (replaced 1 occurrence)\n\
                             💡 Tip: Use 'file_read' to verify the changes",
                            display_path_clone.display()
                        )
                    };

                    Ok(success_msg)
                }
                Err(e) => Err(format!(
                    "❌ Failed to write {}: {}\n\n\
                                                     💡 The patch was prepared but writing failed.\n\
                                                      Possible causes:\n\
                                                     • Insufficient permissions\n\
                                                     • Disk is full\n\
                                                     • File is locked by another process",
                    display_path_clone.display(),
                    e
                )),
            }
        }).await;
        
        // Handle spawn_blocking result
        match result {
            Ok(Ok(success_msg)) => {
                ctx.report_progress("Patch applied successfully!".to_string(), Some(100));
                ToolOutput::success(success_msg)
            }
            Ok(Err(error_msg)) => ToolOutput::error(error_msg),
            Err(e) => ToolOutput::error(format!("Patch task failed: {e}")),
        }
    }
}
