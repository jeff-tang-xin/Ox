use serde_json::{json, Value};
use std::fs;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FilePatchTool;

#[async_trait::async_trait]
impl Tool for FilePatchTool {
    fn name(&self) -> &str {
        "file_patch"
    }

    fn description(&self) -> &str {
        "Apply a search-and-replace patch to a file. The search string must match exactly once in the file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to patch (relative to working directory)"
                },
                "search": {
                    "type": "string",
                    "description": "The exact text to search for (must match exactly once)"
                },
                "replace": {
                    "type": "string",
                    "description": "The replacement text"
                }
            },
            "required": ["path", "search", "replace"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => ctx.working_dir.join(p),
            None => return ToolOutput::error("Missing required parameter: path"),
        };
        let search = match args.get("search").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => return ToolOutput::error("Missing required parameter: search"),
        };
        let replace = match args.get("replace").and_then(|r| r.as_str()) {
            Some(r) => r,
            None => return ToolOutput::error("Missing required parameter: replace"),
        };

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to read {}: {e}", path.display())),
        };

        let count = content.matches(search).count();
        match count {
            0 => ToolOutput::error(format!(
                "Search string not found in {}",
                path.display()
            )),
            1 => {
                let new_content = content.replacen(search, replace, 1);
                match fs::write(&path, &new_content) {
                    Ok(()) => ToolOutput::success(format!(
                        "Patched {} (replaced 1 occurrence)",
                        path.display()
                    )),
                    Err(e) => {
                        ToolOutput::error(format!("Failed to write {}: {e}", path.display()))
                    }
                }
            }
            n => ToolOutput::error(format!(
                "Search string found {n} times in {} (must match exactly once). Provide more context to make it unique.",
                path.display()
            )),
        }
    }
}
