use super::WorkflowEngine;

pub(crate) fn is_code_modifying_tool(tool_name: &str) -> bool {
    matches!(tool_name, "file_write" | "edit_file" | "delete_range")
}

pub(crate) fn validate_single_step_tool(
    engine: &WorkflowEngine,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), String> {
    // 📌 代码修改门禁已移除 — edit_file/file_write/delete_range 在任何阶段都可直接执行。
    // 安全控制由 unified_safety_gate() 在工具执行层兜底（写操作仍需用户确认）。
    // 之前的 allows_code_modification() 门禁会导致 LLM 困惑："工具存在但调用被拒"。
    //
    // NOTE: phase==Complete is intentionally NOT a hard block. `finish` is the
    // LLM's explicit end and yields the turn back to the user; gates/tools must
    // never forbid future actions. The next user round resets the workflow.
    let _ = engine; // engine no longer used for gate checks, but kept for future extensions
    let _ = tool_name;

    // read_guard::check is intentionally NOT called here. It is a
    // stateful gate (records impl_file_read on first re-read) and MUST run
    // exactly once per tool call. Both execution paths invoke it directly
    // next to their cached-response fallback: mod.rs (legacy tool loop) and
    // unified_handler.rs (delegate path). Calling it here too caused a
    // double-invocation that consumed the "allow one re-read" budget in the
    // first call, then wrongly rejected the same read in the second. See P2.

    if tool_name == "file_read"
        && let Some(path) = args.get("path").and_then(|v| v.as_str())
    {
        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
        validate_impl_file_read(engine, path, offset)?;
    }

    Ok(())
}

pub(crate) fn validate_impl_file_read(
    _engine: &WorkflowEngine,
    _path: &str,
    _off: u64,
) -> Result<(), String> {
    Ok(())
}

pub(crate) fn impl_file_read_count(engine: &WorkflowEngine, norm_path: &str) -> usize {
    let key = &format!("{}:{}", super::impl_tracking::IMPL_READ_KEY, norm_path);
    engine
        .get_variable(key)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
}

pub(crate) fn impl_edit_nudge_after_read(
    _engine: &WorkflowEngine,
    _path: &str,
    _preview: &str,
) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionState;
    use crate::agent::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn single_step_engine() -> WorkflowEngine {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine
    }

    // P2 regression: validate_single_step_tool must NOT invoke the stateful
    // read_guard::check. Before the fix it did, consuming the "allow one
    // re-read" budget here and then double-blocking the same read at the
    // caller's own read_guard::check. Proof: after validating a file_read on
    // an already-read path, the re-read allowance is still intact.
    #[test]
    fn single_step_validation_does_not_consume_reread_budget() {
        let e = single_step_engine();
        crate::agent::gate::read_guard::record_file_read(&e, "src/a.rs");
        assert!(!e.impl_file_already_read("src/a.rs"));

        let args = serde_json::json!({"path": "src/a.rs"});
        // Validation itself passes and leaves the re-read budget untouched.
        assert!(validate_single_step_tool(&e, "file_read", &args).is_ok());
        assert!(
            !e.impl_file_already_read("src/a.rs"),
            "validate_single_step_tool must not consume the re-read allowance"
        );

        // The single stateful gate still lives at the caller and works once.
        assert!(crate::agent::gate::read_guard::check("file_read", &args, &e).is_ok());
    }
}
