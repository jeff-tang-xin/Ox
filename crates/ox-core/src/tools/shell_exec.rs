use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct ShellExecTool;

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout/stderr. Use the system's detected shell."
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
            Some(c) => c,
            None => return ToolOutput::error("Missing required parameter: command. Usage: {\"command\": \"<shell command>\"}"),
        };
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|t| t.as_u64())
            .unwrap_or(30000);

        let shell = &ctx.runtime.shell;
        let mut cmd = Command::new(&shell.path);
        for prefix in &shell.exec_prefix {
            cmd.arg(prefix);
        }
        cmd.arg(command);
        cmd.current_dir(&ctx.working_dir);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to spawn shell: {e}")),
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Collect output with timeout.
        let timeout = tokio::time::Duration::from_millis(timeout_ms);
        let result = tokio::time::timeout(timeout, async {
            let mut output_lines = Vec::new();

            if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    output_lines.push(line);
                }
            }

            if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    output_lines.push(format!("[stderr] {line}"));
                }
            }

            let status = child.wait().await;
            (output_lines, status)
        })
        .await;

        match result {
            Ok((lines, status)) => {
                let exit_code = status
                    .map(|s| s.code().unwrap_or(-1))
                    .unwrap_or(-1);

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
                if exit_code == 0 {
                    ToolOutput::success(format!("{output}{suffix}"))
                } else {
                    ToolOutput::error(format!("{output}{suffix}"))
                }
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
