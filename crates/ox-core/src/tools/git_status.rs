use serde_json::{json, Value};
use tokio::process::Command;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct GitStatusTool;

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }

    fn description(&self) -> &str {
        "Show the working tree status (git status --short)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, _args: Value, ctx: &ToolContext) -> ToolOutput {
        run_git(&["status", "--short", "--branch"], ctx).await
    }
}

pub struct GitDiffTool;

#[async_trait::async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn description(&self) -> &str {
        "Show changes in the working directory (git diff). Optionally diff staged changes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "staged": {
                    "type": "boolean",
                    "description": "If true, show staged changes (--cached)"
                },
                "path": {
                    "type": "string",
                    "description": "Limit diff to a specific file or directory"
                }
            }
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let mut git_args = vec!["diff"];
        if args.get("staged").and_then(|s| s.as_bool()).unwrap_or(false) {
            git_args.push("--cached");
        }

        let path_str;
        if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
            git_args.push("--");
            path_str = path.to_string();
            git_args.push(&path_str);
        }

        run_git(&git_args, ctx).await
    }
}

pub struct GitCommitTool;

#[async_trait::async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "Stage files and create a git commit. Stages specified files (or all changes) then commits."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Commit message"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to stage (default: all changes via 'git add .')"
                }
            },
            "required": ["message"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Dangerous
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let message = match args.get("message").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => return ToolOutput::error("Missing required parameter: message. Usage: {\"message\": \"<commit message>\"}"),
        };

        // Stage files.
        let files = args
            .get("files")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            });

        let stage_result = if let Some(files) = &files {
            let mut git_args = vec!["add"];
            let file_refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
            git_args.extend(file_refs);
            run_git(&git_args, ctx).await
        } else {
            run_git(&["add", "."], ctx).await
        };

        if stage_result.is_error {
            return stage_result;
        }

        // Commit.
        run_git(&["commit", "-m", message], ctx).await
    }
}

async fn run_git(args: &[&str], ctx: &ToolContext) -> ToolOutput {
    let output = Command::new("git")
        .args(args)
        .current_dir(&ctx.working_dir)
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = if stderr.is_empty() {
                stdout.to_string()
            } else {
                format!("{stdout}\n{stderr}")
            };

            if out.status.success() {
                ToolOutput::success(combined.trim_end())
            } else {
                ToolOutput::error(combined.trim_end())
            }
        }
        Err(e) => ToolOutput::error(format!("Failed to run git: {e}")),
    }
}
