//! Post-edit AST feedback and language-aware verification gates.
//!
//! After `edit_file` / `file_write`:
//! - Surfaces AST syntax issues to the LLM with a recovery protocol
//! - Tracks touched source files and the project-appropriate verify command
//! - Blocks `## Done` on the coding Execute path until verify passes

use std::path::Path;

use crate::agent::engine::WorkflowEngine;
use crate::message::{Message, ToolCall};

pub const AST_MARKER: &str = "⚠️ AST Syntax Check";

const TOUCHED_KEY: &str = "_code_files_touched";
pub const VERIFY_CMD_KEY: &str = "_verify_command";
pub const VERIFY_STATUS_KEY: &str = "_verify_status";
const AST_PENDING_KEY: &str = "_ast_pending";

/// Project-appropriate verify command for touched source files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyCommand {
    pub label: String,
    pub command: String,
}

/// Scan tool results from this iteration and inject AST recovery prompts.
pub fn check_ast_and_recover(
    messages: &mut Vec<Message>,
    new_messages: &[Message],
    tool_calls: &[ToolCall],
) {
    for tc in tool_calls {
        if !matches!(tc.name.as_str(), "file_write" | "edit_file") {
            continue;
        }
        let Some(content) = tool_result_content(new_messages, &tc.id) else {
            continue;
        };
        if !content.contains(AST_MARKER) {
            continue;
        }
        let path = tool_path(&tc.arguments).unwrap_or_else(|| "<file>".into());
        let summary = extract_ast_summary(&content);
        let fix_attempts = messages
            .iter()
            .filter(|m| {
                matches!(m, Message::System { content } if content.contains("AST SYNTAX ERROR"))
            })
            .count()
            + 1;

        let recovery = if fix_attempts == 1 {
            format!(
                "🔧 AST SYNTAX ERROR (attempt 1/3) in `{path}`\n\n\
                 {summary}\n\n\
                 Recovery protocol — follow IN ORDER:\n\
                 1. **file_read** the file around the reported line(s)\n\
                 2. **Diagnose** — bracket mismatch? missing semicolon? truncated edit?\n\
                 3. **edit_file** to fix the syntax\n\
                 4. Re-check — tool result must NOT contain `{AST_MARKER}`\n\n\
                 DO NOT output ## Done until syntax is clean."
            )
        } else {
            format!(
                "🔧 AST SYNTAX ERROR (attempt {fix_attempts}/3) in `{path}`\n\n\
                 {summary}\n\n\
                 Previous fix did NOT clear the syntax error. Re-read the file and try a different correction."
            )
        };
        messages.push(Message::system(&recovery));

        if fix_attempts >= 3 {
            messages.push(Message::system(
                "3 AST fix attempts exhausted. Report remaining syntax errors to the user.",
            ));
        }
    }
}

/// Update session state from successful code edits in Execute step.
pub fn track_edits_and_verify_plan(
    engine: &WorkflowEngine,
    project_root: &Path,
    tool_calls: &[ToolCall],
    new_messages: &[Message],
    execute_coding: bool,
) {
    if !execute_coding {
        return;
    }

    let mut touched = read_touched(engine);
    let mut ast_pending = read_ast_pending(engine);

    for tc in tool_calls {
        if !matches!(
            tc.name.as_str(),
            "file_write" | "edit_file" | "delete_range"
        ) {
            continue;
        }
        let Some(content) = tool_result_content(new_messages, &tc.id) else {
            continue;
        };
        if content.contains("❌") {
            continue;
        }
        let Some(path) = tool_path(&tc.arguments) else {
            continue;
        };
        if !is_source_path(&path) || path.contains(".ox/skills/") {
            continue;
        }

        if !touched.iter().any(|p| p == &path) {
            touched.push(path.clone());
        }

        if content.contains(AST_MARKER) {
            ast_pending.insert(path.clone(), extract_ast_summary(&content));
        } else {
            ast_pending.remove(&path);
        }
    }

    write_touched(engine, &touched);
    write_ast_pending(engine, &ast_pending);

    if touched.is_empty() {
        engine.set_variable(VERIFY_CMD_KEY, String::new());
        engine.set_variable(VERIFY_STATUS_KEY, String::new());
        return;
    }

    if let Some(cmd) = resolve_verify_command(project_root, &touched) {
        let prev_cmd = engine.get_variable(VERIFY_CMD_KEY).unwrap_or_default();
        if prev_cmd != cmd.command {
            engine.set_variable(VERIFY_CMD_KEY, cmd.command.clone());
            engine.set_variable(VERIFY_STATUS_KEY, "pending".into());
        }
    } else {
        engine.set_variable(VERIFY_CMD_KEY, String::new());
        engine.set_variable(VERIFY_STATUS_KEY, "skipped".into());
    }
}

/// Mark verification passed when a matching shell_exec succeeds.
pub fn note_shell_verify_result(engine: &WorkflowEngine, command: &str, succeeded: bool) {
    let Some(expected) = engine.get_variable(VERIFY_CMD_KEY) else {
        return;
    };
    if expected.is_empty() || !commands_match(&expected, command) {
        return;
    }
    engine.set_variable(
        VERIFY_STATUS_KEY,
        if succeeded { "passed" } else { "failed" }.into(),
    );
    // Track CONSECUTIVE verify failures so the main loop can stop auto-retrying
    // a fix that never converges, instead of spinning silently. A pass clears it.
    if succeeded {
        reset_verify_failures(engine);
    } else {
        bump_verify_failures(engine);
    }
}

/// Max consecutive verify (or AST) failures on the same task before the agent
/// stops auto-retrying and hands control back to the user.
pub const MAX_CONSECUTIVE_VERIFY_FAILS: u32 = 3;

const VERIFY_FAIL_KEY: &str = "_verify_fail_streak";

/// Current consecutive verify-failure streak for this turn.
pub fn verify_fail_streak(engine: &WorkflowEngine) -> u32 {
    engine
        .get_variable(VERIFY_FAIL_KEY)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Increment the consecutive verify-failure streak and return the new value.
pub fn bump_verify_failures(engine: &WorkflowEngine) -> u32 {
    let next = verify_fail_streak(engine) + 1;
    engine.set_variable(VERIFY_FAIL_KEY, next.to_string());
    next
}

/// Clear the verify-failure streak (on a pass, or at the start of a fresh turn).
pub fn reset_verify_failures(engine: &WorkflowEngine) {
    engine.set_variable(VERIFY_FAIL_KEY, String::new());
}

/// True when consecutive failures have hit the stop threshold — the loop should
/// surface a `## Failed`-style hand-off instead of retrying again.
pub fn should_stop_on_repeated_failure(engine: &WorkflowEngine) -> bool {
    verify_fail_streak(engine) >= MAX_CONSECUTIVE_VERIFY_FAILS
}

/// Gate for `## Done` on coding Execute — AST clean + verify passed (if applicable).
pub fn check_execute_done_gate(engine: &WorkflowEngine) -> Option<String> {
    if !engine.is_task_step() || engine.is_perceive_execute() {
        return None;
    }

    let ast = read_ast_pending(engine);
    if !ast.is_empty() {
        let files: Vec<_> = ast.keys().cloned().collect();
        return Some(format!(
            "❌ 不能输出 ## Done：以下文件仍有 AST 语法错误，请先修复：\n{}\n\n\
             用 edit_file 修正后，工具结果中不应再出现 `{AST_MARKER}`。",
            files.join("\n")
        ));
    }

    let status = engine.get_variable(VERIFY_STATUS_KEY).unwrap_or_default();
    if status == "skipped" || status == "passed" {
        return None;
    }
    let cmd = engine.get_variable(VERIFY_CMD_KEY).unwrap_or_default();
    if cmd.is_empty() {
        return None;
    }
    let label = verify_label_for_command(&cmd);
    Some(format!(
        "❌ 编码完成后须验证通过才能 ## Done。\n\n\
         请用 **shell_exec** 运行（需用户确认）：\n\
         ```\n{cmd}\n```\n\
         （{label}）\n\n\
         exit code 0 后再输出 ## Done，并在摘要中写明验证结果。"
    ))
}

pub fn verify_hint_message(engine: &WorkflowEngine) -> Option<String> {
    if !engine.is_task_step() || engine.is_perceive_execute() {
        return None;
    }
    if !read_ast_pending(engine).is_empty() {
        return None;
    }
    let status = engine.get_variable(VERIFY_STATUS_KEY).unwrap_or_default();
    if status == "passed" || status == "skipped" {
        return None;
    }
    let cmd = engine.get_variable(VERIFY_CMD_KEY).unwrap_or_default();
    if cmd.is_empty() {
        return None;
    }
    let label = verify_label_for_command(&cmd);
    Some(format!(
        "📋 编码验证: 完成所有修改后，用 shell_exec 运行 `{cmd}`（{label}），\
         通过后再输出 ## Done。"
    ))
}

pub fn tool_batch_has_ast_issues(new_messages: &[Message], tool_calls: &[ToolCall]) -> bool {
    tool_calls.iter().any(|tc| {
        matches!(tc.name.as_str(), "file_write" | "edit_file")
            && tool_result_content(new_messages, &tc.id).is_some_and(|c| c.contains(AST_MARKER))
    })
}

pub fn resolve_verify_command(project_root: &Path, _touched: &[String]) -> Option<VerifyCommand> {
    if project_root.join("Cargo.toml").exists() {
        return Some(VerifyCommand {
            label: "Rust — cargo check".into(),
            command: "cargo check --message-format=short".into(),
        });
    }
    if project_root.join("go.mod").exists() {
        return Some(VerifyCommand {
            label: "Go — compile all packages".into(),
            command: "go build ./...".into(),
        });
    }
    if project_root.join("pyproject.toml").exists()
        || project_root.join("setup.py").exists()
        || project_root.join("requirements.txt").exists()
    {
        return Some(VerifyCommand {
            label: "Python — syntax compile".into(),
            command: "python -m compileall -q .".into(),
        });
    }
    if project_root.join("package.json").exists() {
        if project_root.join("tsconfig.json").exists() {
            return Some(VerifyCommand {
                label: "TypeScript — typecheck".into(),
                command: "npx tsc --noEmit".into(),
            });
        }
        return Some(VerifyCommand {
            label: "Node — npm test/build".into(),
            command: "npm run build 2>/dev/null || npm test".into(),
        });
    }
    if project_root.join("pom.xml").exists() {
        return Some(VerifyCommand {
            label: "Java — Maven compile".into(),
            command: "mvn -q -DskipTests compile".into(),
        });
    }
    if project_root.join("build.gradle").exists() || project_root.join("build.gradle.kts").exists()
    {
        return Some(VerifyCommand {
            label: "Gradle — compile".into(),
            command: "./gradlew compileJava -q".into(),
        });
    }
    if project_root.join("CMakeLists.txt").exists() {
        return Some(VerifyCommand {
            label: "C/C++ — cmake build dir".into(),
            command: "cmake --build build 2>/dev/null || make -j4".into(),
        });
    }
    None
}

fn verify_label_for_command(cmd: &str) -> &str {
    if cmd.contains("cargo") {
        "Rust"
    } else if cmd.contains("go build") {
        "Go"
    } else if cmd.contains("compileall") {
        "Python"
    } else if cmd.contains("tsc") {
        "TypeScript"
    } else if cmd.contains("npm") {
        "Node.js"
    } else if cmd.contains("mvn") {
        "Java"
    } else if cmd.contains("gradlew") {
        "Gradle"
    } else {
        "project verify"
    }
}

fn is_source_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext,
                "rs" | "py"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "mjs"
                    | "cjs"
                    | "go"
                    | "java"
                    | "kt"
                    | "kts"
                    | "cpp"
                    | "cc"
                    | "cxx"
                    | "c"
                    | "h"
                    | "hpp"
                    | "cs"
                    | "rb"
                    | "php"
                    | "swift"
                    | "scala"
            )
        })
}

fn tool_result_content(new_messages: &[Message], tool_call_id: &str) -> Option<String> {
    new_messages.iter().rev().find_map(|msg| {
        if let Message::ToolResult {
            tool_call_id: id,
            content,
        } = msg
        {
            if id == tool_call_id {
                Some(content.clone())
            } else {
                None
            }
        } else {
            None
        }
    })
}

fn tool_path(arguments: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(String::from))
}

fn extract_ast_summary(content: &str) -> String {
    content
        .lines()
        .skip_while(|l| !l.contains(AST_MARKER))
        .take(8)
        .collect::<Vec<_>>()
        .join("\n")
}

fn commands_match(expected: &str, actual: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let e = norm(expected);
    let a = norm(actual);
    a.contains(&e) || e.contains(&a)
}

fn read_touched(engine: &WorkflowEngine) -> Vec<String> {
    engine
        .get_variable(TOUCHED_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_touched(engine: &WorkflowEngine, paths: &[String]) {
    let json = serde_json::to_string(paths).unwrap_or_else(|_| "[]".into());
    engine.set_variable(TOUCHED_KEY, json);
}

fn read_ast_pending(engine: &WorkflowEngine) -> std::collections::BTreeMap<String, String> {
    engine
        .get_variable(AST_PENDING_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_ast_pending(engine: &WorkflowEngine, map: &std::collections::BTreeMap<String, String>) {
    let json = serde_json::to_string(map).unwrap_or_else(|_| "{}".into());
    engine.set_variable(AST_PENDING_KEY, json);
}

pub fn shell_result_success(tool_content: &str) -> bool {
    tool_content.contains("[exit code: 0]")
}

pub fn tool_result_ast_clean(tool_content: &str) -> bool {
    !tool_content.contains(AST_MARKER)
}

pub fn clear_verify_state(engine: &WorkflowEngine) {
    for key in [
        TOUCHED_KEY,
        VERIFY_CMD_KEY,
        VERIFY_STATUS_KEY,
        AST_PENDING_KEY,
    ] {
        engine.set_variable(key, String::new());
    }
}

/// Files that still have unresolved AST syntax errors after edits.
pub fn ast_pending_files(engine: &WorkflowEngine) -> Vec<String> {
    read_ast_pending(engine).keys().cloned().collect()
}

/// The configured verify command for touched files (empty if none).
pub fn verify_command(engine: &WorkflowEngine) -> String {
    engine.get_variable(VERIFY_CMD_KEY).unwrap_or_default()
}

/// Whether the last project verify shell failed (non-zero exit).
pub fn verify_status_failed(engine: &WorkflowEngine) -> bool {
    engine.get_variable(VERIFY_STATUS_KEY).as_deref() == Some("failed")
}

/// Whether verify is still required before ## Done.
pub fn verify_status_blocks_done(engine: &WorkflowEngine) -> bool {
    let status = engine.get_variable(VERIFY_STATUS_KEY).unwrap_or_default();
    if status == "failed" || status == "pending" {
        return true;
    }
    let cmd = engine.get_variable(VERIFY_CMD_KEY).unwrap_or_default();
    !cmd.is_empty() && status != "passed" && status != "skipped"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use std::sync::Arc;

    fn test_engine_at_execute() -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("test")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        session.blocking_lock().current_step_index = 3;
        engine
    }

    #[test]
    fn resolve_rust_verify() {
        let dir = std::env::temp_dir().join(format!("ox_verify_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
        let cmd = resolve_verify_command(&dir, &["src/main.rs".into()]).unwrap();
        assert!(cmd.command.contains("cargo check"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ast_recovery_injects_system_message() {
        let tc = ToolCall {
            id: "c1".into(),
            name: "edit_file".into(),
            arguments: r#"{"path":"src/a.rs"}"#.into(),
        };
        let new_messages = vec![Message::ToolResult {
            tool_call_id: "c1".into(),
            content: "✅ Patched\n\n⚠️ AST Syntax Check: 1 issue(s):\n   1. Syntax error at line 3: `fn foo`".into(),
        }];
        let mut messages = Vec::new();
        check_ast_and_recover(&mut messages, &new_messages, &[tc]);
        assert!(messages.iter().any(
            |m| matches!(m, Message::System { content } if content.contains("AST SYNTAX ERROR"))
        ));
    }

    #[test]
    fn done_gate_blocks_with_pending_ast() {
        let engine = test_engine_at_execute();
        engine.set_variable(
            AST_PENDING_KEY,
            r#"{"src/a.rs":"⚠️ AST Syntax Check: 1 issue"}"#.into(),
        );
        assert!(check_execute_done_gate(&engine).is_some());
    }

    #[test]
    fn done_gate_requires_verify() {
        let engine = test_engine_at_execute();
        engine.set_variable(VERIFY_CMD_KEY, "cargo check".into());
        engine.set_variable(VERIFY_STATUS_KEY, "pending".into());
        let gate = check_execute_done_gate(&engine).unwrap();
        assert!(gate.contains("cargo check"));
    }

    #[test]
    fn verify_failures_accumulate_then_trigger_stop() {
        let engine = test_engine_at_execute();
        engine.set_variable(VERIFY_CMD_KEY, "cargo check".into());
        // Each failed run of the expected command bumps the streak.
        note_shell_verify_result(&engine, "cargo check", false);
        assert_eq!(verify_fail_streak(&engine), 1);
        assert!(!should_stop_on_repeated_failure(&engine));
        note_shell_verify_result(&engine, "cargo check", false);
        note_shell_verify_result(&engine, "cargo check", false);
        assert_eq!(verify_fail_streak(&engine), 3);
        assert!(should_stop_on_repeated_failure(&engine));
    }

    #[test]
    fn verify_pass_clears_failure_streak() {
        let engine = test_engine_at_execute();
        engine.set_variable(VERIFY_CMD_KEY, "cargo check".into());
        note_shell_verify_result(&engine, "cargo check", false);
        note_shell_verify_result(&engine, "cargo check", false);
        assert_eq!(verify_fail_streak(&engine), 2);
        note_shell_verify_result(&engine, "cargo check", true);
        assert_eq!(verify_fail_streak(&engine), 0);
        assert!(!should_stop_on_repeated_failure(&engine));
    }

    #[test]
    fn shell_verify_marks_passed() {
        let engine = test_engine_at_execute();
        engine.set_variable(VERIFY_CMD_KEY, "cargo check --message-format=short".into());
        engine.set_variable(VERIFY_STATUS_KEY, "pending".into());
        note_shell_verify_result(&engine, "cargo check --message-format=short", true);
        assert_eq!(
            engine.get_variable(VERIFY_STATUS_KEY).as_deref(),
            Some("passed")
        );
    }
}
