//! Prevent redundant file reads and shell-as-read after file_read.

use super::engine::WorkflowEngine;
use super::plan_tracker;

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
    if set.insert(norm) {
        if let Ok(json) = serde_json::to_string(&set) {
            engine.set_variable(TURN_FILES_READ_KEY, json);
        }
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
            let offset = args
                .get("offset")
                .and_then(|o| o.as_u64())
                .unwrap_or(0);
            // Implement: one fresh read per file even if review already digested it.
            if crate::agent::phase::fix_impl_session(engine) && offset == 0 {
                if !engine.impl_file_already_read(path) {
                    return Ok(());
                }
            }
            if path_already_read(engine, path) {
                // Offset reads fetch a different line range (prompt promises this).
                if offset > 0 {
                    return Ok(());
                }
                if crate::agent::phase::fix_impl_session(engine) {
                    if let Some(d) = crate::agent::tool_digest::get_digest(engine, path) {
                        let symbols = if d.symbols.is_empty() {
                            String::new()
                        } else {
                            d.symbols
                                .iter()
                                .take(4)
                                .map(|s| format!("{}@L{}-{}", s.name, s.line_start, s.line_end))
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        return Err(format!(
                            "文件 `{path}` 审查期已读过（digest 在 [WORKSPACE]）。\
                             实施阶段请**直接 edit_file**；符号行号: {symbols}。\
                             若必须续读指定行段，file_read 带 offset>0。"
                        ));
                    }
                }
                return Err(format!(
                    "文件 `{path}` 本轮已读过。请使用 [WORKSPACE].file_digests / 上条 ToolResult，\
                     勿重复 file_read；需要更多行时用 file_read 并设置 offset>0。"
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
            if let Some(path) = extract_path_from_shell(cmd) {
                if path_already_read(engine, &path) {
                    return Err(format!(
                        "禁止用 shell 重复读取 `{path}`（已 file_read）。请基于已有内容继续。"
                    ));
                }
            }
        }
        "find_symbol" | "code_search" | "file_search" | "recall" | "memory_search" => {
            if crate::agent::phase::fix_impl_session(engine)
                && matches!(tool_name, "code_search" | "file_search")
            {
                return Err(format!(
                    "实施阶段禁止广泛探索 `{tool_name}`。架构/约定用 `memory_search`；\
                     定位符号用 `find_symbol`；改代码前按 [WORKSPACE] 先 `file_read`。"
                ));
            }
            if engine.execute_report_already_delivered()
                && crate::agent::findings::load_or_migrate(engine)
                    .is_some_and(|s| !s.findings.is_empty())
                && !crate::agent::phase::fix_impl_session(engine)
            {
                return Err(format!(
                    "审查报告已提交，禁止 `{tool_name}`。补全 ## Done 或等待用户说「修复」进入实施。"
                ));
            }
            if let Some(query) = symbol_query_key(tool_name, args) {
                if symbol_already_queried(engine, &query) {
                    return Err(format!(
                        "符号/查询 `{query}` 本轮已搜过。请使用 [STEP_MEMORY] / 上条 ToolResult，勿重复 {tool_name}。"
                    ));
                }
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
    if set.insert(norm) {
        if let Ok(json) = serde_json::to_string(&set) {
            engine.set_variable(TURN_SYMBOLS_QUERIED_KEY, json);
        }
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
            .get("query")
            .or_else(|| args.get("symbol"))
            .or_else(|| args.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        "code_search" => args
            .get("query")
            .or_else(|| args.get("pattern"))
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
        let impl_hint = if crate::agent::phase::fix_impl_session(engine) {
            "\n💡 实施阶段：用以上行号直接 edit_file；需关联符号可 find_symbol，勿 code_search。"
        } else {
            "\n💡 需要更多行: file_read {\"path\":\"…\", \"offset\":N, \"limit\":200}"
        };
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
    fn blocks_duplicate_file_read() {
        let e = engine();
        record_file_read(&e, "src/a.rs");
        let args = serde_json::json!({"path": "src/a.rs"});
        assert!(check("file_read", &args, &e).is_err());
    }

    #[test]
    fn allows_offset_reread_same_path() {
        let e = engine();
        record_file_read(&e, "src/a.rs");
        let args = serde_json::json!({"path": "src/a.rs", "offset": 140, "limit": 50});
        assert!(check("file_read", &args, &e).is_ok());
    }

    #[test]
    fn blocks_code_search_after_review_report() {
        use crate::agent::findings::{self, Finding, FindingsStore, FindingStatus, Severity};
        let e = engine();
        e.mark_execute_report_delivered();
        findings::save(
            &e,
            &FindingsStore {
                summary: "1 issue".into(),
                findings: vec![Finding {
                    index: 1,
                    severity: Severity::High,
                    file: "a.java".into(),
                    symbol: String::new(),
                    issue: "i".into(),
                    recommendation: String::new(),
                    status: FindingStatus::Open,
                    user_notes: vec![],
                    dispute: None,
                    impl_log: vec![],
                }],
                active_indices: vec![1],
            },
        );
        let args = serde_json::json!({"query": "doHandle"});
        assert!(check("code_search", &args, &e).is_err());
    }

    #[test]
    fn blocks_duplicate_find_symbol() {
        let e = engine();
        let args = serde_json::json!({"query": "MaintainDeliveryRequest"});
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
        crate::agent::tool_digest::record_read(
            &e,
            "src/Foo.java",
            "class Foo {}",
            0,
            Some(1),
        );
        e.set_variable(
            phase::PHASE_STATE_KEY,
            SingleFlowPhase::Implement.as_str().to_string(),
        );
        assert!(check(
            "file_read",
            &serde_json::json!({"path": "src/Foo.java"}),
            &e
        )
        .is_ok());
        e.record_impl_file_read("src/Foo.java", "{}");
        assert!(check(
            "file_read",
            &serde_json::json!({"path": "src/Foo.java"}),
            &e
        )
        .is_err());
    }
}
