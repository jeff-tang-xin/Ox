use serde_json::{json, Value};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileSearchTool;

#[async_trait::async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &str {
        "file_search"
    }

    fn description(&self) -> &str {
        "Search for files by name pattern. Recursively searches from the working directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match file names (e.g. '*.rs', 'Cargo.*')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: working directory)"
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
            None => return ToolOutput::error("Missing required parameter: pattern. Usage: {\"pattern\": \"<glob pattern>\"}"),
        };
        let base = if let Some(p) = args.get("path").and_then(|p| p.as_str()) {
            let resolved = ctx.working_dir.join(p);
            match crate::safety::validate_path_within_workdir(&resolved, &ctx.working_dir) {
                Ok(validated) => validated,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            }
        } else {
            ctx.working_dir.to_path_buf()
        };

        let full_pattern = base.join("**").join(pattern);
        match glob::glob(&full_pattern.to_string_lossy()) {
            Ok(entries) => {
                let mut results = Vec::new();
                for path in entries.take(200).flatten() {
                    let relative = path.strip_prefix(&ctx.working_dir).unwrap_or(&path);
                    results.push(relative.display().to_string());
                }
                if results.is_empty() {
                    ToolOutput::success("No files found matching the pattern.")
                } else {
                    let count = results.len();
                    let mut output = results.join("\n");
                    if count == 200 {
                        output.push_str("\n... (truncated at 200 results)");
                    }
                    ToolOutput::success(output)
                }
            }
            Err(e) => ToolOutput::error(format!("Invalid pattern: {e}")),
        }
    }
}
