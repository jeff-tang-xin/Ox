use base64::Engine;
use encoding_rs::Encoding;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

/// Files smaller than this on disk are read fully; tool results below this stay inline (no ref).
pub const SMALL_FILE_THRESHOLD: u64 = 512 * 1024;
pub const INLINE_CONTENT_THRESHOLD: usize = SMALL_FILE_THRESHOLD as usize;

/// Max image size for inline Base64 encoding. Larger images return a note instead.
pub const IMAGE_MAX_SIZE: u64 = 256 * 1024;

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "tiff", "tif", "avif", "heic",
];

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_lowercase();
            IMAGE_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Max document (PDF/Excel/ODS) size for parsing. Larger files return a note.
pub const DOCUMENT_MAX_SIZE: u64 = 10 * 1024 * 1024;

/// Max rows extracted per spreadsheet worksheet (context-window guard).
pub const SPREADSHEET_MAX_ROWS: usize = 500;

const PDF_EXTENSIONS: &[&str] = &["pdf"];
const SPREADSHEET_EXTENSIONS: &[&str] = &["xlsx", "xls", "xlsm", "ods"];

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_pdf_path(path: &Path) -> bool {
    has_extension(path, PDF_EXTENSIONS)
}

fn is_spreadsheet_path(path: &Path) -> bool {
    has_extension(path, SPREADSHEET_EXTENSIONS)
}

fn image_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("avif") => "image/avif",
        Some("tiff") | Some("tif") => "image/tiff",
        _ => "application/octet-stream",
    }
}

fn read_image_as_base64(path: &Path, file_size: u64) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let mime = image_mime(path);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let kb = file_size / 1024;
    Ok(format!(
        "🖼️ 图片读取 (type={mime}, size={kb}KB)\n\n\
         ```\n\
         data:{mime};base64,{b64}\n\
         ```"
    ))
}

/// Extract plain text from a PDF file.
fn read_pdf_text(path: &Path) -> Result<String, String> {
    let text =
        pdf_extract::extract_text(path).map_err(|e| format!("Cannot extract PDF text: {e}"))?;
    if text.trim().is_empty() {
        return Ok("📄 PDF 已解析，但未提取到文本（可能是扫描件/纯图片 PDF）。".to_string());
    }
    Ok(format!("📄 PDF 文本提取\n\n{text}"))
}

/// Extract text from a spreadsheet (xlsx/xls/xlsm/ods) as tab-separated tables.
fn read_spreadsheet_text(path: &Path) -> Result<String, String> {
    use calamine::{Data, Reader, open_workbook_auto};
    let mut workbook =
        open_workbook_auto(path).map_err(|e| format!("Cannot open spreadsheet: {e}"))?;
    let sheet_names = workbook.sheet_names().to_owned();
    if sheet_names.is_empty() {
        return Ok("📊 表格已打开，但没有任何工作表。".to_string());
    }
    let mut out = String::new();
    for name in &sheet_names {
        let range = match workbook.worksheet_range(name) {
            Ok(r) => r,
            Err(e) => {
                out.push_str(&format!("\n## Sheet: {name}\n⚠️ 读取失败: {e}\n"));
                continue;
            }
        };
        let total_rows = range.rows().count();
        out.push_str(&format!("\n## Sheet: {name} ({total_rows} rows)\n"));
        for (i, row) in range.rows().enumerate() {
            if i >= SPREADSHEET_MAX_ROWS {
                out.push_str(&format!(
                    "… (已截断，仅显示前 {SPREADSHEET_MAX_ROWS} 行 / 共 {total_rows} 行)\n"
                ));
                break;
            }
            let cells: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Data::Empty => String::new(),
                    other => other.to_string(),
                })
                .collect();
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
    }
    Ok(format!("📊 表格文本提取{out}"))
}

/// Dispatch a PDF/spreadsheet path to the right parser, with size +guards.
fn read_document(path: &Path, file_size: u64) -> ToolOutput {
    if file_size > DOCUMENT_MAX_SIZE {
        let mb = DOCUMENT_MAX_SIZE / (1024 * 1024);
        return ToolOutput::success(format!(
            "📄 文档过大（{}KB），超过 {mb}MB 解析上限，未解析。",
            file_size / 1024,
        ));
    }
    let parsed = if is_pdf_path(path) {
        read_pdf_text(path)
    } else {
        read_spreadsheet_text(path)
    };
    match parsed {
        Ok(text) => {
            let capped = if text.chars().count() > MAX_READ_OUTPUT_CHARS {
                let cut: String = text.chars().take(MAX_READ_OUTPUT_CHARS).collect();
                format!("{cut}\n\n⚠️ 内容过大，已截断（前 {MAX_READ_OUTPUT_CHARS} 字符）。")
            } else {
                text
            };
            ToolOutput::success(capped)
        }
        Err(e) => ToolOutput::error(e),
    }
}

/// Hard character cap on a single file_read tool result. The line-based
/// `limit` param does NOT bound output size: a minified file (e.g. a bundled
/// `index.js` compressed onto ONE line) is a single "line" that can be
/// hundreds of KB. Reading it dumps the whole file into one tool result,
/// which — accumulated across a turn — overflows the model's context window
/// and makes the API reject the request (ARK returns `InvalidParameter` 400).
/// This cap is the byte/char-level backstop that the `limit` param is not.
pub const MAX_READ_OUTPUT_CHARS: usize = 60_000;

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

    // Char-level backstop: the line-based `limit` cannot bound a minified file
    // where the whole content is one giant line. Truncate on a char boundary so
    // a single read can never blow the context window / trip an API 400.
    let mut truncated_note = String::new();
    if output.chars().count() > MAX_READ_OUTPUT_CHARS {
        let total_chars = output.chars().count();
        let cut: String = output.chars().take(MAX_READ_OUTPUT_CHARS).collect();
        output = cut;
        truncated_note = format!(
            "\n\n⚠️ 内容过大，已截断（显示前 {} / 共 {} 字符）。\
             \n💡 此文件可能是压缩/单行文件（如 minified JS）。如需特定片段，用 code_search 定位，\
             或用较小的 limit 分页续读: file_read {{\"path\":\"{}\", \"offset\":{}, \"limit\":{}}}",
            MAX_READ_OUTPUT_CHARS,
            total_chars,
            path_str,
            offset + shown,
            limit
        );
    }

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
    output.push_str(&truncated_note);
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
         Large files are NOT read in full — use offset/limit to paginate (e.g. offset=200, limit=200 for next page). \
         Image files (png/jpg/gif/webp/bmp/svg/...) under 256KB are auto-returned as Base64 data URIs. \
         PDF and spreadsheet files (pdf/xlsx/xls/xlsm/ods) under 10MB are auto-parsed to plain text."
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

        // Image detection: auto Base64 for images ≤ IMAGE_MAX_SIZE
        if is_image_path(&path) {
            if file_size <= IMAGE_MAX_SIZE {
                match read_image_as_base64(&path, file_size) {
                    Ok(output) => return ToolOutput::success(output),
                    Err(e) => return ToolOutput::error(e),
                }
            } else {
                let mime = image_mime(&path);
                let kb = file_size / 1024;
                return ToolOutput::success(format!(
                    "🖼️ 图片读取 (type={mime}, size={kb}KB)\n\n\
                     ⚠️ 图片超过 {img_max}KB 限制，无法内嵌 Base64。\n\
                     💡 请使用外部工具查看，或压缩后重试。",
                    img_max = IMAGE_MAX_SIZE / 1024,
                ));
            }
        }

        if is_pdf_path(&path) || is_spreadsheet_path(&path) {
            return read_document(&path, file_size);
        }

        let result = if file_size < SMALL_FILE_THRESHOLD && encoding.is_none() {
            read_full_then_slice(&path, offset, limit)
        } else if encoding.is_some() {
            read_with_encoding_then_slice(&path, encoding, offset, limit)
        } else {
            stream_read_lines(&path, offset, limit)
        };

        match result {
            Ok((content, total_lines)) => {
                let _ = path.to_path_buf();
                // KnowledgeEngine auto-index removed (embedding disabled)

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

    #[test]
    fn minified_single_line_file_is_truncated() {
        // A minified file is one giant line — the line-based `limit` doesn't
        // bound it, so the char-level cap must kick in.
        let dir = std::env::temp_dir().join("ox_test_file_read_minified");
        std::fs::create_dir_all(&dir).unwrap();
        let fp = dir.join("index.js");
        let mut f = std::fs::File::create(&fp).unwrap();
        // One line, well over the char cap.
        let giant = "a".repeat(MAX_READ_OUTPUT_CHARS * 2);
        write!(f, "!function(){{{giant}}}();").unwrap();
        drop(f);
        let abs = fp.to_string_lossy().replace('\\', "/");
        let out = read_file_slice(&dir, &abs, 0, 200).unwrap();
        assert!(
            out.chars().count() < MAX_READ_OUTPUT_CHARS + 2000,
            "output must be capped, got {} chars",
            out.chars().count()
        );
        assert!(out.contains("已截断"), "must include truncation notice");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
