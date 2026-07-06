use grep::regex::RegexMatcher;
use grep::searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct CodeSearchTool;

#[async_trait::async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str {
        "code_search"
    }

    fn description(&self) -> &str {
        "High-performance code search using ripgrep engine. Searches text/regex patterns in file contents with .gitignore support. Returns matching lines with file paths and line numbers."
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
                    "description": "Filter by filename only (e.g. '*.rs'), NOT a path pattern. Default: all files."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to return results from. Default: 20."
                },
                "max_matches_per_file": {
                    "type": "integer",
                    "description": "Maximum number of matches to return per file. Default: 5."
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Enable case-insensitive search. Default: false."
                }
            },
            "required": ["pattern"],
            "examples": [
                {"pattern": "fn main"},
                {"pattern": "pub struct", "file_pattern": "*.rs"},
                {"pattern": "error", "max_files": 10, "max_matches_per_file": 3, "case_insensitive": true}
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
            let normalized_path = p.trim().replace('\\', "/");
            let resolved = ctx.working_dir.join(&normalized_path);

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

        let max_files = args.get("max_files").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let max_matches_per_file = args
            .get("max_matches_per_file")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let working_dir = ctx.working_dir.clone();

        // Run blocking file I/O on a dedicated thread to avoid blocking the Tokio runtime.
        let result = tokio::task::spawn_blocking(move || {
            search_with_ripgrep(
                &pattern,
                &base,
                &file_pattern,
                &working_dir,
                max_files,
                max_matches_per_file,
                case_insensitive,
            )
        })
        .await;

        match result {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("Search task failed: {e}")),
        }
    }
}

fn search_with_ripgrep(
    pattern: &str,
    base: &Path,
    file_pattern: &str,
    working_dir: &Path,
    max_files: usize,
    max_matches_per_file: usize,
    case_insensitive: bool,
) -> ToolOutput {
    // 1. Build regex matcher with ripgrep's optimized engine
    let final_pattern = if case_insensitive {
        format!("(?i){}", pattern)
    } else {
        pattern.to_string()
    };

    let matcher = RegexMatcher::new(&final_pattern)
        .or_else(|_| RegexMatcher::new(&regex::escape(&final_pattern)))
        .map_err(|e| format!("Invalid pattern: {e}"));

    let matcher = match matcher {
        Ok(m) => m,
        Err(e) => return ToolOutput::error(e),
    };

    // 2. Build searcher with line numbers and binary detection
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(grep::searcher::BinaryDetection::quit(b'\x00'))
        .build();

    // 3. Use WalkBuilder for intelligent file traversal (.gitignore-aware)
    let walker = WalkBuilder::new(base)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .hidden(false)
        .build();

    // 4. Collect results
    let total_limit = max_files * max_matches_per_file;
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut files_searched = 0u32;
    let mut files_with_results = 0u32;

    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        // Only process files
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        // Apply file pattern filter
        if file_pattern != "*"
            && let Some(file_name) = entry.file_name().to_str()
                && !glob::Pattern::new(file_pattern)
                    .map(|pat| pat.matches(file_name))
                    .unwrap_or(true)
                {
                    continue;
                }

        files_searched += 1;

        // Check global limits
        if files_with_results >= max_files as u32 {
            break;
        }

        let path = entry.path().to_path_buf();
        let relative_path = path
            .strip_prefix(working_dir)
            .unwrap_or(&path)
            .display()
            .to_string();

        // Create a per-file result collector
        let file_results = Arc::new(Mutex::new(Vec::new()));
        let file_results_clone = Arc::clone(&file_results);

        struct FileSink {
            results: Arc<Mutex<Vec<String>>>,
            file_path: String,
            max_matches: usize,
        }

        impl Sink for FileSink {
            type Error = std::io::Error;

            fn matched(
                &mut self,
                _searcher: &Searcher,
                mat: &SinkMatch<'_>,
            ) -> Result<bool, Self::Error> {
                if let Ok(text) = std::str::from_utf8(mat.bytes()) {
                    let line_with_num = format!(
                        "{}:{}: {}",
                        self.file_path,
                        mat.line_number().unwrap_or(0),
                        text.trim()
                    );

                    let mut results = self.results.lock().unwrap();
                    if results.len() < self.max_matches {
                        results.push(line_with_num);
                    }
                }

                Ok(self.results.lock().unwrap().len() < self.max_matches)
            }
        }

        let sink = FileSink {
            results: file_results_clone,
            file_path: relative_path.clone(),
            max_matches: max_matches_per_file,
        };

        let _ = searcher.search_path(&matcher, &path, sink);

        let file_match_count = file_results.lock().unwrap().len();
        if file_match_count > 0 {
            files_with_results += 1;
            let mut all_results = results.lock().unwrap();
            all_results.extend(file_results.lock().unwrap().iter().cloned());

            if all_results.len() >= total_limit || files_with_results >= max_files as u32 {
                break;
            }
        }
    }

    let final_results = results.lock().unwrap().clone();

    if final_results.is_empty() {
        ToolOutput::success(format!(
            "No matches found for '{pattern}' (searched {files_searched} files)"
        ))
    } else {
        let truncated_msg = if files_with_results >= max_files as u32 {
            format!(
                "\n... (showing top {} results from {} files, searched {} files total)",
                final_results.len(),
                files_with_results,
                files_searched
            )
        } else {
            format!(
                "\n... (showing {} results from {} files, searched {} files total)",
                final_results.len(),
                files_with_results,
                files_searched
            )
        };

        ToolOutput::success(format!("{}{}", final_results.join("\n"), truncated_msg))
    }
}
