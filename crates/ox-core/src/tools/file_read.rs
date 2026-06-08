use encoding_rs::Encoding;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufReader, Read};
use std::sync::Arc;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileReadTool;

#[async_trait::async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read file contents with line numbers. Use to inspect code, configs, or docs before editing. Returns formatted output with 1-based line numbers (e.g. '  42→fn main()')."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace root)."
                },
                "offset": {
                    "type": "integer",
                    "description": "0-based line offset to start reading from. Default: 0."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to read. Default: 200. Set higher for full-file reads."
                },
                "encoding": {
                    "type": "string",
                    "description": "File encoding. Options: 'utf-8' (default), 'gbk', 'gb18030', 'utf-16le', 'utf-16be', 'latin1'. Auto-detected if not specified.",
                    "enum": ["utf-8", "gbk", "gb18030", "utf-16le", "utf-16be", "latin1"]
                }
            },
            "required": ["path"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // ── Resolve path (path-only) ──
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) if !p.is_empty() => p.trim().replace('\\', "/"),
            _ => return ToolOutput::error(
                "❌ Missing or empty 'path' parameter.\nUsage: {\"path\": \"src/main.rs\"}",
            ),
        };
        let resolved_path = if std::path::Path::new(&path_str).is_absolute() {
            std::path::PathBuf::from(&path_str)
        } else {
            ctx.working_dir.join(&path_str)
        };
        let display_path = resolved_path.clone();

        let path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("Path validation failed: {e}")),
            };

        let offset = args
            .get("offset")
            .and_then(|o| o.as_u64())
            .map(|o| o as usize);
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
                // Auto-index symbols in background (use absolute path)
                let abs_path = path.to_path_buf();
                let code_indexer = Arc::clone(&ctx.code_indexer);
                tokio::spawn(async move {
                    let mut idx = code_indexer.lock().await;
                    if let Err(e) = idx.index_file(&abs_path).await {
                        tracing::debug!("[FILE_READ] Auto-index failed for {}: {e}", abs_path.display());
                    }
                });

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
