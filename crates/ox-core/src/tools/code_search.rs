use serde_json::{Value, json};
use std::fs;
use std::path::Path;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};
use crate::file_index::should_exclude_path;

pub struct CodeSearchTool;

#[async_trait::async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str {
        "code_search"
    }

    fn description(&self) -> &str {
        "Search for text/regex patterns in file contents. Returns matching lines with file paths and line numbers. Automatically excludes common directories like node_modules, .git, etc."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "✅ REQUIRED: Text or regex pattern to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Default: working directory."
                },
                "file_pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs'). Default: all files."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to return results from. Default: 20."
                },
                "max_matches_per_file": {
                    "type": "integer",
                    "description": "Maximum number of matches to return per file. Default: 5."
                },
                "exclude_dirs": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Additional directories to exclude. Default excludes: node_modules, .git, target, dist, build, __pycache__, .venv, venv, coverage, .next, .nuxt"
                }
            },
            "required": ["pattern"],
            "examples": [
                {"pattern": "fn main"},
                {"pattern": "pub struct", "file_pattern": "*.rs"},
                {"pattern": "error", "max_files": 10, "max_matches_per_file": 3}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolOutput::error(
                    "Missing required parameter: pattern. Usage: {\"pattern\": \"<search text>\"}",
                );
            }
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
        let max_files = args
            .get("max_files")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let max_matches_per_file = args
            .get("max_matches_per_file")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let exclude_dirs = args
            .get("exclude_dirs")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
            .unwrap_or_default();
        let working_dir = ctx.working_dir.clone();

        // Run blocking file I/O on a dedicated thread to avoid blocking the Tokio runtime.
        let result = tokio::task::spawn_blocking(move || {
            search_files(&pattern, &base, &file_pattern, &working_dir, max_files, max_matches_per_file, &exclude_dirs)
        })
        .await;

        match result {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Search task failed: {e}")),
        }
    }
}

/// Represents a single match result with relevance score
#[derive(Clone)]
struct MatchResult {
    file_path: String,
    line_num: usize,
    line_content: String,
    score: f64,  // Higher score means more relevant
}

fn search_files(
    pattern: &str,
    base: &Path,
    file_pattern: &str,
    working_dir: &Path,
    max_files: usize,
    max_matches_per_file: usize,
    exclude_dirs: &[String],
) -> Result<ToolOutput, String> {
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => match regex::Regex::new(&regex::escape(pattern)) {
            Ok(r) => r,
            Err(e) => return Err(format!("Invalid pattern: {e}")),
        },
    };

    let glob_pattern = base.join("**").join(file_pattern);
    let entries = match glob::glob(&glob_pattern.to_string_lossy()) {
        Ok(e) => e,
        Err(e) => return Err(format!("Invalid file pattern: {e}")),
    };

    let mut all_matches: Vec<MatchResult> = Vec::new();
    let mut files_searched = 0u32;
    let mut files_with_results = 0u32;

    for entry in entries {
        let path = match entry {
            Ok(p) if p.is_file() => p,
            _ => continue,
        };

        // Use unified exclusion logic from file_index module
        let relative_path = path.strip_prefix(working_dir).unwrap_or(&path);
        let rel_path_str = relative_path.to_string_lossy();
        
        // Check against default excludes
        if should_exclude_path(&rel_path_str) {
            continue;
        }
        
        // Also check custom exclude dirs
        let custom_exclude = exclude_dirs.iter().any(|dir| {
            relative_path.components().any(|comp| {
                comp.as_os_str().to_string_lossy() == dir.as_str()
            })
        });
        
        if custom_exclude {
            continue;
        }

        if is_binary_path(&path) {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        files_searched += 1;

        let mut file_matches: Vec<MatchResult> = Vec::new();
        let file_path_str = rel_path_str.to_string();
        
        for (line_num, line) in content.lines().enumerate() {
            if let Some(matched) = re.find(line) {
                // Calculate relevance score based on:
                // 1. Match position (earlier is better)
                // 2. Match length relative to line length
                // 3. Line length (shorter lines often more focused)
                let match_start = matched.start();
                let match_len = matched.end() - matched.start();
                let line_len = line.len();
                
                // Score calculation:
                // - Position factor: earlier matches get higher score (1.0 - start/line_len)
                // - Density factor: longer match relative to line gets higher score
                // - Focus factor: shorter lines get slightly higher score
                let position_factor = 1.0 - (match_start as f64 / line_len.max(1) as f64);
                let density_factor = match_len as f64 / line_len.max(1) as f64;
                let focus_factor = 1.0 / (1.0 + (line_len as f64 / 100.0)); // Normalize line length
                
                let score = position_factor * 0.4 + density_factor * 0.4 + focus_factor * 0.2;
                
                file_matches.push(MatchResult {
                    file_path: file_path_str.clone(),
                    line_num: line_num + 1,
                    line_content: line.trim().to_string(),
                    score,
                });

                if file_matches.len() >= max_matches_per_file {
                    break;
                }
            }
        }

        if !file_matches.is_empty() {
            // Sort matches within file by score (highest first)
            file_matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            all_matches.extend(file_matches);
            files_with_results += 1;
            
            if files_with_results >= max_files as u32 {
                break;
            }
        }
    }

    // Sort all matches by score (highest first)
    all_matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    
    // Limit total results to avoid overwhelming output
    let total_limit = max_files * max_matches_per_file;
    if all_matches.len() > total_limit {
        all_matches.truncate(total_limit);
    }

    if all_matches.is_empty() {
        Ok(ToolOutput::success(format!(
            "No matches found for '{pattern}' (searched {files_searched} files)"
        )))
    } else {
        let results: Vec<String> = all_matches.iter().map(|m| {
            format!("{}:{}: {}", m.file_path, m.line_num, m.line_content)
        }).collect();
        
        let truncated_msg = if files_with_results >= max_files as u32 {
            format!("\n... (showing top {} results from {} files, searched {} files total)", 
                   results.len(), files_with_results, files_searched)
        } else {
            format!("\n... (showing {} results from {} files, searched {} files total)", 
                   results.len(), files_with_results, files_searched)
        };
        
        Ok(ToolOutput::success(format!("{}{}", results.join("\n"), truncated_msg)))
    }
}

fn is_binary_path(path: &Path) -> bool {
    let binary_exts = [
        "exe", "dll", "so", "dylib", "bin", "obj", "o", "a", "lib", "png", "jpg", "jpeg", "gif",
        "bmp", "ico", "svg", "pdf", "zip", "tar", "gz", "7z", "rar", "wasm", "ttf", "otf", "woff",
        "woff2", "mp3", "mp4", "avi", "mov", "pdb", "lock",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| binary_exts.contains(&ext.to_lowercase().as_str()))
}
