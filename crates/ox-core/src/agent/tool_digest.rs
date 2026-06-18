//! Semantic digests for file_read results — keeps LLM context lean.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;
use super::plan_tracker;

const DIGESTS_KEY: &str = "_file_digests";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FileDigest {
    pub path: String,
    pub summary: String,
    pub symbols: Vec<SymbolRef>,
    pub line_count: usize,
    pub linked_findings: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolRef {
    pub name: String,
    pub line_start: u32,
    pub line_end: u32,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DigestStore {
    entries: std::collections::HashMap<String, FileDigest>,
}

pub fn record_read(
    engine: &WorkflowEngine,
    path: &str,
    content: &str,
    offset: u32,
    finding_index: Option<u32>,
) {
    let norm = plan_tracker::normalize_path(path);
    let digest = build_digest(path, content, offset, finding_index);
    let mut store = load_store(engine);
    if let Some(idx) = finding_index {
        if let Some(existing) = store.entries.get_mut(&norm) {
            if !existing.linked_findings.contains(&idx) {
                existing.linked_findings.push(idx);
            }
            merge_digest(existing, &digest);
        } else {
            store.entries.insert(norm, digest);
        }
    } else {
        store.entries.insert(norm, digest);
    }
    save_store(engine, &store);
}

pub fn get_digest(engine: &WorkflowEngine, path: &str) -> Option<FileDigest> {
    let norm = plan_tracker::normalize_path(path);
    load_store(engine).entries.get(&norm).cloned()
}

pub fn all_digests(engine: &WorkflowEngine) -> Vec<FileDigest> {
    load_store(engine).entries.into_values().collect()
}

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(DIGESTS_KEY, String::new());
}

/// Tool result shown in message history (full content stays in exploration dir).
pub fn format_tool_result_for_history(path: &str, content: &str, digest: &FileDigest) -> String {
    let preview: String = content.lines().take(8).collect::<Vec<_>>().join("\n");
    format!(
        "📄 `{path}` (digest)\n\
         {summary}\n\
         符号: {symbols}\n\
         ---\n\
         {preview}\n\
         …（完整内容见 .ox/exploration/ 或续读 offset）",
        summary = digest.summary,
        symbols = if digest.symbols.is_empty() {
            "（无）".to_string()
        } else {
            digest
                .symbols
                .iter()
                .take(6)
                .map(|s| format!("{}@L{}-{}", s.name, s.line_start, s.line_end))
                .collect::<Vec<_>>()
                .join(", ")
        }
    )
}

fn build_digest(path: &str, content: &str, offset: u32, finding_index: Option<u32>) -> FileDigest {
    let lines: Vec<&str> = content.lines().collect();
    let symbols = extract_symbols(&lines, offset);
    let summary = summarize_content(path, &lines, &symbols);
    FileDigest {
        path: path.to_string(),
        summary,
        symbols,
        line_count: lines.len(),
        linked_findings: finding_index.into_iter().collect(),
    }
}

fn merge_digest(existing: &mut FileDigest, new: &FileDigest) {
    if new.summary.len() > existing.summary.len() {
        existing.summary = new.summary.clone();
    }
    existing.line_count = existing.line_count.saturating_add(new.line_count);
    for s in &new.symbols {
        if !existing.symbols.iter().any(|e| e.name == s.name && e.line_start == s.line_start) {
            existing.symbols.push(s.clone());
        }
    }
}

fn extract_symbols(lines: &[&str], line_offset: u32) -> Vec<SymbolRef> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let (name, role) = if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("fn ")
            || trimmed.contains(" fn ")
        {
            let name = trimmed
                .split('(')
                .next()
                .and_then(|s| s.split_whitespace().last())
                .unwrap_or("fn")
                .to_string();
            (name, "function".to_string())
        } else if trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ") {
            let name = trimmed
                .split_whitespace()
                .nth(1)
                .unwrap_or("struct")
                .trim_end_matches('{')
                .to_string();
            (name, "struct".to_string())
        } else if trimmed.starts_with("pub enum ") || trimmed.starts_with("enum ") {
            let name = trimmed
                .split_whitespace()
                .nth(1)
                .unwrap_or("enum")
                .trim_end_matches('{')
                .to_string();
            (name, "enum".to_string())
        } else if trimmed.starts_with("impl ") {
            let name = trimmed
                .split_whitespace()
                .nth(1)
                .unwrap_or("impl")
                .to_string();
            (name, "impl".to_string())
        } else {
            continue;
        };
        let line = line_offset + i as u32 + 1;
        out.push(SymbolRef {
            name,
            line_start: line,
            line_end: line,
            role,
        });
    }
    out.truncate(12);
    out
}

fn summarize_content(path: &str, lines: &[&str], symbols: &[SymbolRef]) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    let sym = if symbols.is_empty() {
        String::new()
    } else {
        format!(
            "；符号: {}",
            symbols
                .iter()
                .take(4)
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "已读 `{}`（{} 行，{}）{}",
        path,
        lines.len(),
        ext,
        sym
    )
}

fn load_store(engine: &WorkflowEngine) -> DigestStore {
    engine
        .get_variable(DIGESTS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_store(engine: &WorkflowEngine, store: &DigestStore) {
    if let Ok(json) = serde_json::to_string(store) {
        engine.set_variable(DIGESTS_KEY, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_rust_fn() {
        let content = "pub fn foo() {\n}\npub struct Bar {\n}\n";
        let d = build_digest("a.rs", content, 0, Some(1));
        assert!(d.symbols.iter().any(|s| s.name == "foo"));
        assert!(d.symbols.iter().any(|s| s.name == "Bar"));
    }
}
