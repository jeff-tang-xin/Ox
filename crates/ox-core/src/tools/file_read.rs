use serde_json::{json, Value};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileReadTool;

#[async_trait::async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns the full text content."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to working directory)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based, optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (optional)"
                }
            },
            "required": ["path"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let raw_path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) if !p.is_empty() => p,
            Some(_) => return ToolOutput::error(
                "❌ Parameter Error: 'path' cannot be empty\n\n\
                 💡 Example usage:\n\
                 {\"path\": \"src/main.rs\", \"limit\": 100}\n\n\
                 Please provide a valid file path."
            ),
            None => return ToolOutput::error(
                "❌ Missing Required Parameter: 'path'\n\n\
                 💡 How to fix:\n\
                 • Add the 'path' parameter with the file location\n\
                 • Path can be relative to working directory\n\
                 • Use forward slashes (/) for paths\n\n\
                 📝 Example usage:\n\
                 {\"path\": \"src/main.rs\"} - Read entire file\n\
                 {\"path\": \"src/main.rs\", \"offset\": 10, \"limit\": 50} - Read lines 10-60"
            ),
        };
        
        // Normalize path: 
        // 1. Replace backslashes with forward slashes for consistency
        // 2. Trim whitespace that LLM might accidentally include
        let normalized_path = raw_path.trim().replace('\\', "/");
        
        // Handle edge case: if LLM provides an absolute path, use it directly
        // Otherwise, join with working directory
        let resolved_path = if std::path::Path::new(&normalized_path).is_absolute() {
            std::path::PathBuf::from(&normalized_path)
        } else {
            ctx.working_dir.join(&normalized_path)
        };

        // Path traversal protection.
        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };

        let offset = args
            .get("offset")
            .and_then(|o| o.as_u64())
            .map(|o| o.saturating_sub(1) as usize); // 1-based to 0-based
        let limit = args.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize);

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.unwrap_or(0).min(lines.len());
                let end = limit
                    .map(|l| (start + l).min(lines.len()))
                    .unwrap_or(lines.len());

                let selected: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4}\t{line}", start + i + 1))
                    .collect();

                ToolOutput::success(selected.join("\n"))
            }
            Err(e) => ToolOutput::error(format!("Failed to read {}: {e}", path.display())),
        }
    }
}
