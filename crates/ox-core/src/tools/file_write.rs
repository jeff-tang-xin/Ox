use serde_json::{json, Value};
use std::fs;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileWriteTool;

#[async_trait::async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Creates parent directories as needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to working directory)"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let raw_path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolOutput::error("Missing required parameter: path. Usage: {\"path\": \"<file path>\", \"content\": \"<content>\"}"),
        };
        let resolved_path = ctx.working_dir.join(raw_path);
        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return ToolOutput::error("Missing required parameter: content. Usage: {\"path\": \"<file path>\", \"content\": \"<content>\"}"),
        };

        // Create parent directories.
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent) {
                return ToolOutput::error(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ));
            }

        match fs::write(&path, content) {
            Ok(()) => ToolOutput::success(format!(
                "Written {} bytes to {}",
                content.len(),
                path.display()
            )),
            Err(e) => ToolOutput::error(format!("Failed to write {}: {e}", path.display())),
        }
    }
}
