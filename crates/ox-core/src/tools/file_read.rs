use encoding_rs::Encoding;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufReader, Read};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileReadTool;

#[async_trait::async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read file contents. Use for viewing code, configs, or docs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative). Required unless using file_id or filename."
                },
                "filename": {
                    "type": "string",
                    "description": "Filename to search in index. Errors if multiple matches."
                },
                "file_id": {
                    "type": "integer",
                    "description": "File ID from index (most reliable). Use file_list to get IDs."
                },
                "offset": {
                    "type": "integer",
                    "description": "Start line (1-based). Default: 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to read. Default: 200. Set higher for full file reads."
                },
                "encoding": {
                    "type": "string",
                    "description": "File encoding. Options: 'utf-8' (default), 'gbk', 'gb18030', 'utf-16le', 'utf-16be', 'latin1'. Auto-detected if not specified.",
                    "enum": ["utf-8", "gbk", "gb18030", "utf-16le", "utf-16be", "latin1"]
                }
            },
            "examples": [
                {"path": "src/main.rs"},
                {"path": "config.toml", "limit": 100},
                {"file_id": 123},
                {"path": "legacy.txt", "encoding": "gbk"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // Determine file path from parameters (priority: file_id > filename > path)
        let resolved_path = if let Some(file_id) = args.get("file_id").and_then(|id| id.as_i64()) {
            // Method 1: Use file_id for precise matching
            match ctx.file_index.find_by_id(file_id) {
                Ok(Some(entry)) => ctx.working_dir.join(&entry.full_path),
                Ok(None) => {
                    return ToolOutput::error(format!(
                        "❌ File Not Found: No file with ID {}\n\n\
                             💡 How to fix:\n\
                             • Use file_list tool to see available files and their IDs\n\
                             • Or use 'filename' or 'path' parameter instead",
                        file_id
                    ));
                }
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(filename) = args.get("filename").and_then(|f| f.as_str()) {
            // Method 2: Use filename (may have multiple matches)
            match ctx.file_index.find_by_filename(filename) {
                Ok(matches) if matches.len() == 1 => ctx.working_dir.join(&matches[0].full_path),
                Ok(matches) if matches.len() > 1 => {
                    // Multiple matches - return options for LLM to choose
                    let options: Vec<String> = matches
                        .iter()
                        .map(|e| format!("  [ID: {}] {}", e.id, e.full_path))
                        .collect();

                    return ToolOutput::error(format!(
                        "❌ Multiple Files Matched '{}':\n{}\n\n\
                                 💡 How to fix:\n\
                                 • Retry with 'file_id' parameter for precise matching\n\
                                 • Example: {{\"file_id\": {}}}",
                        filename,
                        options.join("\n"),
                        matches[0].id
                    ));
                }
                Ok(_) => {
                    return ToolOutput::error(format!(
                        "❌ File Not Found: '{}' not in index\n\n\
                                 💡 How to fix:\n\
                                 • Check filename spelling\n\
                                 • Use file_list tool to see all indexed files\n\
                                 • Or use 'path' parameter with full relative path",
                        filename
                    ));
                }
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(raw_path) = args.get("path").and_then(|p| p.as_str()) {
            // Method 3: Traditional path-based approach (backward compatible)
            if raw_path.is_empty() {
                return ToolOutput::error(
                    "❌ Parameter Error: 'path' cannot be empty\n\n\
                     💡 Example usage:\n\
                     {\"path\": \"src/main.rs\", \"limit\": 100}\n\n\
                     Please provide a valid file path.",
                );
            }

            // Normalize path: trim whitespace and standardize separators
            let normalized_path = raw_path.trim().replace('\\', "/");

            // Handle absolute vs relative paths
            if std::path::Path::new(&normalized_path).is_absolute() {
                std::path::PathBuf::from(&normalized_path)
            } else {
                ctx.working_dir.join(&normalized_path)
            }
        } else {
            return ToolOutput::error(
                "❌ Missing Required Parameter\n\n\
                 💡 How to fix - provide ONE of:\n\
                 • 'file_id': Precise file ID from index (recommended)\n\
                 • 'filename': Filename to search (must be unique)\n\
                 • 'path': Full relative path (traditional method)\n\n\
                 📝 Examples:\n\
                 {\"file_id\": 123} - Read by ID\n\
                 {\"filename\": \"main.rs\"} - Read by filename\n\
                 {\"path\": \"src/main.rs\"} - Read by path",
            );
        };

        // Keep user-friendly path for error messages
        let display_path = resolved_path.clone();

        // Path traversal protection.
        let path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            };

        let offset = args
            .get("offset")
            .and_then(|o| o.as_u64())
            .map(|o| o.saturating_sub(1) as usize); // 1-based to 0-based
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize)
            .or(Some(200)); // Default: 200 lines
        
        // Get encoding parameter
        let encoding = args
            .get("encoding")
            .and_then(|e| e.as_str())
            .map(|e| match e.to_lowercase().as_str() {
                "gbk" | "gb2312" => encoding_rs::GBK,
                "gb18030" => encoding_rs::GB18030,
                "utf-16le" => encoding_rs::UTF_16LE,
                "utf-16be" => encoding_rs::UTF_16BE,
                "latin1" | "iso-8859-1" => encoding_rs::WINDOWS_1252,
                _ => encoding_rs::UTF_8,
            });

        // Read file with encoding support
        match read_file_with_encoding(&path, encoding) {
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
            Err(e) => ToolOutput::error(format!("Failed to read {}: {e}", display_path.display())),
        }
    }
}

/// Read file with encoding support
/// - Uses BufReader for performance
/// - Supports GBK, GB18030, UTF-16, Latin1, etc.
/// - Falls back to UTF-8 if encoding not specified
fn read_file_with_encoding(path: &std::path::Path, encoding: Option<&'static Encoding>) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let reader = BufReader::new(file);

    // Read raw bytes to handle non-UTF-8 encodings
    let bytes: Vec<u8> = reader.bytes()
        .filter_map(|b| b.ok())
        .collect();
    
    // Decode using specified encoding or default to UTF-8
    let (cow, _encoding_used, had_errors) = match encoding {
        Some(enc) => enc.decode(&bytes),
        None => encoding_rs::UTF_8.decode(&bytes),
    };
    
    if had_errors {
        tracing::warn!(
            "File {} may have encoding issues. Some characters might be replaced.",
            path.display()
        );
    }
    
    Ok(cow.into_owned())
}
