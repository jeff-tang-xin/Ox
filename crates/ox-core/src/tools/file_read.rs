use encoding_rs::Encoding;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

/// Files smaller than this on disk are read fully; tool results below this stay inline (no ref).
pub const SMALL_FILE_THRESHOLD: u64 = 512 * 1024;
pub const INLINE_CONTENT_THRESHOLD: usize = SMALL_FILE_THRESHOLD as usize;

/// Read a line slice from a workspace-relative path (shared by tool + exploration cache).
pub fn read_file_slice(
    working_dir: &std::path::Path,
    path_str: &str,
    offset: usize,
    limit: usize,
) -> Result<String, String> {
    let path_str = path_str.trim().replace('\\', "/");
    let resolved_path = if std::path::Path::new(&path_str).is_absolute() {
        std::path::PathBuf::from(&path_str)
    } else {
        working_dir.join(&path_str)
    };

    let path = match crate::safety::validate_path_within_workdir(&resolved_path, working_dir) {
        Ok(p) => p,
        Err(e) => return Err(format!("Path validation failed: {e}")),
    };

    let file_size = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) => return Err(format!("Cannot access file: {e}")),
    };

    let (content, total_lines) = if file_size < SMALL_FILE_THRESHOLD {
        read_full_then_slice(&path, offset, limit)?
    } else {
        stream_read_lines(&path, offset, limit)?
    };

    Ok(format_read_output(
        &path_str,
        content,
        offset,
        limit,
        total_lines,
    ))
}

fn format_read_output(
    path_str: &str,
    content: String,
    offset: usize,
    limit: usize,
    total_lines: usize,
) -> String {
    let shown = content.matches('\n').count() + if content.is_empty() { 0 } else { 1 };
    let mut output = content;
    if total_lines > 0 {
        output.push_str(&format!(
            "\n\n📄 {} lines total (showing {}-{})",
            total_lines,
            offset + 1,
            (offset + shown).min(total_lines)
        ));
        if offset + shown < total_lines {
            output.push_str(&format!(
                "\n💡 未读完。续读: file_read {{\"path\":\"{}\", \"offset\":{}, \"limit\":{}}}",
                path_str,
                offset + shown,
                limit
            ));
        }
    } else if shown == limit {
        output.push_str(&format!(
            "\n\n📄 showing {} lines starting at line {} (large file, total unknown)",
            shown,
            offset + 1
        ));
        output.push_str(&format!(
            "\n💡 可能还有更多。续读: file_read {{\"path\":\"{}\", \"offset\":{}, \"limit\":{}}}",
            path_str,
            offset + shown,
            limit
        ));
    }
    output
}

pub struct FileReadTool;

#[async_trait::async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read file contents with line numbers. Default: 200 lines from offset 0. \
         Large files are NOT read in full — use offset/limit to paginate (e.g. offset=200, limit=200 for next page)."
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
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) if !p.is_empty() => p.trim().replace('\\', "/"),
            _ => {
                return ToolOutput::error(
                    "❌ Missing or empty 'path' parameter.\nUsage: {\"path\": \"src/main.rs\"}",
                );
            }
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
            .map(|o| o as usize)
            .unwrap_or(0);
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize)
            .unwrap_or(200);

        // Get encoding parameter
        let encoding = args.get("encoding").and_then(|e| e.as_str()).map(|e| {
            match e.to_lowercase().as_str() {
                "gbk" | "gb2312" => encoding_rs::GBK,
                "gb18030" => encoding_rs::GB18030,
                "utf-16le" => encoding_rs::UTF_16LE,
                "utf-16be" => encoding_rs::UTF_16BE,
                "latin1" | "iso-8859-1" => encoding_rs::WINDOWS_1252,
                _ => encoding_rs::UTF_8,
            }
        });

        // Check file size — small files read fully, large files stream
        let file_size = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) => return ToolOutput::error(format!("Cannot access file: {e}")),
        };

        let result = if file_size < SMALL_FILE_THRESHOLD && encoding.is_none() {
            read_full_then_slice(&path, offset, limit)
        } else if encoding.is_some() {
            read_with_encoding_then_slice(&path, encoding, offset, limit)
        } else {
            stream_read_lines(&path, offset, limit)
        };

        match result {
            Ok((content, total_lines)) => {
                let abs_path = path.to_path_buf();
                if let Some(ref knowledge) = ctx.knowledge {
                    let knowledge = knowledge.clone();
                    tokio::spawn(async move {
                        if let Ok(mut engine) = knowledge.try_write() {
                            if let Err(e) = engine.index_file(&abs_path) {
                                tracing::debug!(
                                    "[FILE_READ] Auto-index failed for {}: {e}",
                                    abs_path.display()
                                );
                            }
                        }
                    });
                }

                let output = format_read_output(&path_str, content, offset, limit, total_lines);
                ToolOutput::success(output)
            }
            Err(e) => ToolOutput::error(format!("Failed to read {}: {e}", display_path.display())),
        }
    }
}

/// Small file or unknown encoding: read entire file, decode, then slice lines.
fn read_full_then_slice(
    path: &std::path::Path,
    offset: usize,
    limit: usize,
) -> Result<(String, usize), String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("Cannot read file: {e}"))?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = offset.min(total);
    let end = (start + limit).min(total);
    let formatted: Vec<String> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}\t{line}", start + i + 1))
        .collect();
    Ok((formatted.join("\n"), total))
}

/// Explicit encoding: read raw bytes, decode, then slice lines.
fn read_with_encoding_then_slice(
    path: &std::path::Path,
    encoding: Option<&'static Encoding>,
    offset: usize,
    limit: usize,
) -> Result<(String, usize), String> {
    let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Read error: {e}"))?;

    let (cow, _enc, had_errors) = match encoding {
        Some(enc) => enc.decode(&bytes),
        None => encoding_rs::UTF_8.decode(&bytes),
    };
    if had_errors {
        tracing::warn!("File {} may have encoding issues.", path.display());
    }
    let content = cow.into_owned();
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = offset.min(total);
    let end = (start + limit).min(total);
    let formatted: Vec<String> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}\t{line}", start + i + 1))
        .collect();
    Ok((formatted.join("\n"), total))
}

/// Large UTF-8 file: stream-read only the needed lines using BufRead.
/// Does NOT load the entire file into memory.
fn stream_read_lines(
    path: &std::path::Path,
    offset: usize,
    limit: usize,
) -> Result<(String, usize), String> {
    let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let reader = BufReader::new(file);

    let mut formatted = Vec::with_capacity(limit.min(500));
    let mut line_num: usize = 0;

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| format!("Read error at line {}: {e}", line_num + 1))?;

        if line_num >= offset && (line_num - offset) < limit {
            formatted.push(format!("{:>4}\t{line}", line_num + 1));
        }
        line_num += 1;

        // Stop reading once we've captured the requested range
        if line_num >= offset + limit {
            break; // Don't scan rest of file for total count — too expensive for large files
        }
    }

    // For large files, we may not know the exact total — show what we know
    let total_lines = if line_num < offset + limit {
        line_num
    } else {
        0 // 0 means "unknown total" for large files
    };

    Ok((formatted.join("\n"), total_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_with_absolute_windows_path() {
        let dir = std::env::temp_dir().join("ox_test_file_read");
        std::fs::create_dir_all(&dir).unwrap();
        let fp = dir.join("Test.java");
        let mut f = std::fs::File::create(&fp).unwrap();
        for i in 1..=150 {
            writeln!(f, "line {}", i).unwrap();
        }
        drop(f);
        let abs = fp.to_string_lossy().replace('\\', "/");
        let r = read_file_slice(&dir, &abs, 74, 30);
        assert!(r.is_ok(), "fail: {:?}", r.err());
        assert!(r.unwrap().contains("line 75"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
