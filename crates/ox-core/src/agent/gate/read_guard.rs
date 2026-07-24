//! Prevent redundant file reads and shell-as-read after file_read.

use super::super::engine::WorkflowEngine;
use super::super::plan_tracker;

const TURN_FILES_READ_KEY: &str = "_turn_files_read";
const TURN_SYMBOLS_QUERIED_KEY: &str = "_turn_symbols_queried";

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(TURN_FILES_READ_KEY, "[]".to_string());
    clear_symbol_queries(engine);
}

/// Reset per-turn symbol search dedup (new agent spawn — file-read state may persist).
pub fn clear_symbol_queries(engine: &WorkflowEngine) {
    engine.set_variable(TURN_SYMBOLS_QUERIED_KEY, "[]".to_string());
}

pub fn record_file_read(engine: &WorkflowEngine, path: &str) {
    let norm = plan_tracker::normalize_path(path);
    let mut set: std::collections::HashSet<String> = engine
        .get_variable(TURN_FILES_READ_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if set.insert(norm)
        && let Ok(json) = serde_json::to_string(&set)
    {
        engine.set_variable(TURN_FILES_READ_KEY, json);
    }
}

pub fn paths_read(engine: &WorkflowEngine) -> Vec<String> {
    engine
        .get_variable(TURN_FILES_READ_KEY)
        .and_then(|s| serde_json::from_str::<std::collections::HashSet<String>>(&s).ok())
        .map(|s| s.into_iter().collect())
        .unwrap_or_default()
}

pub fn path_already_read(engine: &WorkflowEngine, path: &str) -> bool {
    let norm = plan_tracker::normalize_path(path);
    engine
        .get_variable(TURN_FILES_READ_KEY)
        .and_then(|s| serde_json::from_str::<std::collections::HashSet<String>>(&s).ok())
        .is_some_and(|set| set.contains(&norm))
        || crate::agent::tool_digest::get_digest(engine, path).is_some()
}

/// Block duplicate reads / shell cat-type on already-read paths.
pub fn check(
    tool_name: &str,
    args: &serde_json::Value,
    engine: &WorkflowEngine,
) -> Result<(), String> {
    match tool_name {
        "file_read" => {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if path.is_empty() {
                return Ok(());
            }
            let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
            if path_already_read(engine, path) {
                // Allow re-reads when:
                // 1. Reading a different portion (offset > 0)
                // 2. First re-read for verification (in any phase)
                // 3. Implementation phase: allow one fresh read after review
                if offset > 0 {
                    return Ok(());
                }
                // Track re-reads to allow first one but block subsequent ones
                if !engine.impl_file_already_read(path) {
                    engine.record_impl_file_read(path, "");
                    return Ok(());
                }
                return Err(format!(
                    "该文件已读过 2 次以上，建议基于已有内容推进。如需重读请加 offset。"
                ));
            }
        }
        "shell_exec" => {
            let cmd = args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if !WorkflowEngine::shell_looks_like_file_read(cmd) {
                return Ok(());
            }
            if let Some(path) = extract_path_from_shell(cmd)
                && path_already_read(engine, &path)
            {
                return Err(format!(
                    "禁止用 shell 重复读取 `{path}`（已 file_read）。请基于已有内容继续。"
                ));
            }
        }
        "find_symbol" | "code_search" | "file_search" => {
            // Simplified: only block truly duplicate queries within the same turn.
            // Phase-based gating (execute_report_already_delivered) is removed
            // to allow simple tasks to proceed without artificial barriers.
            if let Some(query) = symbol_query_key(tool_name, args)
                && symbol_already_queried(engine, &query)
            {
                return Err(format!(
                    "⛔ 禁止重复 {tool_name}({query}) — 该查询本轮已执行过。"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn record_symbol_query(engine: &WorkflowEngine, tool_name: &str, args: &serde_json::Value) {
    let Some(query) = symbol_query_key(tool_name, args) else {
        return;
    };
    let norm = query.to_lowercase();
    let mut set: std::collections::HashSet<String> = engine
        .get_variable(TURN_SYMBOLS_QUERIED_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if set.insert(norm)
        && let Ok(json) = serde_json::to_string(&set)
    {
        engine.set_variable(TURN_SYMBOLS_QUERIED_KEY, json);
    }
}

/// True when a read-only tool call would surface *new* information this turn —
/// a not-yet-read file, a further slice of a file, or a not-yet-run symbol
/// query. Structural listings count as discovery; status/recall calls do not.
///
/// Drives the exploration budget's information-gain accounting: a turn that
/// discovers something new is genuine progress and must not be penalized as
/// "circling", however large the project.
pub fn is_discovery_call(
    engine: &WorkflowEngine,
    tool_name: &str,
    args: &serde_json::Value,
) -> bool {
    match tool_name {
        "file_read" => {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if path.is_empty() {
                return false;
            }
            // Reading further into an already-read file still yields new content.
            if args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) > 0 {
                return true;
            }
            !path_already_read(engine, path)
        }
        "find_symbol" | "code_search" => match symbol_query_key(tool_name, args) {
            Some(q) => !symbol_already_queried(engine, &q),
            None => false,
        },
        // Structural exploration — almost always surfaces new layout/paths.
        "file_list" | "file_search" | "project_detect" | "code_graph" => true,
        // Read-only but not obviously novel — a repeat here is likely circling,
        // so it must NOT count as discovery (otherwise the budget never trips).
        "git_status" | "git_diff" | "load_skill" | "recall" | "web_fetch" => false,
        // Unknown tool: treat as progress (don't penalize).
        _ => true,
    }
}

fn symbol_already_queried(engine: &WorkflowEngine, query: &str) -> bool {
    let norm = query.to_lowercase();
    engine
        .get_variable(TURN_SYMBOLS_QUERIED_KEY)
        .and_then(|s| serde_json::from_str::<std::collections::HashSet<String>>(&s).ok())
        .is_some_and(|set| set.contains(&norm))
}

fn symbol_query_key(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    let raw = match tool_name {
        "find_symbol" => args
            .get("name")
            .or_else(|| args.get("query"))
            .or_else(|| args.get("symbol"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        "code_search" => args
            .get("pattern")
            .or_else(|| args.get("query"))
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(120).collect::<String>())
            .filter(|s: &String| !s.is_empty()),
        _ => None,
    }?;
    // Per-tool keys — parallel find_symbol + code_search in one LLM response must not collide.
    Some(format!("{tool_name}:{}", raw.to_lowercase()))
}

/// Return cached digest text instead of re-executing file_read.
pub fn cached_file_read_response(engine: &WorkflowEngine, path: &str) -> Option<String> {
    if !path_already_read(engine, path) {
        return None;
    }
    crate::agent::tool_digest::get_digest(engine, path).map(|d| {
        let symbols = if d.symbols.is_empty() {
            "（无）".to_string()
        } else {
            d.symbols
                .iter()
                .take(8)
                .map(|s| format!("{}@L{}-{}", s.name, s.line_start, s.line_end))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let impl_hint = "\n💡 需要更多行: file_read {\"path\":\"…\", \"offset\":N, \"limit\":200}";
        format!(
            "✅ `{path}` 本轮已读过（返回 digest，未重复 IO）\n\
             摘要: {}\n\
             符号: {symbols}{impl_hint}",
            d.summary,
        )
    })
}

fn extract_path_from_shell(cmd: &str) -> Option<String> {
    let lower = cmd.to_lowercase();
    for prefix in ["type ", "cat ", "get-content ", "head ", "tail "] {
        if let Some(rest) = lower.find(prefix) {
            let after = cmd[rest + prefix.len()..].trim();
            let path = after
                .trim_matches('"')
                .trim_matches('\'')
                .split_whitespace()
                .next()?;
            if !path.is_empty() {
                return Some(path.replace('\\', "/"));
            }
        }
    }
    None
}

pub fn provenance_paths(engine: &WorkflowEngine) -> std::collections::HashSet<String> {
    let mut set: std::collections::HashSet<String> = engine
        .get_variable(TURN_FILES_READ_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if crate::agent::phase::fix_impl_session(engine) {
        let impl_reads: std::collections::HashSet<String> = engine
            .get_variable("_impl_files_read")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        for path in impl_reads {
            set.insert(path);
        }
    }
    for d in crate::agent::tool_digest::all_digests(engine) {
        set.insert(plan_tracker::normalize_path(&d.path));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use std::sync::Arc;

    fn engine() -> WorkflowEngine {
        WorkflowEngine::new(Arc::new(tokio::sync::Mutex::new(SessionState::new("t"))))
    }

    #[test]
    fn allows_first_reread_then_blocks_duplicate() {
        let e = engine();
        record_file_read(&e, "src/a.rs");
        // First re-read is allowed (for verification)
        let args = serde_json::json!({"path": "src/a.rs"});
        assert!(check("file_read", &args, &e).is_ok());
        // Second re-read is blocked
        let args2 = serde_json::json!({"path": "src/a.rs"});
        assert!(check("file_read", &args2, &e).is_err());
    }

    #[test]
    fn allows_offset_reread_same_path() {
        let e = engine();
        record_file_read(&e, "src/a.rs");
        let args = serde_json::json!({"path": "src/a.rs", "offset": 140, "limit": 50});
        assert!(check("file_read", &args, &e).is_ok());
    }

    #[test]
    fn allows_code_search_after_review_report() {
        // Simplified: phase-based search restrictions removed.
        // Only truly duplicate queries are blocked.
        let e = engine();
        e.mark_execute_report_delivered();
        let args = serde_json::json!({"pattern": "doHandle"});
        // First search is allowed even after review report
        assert!(check("code_search", &args, &e).is_ok());
    }

    #[test]
    fn blocks_duplicate_find_symbol() {
        let e = engine();
        let args = serde_json::json!({"name": "MaintainDeliveryRequest"});
        record_symbol_query(&e, "find_symbol", &args);
        assert!(check("find_symbol", &args, &e).is_err());
    }

    #[test]
    fn find_symbol_does_not_block_code_search_same_name() {
        let e = engine();
        let sym = serde_json::json!({"name": "MaintainDeliveryStrategy"});
        let search = serde_json::json!({"pattern": "MaintainDeliveryStrategy"});
        record_symbol_query(&e, "find_symbol", &sym);
        assert!(check("code_search", &search, &e).is_ok());
    }

    #[test]
    fn implement_allows_one_fresh_read_after_review_digest() {
        use crate::agent::phase::{self, SingleFlowPhase};
        let e = engine();
        record_file_read(&e, "src/Foo.java");
        crate::agent::tool_digest::record_read(&e, "src/Foo.java", "class Foo {}", 0, Some(1));
        e.set_variable(
            phase::PHASE_STATE_KEY,
            SingleFlowPhase::Implement.as_str().to_string(),
        );
        // First re-read in implement phase is allowed (fresh context after compaction)
        assert!(
            check(
                "file_read",
                &serde_json::json!({"path": "src/Foo.java"}),
                &e
            )
            .is_ok()
        );
        e.record_impl_file_read("src/Foo.java", "{}");
        // Second re-read is blocked — already consumed the fresh-read allowance
        assert!(
            check(
                "file_read",
                &serde_json::json!({"path": "src/Foo.java"}),
                &e
            )
            .is_err()
        );
    }
}