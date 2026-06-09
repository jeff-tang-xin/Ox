use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct FileWriteTool;

#[async_trait::async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file. Use for new files or complete rewrites (>50% changed). For targeted edits, use edit_file.\n\n\
         You MUST provide the complete relative path (e.g. 'src/utils/helper.rs'), not just a filename."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Complete relative file path, including directory (e.g. 'src/main.rs', 'docs/guide.md')."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write."
                },
                "encoding": {
                    "type": "string",
                    "description": "File encoding: 'utf-8' (default), 'gbk', 'gb18030', 'utf-16le', 'utf-16be', 'latin1'.",
                    "enum": ["utf-8", "gbk", "gb18030", "utf-16le", "utf-16be", "latin1"]
                }
            },
            "required": ["path", "content"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // ── Resolve path (path-only) ──
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) if !p.is_empty() => p.trim().replace('\\', "/"),
            _ => return ToolOutput::error(
                "❌ Missing or empty 'path' parameter.\nUsage: {\"path\": \"src/output.rs\", \"content\": \"...\"}",
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
                Err(e) => {
                    return ToolOutput::error(format!(
                        "❌ Security Error: {}\n\nWorking directory: {}",
                        e,
                        ctx.working_dir.display()
                    ));
                }
            };

        // Validate path for platform-specific invalid characters
        let path_str = path.to_string_lossy();
        
        // 🚨 WORKFLOW VALIDATION: Check if file is being created in correct location during Spec Mode
        // When in workflow mode, files should be in .ox/{requirement_name}/ not directly in .ox/
        let relative_path = path.strip_prefix(&ctx.working_dir).unwrap_or(&path);
        let rel_str = relative_path.to_string_lossy();
        
        // Check if file is being written directly to .ox/ without subdirectory
        // Pattern: ".ox/something.md" (wrong) vs ".ox/name/something.md" (correct)
        if rel_str.starts_with(".ox/") {
            let after_ox = &rel_str[4..]; // Remove ".ox/"
            if !after_ox.contains('/') && !after_ox.contains('\\') {
                // File is directly in .ox/ (e.g., .ox/spec.md) - NO subdirectory
                tracing::warn!(
                    "[FILE_WRITE] ⚠️  WARNING: File being written directly to .ox/: {}\n                         This violates Spec Mode requirements!\n                         Files MUST be in .ox/{{requirement_name}}/ subdirectory.\n                         Example: .ox/order-optimization/spec.md",
                    rel_str
                );
            }
        }
        
        if cfg!(windows) {
            // Strip Windows UNC prefix if present (\\?\ or \\?\UNC\)
            let clean_path = if path_str.starts_with("\\\\?\\") {
                &path_str[4..] // Remove "\\?\" prefix
            } else {
                path_str.as_ref()
            };

            // Check for invalid characters, but allow ':' in drive letter position (e.g., C:)
            // Invalid chars: < > " | ? *
            // Exception: ':' is allowed at position 1 for drive letters (C:, D:, etc.)
            for (i, c) in clean_path.char_indices() {
                match c {
                    '<' | '>' | '"' | '|' | '?' | '*' => {
                        return ToolOutput::error(format!(
                            "❌ Invalid Path Character: '{}' is not allowed in Windows filenames\n\n\
                             💡 Problem: {}\n\
                             🔧 Solution: Remove or replace the invalid character\n\n\
                             📝 Valid example: output.txt\n\
                             ❌ Invalid example: output<1>.txt",
                            c,
                            display_path.display()
                        ));
                    }
                    ':' => {
                        // ':' is only valid at position 1 (drive letter separator)
                        if i != 1 {
                            return ToolOutput::error(format!(
                                "❌ Invalid Path Character: ':' is not allowed in Windows filenames (except for drive letter)\n\n\
                                 💡 Problem: {} contains ':' at position {}\n\
                                 🔧 Solution: Use a valid path like 'C:\\path\\file.txt'",
                                display_path.display(),
                                i
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        // Warn about deeply nested paths (>10 levels)
        let depth = path.components().count();
        if depth > 10 {
            tracing::warn!(
                "[FILE_WRITE] Deeply nested path ({} levels): {}",
                depth,
                display_path.display()
            );
        }

        // Get content and determine write strategy
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => {
                return ToolOutput::error(
                    "❌ Missing Required Parameter: 'content'\n\n\
                 💡 How to fix:\n\
                 • Add the 'content' parameter with the file content\n\
                 • Content should be a string (escape special characters)\n\n\
                 📝 Example: {\"path\": \"hello.txt\", \"content\": \"Hello, World!\\nSecond line\"}",
                );
            }
        };

        // Get encoding parameter and convert content to bytes
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
        
        let content_bytes = match encoding {
            Some(enc) => {
                let (bytes, _encoding_used, had_errors) = enc.encode(content);
                if had_errors {
                    tracing::warn!(
                        "Some characters could not be encoded in {:?}, they will be replaced",
                        enc.name()
                    );
                }
                bytes.into_owned()
            }
            None => content.as_bytes().to_vec(),
        };

        // Auto-detect large files and use chunked writing (>1 MB)
        const AUTO_CHUNK_THRESHOLD: usize = 1 * 1024 * 1024; // 1 MB
        const CHUNK_SIZE: usize = 512 * 1024; // 512 KB per chunk

        let is_large_file = content_bytes.len() > AUTO_CHUNK_THRESHOLD;
        if is_large_file {
            tracing::info!(
                "[FILE_WRITE] Large file detected ({:.2} MB), using chunked write strategy",
                content_bytes.len() as f64 / 1024.0 / 1024.0
            );
        }

        // Validate content quality using shared validation logic
        if let Err(e) = content_validation::validate_content(content) {
            return ToolOutput::error(e);
        }

        // Create parent directories.
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            return ToolOutput::error(format!(
                "❌ Directory Creation Failed: Cannot create {}\n\n\
                     💡 Error: {}\n\
                     🔍 Possible causes:\n\
                     • Insufficient permissions\n\
                     • Disk is full\n\
                     • Path contains invalid characters",
                parent.display(),
                e
            ));
        }

        // Report progress before blocking I/O
        tracing::info!("[FILE_WRITE] Starting write operation for: {:?}", display_path);
        ctx.report_progress("Starting file write...".to_string(), Some(10));

        // Write file with automatic strategy selection
        let temp_path = create_temp_path(&path);
        
        // Run blocking file I/O on a dedicated thread to avoid blocking the Tokio runtime.
        let path_clone = path.clone();
        let display_path_clone = display_path.clone();
        let content_bytes_clone = content_bytes.to_vec();
        let is_large_file_clone = is_large_file;
        let chunk_size = CHUNK_SIZE;
        let temp_path_clone = temp_path.clone();
        
        tracing::info!("[FILE_WRITE] Spawning blocking task for: {:?}", display_path_clone);
        let result = tokio::task::spawn_blocking(move || {
            tracing::info!("[FILE_WRITE] Blocking task started, writing file: {:?}", path_clone);
            
            // Execute the write operation (blocking I/O)
            if is_large_file_clone {
                chunked_write_sync(&temp_path_clone, &path_clone, &content_bytes_clone, chunk_size)
            } else {
                atomic_write_sync(&temp_path_clone, &path_clone, &content_bytes_clone)
            }
        }).await;
        
        // Handle spawn_blocking result
        let final_result = match result {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Write task failed: {e}")),
        };

        match final_result {
            Ok(bytes_written) => {
                let size_info = if is_large_file {
                    format!(
                        "\n📦 Strategy: Chunked write ({} chunks of {} KB)",
                        (content_bytes.len() + CHUNK_SIZE - 1) / CHUNK_SIZE,
                        CHUNK_SIZE / 1024
                    )
                } else {
                    String::new()
                };
                
                let encoding_name = match encoding {
                    Some(enc) => enc.name(),
                    None => "UTF-8",
                };

                // ── AST syntax check: parse the written file and warn on errors ──
                let ast_warning = {
                    let code_indexer = Arc::clone(&ctx.code_indexer);
                    let check_path = display_path.clone();
                    tokio::spawn(async move {
                        let mut idx = code_indexer.lock().await;
                        // Re-read the written file for syntax check
                        if let Ok(code) = std::fs::read_to_string(&check_path) {
                            idx.check_syntax(&check_path, &code)
                        } else {
                            None
                        }
                    }).await
                };
                let ast_warning = match ast_warning {
                    Ok(Some(errors)) => {
                        let mut warn = format!("\n\n⚠️ AST Syntax Check: {} issue(s) detected:", errors.len());
                        for (i, err) in errors.iter().take(5).enumerate() {
                            warn.push_str(&format!("\n   {}. {}", i + 1, err.description));
                        }
                        if errors.len() > 5 {
                            warn.push_str(&format!("\n   ... and {} more", errors.len() - 5));
                        }
                        warn.push_str("\n   💡 Fix syntax errors before proceeding.");
                        warn
                    }
                    _ => String::new(),
                };

                ToolOutput::success(format!(
                    "✅ Successfully written {} bytes to {}{}\n\
                     📄 Encoding: {}{}",
                    bytes_written,
                    display_path.display(),
                    size_info,
                    encoding_name,
                    ast_warning
                ))
            }
            Err(e) => {
                // Clean up temp file if it exists
                let _ = fs::remove_file(&temp_path);

                ToolOutput::error(format!(
                    "❌ File Write Failed: {}\n\n\
                     💡 Path: {}\n\
                     🔍 Common solutions:\n\
                     • Check disk space: 'df -h' (Linux/Mac) or check Properties (Windows)\n\
                     • Verify write permissions for the directory\n\
                     • Close any programs that might have the file open\n\
                     • Try writing to a different location",
                    e,
                    display_path.display()
                ))
            }
        }
    }
}

/// Create a temporary file path in the same directory as the target
fn create_temp_path(target: &std::path::Path) -> PathBuf {
    let mut temp = target.to_path_buf();
    let file_name = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Add .tmp extension and random suffix to avoid conflicts
    let temp_name = format!("{}.tmp.{}", file_name, std::process::id());
    temp.set_file_name(temp_name);
    temp
}

/// Atomically write content to a file using temp file + rename strategy (synchronous version)
/// This ensures the target file is never left in a corrupted state
fn atomic_write_sync(
    temp_path: &PathBuf,
    target: &std::path::Path,
    content: &[u8],
) -> Result<usize, String> {
    // Step 1: Write to temp file
    let mut file =
        fs::File::create(temp_path).map_err(|e| format!("Cannot create temporary file: {}", e))?;

    file.write_all(content)
        .map_err(|e| format!("Failed to write data: {}", e))?;

    // Flush to ensure data is written to disk
    file.flush()
        .map_err(|e| format!("Failed to flush data: {}", e))?;

    // Sync to ensure data is physically on disk (not just in OS cache)
    file.sync_all()
        .map_err(|e| format!("Failed to sync to disk: {}", e))?;

    drop(file); // Close the file before renaming

    let bytes_written = content.len();

    // Step 2: Atomic rename (on most filesystems, rename is atomic)
    fs::rename(temp_path, target).map_err(|e| format!("Failed to finalize file: {}", e))?;

    Ok(bytes_written)
}

/// Synchronous chunked write (for use inside spawn_blocking).
/// Writes large files in chunks to avoid memory pressure.
fn chunked_write_sync(
    temp_path: &PathBuf,
    target: &std::path::Path,
    content: &[u8],
    chunk_size: usize,
) -> Result<usize, String> {
    let total_bytes = content.len();
    let mut file = fs::File::create(temp_path)
        .map_err(|e| format!("Cannot create temporary file: {}", e))?;

    let mut offset = 0;
    while offset < total_bytes {
        let end = std::cmp::min(offset + chunk_size, total_bytes);
        let chunk = &content[offset..end];
        file.write_all(chunk)
            .map_err(|e| format!("Failed to write chunk at offset {}: {}", offset, e))?;
        offset = end;
    }

    file.flush()
        .map_err(|e| format!("Failed to flush data: {}", e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to sync to disk: {}", e))?;
    drop(file);

    fs::rename(temp_path, target)
        .map_err(|e| format!("Failed to finalize file: {}", e))?;

    Ok(total_bytes)
}

