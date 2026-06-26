use super::{SafetyLevel, Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use std::path::Path;

pub struct FileListTool;

const EXCLUDE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    ".ox",
    ".idea",
    ".vscode",
];

const SINGLE_LEVEL_FOOTER: &str = "\n\
💡 file_list 只列【当前目录单层】，不会展开子目录内容。\n\
   要看更深：对每个子目录再调 file_list(\"子目录路径\")，例如 file_list(\"crates\") → file_list(\"crates/ox-core\")。\n\
   要按文件名递归搜索：用 file_search(pattern)，不是 file_list。";

#[async_trait::async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List ONE directory level only (non-recursive). Returns immediate files and subdirectory \
         names — does NOT list files inside subdirectories. To go deeper, call file_list again \
         with each subdirectory path. For recursive filename search use file_search(glob), not file_list."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list (ONE level only). Example drill-down: file_list(\".\") → file_list(\"crates\") → file_list(\"crates/ox-core\"). Default: project root."
                }
            },
            "examples": [
                {"path": "."},
                {"path": "crates"},
                {"path": "src/components"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let path_str = args
            .get("path")
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());
        let working_dir = ctx.working_dir.clone();

        let result = tokio::task::spawn_blocking(move || {
            let dir = match path_str {
                Some(p) => {
                    if Path::new(&p).is_absolute() {
                        Path::new(&p).to_path_buf()
                    } else {
                        working_dir.join(p)
                    }
                }
                None => working_dir.clone(),
            };

            if !dir.exists() {
                return Err(format!("Path does not exist: {}", dir.display()));
            }
            if !dir.is_dir() {
                return Err(format!("Not a directory: {}", dir.display()));
            }

            list_single_level(&dir)
        })
        .await;

        match result {
            Ok(Ok(output)) => ToolOutput::success(output),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("File list task panicked: {e}")),
        }
    }
}

/// List immediate children only — no recursion into subdirectories.
fn list_single_level(dir: &Path) -> Result<String, String> {
    let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(dir) {
        Ok(iter) => iter.filter_map(|e| e.ok()).collect(),
        Err(e) => return Err(format!("Cannot read {}: {e}", dir.display())),
    };

    entries.sort_by(|a, b| {
        let a_is_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let b_is_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
        b_is_dir
            .cmp(&a_is_dir)
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });

    let mut output = format!("📁 {}/ (single level)\n\n", dir.display());
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
            output.push_str(&format!("  {name}/\n"));
        } else {
            file_count += 1;
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let size_str = format_file_size(size);
            output.push_str(&format!("  {name} ({size_str})\n"));
        }
    }

    output.push_str(&format!(
        "\n{dir_count} subdirectories, {file_count} files (this level only)"
    ));
    output.push_str(SINGLE_LEVEL_FOOTER);
    Ok(output)
}

fn format_file_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn single_level_does_not_recurse_into_subdirs() {
        let tmp = std::env::temp_dir().join(format!("ox_file_list_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::write(tmp.join("root.txt"), "a").unwrap();
        fs::write(tmp.join("sub/inner.txt"), "b").unwrap();

        let out = list_single_level(&tmp).unwrap();
        assert!(out.contains("root.txt"));
        assert!(out.contains("sub/"));
        assert!(
            !out.contains("inner.txt"),
            "must not list files inside sub/"
        );
        let _ = fs::remove_dir_all(&tmp);
    }
}
