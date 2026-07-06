use chrono::Utc;
use std::fs;
/// Context Offloading - Save verbose tool outputs to external files
///
/// Inspired by TencentDB-Agent-Memory's approach:
/// - Complete results saved to .ox/refs/{task_id}_{step}.md
/// - Message context only keeps summary + node_id reference
/// - Reduces token usage by 60%+ for long-running tasks
/// - Maintains full traceability via node_id lookup
use std::path::{Path, PathBuf};

/// Represents a tool execution result with offloading support
#[derive(Debug, Clone)]
pub struct OffloadedResult {
    /// Summary of the result (kept in context)
    pub summary: String,
    /// Reference ID for retrieving full content (node_id)
    pub node_id: String,
    /// Path to the full content file (if offloaded)
    pub ref_path: Option<PathBuf>,
    /// Whether this was actually offloaded
    pub is_offloaded: bool,
}

impl OffloadedResult {
    /// Create a new offloaded result
    pub fn new(summary: String, node_id: String, ref_path: Option<PathBuf>) -> Self {
        let is_offloaded = ref_path.is_some();
        Self {
            summary,
            node_id,
            ref_path,
            is_offloaded,
        }
    }

    /// Format as message content for LLM context
    pub fn to_context_message(&self) -> String {
        if self.is_offloaded {
            let path = self.ref_path.as_ref().unwrap();
            format!(
                "📄 Summary: {}\n💡 Full content saved to `{}` — use `file_read` with this path to retrieve the complete output.",
                self.summary,
                path.display()
            )
        } else {
            self.summary.clone()
        }
    }
}

/// Context offloader - manages saving and retrieving tool outputs
pub struct ContextOffloader {
    /// Base directory for storing references (.ox/refs/)
    refs_dir: PathBuf,
    /// Current task/session ID for organizing refs
    task_id: String,
    /// Symbolic task canvas (Mermaid) — accumulates offloaded nodes for a task map
    canvas: crate::agent::task_canvas::TaskCanvas,
}

impl ContextOffloader {
    /// Create a new context offloader
    pub fn new(working_dir: &Path, task_id: &str) -> Self {
        let refs_dir = working_dir.join(".ox").join("refs");

        // Ensure refs directory exists
        if let Err(e) = fs::create_dir_all(&refs_dir) {
            tracing::warn!("Failed to create refs directory: {}", e);
        }

        Self {
            refs_dir,
            task_id: task_id.to_string(),
            canvas: crate::agent::task_canvas::TaskCanvas::new("Task Progress"),
        }
    }

    /// Process a tool result and decide whether to offload
    ///
    /// Offloading criteria:
    /// - Content length > threshold (default: 2000 chars)
    /// - Contains file paths or code blocks
    /// - Is a search/listing operation result
    pub fn process_result(
        &mut self,
        tool_name: &str,
        tool_args: &str,
        content: &str,
        step_index: usize,
        threshold: usize,
    ) -> OffloadedResult {
        let should_offload = self.should_offload(tool_name, tool_args, content, threshold);

        if should_offload {
            self.offload_result(tool_name, content, step_index)
        } else {
            // Keep in context without offloading
            OffloadedResult::new(content.to_string(), format!("inline_{}", step_index), None)
        }
    }

    /// Determine if a result should be offloaded
    fn should_offload(
        &self,
        tool_name: &str,
        tool_args: &str,
        content: &str,
        threshold: usize,
    ) -> bool {
        // Never offload recall results — they're already retrieving offloaded content,
        // offloading them again would create a pointless feedback loop.
        if tool_name == "recall" {
            return false;
        }

        // Never offload file_read when it's reading a refs file — same feedback loop.
        if tool_name == "file_read" && tool_args.contains(".ox/refs/") {
            return false;
        }

        // Always offload if content exceeds threshold
        if content.len() > threshold {
            return true;
        }

        // Offload specific tool types that tend to produce verbose output
        match tool_name {
            "code_search" | "file_list" | "grep" => {
                // Search results — only offload when there are many results
                content.lines().count() > 50
            }
            "shell_exec" => {
                // Shell output can be very verbose — only offload truly enormous output
                content.len() > 8000 || content.lines().count() > 100
            }
            "file_read" => {
                // mod.rs passes usize::MAX to disable offload; only ref above file_read 512KB gate
                threshold != usize::MAX
                    && content.len() > crate::tools::file_read::INLINE_CONTENT_THRESHOLD
            }
            _ => false,
        }
    }

    /// Offload a result to external file
    fn offload_result(
        &mut self,
        tool_name: &str,
        content: &str,
        step_index: usize,
    ) -> OffloadedResult {
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let node_id = format!("{}_step{}_{}", self.task_id, step_index, timestamp);
        let filename = format!("{}.md", node_id);
        let ref_path = self.refs_dir.join(&filename);

        // Generate summary (first 200 chars + line count) - safe UTF-8 truncation
        let summary = if content.len() > 200 {
            let boundary = content
                .char_indices()
                .take_while(|(i, _)| *i < 200)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(200);
            format!(
                "{}... ({} lines total)",
                &content[..boundary],
                content.lines().count()
            )
        } else {
            content.to_string()
        };

        // Write full content to file
        let file_content = format!(
            "# Tool Execution Result\n\n\
             **Tool**: {}\n\
             **Step**: {}\n\
             **Timestamp**: {}\n\
             **Node ID**: {}\n\n\
             ---\n\n\
             {}\n",
            tool_name,
            step_index,
            Utc::now().to_rfc3339(),
            node_id,
            content
        );

        if let Err(e) = fs::write(&ref_path, &file_content) {
            tracing::error!(
                "Failed to write offloaded result to {}: {}",
                ref_path.display(),
                e
            );
            // Fallback: keep in context
            return OffloadedResult::new(
                content.to_string(),
                format!("fallback_{}", step_index),
                None,
            );
        }

        tracing::info!(
            "Offloaded tool result: {} -> {} ({} bytes)",
            tool_name,
            ref_path.display(),
            file_content.len()
        );

        // Add to task canvas (symbolic memory)
        let canvas_node = crate::agent::task_canvas::TaskNode::new(&node_id, tool_name)
            .with_ref(&ref_path.display().to_string())
            .with_description(&summary);
        self.canvas.add_node(canvas_node);
        tracing::debug!(
            "[CANVAS] Added node {} to task canvas ({} total nodes)",
            node_id,
            self.canvas.nodes.len()
        );

        OffloadedResult::new(summary, node_id, Some(ref_path))
    }

    /// Get the accumulated task canvas (for injection into context)
    pub fn get_canvas(&self) -> &crate::agent::task_canvas::TaskCanvas {
        &self.canvas
    }

    /// Get compact Mermaid canvas for context injection (only if there are nodes)
    pub fn get_canvas_context(&self) -> Option<String> {
        if self.canvas.nodes.is_empty() {
            return None;
        }
        let summary = self.canvas.status_summary();
        let total: usize = summary.values().sum();
        let mermaid = self.canvas.to_compact_mermaid();
        Some(format!(
            "## 📊 Task Progress ({total} steps offloaded)\n\n{mermaid}\n\n💡 Use `recall <node_id>` to retrieve any step's full content.\n"
        ))
    }

    /// Retrieve full content from an offloaded reference
    pub fn retrieve_full_content(&self, node_id: &str) -> Option<String> {
        let filename = format!("{}.md", node_id);
        let ref_path = self.refs_dir.join(&filename);

        if ref_path.exists() {
            match fs::read_to_string(&ref_path) {
                Ok(content) => {
                    tracing::info!("Retrieved full content for node_id: {}", node_id);
                    Some(content)
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to read offloaded content {}: {}",
                        ref_path.display(),
                        e
                    );
                    None
                }
            }
        } else {
            tracing::warn!("Offloaded content not found for node_id: {}", node_id);
            None
        }
    }

    /// Clean up old references (keep last N per task)
    pub fn cleanup_old_refs(&self, keep_count: usize) -> Result<usize, String> {
        if !self.refs_dir.exists() {
            return Ok(0);
        }

        let mut entries: Vec<_> = fs::read_dir(&self.refs_dir)
            .map_err(|e| format!("Failed to read refs dir: {}", e))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&self.task_id)
            })
            .collect();

        // Sort by modification time (oldest first)
        entries.sort_by_key(|entry| {
            entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        let total_count = entries.len();
        let to_delete = total_count.saturating_sub(keep_count);

        let mut deleted = 0;
        for entry in entries.iter().take(to_delete) {
            if let Err(e) = fs::remove_file(entry.path()) {
                tracing::warn!("Failed to delete old ref {}: {}", entry.path().display(), e);
            } else {
                deleted += 1;
            }
        }

        if deleted > 0 {
            tracing::info!(
                "Cleaned up {} old references (kept {})",
                deleted,
                keep_count
            );
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_offload_large_content() {
        let temp_dir = TempDir::new().unwrap();
        let mut offloader = ContextOffloader::new(temp_dir.path(), "test_task");

        let large_content = "Line 1\n".repeat(100);
        let result = offloader.process_result("some_tool", "", &large_content, 1, 500);

        assert!(result.is_offloaded);
        assert!(result.ref_path.is_some());
        assert!(result.ref_path.as_ref().unwrap().exists());
    }

    #[test]
    fn test_keep_small_content() {
        let temp_dir = TempDir::new().unwrap();
        let mut offloader = ContextOffloader::new(temp_dir.path(), "test_task");

        let small_content = "Short result";
        let result = offloader.process_result("file_write", "", small_content, 1, 2000);

        assert!(!result.is_offloaded);
        assert!(result.ref_path.is_none());
        assert_eq!(result.summary, small_content);
    }

    #[test]
    fn test_retrieve_content() {
        let temp_dir = TempDir::new().unwrap();
        let mut offloader = ContextOffloader::new(temp_dir.path(), "test_task");

        let content = "Test content to retrieve";
        let result = offloader.process_result("file_read", "", content, 1, 10);

        if result.is_offloaded {
            let retrieved = offloader.retrieve_full_content(&result.node_id);
            assert!(retrieved.is_some());
            assert!(retrieved.unwrap().contains(content));
        }
    }
}
