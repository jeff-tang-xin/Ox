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

/// Validate file content to prevent garbled/corrupted text from being written
fn validate_content(content: &str) -> Result<(), String> {
    // Check 1: Validate UTF-8 encoding
    if !content.is_ascii() && !String::from_utf8(content.as_bytes().to_vec()).is_ok() {
        return Err(" Invalid Content: File content contains invalid UTF-8 encoding\n\n\
                    💡 Possible causes:\n\
                    • Content was copied from a corrupted source\n\
                    • Binary data was included by mistake\n\
                    • Encoding mismatch (e.g., GBK vs UTF-8)\n\n\
                    📝 Please verify the content encoding and retry.".to_string());
    }

    // Check 2: Detect excessive non-printable characters (garbled text indicator)
    let total_chars = content.chars().count();
    if total_chars == 0 {
        return Err("❌ Empty Content: File content cannot be empty\n\n\
                    💡 Please provide valid file content.".to_string());
    }

    // Count non-printable characters (excluding whitespace and common control chars)
    let non_printable_count = content
        .chars()
        .filter(|c| {
            !c.is_whitespace()
                && !c.is_ascii_graphic()
                && !c.is_ascii_punctuation()
                && !matches!(*c, '\n' | '\r' | '\t') // Allow common whitespace
        })
        .count();

    let non_printable_ratio = non_printable_count as f64 / total_chars as f64;

    // Allow up to 2% non-printable chars (some source code may have special chars)
    if non_printable_ratio > 0.02 {
        return Err(format!(
            "❌ Suspicious Content: {:.1}% of content contains non-printable characters ({}/{} chars)\n\n\
             💡 This usually indicates:\n\
             • Garbled/corrupted text (乱码)\n\
             • Binary data accidentally included\n\
             • Encoding conversion errors\n\n\
             📝 Please verify the content is valid text and retry.",
            non_printable_ratio * 100.0,
            non_printable_count,
            total_chars
        ));
    }

    // Check 3: Detect common garbled text patterns
    if contains_garbled_patterns(content) {
        return Err("❌ Garbled Text Detected: Content appears to contain garbled characters\n\n\
                    💡 Common causes:\n\
                    • Character encoding mismatch (UTF-8 vs GBK/GB2312)\n\
                    • Copy-paste errors from corrupted sources\n\
                    • Binary data mixed with text\n\n\
                    📝 Please re-generate the content with correct encoding.".to_string());
    }

    // Check 4: Detect null bytes (common corruption indicator)
    if content.contains('\x00') {
        return Err("❌ Corrupted Content: File contains null bytes (\\x00)\n\n\
                    💡 This indicates:\n\
                    • File corruption\n\
                    • Binary data mixed with text\n\
                    • Encoding errors\n\n\
                     Please verify and regenerate the content.".to_string());
    }

    // Check 5: Detect replacement characters (U+FFFD - encoding failure indicator)
    if content.contains('\u{FFFD}') {
        return Err("❌ Encoding Errors Detected: Content contains replacement characters (U+FFFD)\n\n\
                    💡 This means:\n\
                    • Original text had invalid encoding\n\
                    • Conversion between encodings failed\n\
                    • Data was corrupted during transfer\n\n\
                    📝 Please use the original source with correct encoding.".to_string());
    }

    Ok(())
}

/// Detect common garbled text patterns
fn contains_garbled_patterns(content: &str) -> bool {
    // Pattern 1: Consecutive non-printable characters (≥5)
    let mut consecutive_non_printable = 0;
    for c in content.chars() {
        if !c.is_whitespace() && !c.is_ascii_graphic() && !c.is_ascii_punctuation() {
            consecutive_non_printable += 1;
            if consecutive_non_printable >= 5 {
                return true;
            }
        } else {
            consecutive_non_printable = 0;
        }
    }

    // Pattern 2: Mixed CJK and garbage characters (common in encoding errors)
    // Detect sequences like: 文字 or 文件   
    let mut mixed_sequence = 0;
    for c in content.chars() {
        if (c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace())
            || (c.is_ascii_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            mixed_sequence += 1;
            if mixed_sequence >= 10 {
                return true;
            }
        } else {
            mixed_sequence = 0;
        }
    }

    false
}
