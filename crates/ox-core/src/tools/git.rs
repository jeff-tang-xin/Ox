//! Git tools — dedicated git operations with structured output.
//! Separated from shell_exec for safety and parseability.

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};

pub struct GitStatusTool;
pub struct GitDiffTool;

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn description(&self) -> &str {
        "Show git working tree status. Returns structured output: branch name, \
         staged/unstaged/untracked files. Safer and more parseable than shell_exec('git status')."
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, _args: Value, ctx: &ToolContext) -> ToolOutput {
        let wd = ctx.working_dir.to_string_lossy().to_string();
        let output = std::process::Command::new("git")
            .args(["-C", &wd, "status", "--porcelain", "-b"])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if !o.status.success() {
                    return ToolOutput::error(format!("git status failed: {}", stderr.trim()));
                }
                let lines: Vec<&str> = stdout.lines().collect();
                let branch = lines
                    .first()
                    .map(|l| l.trim_start_matches("## "))
                    .unwrap_or("unknown");
                let changes: Vec<&str> = lines.iter().skip(1).cloned().collect();

                let mut result = format!("Branch: {}\n", branch);
                if changes.is_empty() {
                    result.push_str("Working tree clean.");
                } else {
                    let staged = changes
                        .iter()
                        .filter(|l| !l.starts_with(' ') && !l.starts_with('?'))
                        .count();
                    let unstaged = changes.iter().filter(|l| l.starts_with(' ')).count();
                    let untracked = changes.iter().filter(|l| l.starts_with("??")).count();
                    result.push_str(&format!(
                        "Staged: {}, Unstaged: {}, Untracked: {}\n\n",
                        staged, unstaged, untracked
                    ));
                    for c in changes {
                        let (status, file) = if c.len() >= 4 {
                            (&c[..2], c[3..].trim())
                        } else {
                            (c, c)
                        };
                        result.push_str(&format!("  {} {}\n", status, file));
                    }
                }
                ToolOutput::success(result)
            }
            Err(e) => ToolOutput::error(format!("Failed to run git: {}", e)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }
    fn description(&self) -> &str {
        "Show git diff. Options: staged (--staged), file path, or unstaged by default. \
         Returns diff output. Safer than shell_exec('git diff'). \
         Example: {} or {\"staged\": true} or {\"path\": \"src/main.rs\"}"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "staged": { "type": "boolean", "description": "Show staged changes (--staged)" },
                "path": { "type": "string", "description": "Limit diff to this file path" }
            }
        })
    }
    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let staged = args
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let file_path = args.get("path").and_then(|v| v.as_str());

        let wd = ctx.working_dir.to_string_lossy().to_string();
        let mut args = vec!["-C", wd.as_str(), "diff"];
        if staged {
            args.push("--staged");
        }
        if let Some(p) = file_path {
            args.push("--");
            args.push(p);
        }

        let output = std::process::Command::new("git").args(&args).output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if !o.status.success() {
                    return ToolOutput::error(format!("git diff failed: {}", stderr.trim()));
                }
                if stdout.trim().is_empty() {
                    ToolOutput::success("No changes.")
                } else {
                    ToolOutput::success(stdout.into_owned())
                }
            }
            Err(e) => ToolOutput::error(format!("Failed to run git: {}", e)),
        }
    }
}
