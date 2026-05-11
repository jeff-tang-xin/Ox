use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput, content_validation};

pub struct FileWriteTool;

#[async_trait::async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file. Use ONLY for new files or complete rewrites (>50% changed). For small edits, use file_patch."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative). Required unless using filename or file_id."
                },
                "filename": {
                    "type": "string",
                    "description": "Filename for new/existing file. Alternative to path."
                },
                "file_id": {
                    "type": "integer",
                    "description": "File ID from index. For existing files only."
                },
                "content": {
                    "type": "string",
                    "description": "✅ REQUIRED: File content. Large files (>1 MB) auto-chunked."
                }
            },
            "required": ["content"],
            "oneOf": [
                {"required": ["path"]},
                {"required": ["filename"]},
                {"required": ["file_id"]}
            ],
            "examples": [
                {"path": "output.txt", "content": "Hello World"},
                {"path": "src/main.rs", "content": "fn main() {}"}
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
                "❌ Missing Required Parameter: No path specified\n\n\
                 💡 You MUST provide ONE of:\n\
                 • 'path': Full relative path (most common)\n\
                 • 'filename': Filename for new/existing file\n\
                 • 'file_id': Precise file ID from index\n\n\
                 📝 Correct Examples:\n\
                 {\"path\": \"src/output.txt\", \"content\": \"Hello\"}\n\
                 {\"filename\": \"new_file.txt\", \"content\": \"Content\"}\n\
                 {\"file_id\": 123, \"content\": \"Updated content\"}\n\n\
                 ❌ Wrong Example:\n\
                 {\"content\": \"Hello\"} ← Missing path parameter!"
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

        let content_bytes = content.as_bytes();

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

        // Write file with automatic strategy selection
        let temp_path = create_temp_path(&path);

        let result = if is_large_file {
            // Automatic chunked write for large files (>1 MB)
            chunked_write_with_retry(&temp_path, &path, content_bytes, CHUNK_SIZE, 3).await
        } else {
            // Standard atomic write for normal files
            atomic_write_with_retry(&temp_path, &path, content_bytes, 3).await
        };

        match result {
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

                ToolOutput::success(format!(
                    "✅ Successfully written {} bytes to {}{}\n\
                     📄 Encoding: UTF-8 (without BOM)\n\
                     💡 Tip: Use 'file_read' to verify the content",
                    bytes_written,
                    display_path.display(),
                    size_info
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

/// Atomically write content to a file using temp file + rename strategy
/// This ensures the target file is never left in a corrupted state
fn atomic_write(
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
        match atomic_write(temp_path, target, content) {
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
) -> Result<usize, String> {
    let total_bytes = content.len();
    let mut bytes_written = 0;
    let mut last_error = String::new();

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

        while offset < total_bytes {
            let end = std::cmp::min(offset + chunk_size, total_bytes);
            let chunk = &content[offset..end];

            match file.write_all(chunk) {
                Ok(_) => {
                    bytes_written = end;
                    offset = end;
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
