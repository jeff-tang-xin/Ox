//! Error recovery — auto-fix build/test failures.
//!
//! After tool execution, checks if any `shell_exec` tool calls resulted in
//! build or test failures, and injects structured recovery prompts.

use crate::message::Message;
use std::sync::Arc;

/// Analyze tool results for build/test failures and generate recovery prompts.
///
/// Scans `new_messages` for failed `shell_exec` results (non-zero exit codes
/// from build/test commands), and injects escalating recovery prompts:
/// - Attempt 1: Read error → Read source → Diagnose → Fix → Verify
/// - Attempt 2-3: Different approach, re-read source
/// - Attempt 4+: Report to user and ask for guidance
///
/// When gitnexus is provided and a build error occurs, also runs impact analysis
/// to understand what might be affected by the fix.
pub fn check_and_recover(
    messages: &mut Vec<Message>,
    new_messages: &[Message],
    tool_calls: &[crate::message::ToolCall],
    gitnexus: Option<&Arc<crate::mcp::GitNexusService>>,
) {
    for tc in tool_calls {
        if tc.name != "shell_exec" {
            continue;
        }

        let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) else {
            continue;
        };
        let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");

        let is_build_or_test = cmd.contains("cargo build")
            || cmd.contains("cargo check")
            || cmd.contains("cargo test")
            || cmd.contains("npm test")
            || cmd.contains("npm run build")
            || cmd.contains("npx tsc")
            || cmd.contains("pytest")
            || cmd.contains("go build")
            || cmd.contains("go test")
            || cmd.contains("cargo clippy")
            || cmd.contains("npm run lint")
            || cmd.contains("mvn ")
            || cmd.contains("gradlew")
            || cmd.contains("compileall");

        if !is_build_or_test {
            continue;
        }

        // Find the tool result for this shell_exec
        for msg in new_messages.iter().rev() {
            if let Message::ToolResult {
                tool_call_id,
                content,
                ..
            } = msg
            {
                if tool_call_id != &tc.id {
                    continue;
                }
                if !content.contains("[exit code:") {
                    continue;
                }

                let exit_code = content
                    .lines()
                    .filter(|l| l.contains("exit code:"))
                    .last()
                    .unwrap_or("");

                if exit_code.contains("exit code: 0") {
                    continue; // Success, no recovery needed
                }

                // Extract error context
                let error_lines: Vec<&str> = content
                    .lines()
                    .filter(|l| {
                        l.contains("error[")
                            || l.contains("error:")
                            || l.contains("Error:")
                            || l.contains("❌")
                            || l.contains("fatal")
                            || l.contains("cannot find")
                            || l.contains("expected")
                            || l.contains("unexpected")
                            || l.contains("not found")
                            || l.contains("undefined")
                    })
                    .collect();

                let error_summary = if error_lines.is_empty() {
                    content.lines().take(5).collect::<Vec<_>>().join("\n")
                } else {
                    error_lines
                        .iter()
                        .take(5)
                        .map(|l| l.trim())
                        .collect::<Vec<_>>()
                        .join("\n")
                };

                // ── GitNexus impact analysis (if available) ──
                let impact_info = if let Some(gn) = gitnexus {
                    // Try to extract a symbol name from the error for impact analysis
                    let symbol_from_error = extract_symbol_from_error(&error_summary);
                    if let Some(symbol) = symbol_from_error {
                        let params = crate::mcp::gitnexus::ImpactParams::new(&symbol, "downstream");
                        match tokio::runtime::Handle::current().block_on(async {
                            if gn.is_ready().await {
                                gn.impact(&params).await.ok()
                            } else {
                                None
                            }
                        }) {
                            Some(result) if !result.is_error => {
                                let summary = result.text.lines().take(10).collect::<Vec<_>>().join("\n");
                                Some(format!("\n📊 Code impact preview:\n```\n{}\n```", summary))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let fix_attempts = messages
                    .iter()
                    .filter(|m| {
                        matches!(m, Message::System { content } if content.contains("BUILD/TOOL FAILED"))
                    })
                    .count()
                    + 1;

                let recovery_msg = if fix_attempts == 1 {
                    let impact_hint = impact_info.as_deref().unwrap_or("");
                    format!(
                        "🔧 BUILD/TOOL FAILED (attempt 1/3)\n\n\
                         Error summary:\n```\n{}\n```\n{}{}\n\n\
                         Recovery protocol — follow these steps IN ORDER:\n\
                         1. **Read the error** — the relevant lines are shown above\n\
                         2. **Read the affected source code** — use `file_read` on the files mentioned in the error\n\
                         3. **Diagnose root cause** — understand WHY (wrong type? missing import? syntax error?)\n\
                         4. **Fix the issue** — use `edit_file` to apply the correction\n\
                         5. **Verify** — re-run the build/test command to confirm\n\n\
                         DO NOT guess. Read the source code first, then fix.",
                        error_summary,
                        impact_hint,
                        if impact_info.is_some() { "\n💡 Use code_graph(op=\"context\", name=\"<symbol>\") for full impact view" } else { "" }
                    )
                } else {
                    format!(
                        "🔧 BUILD/TOOL FAILED (attempt {}/3)\n\n\
                         Error summary:\n```\n{}\n```\n\n\
                         Previous fix did NOT resolve the issue. Try a different approach:\n\
                         1. **Re-read the error** — you may have misread it\n\
                         2. **Re-read the source code** — the actual code may differ from what you expect\n\
                         3. **Change your approach** — the fix you tried was incorrect, try something else\n\
                         4. **Verify** — re-run and check if the error changes",
                        fix_attempts, error_summary
                    )
                };

                messages.push(Message::system(&recovery_msg));

                if fix_attempts >= 3 {
                    messages.push(Message::system(
                        "3 fix attempts exhausted. Report the remaining error to the user and ask for guidance.",
                    ));
                }
                break;
            }
        }
    }
}

/// Try to extract a function/type symbol name from an error message.
/// Looks for common error patterns like "cannot find symbol X" or "undefined: Y".
fn extract_symbol_from_error(error: &str) -> Option<String> {
    // Pattern: "cannot find symbol X in ..."
    if let Some(pos) = error.find("cannot find symbol ") {
        let rest = &error[pos + 19..];
        let end = rest.find(|c: char| !c.is_alphanumeric() && c != '_').unwrap_or(rest.len());
        if end > 0 && end < 50 {
            return Some(rest[..end].to_string());
        }
    }
    // Pattern: "undefined: X" or "undefined method X"
    if let Some(pos) = error.find("undefined") {
        let rest = &error[pos + 9..];
        let end = rest.find(|c: char| !c.is_alphanumeric() && c != '_').unwrap_or(rest.len());
        if end > 0 && end < 50 {
            return Some(rest[..end].to_string());
        }
    }
    // Pattern: "cannot find function X"
    if let Some(pos) = error.find("cannot find function ") {
        let rest = &error[pos + 21..];
        let end = rest.find(|c: char| !c.is_alphanumeric() && c != '_').unwrap_or(rest.len());
        if end > 0 && end < 50 {
            return Some(rest[..end].to_string());
        }
    }
    None
}
