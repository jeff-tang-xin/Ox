use super::WorkflowEngine;

pub(crate) fn is_code_modifying_tool(tool_name: &str) -> bool {
    matches!(tool_name, "file_write" | "edit_file" | "delete_range")
}

pub(crate) fn validate_single_step_tool(
    engine: &WorkflowEngine,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), String> {
    // Business gate: only block write/edit/shell tools, not read-only tools.
    // LLM must be able to file_read during scope discussion.
    if crate::agent::gate::business_gate::is_pending_scope(engine)
        && is_code_modifying_tool(tool_name)
    {
        return Err(
            "⏸️ 业务流程门禁 — 等待用户确认 findings 范围（c /confirm）；讨论请直接输入文字。"
                .to_string(),
        );
    }
    // NOTE: phase==Complete is intentionally NOT a hard block. `finish` is the
    // LLM's explicit end and yields the turn back to the user; gates/tools must
    // never forbid future actions. The next user round resets the workflow.

    if !engine.allows_code_modification() && is_code_modifying_tool(tool_name) {
        return Err(format!(
            "🔒 只读阶段 — 动手前先 finish(finding_json=[...]) 提交计划，用户 c 确认后解锁。禁止 {tool_name}。"
        ));
    }

    if crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::Implement {
        if matches!(tool_name, "file_search" | "file_list") {
            return Err(format!(
                "实施阶段禁止 {tool_name} — 用 find_symbol/file_read 定位。"
            ));
        }
        // Impact gate: require code_graph impact analysis before reading/editing
        // finding-related files, so the LLM understands the blast radius.
        if matches!(tool_name, "file_read" | "edit_file")
            && let Some(path) = args.get("path").and_then(|v| v.as_str())
            && !path.trim().is_empty()
        {
            let target_val = serde_json::Value::String(path.to_string());
            if let Some(idx) =
                crate::agent::findings::finding_index_for_target(engine, &target_val)
                && !engine.impl_impact_done(idx)
            {
                return Err(format!(
                    "📊 影响范围门禁 — 编辑 `{path}` 前请先评估改动影响。\n\
                                 先调用 complete_and_check(action=\"code_graph\", \
                                 params={{\"op\":\"impact\",\"target\":\"{symbol}\",\"direction\":\"downstream\"}}) \
                                 查看调用链影响范围。",
                    symbol = path
                        .rsplit('/')
                        .next_back()
                        .unwrap_or(path)
                        .rsplit('\\')
                        .next_back()
                        .unwrap_or(path)
                        .rsplit('.')
                        .next()
                        .unwrap_or(path)
                ));
            }
        }
    }

    crate::agent::gate::read_guard::check(tool_name, args, engine)?;

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
    _offset: u64,
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