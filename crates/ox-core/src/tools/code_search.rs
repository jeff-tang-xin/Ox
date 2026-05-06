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
            Some(p) => p.to_string(),
            None => return ToolOutput::error("Missing required parameter: pattern. Usage: {\"pattern\": \"<search text>\"}"),
        };
        let base = if let Some(p) = args.get("path").and_then(|p| p.as_str()) {
            // Normalize path: trim whitespace and standardize separators
            let normalized_path = p.trim().replace('\\', "/");
            let resolved = ctx.working_dir.join(&normalized_path);
            
            // Keep user-friendly path for error messages
            let _display_base = resolved.clone();
            
            match crate::safety::validate_path_within_workdir(&resolved, &ctx.working_dir) {
                Ok(validated) => validated,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            }
        } else {
            ctx.working_dir.to_path_buf()
        };
        let file_pattern = args
            .get("file_pattern")
            .and_then(|p| p.as_str())
            .unwrap_or("*")
            .to_string();
        let working_dir = ctx.working_dir.clone();

        // Run blocking file I/O on a dedicated thread to avoid blocking the Tokio runtime.
        let result = tokio::task::spawn_blocking(move || {
            search_files(&pattern, &base, &file_pattern, &working_dir)
        })
        .await;

        match result {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Search task failed: {e}")),
        }
    }
}

fn search_files(pattern: &str, base: &Path, file_pattern: &str, working_dir: &Path) -> Result<ToolOutput, String> {
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => {
            match regex::Regex::new(&regex::escape(pattern)) {
                Ok(r) => r,
                Err(e) => return Err(format!("Invalid pattern: {e}")),
            }
        }
    };

    let glob_pattern = base.join("**").join(file_pattern);
    let entries = match glob::glob(&glob_pattern.to_string_lossy()) {
        Ok(e) => e,
        Err(e) => return Err(format!("Invalid file pattern: {e}")),
    };

    let mut results = Vec::new();
    let mut files_searched = 0u32;

    for entry in entries {
        let path = match entry {
            Ok(p) if p.is_file() => p,
            _ => continue,
        };

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
                let relative = path.strip_prefix(working_dir).unwrap_or(&path);
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
                    return Ok(ToolOutput::success(results.join("\n")));
                }
            }
        }
    }

    if results.is_empty() {
        Ok(ToolOutput::success(format!(
            "No matches found for '{pattern}' (searched {files_searched} files)"
        )))
    } else {
        Ok(ToolOutput::success(results.join("\n")))
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
