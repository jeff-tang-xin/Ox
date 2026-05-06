use serde_json::{json, Value};
use std::fs;

use super::{content_validation, SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FilePatchTool;

#[async_trait::async_trait]
impl Tool for FilePatchTool {
    fn name(&self) -> &str {
        "file_patch"
    }

    fn description(&self) -> &str {
        "Apply a targeted edit to an existing file by replacing specific text. \
         Use this for small changes (<50% of file). The search string must match EXACTLY ONCE in the file. \
         For creating new files or rewriting entire files, use file_write instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file (relative to working directory). Example: 'src/main.rs'"
                },
                "search": {
                    "type": "string",
                    "description": "The EXACT text to find and replace. Must match exactly once in the file. Include enough context to be unique."
                },
                "replace": {
                    "type": "string",
                    "description": "The replacement text. Use \\n for newlines. Escape special characters properly in JSON."
                }
            },
            "required": ["path", "search", "replace"],
            "examples": [
                {
                    "path": "src/main.rs",
                    "search": "fn old_function() {\n    println!(\"old\");\n}",
                    "replace": "fn new_function() {\n    println!(\"new\");\n}"
                }
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolOutput::error("Missing required parameter: path. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}"),
        };
        
        // Normalize path: trim whitespace and standardize separators
        let normalized_path = path_str.trim().replace('\\', "/");
        
        // Handle absolute vs relative paths
        let resolved_path = if std::path::Path::new(&normalized_path).is_absolute() {
            std::path::PathBuf::from(&normalized_path)
        } else {
            ctx.working_dir.join(&normalized_path)
        };
        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
        };
        let search = match args.get("search").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => return ToolOutput::error("Missing required parameter: search. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}"),
        };
        let replace = match args.get("replace").and_then(|r| r.as_str()) {
            Some(r) => r,
            None => return ToolOutput::error("Missing required parameter: replace. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}"),
        };

        // Validate replacement content using shared validation logic
        if let Err(e) = content_validation::validate_content(replace) {
            return ToolOutput::error(e);
        }

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
