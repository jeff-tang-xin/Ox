use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct ShellExecTool;

/// Parse a `cd <path>` target from a command string.
/// Returns the resolved absolute path if the command starts with `cd`.
fn detect_cd_target(command: &str, current_dir: &Path) -> Option<PathBuf> {
    let trimmed = command.trim();
    if !trimmed.starts_with("cd ") && !trimmed.starts_with("cd\t") && trimmed != "cd" {
        return None;
    }
    let rest = if trimmed == "cd" {
        ""
    } else {
        trimmed[3..].trim()
    };
    // Stop at && or ; (compound commands like `cd /tmp && ls`).
    let path_str = rest.split(&['&', ';'][..]).next().unwrap_or("").trim();
    // Strip surrounding quotes.
    let path_str = path_str.trim_matches(|c| c == '"' || c == '\'');
    if path_str.is_empty() {
        return None;
    }
    let target = if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        current_dir.join(path_str)
    };
    target.canonicalize().ok().filter(|p| p.is_dir())
}

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "🐚 Execute shell commands in the system shell (bash/cmd/PowerShell).
         
         💡 Common use cases:
         • Git operations: git status, git diff, git commit
         • Build & run: cargo build, npm run dev, python script.py
         • System info: ls, pwd, uname, whoami
         • Package management: pip install, npm install, apt-get
         
         ⚠️ Safety:
         • This is a DANGEROUS tool - requires user confirmation
         • Avoid destructive commands (rm -rf, del /s, etc.)
         • Commands timeout after 30 seconds by default
         
         📝 Examples:
         • {\"command\": \"git status\"} - Check git status
         • {\"command\": \"git diff HEAD~1\"} - View last commit changes
         • {\"command\": \"git add . && git commit -m 'fix bug'\"} - Commit changes
         • {\"command\": \"cargo test\", \"timeout_ms\": 60000} - Run tests with 60s timeout"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default: 30000)"
                }
            },
            "required": ["command"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Dangerous
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let command = match args.get("command").and_then(|c| c.as_str()) {
            Some(c) if !c.is_empty() => c,
            Some(_) => {
                return ToolOutput::error(
                    "❌ Parameter Error: 'command' cannot be empty\n\n\
                 💡 Example: {\"command\": \"ls -la\", \"timeout_ms\": 5000}",
                );
            }
            None => {
                return ToolOutput::error(
                    "❌ Missing Required Parameter: 'command'\n\n\
                 💡 How to fix:\n\
                 • Add the 'command' parameter with your shell command\n\
                 • Command will run in the system shell (bash on Linux/Mac, cmd on Windows)\n\
                 • Use caution with destructive commands (rm, del, etc.)\n\n\
                 📝 Examples:\n\
                 • {\"command\": \"ls -la\"} - List files\n\
                 • {\"command\": \"cargo build\"} - Build Rust project\n\
                 • {\"command\": \"git status\"} - Check git status\n\
                 • {\"command\": \"git diff\"} - View changes\n\
                 • {\"command\": \"git add . && git commit -m 'msg'\"} - Commit\n\
                 • {\"command\": \"python main.py\"} - Run Python script",
                );
            }
        };
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|t| t.as_u64())
            .unwrap_or(30000);

        let shell = &ctx.runtime.shell;
        let mut cmd = Command::new(&shell.path);

        // On Windows, set PowerShell output encoding to UTF-8 to avoid garbled Chinese text
        if cfg!(windows) && (shell.name == "powershell" || shell.name == "pwsh") {
            // Set UTF-8 encoding for PowerShell
            cmd.arg("-NoProfile");
            cmd.arg("-OutputFormat");
            cmd.arg("Text");
            cmd.arg("-Command");
            // Wrap command with UTF-8 encoding setup
            let utf8_wrapper = format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $PSDefaultParameterValues['Out-File:Encoding'] = 'utf8'; chcp 65001 | Out-Null; {}",
                command
            );
            cmd.arg(&utf8_wrapper);
        } else {
            // Linux/Mac or cmd.exe
            for prefix in &shell.exec_prefix {
                cmd.arg(prefix);
            }
            cmd.arg(command);
        }
        cmd.current_dir(&ctx.working_dir);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to spawn shell: {e}")),
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let timeout = tokio::time::Duration::from_millis(timeout_ms);
        let result = tokio::time::timeout(timeout, async {
            let stdout_lines = tokio::spawn(async move {
                let mut lines = Vec::new();
                if let Some(stdout) = stdout {
                    let reader = BufReader::new(stdout);
                    let mut lines_reader = reader.lines();
                    while let Ok(Some(line)) = lines_reader.next_line().await {
                        lines.push(line);
                    }
                }
                lines
            });

            let stderr_lines = tokio::spawn(async move {
                let mut lines = Vec::new();
                if let Some(stderr) = stderr {
                    let reader = BufReader::new(stderr);
                    let mut lines_reader = reader.lines();
                    while let Ok(Some(line)) = lines_reader.next_line().await {
                        lines.push(format!("[stderr] {line}"));
                    }
                }
                lines
            });

            let out = stdout_lines.await.unwrap_or_default();
            let err = stderr_lines.await.unwrap_or_default();

            let mut output_lines = out;
            output_lines.extend(err);

            let status = child.wait().await;
            (output_lines, status)
        })
        .await;

        match result {
            Ok((lines, status)) => {
                let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

                // Truncate to last 50 lines for LLM context.
                let truncated = if lines.len() > 50 {
                    let skipped = lines.len() - 50;
                    let mut result = vec![format!("... ({skipped} lines omitted)")];
                    result.extend(lines[lines.len() - 50..].iter().cloned());
                    result
                } else {
                    lines
                };

                let output = truncated.join("\n");
                let suffix = format!("\n[exit code: {exit_code}]");
                let mut tool_output = if exit_code == 0 {
                    ToolOutput::success(format!("{output}{suffix}"))
                } else {
                    ToolOutput::error(format!("{output}{suffix}"))
                };
                // Detect cd: if command succeeded and contains a cd target, carry the new dir.
                if exit_code == 0 {
                    tool_output.new_working_dir = detect_cd_target(command, &ctx.working_dir);
                }
                tool_output
            }
            Err(_) => {
                // Timeout — kill the process.
                let _ = child.kill().await;
                ToolOutput::error(format!(
                    "Command timed out after {timeout_ms}ms. Process killed."
                ))
            }
        }
    }
}
