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
        SafetyLevel::RequiresConfirmation
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

        // Spawn tasks to read stdout and stderr in parallel
        // Use tokio::spawn with careful error handling
        let stdout_handle = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                let mut lines_reader = reader.lines();
                while let Ok(Some(line)) = lines_reader.next_line().await {
                    lines.push(line);
                }
            }
            tracing::debug!("[SHELL] stdout read {} lines", lines.len());
            lines
        });

        let stderr_handle = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                let mut lines_reader = reader.lines();
                while let Ok(Some(line)) = lines_reader.next_line().await {
                    lines.push(line);
                }
            }
            tracing::debug!("[SHELL] stderr read {} lines", lines.len());
            lines
        });

        let result = tokio::time::timeout(timeout, async {
            // Wait for output reading to complete
            let stdout_lines = stdout_handle.await.unwrap_or_else(|e| {
                tracing::error!("[SHELL] stdout task panicked: {}", e);
                Vec::new()
            });
            let stderr_lines = stderr_handle.await.unwrap_or_else(|e| {
                tracing::error!("[SHELL] stderr task panicked: {}", e);
                Vec::new()
            });

            // Wait for process to exit
            let status = child.wait().await;
            (stdout_lines, stderr_lines, status)
        })
        .await;

        match result {
            Ok((stdout_lines, stderr_lines, status)) => {
                let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

                // Build a structured output with stdout, stderr, and analysis
                let mut parts = Vec::new();

                // Stdout section (only if non-empty)
                if !stdout_lines.is_empty() {
                    parts.push(format!("── stdout ──\n{}", stdout_lines.join("\n")));
                }

                let stderr_text = stderr_lines.join("\n");

                if exit_code != 0 {
                    // ── Error analysis for non-zero exits ──
                    // Count error/warning keywords in combined output
                    let combined = format!("{}\n{}", stdout_lines.join("\n"), stderr_text);
                    let line_count = combined.lines().count();
                    let error_count = combined.matches("error[").count()
                        + combined.matches("error:").count()
                        + combined.matches("Error:").count()
                        + combined.matches("ERROR").count()
                        + combined.matches("❌").count()
                        + combined.matches("failed").count();
                    let warning_count = combined.matches("warning[").count()
                        + combined.matches("warning:").count()
                        + combined.matches("Warning:").count()
                        + combined.matches("WARNING").count()
                        + combined.matches("⚠️").count()
                        + combined.matches("WARN").count()
                        + combined.matches("\x1b[33m").count(); // yellow ANSI

                    // stderr section
                    if !stderr_text.is_empty() {
                        parts.push(format!(
                            "── stderr ({n} lines) ──\n{stderr_text}",
                            n = stderr_lines.len()
                        ));
                    }

                    // Extract first relevant error line for quick diagnosis
                    let first_error = combined
                        .lines()
                        .find(|l| {
                            l.contains("error[")
                                || l.contains("error:")
                                || l.contains("Error:")
                                || l.contains("❌")
                                || l.contains("fatal")
                                || l.contains("cannot find")
                        })
                        .map(|l| l.trim().to_string());

                    // Build concise analysis block
                    let mut analysis = format!(
                        "\n── Analysis ──\n📊 {} lines | {} errors | {} warnings\n💥 Exit code: {exit_code}",
                        line_count, error_count, warning_count
                    );

                    if let Some(ref first) = first_error {
                        analysis.push_str(&format!("\n🔍 First error: {}", first));
                    }

                    // Common error hints
                    if combined.contains("not found") || combined.contains("No such file") {
                        analysis.push_str(
                            "\n💡 Hint: A file or command was not found. Check the path and name.",
                        );
                    }
                    if combined.contains("syntax error")
                        || combined.contains("parse error")
                        || combined.contains("unexpected")
                    {
                        analysis.push_str(
                            "\n💡 Hint: There may be a syntax error. Check the command syntax.",
                        );
                    }
                    if combined.contains("permission denied")
                        || combined.contains("Permission denied")
                        || combined.contains("EACCES")
                    {
                        analysis.push_str("\n💡 Hint: Permission denied. Try with appropriate permissions or check file ownership.");
                    }
                    if combined.contains("does not exist")
                        || combined.contains("No such file or directory")
                    {
                        analysis.push_str("\n💡 Hint: Path not found. Use `ls` to verify the file/directory exists.");
                    }
                    if combined.contains("connection refused")
                        || combined.contains("Connection refused")
                        || combined.contains("timed out")
                    {
                        analysis.push_str("\n💡 Hint: Network issue. Check if the service is running and reachable.");
                    }

                    parts.push(analysis);
                } else {
                    // Success — just include stderr if present (warnings, etc.)
                    if !stderr_text.is_empty() {
                        parts.push(format!("── stderr ──\n{stderr_text}"));
                    }
                    parts.push(format!("\n✅ Exit code: 0"));
                }

                let output = parts.join("\n\n");
                let mut tool_output = if exit_code == 0 {
                    ToolOutput::success(output)
                } else {
                    ToolOutput::error(output)
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
