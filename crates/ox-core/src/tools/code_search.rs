use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct CodeSearchTool;

#[async_trait::async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str {
        "code_search"
    }

    fn description(&self) -> &str {
        "Search for a text pattern in file contents. Returns matching lines with file paths and line numbers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Text or regex pattern to search for in file contents"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: working directory)"
                },
                "file_pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. '*.rs', '*.py')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolOutput::error("Missing required parameter: pattern"),
        };
        let base = args
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| ctx.working_dir.join(p))
            .unwrap_or_else(|| ctx.working_dir.to_path_buf());
        let file_pattern = args
            .get("file_pattern")
            .and_then(|p| p.as_str())
            .unwrap_or("*");

        let re = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(_) => {
                // Fall back to literal matching if not valid regex.
                match regex::Regex::new(&regex::escape(pattern)) {
                    Ok(r) => r,
                    Err(e) => return ToolOutput::error(format!("Invalid pattern: {e}")),
                }
            }
        };

        let glob_pattern = base.join("**").join(file_pattern);
        let entries = match glob::glob(&glob_pattern.to_string_lossy()) {
            Ok(e) => e,
            Err(e) => return ToolOutput::error(format!("Invalid file pattern: {e}")),
        };

        let mut results = Vec::new();
        let mut files_searched = 0u32;

        for entry in entries {
            let path = match entry {
                Ok(p) if p.is_file() => p,
                _ => continue,
            };

            // Skip binary files and very large files.
            if is_binary_path(&path) {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            files_searched += 1;

            for (line_num, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    let relative = path.strip_prefix(&ctx.working_dir).unwrap_or(&path);
                    results.push(format!(
                        "{}:{}: {}",
                        relative.display(),
                        line_num + 1,
                        line.trim()
                    ));

                    if results.len() >= 100 {
                        results.push(format!(
                            "... (truncated at 100 matches, searched {files_searched} files)"
                        ));
                        return ToolOutput::success(results.join("\n"));
                    }
                }
            }
        }

        if results.is_empty() {
            ToolOutput::success(format!(
                "No matches found for '{pattern}' (searched {files_searched} files)"
            ))
        } else {
            ToolOutput::success(results.join("\n"))
        }
    }
}

fn is_binary_path(path: &Path) -> bool {
    let binary_exts = [
        "exe", "dll", "so", "dylib", "bin", "obj", "o", "a", "lib", "png", "jpg", "jpeg", "gif",
        "bmp", "ico", "svg", "pdf", "zip", "tar", "gz", "7z", "rar", "wasm", "ttf", "otf",
        "woff", "woff2", "mp3", "mp4", "avi", "mov", "pdb", "lock",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| binary_exts.contains(&ext.to_lowercase().as_str()))
}
