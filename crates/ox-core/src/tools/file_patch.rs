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
        let raw_path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => return ToolOutput::error("Missing required parameter: path. Usage: {\"path\": \"<file>\", \"search\": \"<text>\", \"replace\": \"<text>\"}"),
        };
        let resolved_path = ctx.working_dir.join(raw_path);
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

        // Validate replacement content quality
        if let Err(e) = validate_content(replace) {
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
        return Err("❌ Empty Content: Replacement content cannot be empty\n\n\
                    💡 Please provide valid replacement text.".to_string());
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
            "❌ Suspicious Content: {:.1}% of replacement content contains non-printable characters ({}/{} chars)\n\n\
             💡 This usually indicates:\n\
             • Garbled/corrupted text (乱码)\n\
             • Binary data accidentally included\n\
             • Encoding conversion errors\n\n\
              Please verify the content is valid text and retry.",
            non_printable_ratio * 100.0,
            non_printable_count,
            total_chars
        ));
    }

    // Check 3: Detect common garbled text patterns
    if contains_garbled_patterns(content) {
        return Err("❌ Garbled Text Detected: Replacement content appears to contain garbled characters\n\n\
                    💡 Common causes:\n\
                    • Character encoding mismatch (UTF-8 vs GBK/GB2312)\n\
                    • Copy-paste errors from corrupted sources\n\
                    • Binary data mixed with text\n\n\
                    📝 Please re-generate the content with correct encoding.".to_string());
    }

    // Check 4: Detect null bytes (common corruption indicator)
    if content.contains('\x00') {
        return Err("❌ Corrupted Content: Replacement content contains null bytes (\\x00)\n\n\
                    💡 This indicates:\n\
                    • File corruption\n\
                    • Binary data mixed with text\n\
                    • Encoding errors\n\n\
                    📝 Please verify and regenerate the content.".to_string());
    }

    // Check 5: Detect replacement characters (U+FFFD - encoding failure indicator)
    if content.contains('\u{FFFD}') {
        return Err("❌ Encoding Errors Detected: Replacement content contains replacement characters (U+FFFD)\n\n\
                    💡 This means:\n\
                    • Original text had invalid encoding\n\
                    • Conversion between encodings failed\n\
                    • Data was corrupted during transfer\n\n\
                     Please use the original source with correct encoding.".to_string());
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
