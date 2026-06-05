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
        "Apply small edits to existing files via search/replace. For simple exact-string replacement, prefer edit_file.\n\n\
         Two modes:\n\
         1. Text-based: Provide 'search' + 'replace' for fuzzy search/replace (handles whitespace differences)\n\
         2. Line-based: Provide 'start_line' + 'end_line' + 'new_content' to replace by line numbers\n\n\
         After using file_read to inspect line numbers, line-based mode is the most reliable."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace)."
                },
                "search": {
                    "type": "string",
                    "description": "【Text-based mode】Code to find. Handles whitespace/indent differences."
                },
                "replace": {
                    "type": "string",
                    "description": "【Text-based mode】Replacement text."
                },
                "start_line": {
                    "type": "integer",
                    "description": "【Line-based mode】Start line number (1-based). Get this from file_read output."
                },
                "end_line": {
                    "type": "integer",
                    "description": "【Line-based mode】End line number (inclusive, 1-based)."
                },
                "new_content": {
                    "type": "string",
                    "description": "【Line-based mode】New content to replace lines start_line..end_line."
                }
            },
            "required": ["path"],
            "oneOf": [
                { "required": ["search", "replace"], "description": "Text-based: find and replace by content" },
                { "required": ["start_line", "end_line", "new_content"], "description": "Line-based: replace by line numbers (most reliable, use after file_read)" }
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // ── Resolve path (path-only, no file_id/filename) ──
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p.trim().replace('\\', "/"),
            None => return ToolOutput::error(
                "❌ Missing required parameter: 'path'. Usage: {\"path\": \"<file>\", ...}",
            ),
        };
        let resolved_path = if std::path::Path::new(&path_str).is_absolute() {
            std::path::PathBuf::from(&path_str)
        } else {
            ctx.working_dir.join(&path_str)
        };
        let display_path = resolved_path.clone();
        let path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            };

        // Determine edit mode: line-based or text-based
        let edit_mode = if args.get("start_line").is_some() {
            "line_based"
        } else {
            "text_based"
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
        let edit_mode_clone = edit_mode;
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
                // ── Text-based mode: search and replace ──
                //
                // Strategy cascade (inspired by Aider research):
                // 1. Exact match (fast path, zero risk)
                // 2. Relative-indent normalized match — handles tab vs spaces, indent level changes
                // 3. Line-smart fuzzy location finding → read exact text → clean replace
                //
                // ✅ Always line-based replacement — no character-level diffing.

                // Step 1: Try exact match first (fast path, zero risk)
                if content.contains(&search_clone) {
                    let count = content.matches(&search_clone).count();
                    if count == 1 {
                        content.replacen(&search_clone, &replace_clone, 1)
                    } else if count > 1 {
                        return Err(format!(
                            "Search string found {count} times in {} (must match exactly once). Provide more context to make it unique.",
                            display_path_clone.display()
                        ));
                    } else {
                        unreachable!() // contains() returned true
                    }
                } else {
                    // ── Preprocess: convert both to relative-indent form ──
                    // This makes indentation differences (tab vs space, 4 vs 2 spaces) irrelevant,
                    // because relative-indent encodes only the CHANGE from each line to the next.
                    // Inspired by aider/ RelativeIndenter.

                    let file_lines: Vec<&str> = content.lines().collect();
                    let search_lines: Vec<&str> = search_clone.lines().collect();

                    if search_lines.is_empty() {
                        return Err("Search string is empty.".to_string());
                    }

                    /// Convert absolute indentation to relative indentation.
                    /// First line keeps its full indent (the base), subsequent lines
                    /// encode only the INDENT CHANGE relative to the previous line.
                    fn to_relative_indent(lines: &[&str]) -> Vec<String> {
                        let mut out = Vec::with_capacity(lines.len());
                        let mut prev_indent: usize = 0;
                        // Choose a marker character not present in the text
                        let marker = '←';
                        for line in lines {
                            let trimmed = line.trim_start();
                            let indent = line.len() - trimmed.len();
                            let indent_str: String;
                            if out.is_empty() {
                                // First line: keep full indent as base
                                indent_str = " ".repeat(indent);
                            } else if indent > prev_indent {
                                // More indented: keep only the NEW whitespace
                                let diff = indent - prev_indent;
                                indent_str = " ".repeat(diff);
                            } else if indent == prev_indent {
                                // Same indent: no whitespace prefix
                                indent_str = String::new();
                            } else {
                                // Less indented: marker for each level of outdent
                                let diff = prev_indent - indent;
                                indent_str = marker.to_string().repeat(diff);
                            }
                            out.push(format!("{}{}", indent_str, trimmed));
                            prev_indent = indent;
                        }
                        out
                    }

                    /// Convert relative-indent lines back to absolute (using file's indent as reference).
                    fn from_relative_indent(rel: &[String], file_lines: &[&str], start: usize) -> Vec<String> {
                        let marker = '←';
                        let mut out = Vec::with_capacity(rel.len());
                        let mut prev_indent: usize = 0;
                        // Use the file's first-line indent as reference
                        if start < file_lines.len() {
                            let first = file_lines[start];
                            prev_indent = first.len() - first.trim_start().len();
                        }
                        for line in rel {
                            let trimmed = line.trim_start();
                            let prefix = &line[..line.len() - trimmed.len()];
                            if prefix.contains(marker) {
                                // Outdent: subtract marker count
                                let outdent = prefix.chars().filter(|&c| c == marker).count();
                                prev_indent = prev_indent.saturating_sub(outdent);
                                out.push(format!("{:indent$}{}", "", trimmed, indent = prev_indent));
                            } else if prefix.is_empty() && trimmed.len() < line.len() {
                                // Same indent as previous
                                out.push(format!("{:indent$}{}", "", trimmed, indent = prev_indent));
                            } else {
                                // More indented (or first line)
                                let add = prefix.len();
                                prev_indent += add;
                                out.push(format!("{:indent$}{}", "", trimmed, indent = prev_indent));
                            }
                        }
                        out
                    }

                    // Step 2: Try relative-indent normalized match
                    let search_rel = to_relative_indent(&search_lines);
                    let file_rel = to_relative_indent(&file_lines);

                    let n_search = search_rel.len();
                    let n_file = file_rel.len();

                    let mut best_start = 0usize;
                    let mut best_end = 0usize;
                    let mut best_score = 0usize;
                    // Track duplicates for disambiguation
                    let mut duplicate_positions: Vec<(usize, usize)> = Vec::new();

                    for start in 0..n_file {
                        let mut matched = 0usize;
                        let mut si = 0usize;
                        let mut fi = start;

                        while si < n_search && fi < n_file {
                            if search_rel[si] == file_rel[fi] {
                                matched += 1;
                                si += 1;
                                fi += 1;
                            } else if search_lines[si].trim().is_empty() {
                                // Allow skipping blank lines in search
                                si += 1;
                            } else if file_lines[fi].trim().is_empty() {
                                // Allow skipping blank lines in file
                                fi += 1;
                            } else {
                                break;
                            }
                        }

                        if si == n_search {
                            if matched > best_score {
                                best_score = matched;
                                best_start = start;
                                best_end = fi;
                                duplicate_positions.clear();
                                duplicate_positions.push((start, fi));
                            } else if matched == best_score {
                                duplicate_positions.push((start, fi));
                            }
                        }
                    }

                    let threshold = if n_search >= 3 { n_search - 1 } else { n_search };

                    if best_score >= threshold {
                        // Check for duplicates
                        if duplicate_positions.len() > 1 {
                            let locations: Vec<String> = duplicate_positions.iter()
                                .enumerate()
                                .map(|(idx, (s, _))| {
                                    let preview = file_lines[*s.min(&(file_lines.len() - 1))]
                                        .trim()
                                        .chars()
                                        .take(80)
                                        .collect::<String>();
                                    format!("  {}. Line {}: {:.80}", idx + 1, s + 1, preview)
                                })
                                .collect();
                            return Err(format!(
                                "❌ Search matched {n} locations in {f}\n\n\
                                 Options:\n{locs}\n\n\
                                 💡 Fix: Add more unique context (like surrounding method name or comments) \
                                 to disambiguate. Or use file_write for a full rewrite.",
                                n = duplicate_positions.len(),
                                f = display_path_clone.display(),
                                locs = locations.join("\n"),
                            ));
                        }

                        // Found it! Apply clean line-based replacement.
                        let replace_text = replace_clone;
                        tracing::info!(
                            "[FILE_PATCH] Relative-indent match at lines {}-{} (score: {}/{}), applying clean replacement",
                            best_start + 1, best_end, best_score, n_search
                        );

                        // Convert the LLM's replacement text to relative-indent too,
                        // then back to absolute using the FILE's indent context.
                        // This ensures the replacement uses the file's actual indentation,
                        // not whatever indent style the LLM used.
                        let replace_lines: Vec<&str> = replace_text.lines().collect();
                        if replace_lines.is_empty() {
                            // Deleting lines
                            let mut result: Vec<String> = Vec::new();
                            for i in 0..best_start {
                                result.push(file_lines[i].to_string());
                            }
                            for i in best_end..n_file {
                                result.push(file_lines[i].to_string());
                            }
                            result.join("\n")
                        } else {
                            let replace_rel = to_relative_indent(&replace_lines);
                            let replace_abs = from_relative_indent(&replace_rel, &file_lines, best_start);

                            let mut result: Vec<String> = Vec::new();
                            for i in 0..best_start {
                                result.push(file_lines[i].to_string());
                            }
                            result.extend(replace_abs);
                            for i in best_end..n_file {
                                result.push(file_lines[i].to_string());
                            }
                            result.join("\n")
                        }
                    } else {
                        // ── All strategies failed — provide diagnostic ──
                        let search_first = search_lines[0].trim();
                        let mut similar = Vec::new();
                        for (i, line) in file_lines.iter().enumerate() {
                            if line.trim().contains(search_first) || search_first.contains(line.trim()) {
                                similar.push((i + 1, line.trim()));
                                if similar.len() >= 5 { break; }
                            }
                        }
                        if similar.is_empty() {
                            return Err(format!(
                                "❌ Search content not found in {} ({} lines)\n\
                                 🔍 No similar content found.\n\
                                 💡 Fix: Use file_read to verify the file content",
                                display_path_clone.display(),
                                n_file,
                            ));
                        }
                        let sim: Vec<String> = similar.iter()
                            .map(|(n, t)| format!("  Line {}: {}", n, t))
                            .collect();
                        return Err(format!(
                            "❌ Search content not found in {} (matched only {}/{} lines)\n\
                             🔍 Closest:\n{}\n\n\
                             💡 Fix:\n\
                             - Use file_read to get EXACT content\n\
                             - Include 2-3 unique lines in search\n\
                             - Or use file_write for full rewrite",
                            display_path_clone.display(),
                            best_score, n_search,
                            sim.join("\n"),
                        ));
                    }
                }
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
