use encoding_rs::Encoding;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct FileWriteTool;

#[async_trait::async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file. Use ONLY for new files or complete rewrites (>50% changed). For small edits, use file_patch.\n\n\
         ⚠️ CRITICAL: You MUST provide the 'path' parameter with a COMPLETE file path:\n\
         • For NEW files: Always specify full relative path (e.g., 'src/output.txt', 'docs/readme.md')\n\
         • For EXISTING files: Can use 'path', 'filename', or 'file_id'\n\n\
         💡 IMPORTANT: Large files (>1 MB) are automatically written in chunks - you can provide the full content without worrying about size limits!\n\n\
         💡 Examples:\n\
         - New file: {\"path\": \"src/utils/helper.rs\", \"content\": \"...\"}\n\
         - Existing: {\"filename\": \"main.rs\", \"content\": \"...\"}"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "⚠️ ALWAYS REQUIRED for new files: Complete relative path including directories (e.g., 'src/main.rs', 'docs/guide.md'). For existing files, can also use filename or file_id instead."
                },
                "filename": {
                    "type": "string",
                    "description": "Alternative for EXISTING files only: Search by filename in index. NOT recommended for new files (use 'path' instead)."
                },
                "file_id": {
                    "type": "integer",
                    "description": "Alternative for EXISTING files only: Precise file ID from index. Use file_list to get IDs. Cannot be used for new files."
                },
                "content": {
                    "type": "string",
                    "description": "✅ REQUIRED: The content to write to the file. Large files (>1 MB) will be automatically written in chunks."
                },
                "encoding": {
                    "type": "string",
                    "description": "File encoding for writing. Options: 'utf-8' (default), 'gbk', 'gb18030', 'utf-16le', 'utf-16be', 'latin1'. Default is UTF-8.",
                    "enum": ["utf-8", "gbk", "gb18030", "utf-16le", "utf-16be", "latin1"]
                }
            },
            "required": ["content"],
            "oneOf": [
                {"required": ["path"]},
                {"required": ["filename"]},
                {"required": ["file_id"]}
            ],
            "examples": [
                {"path": "src/new_file.rs", "content": "// New file with full path"},
                {"path": "docs/tutorial.md", "content": "# Tutorial"},
                {"filename": "existing.rs", "content": "// Modifying existing file"},
                {"path": "legacy.txt", "content": "中文内容", "encoding": "gbk"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::RequiresConfirmation
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
                    // File not in index - treat as new file creation
                    ctx.working_dir.join(filename)
                }
                Err(e) => return ToolOutput::error(format!("Failed to query file index: {}", e)),
            }
        } else if let Some(raw_path) = args.get("path").and_then(|p| p.as_str()) {
            // Method 3: Traditional path-based approach (backward compatible)
            if raw_path.is_empty() {
                return ToolOutput::error(
                    "❌ Parameter Error: 'path' cannot be empty\n\n\
                     💡 Example: {\"path\": \"output.txt\", \"content\": \"Hello World\"}",
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
                "❌ CRITICAL ERROR: Missing 'path' parameter for file_write!\n\n\
                 💡 For NEW files, you MUST provide a COMPLETE path:\n\
                 • Include directory structure (e.g., 'src/utils/helper.rs')\n\
                 • NOT just filename (e.g., 'helper.rs' is WRONG)\n\n\
                 📝 Correct Examples:\n\
                 {\"path\": \"src/main.rs\", \"content\": \"...\"}\n\
                 {\"path\": \"docs/guide.md\", \"content\": \"...\"}\n\
                 {\"path\": \"tests/unit_test.rs\", \"content\": \"...\"}\n\n\
                 ❌ Wrong Example:\n\
                 {\"content\": \"...\"} ← NO PATH PROVIDED!\n\
                 {\"filename\": \"main.rs\"} ← Only works for EXISTING files!"
            );
        };

        // Keep user-friendly path for error messages
        let display_path = resolved_path.clone();

        let path =
            match crate::safety::validate_path_within_workdir(&resolved_path, &ctx.working_dir) {
                Ok(p) => p,
                Err(e) => {
                    return ToolOutput::error(format!(
                        "❌ Security Error: {}\n\n\
                     💡 The file path must be within the working directory:\n\
                     {}",
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
        let working_dir = ctx.working_dir.clone();
        let file_index = Arc::clone(&ctx.file_index);
        let is_large_file_clone = is_large_file;
        let chunk_size = CHUNK_SIZE;
        let temp_path_clone = temp_path.clone(); // Clone for spawn_blocking
        
        tracing::info!("[FILE_WRITE] Spawning blocking task for: {:?}", display_path_clone);
        let result = tokio::task::spawn_blocking(move || {
            tracing::info!("[FILE_WRITE] Blocking task started, writing file: {:?}", path_clone);
            
            // Execute the write operation (blocking I/O)
            let write_result = if is_large_file_clone {
                // For large files, use chunked write
                chunked_write_sync(&temp_path_clone, &path_clone, &content_bytes_clone, chunk_size)
            } else {
                // For normal files, use atomic write
                atomic_write_sync(&temp_path_clone, &path_clone, &content_bytes_clone)
            };
            
            // Update file index if successful
            if write_result.is_ok() {
                if let Ok(relative_path) = path_clone.strip_prefix(&working_dir) {
                    let rel_str = relative_path.to_string_lossy();
                    if let Err(e) = file_index.add_file(&rel_str) {
                        tracing::warn!("Failed to update file index: {}", e);
                    }
                }
            }
            
            write_result
        }).await;
        
        // Handle spawn_blocking result
        let final_result = match result {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Write task failed: {e}")),
        };

        match final_result {
            Ok(bytes_written) => {
                // Update file index immediately for real-time availability
                if let Ok(relative_path) = path.strip_prefix(&ctx.working_dir) {
                    let rel_str = relative_path.to_string_lossy();
                    if let Err(e) = ctx.file_index.add_file(&rel_str) {
                        tracing::warn!("Failed to update file index: {}", e);
                    }
                }

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

                ToolOutput::success(format!(
                    "✅ Successfully written {} bytes to {}{}\n\
                     📄 Encoding: {}\n\
                     💡 Tip: Use 'file_read' to verify the content",
                    bytes_written,
                    display_path.display(),
                    size_info,
                    encoding_name
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

/// Atomically write with retry mechanism for transient failures
async fn atomic_write_with_retry(
    temp_path: &PathBuf,
    target: &std::path::Path,
    content: &[u8],
    max_retries: u32,
) -> Result<usize, String> {
    let mut last_error = String::new();

    for attempt in 1..=max_retries {
        match atomic_write_sync(temp_path, target, content) {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                last_error = e.clone();

                // Check if error is retryable
                if is_retryable_error(&e) && attempt < max_retries {
                    let delay = Duration::from_millis(100 * attempt as u64); // Exponential backoff
                    tracing::warn!(
                        "[FILE_WRITE] Attempt {} failed, retrying in {:?}: {}",
                        attempt,
                        delay,
                        e
                    );
                    tokio::time::sleep(delay).await;
                } else {
                    break;
                }
            }
        }
    }

    Err(format!(
        "Failed after {} attempts: {}",
        max_retries, last_error
    ))
}

/// Synchronous chunked write (for use inside spawn_blocking)
fn chunked_write_sync(
    temp_path: &PathBuf,
    target: &std::path::Path,
    content: &[u8],
    chunk_size: usize,
) -> Result<usize, String> {
    let total_bytes = content.len();
    
    // Create temp file
    let mut file = fs::File::create(temp_path)
        .map_err(|e| format!("Cannot create temporary file: {}", e))?;

    // Write in chunks
    let mut offset = 0;
    while offset < total_bytes {
        let end = std::cmp::min(offset + chunk_size, total_bytes);
        let chunk = &content[offset..end];

        file.write_all(chunk)
            .map_err(|e| format!("Failed to write chunk at offset {}: {}", offset, e))?;
        
        offset = end;
    }

    // Flush and sync
    file.flush()
        .map_err(|e| format!("Failed to flush data: {}", e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to sync to disk: {}", e))?;

    drop(file);

    // Atomic rename
    fs::rename(temp_path, target)
        .map_err(|e| format!("Failed to finalize file: {}", e))?;

    tracing::info!(
        "[FILE_WRITE] Chunked write successful: {} bytes in {} chunks",
        total_bytes,
        (total_bytes + chunk_size - 1) / chunk_size
    );
    
    Ok(total_bytes)
}

/// Determine if an error is transient and worth retrying
fn is_retryable_error(error: &str) -> bool {
    error.contains("being used by another process") ||  // Windows file lock
    error.contains("resource busy") ||                   // Unix file lock
    error.contains("disk I/O error") ||                  // Temporary disk issue
    error.contains("device or resource busy") ||
    error.contains("too many open files") // File descriptor exhaustion
}

/// Write content in chunks with progress tracking
async fn chunked_write_with_retry(
    temp_path: &PathBuf,
    target: &std::path::Path,
    content: &[u8],
    chunk_size: usize,
    max_retries: u32,
    ctx: &super::ToolContext,
) -> Result<usize, String> {
    let total_bytes = content.len();
    let mut bytes_written = 0;
    let mut last_error = String::new();
    
    // Calculate total chunks for progress reporting
    let total_chunks = (total_bytes + chunk_size - 1) / chunk_size;

    for attempt in 1..=max_retries {
        // Create temp file
        let mut file = match fs::File::create(temp_path) {
            Ok(f) => f,
            Err(e) => {
                last_error = format!("Cannot create temporary file: {}", e);
                if attempt < max_retries {
                    tracing::warn!("[FILE_WRITE] Attempt {} failed: {}", attempt, last_error);
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                    continue;
                }
                return Err(last_error);
            }
        };

        // Write in chunks
        let mut offset = 0;
        let mut chunk_success = true;
        let mut current_chunk = 0;

        while offset < total_bytes {
            let end = std::cmp::min(offset + chunk_size, total_bytes);
            let chunk = &content[offset..end];
            current_chunk += 1;

            match file.write_all(chunk) {
                Ok(_) => {
                    bytes_written = end;
                    offset = end;
                    
                    // 🚀 Report progress after each chunk
                    let percent = ((current_chunk as f64 / total_chunks as f64) * 100.0) as u8;
                    ctx.report_progress(
                        format!("Writing chunk {}/{} ({:.1} MB / {:.1} MB)", 
                            current_chunk, total_chunks,
                            offset as f64 / 1024.0 / 1024.0,
                            total_bytes as f64 / 1024.0 / 1024.0),
                        Some(percent.min(99)), // Cap at 99% until complete
                    );
                    
                    // ⚡ Yield to allow UI to update
                    tokio::task::yield_now().await;
                }
                Err(e) => {
                    chunk_success = false;
                    last_error = format!("Failed to write chunk at offset {}: {}", offset, e);
                    tracing::warn!("[FILE_WRITE] Chunk write failed: {}", last_error);
                    break;
                }
            }
        }

        if !chunk_success {
            drop(file);
            let _ = fs::remove_file(temp_path);

            if attempt < max_retries {
                tracing::warn!("[FILE_WRITE] Attempt {} failed, retrying...", attempt);
                tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                continue;
            }
            return Err(format!(
                "Failed after {} attempts: {}",
                max_retries, last_error
            ));
        }

        // Flush and sync
        if let Err(e) = file.flush() {
            drop(file);
            let _ = fs::remove_file(temp_path);
            return Err(format!("Failed to flush data: {}", e));
        }

        if let Err(e) = file.sync_all() {
            drop(file);
            let _ = fs::remove_file(temp_path);
            return Err(format!("Failed to sync to disk: {}", e));
        }

        drop(file);

        // Atomic rename
        match fs::rename(temp_path, target) {
            Ok(_) => {
                tracing::info!(
                    "[FILE_WRITE] Chunked write successful: {} bytes in {} chunks",
                    total_bytes,
                    (total_bytes + chunk_size - 1) / chunk_size
                );
                return Ok(total_bytes);
            }
            Err(e) => {
                last_error = format!("Failed to finalize file: {}", e);
                let _ = fs::remove_file(temp_path);

                if attempt < max_retries {
                    tracing::warn!("[FILE_WRITE] Rename failed, retrying...",);
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                    continue;
                }
                return Err(format!(
                    "Failed after {} attempts: {}",
                    max_retries, last_error
                ));
            }
        }
    }

    Err(format!(
        "Failed after {} attempts: {}",
        max_retries, last_error
    ))
}
