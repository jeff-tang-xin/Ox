use super::WorkflowEngine;
use std::path::Path;

pub(crate) fn normalize_explore_path(path: &str) -> String {
    let p = path.trim().trim_matches(|c| c == '/' || c == '\\');
    if p.is_empty() {
        ".".to_string()
    } else {
        p.to_lowercase()
    }
}

pub(crate) fn record_explored_path(engine: &WorkflowEngine, tool: &str, path: &str) {
    let key = format!("{}:{}", tool, normalize_explore_path(path));
    let mut paths = get_explored_path_set(engine);
    if paths.insert(key)
        && let Ok(json) = serde_json::to_string(&paths)
    {
        engine.set_variable("_explored_paths", json);
    }
}

pub(crate) fn is_path_explored(engine: &WorkflowEngine, tool: &str, path: &str) -> bool {
    let key = format!("{}:{}", tool, normalize_explore_path(path));
    get_explored_path_set(engine).contains(&key)
}

pub(crate) fn record_exploration_result(
    engine: &WorkflowEngine,
    working_dir: &Path,
    tool: &str,
    target: &str,
    raw_result: &str,
) {
    if !crate::agent::exploration_snapshot::is_snapshot_tool(tool) {
        return;
    }
    let content = crate::agent::exploration_snapshot::extract_data_content(raw_result);
    let mut entries = get_exploration_entries(engine);
    crate::agent::exploration_snapshot::merge_entry(
        &mut entries,
        working_dir,
        tool,
        target,
        &content,
    );
    engine.set_variable(
        "_exploration_snapshot",
        crate::agent::exploration_snapshot::entries_to_json(&entries),
    );
}

pub(crate) fn exploration_snapshot_summary(engine: &WorkflowEngine) -> String {
    let entries = get_exploration_entries(engine);
    crate::agent::exploration_snapshot::format_summary(&entries, 24_000)
}

pub(crate) fn get_exploration_entries(
    engine: &WorkflowEngine,
) -> Vec<crate::agent::exploration_snapshot::ExplorationEntry> {
    engine
        .get_variable("_exploration_snapshot")
        .map(|s| crate::agent::exploration_snapshot::entries_from_json(&s))
        .unwrap_or_default()
}

pub(crate) fn lookup_exploration_cache(
    engine: &WorkflowEngine,
    working_dir: &Path,
    tool: &str,
    target: &str,
) -> Option<String> {
    if tool == "file_read" {
        let path = crate::agent::exploration_snapshot::file_path_from_target(target);
        let entries = get_exploration_entries(engine);
        if crate::agent::exploration_snapshot::find_file_read_entry(&entries, path).is_some() {
            let args = serde_json::json!({ "path": path }).to_string();
            return Some(crate::agent::exploration_snapshot::resolve_file_read_cache(
                working_dir,
                &entries,
                path,
                &args,
            ));
        }
    }

    let norm = crate::agent::plan_tracker::normalize_path(target);
    get_exploration_entries(engine)
        .into_iter()
        .find(|e| {
            e.tool == tool && crate::agent::plan_tracker::normalize_path(&e.target) == norm
        })
        .map(|e| {
            let mut out = format!(
                "✅ 【缓存】已探索过 `{target}`（勿重复 {tool}）\n\n{}",
                e.content
            );
            if let Some(ref rp) = e.ref_path {
                out.push_str(&format!("\n\n完整快照: `{rp}`"));
            }
            out
        })
}

pub(crate) fn lookup_execute_exploration_cache(
    engine: &WorkflowEngine,
    working_dir: &Path,
    tool: &str,
    arguments: &str,
) -> Option<String> {
    let target = crate::agent::exploration_snapshot::target_from_tool_args(tool, arguments);
    if tool == "file_read"
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(path) = v.get("path").and_then(|p| p.as_str())
    {
        let entries = get_exploration_entries(engine);
        if crate::agent::exploration_snapshot::find_file_read_entry(&entries, path).is_some()
            || is_path_explored(engine, "file_read", path)
        {
            return Some(crate::agent::exploration_snapshot::resolve_file_read_cache(
                working_dir,
                &entries,
                path,
                arguments,
            ));
        }
    }
    if let Some(hit) = lookup_exploration_cache(engine, working_dir, tool, &target) {
        return Some(hit);
    }
    if matches!(tool, "code_search" | "find_symbol" | "file_search")
        && is_path_explored(engine, tool, &target)
    {
        return lookup_exploration_cache(engine, working_dir, tool, &target);
    }
    None
}

pub(crate) fn has_file_read_snapshot(engine: &WorkflowEngine, path: &str) -> bool {
    crate::agent::exploration_snapshot::find_file_read_entry(
        &get_exploration_entries(engine),
        path,
    )
    .is_some()
}

pub(crate) fn get_explored_path_set(
    engine: &WorkflowEngine,
) -> std::collections::HashSet<String> {
    engine
        .get_variable("_explored_paths")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn shell_looks_like_file_read(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    [
        "cat ",
        "type ",
        "head ",
        "tail ",
        "more ",
        "less ",
        "get-content",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

pub(crate) fn tool_calls_are_reexplore_only(tool_calls: &[crate::message::ToolCall]) -> bool {
    !tool_calls.is_empty()
        && tool_calls.iter().all(|tc| {
            matches!(
                tc.name.as_str(),
                "file_read" | "file_list" | "code_search" | "find_symbol" | "file_search"
            )
        })
}