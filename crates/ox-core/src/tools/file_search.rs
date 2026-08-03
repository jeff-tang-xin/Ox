use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileSearchTool;

#[async_trait::async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &str {
        "file_search"
    }

    fn description(&self) -> &str {
        "Search for files by name pattern (glob). Recursively searches from working directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g., '*.rs', 'Cargo.*')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Default: working directory."
                }
            },
            "required": ["pattern"],
            "examples": [
                {"pattern": "*.rs"},
                {"pattern": "Cargo.*", "path": "src/"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim(),
            _ => {
                return ToolOutput::error(
                    "Missing required parameter: pattern. Usage: {\"pattern\": \"*.java\"}",
                );
            }
        };
        // 📌 Resolve relative paths against the STABLE project root (path_base).
        let path_base = ctx.path_base();
        let base = if let Some(p) = args.get("path").and_then(|p| p.as_str()) {
            let normalized_path = p.trim().replace('\\', "/");
            let resolved = path_base.join(&normalized_path);

            match crate::safety::validate_path_within_workdir(&resolved, &path_base) {
                Ok(validated) => validated,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            }
        } else {
            path_base.clone()
        };

        let full_pattern = base.join("**").join(pattern);
        let pattern_str = full_pattern.to_string_lossy().into_owned();
        let workdir = path_base;

        // glob walk is synchronous — run off the async runtime with a hard timeout
        let search = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            tokio::task::spawn_blocking(move || run_glob_search(&pattern_str, &workdir)),
        )
        .await;

        match search {
            Ok(Ok(Ok(output))) => ToolOutput::success(output),
            Ok(Ok(Err(e))) => ToolOutput::error(e),
            Ok(Err(e)) => ToolOutput::error(format!("file_search task failed: {e}")),
            Err(_) => ToolOutput::error(
                "file_search timed out after 45s (directory too large). \
                 Narrow `path` to a submodule, or use file_list + file_read instead.",
            ),
        }
    }
}

fn run_glob_search(pattern_str: &str, workdir: &std::path::Path) -> Result<String, String> {
    match glob::glob(pattern_str) {
        Ok(entries) => {
            let mut results = Vec::new();
            for path in entries.flatten().take(200) {
                let relative = path.strip_prefix(workdir).unwrap_or(&path);
                results.push(relative.display().to_string());
            }
            if results.is_empty() {
                Ok("No files found matching the pattern.".to_string())
            } else {
                let count = results.len();
                let mut output = results.join("\n");
                if count == 200 {
                    output.push_str("\n... (truncated at 200 results)");
                }
                output.push_str(
                    "\n\n💡 file_search 递归搜文件名。要列目录结构用 file_list（单层，逐层向下）。",
                );
                Ok(output)
            }
        }
        Err(e) => Err(format!("Invalid pattern: {e}")),
    }
}
