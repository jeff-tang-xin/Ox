use serde_json::{Value, json};
use std::path::Path;
use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FileListTool;

const EXCLUDE_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build",
    "__pycache__", ".venv", ".ox", ".idea", ".vscode",
];

#[async_trait::async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List files and directories. Supports glob patterns. Use to explore project structure."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path or glob pattern (e.g., 'src/**/*.rs'). Default: list root."
                }
            }
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let path_str = args.get("path").and_then(|p| p.as_str()).map(|s| s.to_string());
        let working_dir = ctx.working_dir.clone();

        // Run blocking I/O on a dedicated thread
        let result = tokio::task::spawn_blocking(move || {
            let dir = match path_str {
                Some(p) => {
                    let resolved = if Path::new(&p).is_absolute() {
                        Path::new(&p).to_path_buf()
                    } else {
                        working_dir.join(p)
                    };
                    resolved
                }
                None => working_dir.clone(),
            };

            if !dir.exists() {
                return Err(format!("Path does not exist: {}", dir.display()));
            }
            if !dir.is_dir() {
                return Err(format!("Not a directory: {}", dir.display()));
            }

            list_directory(&dir, 0, 2) // max 2 levels deep
        }).await;

        match result {
            Ok(Ok(output)) => ToolOutput::success(output),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("File list task panicked: {e}")),
        }
    }
}

/// List directory recursively up to `max_depth` levels.
fn list_directory(dir: &Path, depth: usize, max_depth: usize) -> Result<String, String> {
    if depth > max_depth {
        return Ok(String::new());
    }

    let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(dir) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(e) => return Err(format!("Cannot read {}: {e}", dir.display())),
    };

    // Sort: dirs first, then files, alphabetical
    entries.sort_by(|a, b| {
        let a_is_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let b_is_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
        b_is_dir.cmp(&a_is_dir).then_with(|| a.file_name().cmp(&b.file_name()))
    });

    let indent = "  ".repeat(depth);
    let mut output = String::new();

    if depth == 0 {
        output.push_str(&format!("📁 {}/\n\n", dir.display()));
    }

    let mut dir_count = 0usize;
    let mut file_count = 0usize;

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if EXCLUDE_DIRS.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dir_count += 1;
            output.push_str(&format!("{}{}/\n", indent, name));
            // Recurse one level
            if depth < max_depth {
                let sub = list_directory(&entry.path(), depth + 1, max_depth).unwrap_or_default();
                if !sub.is_empty() {
                    output.push_str(&sub);
                }
            }
        } else {
            file_count += 1;
            let size = entry.metadata()
                .map(|m| m.len())
                .unwrap_or(0);
            let size_str = if size < 1024 {
                format!("{} B", size)
            } else if size < 1024 * 1024 {
                format!("{:.1} KB", size as f64 / 1024.0)
            } else {
                format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
            };
            output.push_str(&format!("{}  {} ({})\n", indent, name, size_str));
        }
    }

    if depth == 0 {
        output.push_str(&format!(
            "\n{} directories, {} files\n",
            dir_count, file_count
        ));
    }

    Ok(output)
}
