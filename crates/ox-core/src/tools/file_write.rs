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
        "Create a new file or completely overwrite an existing file with new content. \
         Use this ONLY for: (1) creating brand new files, (2) rewriting entire files (>50% changed). \
         For small edits to existing files, use file_patch instead. \
         Automatically creates parent directories if they don't exist."
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
            Some(p) if !p.is_empty() => p,
            Some(_) => return ToolOutput::error(
                "❌ Parameter Error: 'path' cannot be empty\n\n\
                 💡 Example: {\"path\": \"output.txt\", \"content\": \"Hello World\"}"
            ),
            None => return ToolOutput::error(
                "❌ Missing Required Parameter: 'path'\n\n\
                 💡 How to fix:\n\
                 • Add the 'path' parameter with the file location\n\
                 • Path can be relative to working directory\n\n\
                 📝 Example: {\"path\": \"output.txt\", \"content\": \"Your content here\"}"
            ),
        };
        let resolved_path = ctx.working_dir.join(raw_path);
        let path = match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(
                format!(
                    "❌ Security Error: {}\n\n\
                     💡 The file path must be within the working directory:\n\
                     {}",
                    e, ctx.working_dir.display()
                )
            ),
        };
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return ToolOutput::error(
                "❌ Missing Required Parameter: 'content'\n\n\
                 💡 How to fix:\n\
                 • Add the 'content' parameter with the file content\n\
                 • Content should be a string (escape special characters)\n\n\
                 📝 Example: {\"path\": \"hello.txt\", \"content\": \"Hello, World!\\nSecond line\"}"
            ),
        };

        // Validate content quality - prevent garbled/corrupted text
        if let Err(e) = validate_content(content) {
            return ToolOutput::error(e);
        }

        // Create parent directories.
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent) {
                return ToolOutput::error(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ));
            }

        // Write file with UTF-8 encoding
        // For text files (.txt, .md, .log, etc.), add BOM for Windows compatibility
        // For code files (.rs, .py, .js, etc.), write without BOM (compilers may reject BOM)
        let should_add_bom = path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| matches!(ext.to_lowercase().as_str(), 
                "txt" | "md" | "markdown" | "log" | "csv" | "json" | "xml" | "html" | "css"
            ))
            .unwrap_or(false);
        
        let bytes_to_write = if should_add_bom {
            // Add UTF-8 BOM (0xEF 0xBB 0xBF) for text files on Windows
            let mut bytes = Vec::with_capacity(3 + content.len());
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            bytes.extend_from_slice(content.as_bytes());
            bytes
        } else {
            // Write code files without BOM
            content.as_bytes().to_vec()
        };
        
        match fs::write(&path, &bytes_to_write) {
            Ok(()) => {
                let encoding_info = if should_add_bom { "UTF-8 with BOM" } else { "UTF-8" };
                ToolOutput::success(format!(
                    "Written {} bytes to {} ({})",
                    bytes_to_write.len(),
                    path.display(),
                    encoding_info
                ))
            },
            Err(e) => ToolOutput::error(format!("Failed to write {}: {e}", path.display())),
        }
    }
}

/// Validate file content to prevent garbled/corrupted text from being written
fn validate_content(content: &str) -> Result<(), String> {
    // Check 1: Validate UTF-8 encoding (most important for Chinese)
    if !content.is_ascii() && !String::from_utf8(content.as_bytes().to_vec()).is_ok() {
        return Err("❌ Invalid Content: File content contains invalid UTF-8 encoding\n\n\
                    💡 Possible causes:\n\
                    • Content was copied from a corrupted source\n\
                    • Binary data was included by mistake\n\
                    • Encoding mismatch (e.g., GBK vs UTF-8)\n\n\
                    📝 Please verify the content encoding and retry.".to_string());
    }

    // Check 2: Detect null bytes (common corruption indicator)
    if content.contains('\x00') {
        return Err("❌ Corrupted Content: File contains null bytes (\\x00)\n\n\
                    💡 This indicates:\n\
                    • File corruption\n\
                    • Binary data mixed with text\n\
                    • Encoding errors\n\n\
                     Please verify and regenerate the content.".to_string());
    }

    // Check 3: Detect replacement characters (U+FFFD - encoding failure indicator)
    if content.contains('\u{FFFD}') {
        return Err("❌ Encoding Errors Detected: Content contains replacement characters (U+FFFD)\n\n\
                    💡 This means:\n\
                    • Original text had invalid encoding\n\
                    • Conversion between encodings failed\n\
                    • Data was corrupted during transfer\n\n\
                    📝 Please use the original source with correct encoding.".to_string());
    }

    // All checks passed - content is valid
    Ok(())
}
