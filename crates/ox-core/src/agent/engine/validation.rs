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
        // Edit gate: ensure file has been read before editing.
        // Exception: file_write for a NEW file (file doesn't exist yet) — no need to read first.
        // Impact analysis runs automatically in unified_handler before edit/write/delete.
        if matches!(tool_name, "edit_file" | "file_write" | "delete_range")
            && let Some(path) = args.get("path").and_then(|v| v.as_str())
            && !path.trim().is_empty()
            && !crate::agent::engine::impl_tracking::impl_file_already_read(engine, path)
        {
            // For file_write: skip the read gate if the file doesn't exist yet (new file creation).
            let is_new_file = if tool_name == "file_write" {
                let p = std::path::Path::new(path);
                if p.is_absolute() {
                    !p.exists()
                } else {
                    std::env::current_dir()
                        .ok()
                        .map(|d| !d.join(p).exists())
                        .unwrap_or(false)
                }
            } else {
                false
            };
            if !is_new_file {
                return Err(format!(
                    "📊 编辑门禁 — `{path}` 尚未读取。\n\
                                 先调用 file_read 读取文件内容再编辑。"
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