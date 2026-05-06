use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use super::{content_validation, SafetyLevel, Tool, ToolContext, ToolOutput};

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
        
        // Normalize path: trim whitespace and standardize separators
        let normalized_path = raw_path.trim().replace('\\', "/");
        
        // Handle absolute vs relative paths
        let resolved_path = if std::path::Path::new(&normalized_path).is_absolute() {
            std::path::PathBuf::from(&normalized_path)
        } else {
            ctx.working_dir.join(&normalized_path)
        };
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

        // Validate path for platform-specific invalid characters
        let path_str = path.to_string_lossy();
        if cfg!(windows) {
            // Strip Windows UNC prefix if present (\\?\ or \\?\UNC\)
            let clean_path = if path_str.starts_with("\\\\?\\") {
                &path_str[4..]  // Remove "\\?\" prefix
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
                            c, path.display()
                        ));
                    }
                    ':' => {
                        // ':' is only valid at position 1 (drive letter separator)
                        if i != 1 {
                            return ToolOutput::error(format!(
                                "❌ Invalid Path Character: ':' is not allowed in Windows filenames (except for drive letter)\n\n\
                                 💡 Problem: {} contains ':' at position {}\n\
                                 🔧 Solution: Use a valid path like 'C:\\path\\file.txt'",
                                path.display(), i
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
                depth, path.display()
            );
        }
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

        // Check file size limit (5 MB)
        const MAX_FILE_SIZE: usize = 5 * 1024 * 1024;
        let content_bytes = content.as_bytes();
        if content_bytes.len() > MAX_FILE_SIZE {
            return ToolOutput::error(format!(
                "❌ File Too Large: Content is {:.2} MB (limit: {} MB)\n\n\
                 💡 Recommendations:\n\
                 • Split into multiple smaller files\n\
                 • Use file_patch for incremental changes\n\
                 • Compress or summarize the content",
                content_bytes.len() as f64 / 1024.0 / 1024.0,
                MAX_FILE_SIZE as f64 / 1024.0 / 1024.0
            ));
        }

        // Validate content quality using shared validation logic
        if let Err(e) = content_validation::validate_content(content) {
            return ToolOutput::error(e);
        }

        // Create parent directories.
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent) {
                return ToolOutput::error(format!(
                    "❌ Directory Creation Failed: Cannot create {}\n\n\
                     💡 Error: {}\n\
                     🔍 Possible causes:\n\
                     • Insufficient permissions\n\
                     • Disk is full\n\
                     • Path contains invalid characters",
                    parent.display(), e
                ));
            }

        // Atomic write with retry mechanism for transient failures
        let temp_path = create_temp_path(&path);
        
        match atomic_write_with_retry(&temp_path, &path, content.as_bytes(), 3).await {
            Ok(bytes_written) => {
                ToolOutput::success(format!(
                    "✅ Successfully written {} bytes to {}\n\
                     📄 Encoding: UTF-8 (without BOM)\n\
                     💡 Tip: Use 'file_read' to verify the content",
                    bytes_written,
                    path.display()
                ))
            },
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
                    e, path.display()
                ))
            }
        }
    }
}

/// Create a temporary file path in the same directory as the target
fn create_temp_path(target: &std::path::Path) -> PathBuf {
    let mut temp = target.to_path_buf();
    let file_name = target.file_name()
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
fn atomic_write(temp_path: &PathBuf, target: &std::path::Path, content: &[u8]) -> Result<usize, String> {
    // Step 1: Write to temp file
    let mut file = fs::File::create(temp_path).map_err(|e| {
        format!("Cannot create temporary file: {}", e)
    })?;
    
    file.write_all(content).map_err(|e| {
        format!("Failed to write data: {}", e)
    })?;
    
    // Flush to ensure data is written to disk
    file.flush().map_err(|e| {
        format!("Failed to flush data: {}", e)
    })?;
    
    // Sync to ensure data is physically on disk (not just in OS cache)
    file.sync_all().map_err(|e| {
        format!("Failed to sync to disk: {}", e)
    })?;
    
    drop(file); // Close the file before renaming
    
    let bytes_written = content.len();
    
    // Step 2: Atomic rename (on most filesystems, rename is atomic)
    fs::rename(temp_path, target).map_err(|e| {
        format!("Failed to finalize file: {}", e)
    })?;
    
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
                        attempt, delay, e
                    );
                    tokio::time::sleep(delay).await;
                } else {
                    break;
                }
            }
        }
    }
    
    Err(format!("Failed after {} attempts: {}", max_retries, last_error))
}

/// Determine if an error is transient and worth retrying
fn is_retryable_error(error: &str) -> bool {
    error.contains("being used by another process") ||  // Windows file lock
    error.contains("resource busy") ||                   // Unix file lock
    error.contains("disk I/O error") ||                  // Temporary disk issue
    error.contains("device or resource busy") ||
    error.contains("too many open files")                // File descriptor exhaustion
}
