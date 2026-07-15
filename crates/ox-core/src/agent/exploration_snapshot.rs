//! Exploration snapshot — persists Plan-step tool results for later workflow steps.
//!
//! Large `file_read` results are written to `.ox/exploration/` in full; only a
//! structural preview goes into the prompt. Review / Execute can `file_read` the
//! ref path to recover anything missing from the preview.
//!
//! Also updated during Execute so within-step tool loops retain durable refs.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// One captured tool result from Plan-step exploration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplorationEntry {
    pub tool: String,
    pub target: String,
    /// Preview text injected into prompts (may be a smart excerpt).
    pub content: String,
    /// Relative path to the full payload on disk (project-root relative).
    #[serde(default)]
    pub ref_path: Option<String>,
    /// Original payload size in characters.
    #[serde(default)]
    pub full_chars: usize,
}

const MAX_ENTRIES: usize = 40;
/// `file_read` results below 512KB stay inline (aligned with on-disk read gate).
const FILE_READ_INLINE_MAX_CHARS: usize = crate::tools::file_read::INLINE_CONTENT_THRESHOLD;
/// Other exploration tools: smaller inline budget.
const DEFAULT_INLINE_MAX_CHARS: usize = 6_000;
/// Default preview size for prompt injection when content is offloaded to disk.
const PREVIEW_MAX_CHARS: usize = 3_500;
/// Total prompt budget for the formatted snapshot block.
const MAX_TOTAL_INJECT_CHARS: usize = 24_000;

/// Tools snapshotted during Plan exploration.
pub fn is_snapshot_tool(tool: &str) -> bool {
    matches!(
        tool,
        "file_list"
            | "file_read"
            | "project_detect"
            | "find_symbol"
            | "code_search"
            | "file_search"
            | "load_skill"
    )
}

/// Tools snapshotted during Execute (within-step iteration memory).
pub fn is_execute_snapshot_tool(tool: &str) -> bool {
    matches!(
        tool,
        "file_read"
            | "find_symbol"
            | "code_search"
            | "file_search"
            | "file_write"
            | "edit_file"
            | "delete_range"
            | "shell_exec"
    )
}

pub fn should_snapshot_for_step(step_index: usize, tool: &str) -> bool {
    match step_index {
        1 => is_snapshot_tool(tool),
        3 => is_execute_snapshot_tool(tool),
        _ => false,
    }
}

/// Extract the payload inside a `── DATA (tool) ──` block, or return the raw string.
pub fn extract_data_content(formatted: &str) -> String {
    const START: &str = "── DATA (";
    const END: &str = "\n── END DATA ──";
    if let Some(s) = formatted.find(START) {
        let rest = &formatted[s + START.len()..];
        if let Some(name_end) = rest.find(") ──\n") {
            let content_start = s + START.len() + name_end + ") ──\n".len();
            if let Some(e) = formatted[content_start..].find(END) {
                return formatted[content_start..content_start + e].to_string();
            }
        }
    }
    formatted.to_string()
}

/// Derive a stable target key from tool arguments (path, query, skill name, etc.).
pub fn target_from_tool_args(tool: &str, arguments: &str) -> String {
    if tool == crate::agent::unified_action::TOOL_NAME {
        if let Ok(req) = crate::agent::unified_action::parse_request(arguments) {
            if let Some(inner) = crate::agent::unified_action::action_to_tool_name(&req.action) {
                return target_from_tool_args(inner, &req.params.to_string());
            }
            return req.action;
        }
        return "complete_and_check".into();
    }
    let v = serde_json::from_str::<serde_json::Value>(arguments).ok();
    match tool {
        "file_list" | "file_read" | "file_write" | "edit_file" => {
            let path = v
                .as_ref()
                .and_then(|j| {
                    j.get("path")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| ".".into());
            if tool == "file_read" {
                let offset = v
                    .as_ref()
                    .and_then(|j| j.get("offset").and_then(|o| o.as_u64()))
                    .unwrap_or(0);
                let limit = v
                    .as_ref()
                    .and_then(|j| j.get("limit").and_then(|l| l.as_u64()))
                    .unwrap_or(200);
                format!("{path}@{offset}+{limit}")
            } else {
                path
            }
        }
        "find_symbol" => v
            .and_then(|j| {
                j.get("name")
                    .or_else(|| j.get("query"))
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default(),
        "code_search" | "file_search" => v
            .and_then(|j| {
                j.get("query")
                    .or_else(|| j.get("pattern"))
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default(),
        "load_skill" => v
            .and_then(|j| {
                j.get("name")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default(),
        "shell_exec" => v
            .and_then(|j| {
                j.get("command")
                    .or_else(|| j.get("cmd"))
                    .and_then(|p| p.as_str())
                    .map(|s| s.chars().take(80).collect::<String>())
            })
            .unwrap_or_default(),
        "project_detect" => ".".into(),
        _ => String::new(),
    }
}

fn inline_threshold(tool: &str) -> usize {
    match tool {
        "file_read" => FILE_READ_INLINE_MAX_CHARS,
        "file_list" | "project_detect" | "load_skill" => DEFAULT_INLINE_MAX_CHARS,
        "shell_exec" => 2_000,
        "file_write" | "edit_file" | "delete_range" => 1_500,
        _ => 8_000,
    }
}

/// Strip `path@offset+limit` down to `path`.
pub fn file_path_from_target(target: &str) -> &str {
    target.split('@').next().unwrap_or(target)
}

fn persisted_body(stored: &str) -> &str {
    const SEP: &str = "\n---\n\n";
    stored
        .find(SEP)
        .map(|i| &stored[i + SEP.len()..])
        .unwrap_or(stored)
}

/// Load full payload previously written under `.ox/exploration/`.
pub fn load_persisted_full(working_dir: &Path, rel: &str) -> Option<String> {
    let path = working_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let stored = fs::read_to_string(&path).ok()?;
    Some(persisted_body(&stored).to_string())
}

pub fn find_file_read_entry<'a>(
    entries: &'a [ExplorationEntry],
    path: &str,
) -> Option<&'a ExplorationEntry> {
    let norm = crate::agent::plan_tracker::normalize_path(path);
    entries.iter().find(|e| {
        e.tool == "file_read"
            && crate::agent::plan_tracker::normalize_path(file_path_from_target(&e.target)) == norm
    })
}

fn file_read_offset_limit(arguments: &str) -> (usize, usize) {
    let v = serde_json::from_str::<serde_json::Value>(arguments).ok();
    let offset = v
        .as_ref()
        .and_then(|j| j.get("offset").and_then(|o| o.as_u64()))
        .unwrap_or(0) as usize;
    let limit = v
        .as_ref()
        .and_then(|j| j.get("limit").and_then(|l| l.as_u64()))
        .unwrap_or(200) as usize;
    (offset, limit)
}

/// When exploration cache blocks a duplicate `file_read`, return real file bytes — not a preview.
pub fn resolve_file_read_cache(
    working_dir: &Path,
    entries: &[ExplorationEntry],
    path: &str,
    arguments: &str,
) -> String {
    let (offset, limit) = file_read_offset_limit(arguments);
    let entry = find_file_read_entry(entries, path);

    if let Ok(body) = crate::tools::file_read::read_file_slice(working_dir, path, offset, limit) {
        return format!(
            "✅ 【快照恢复】`{path}` 已探索过；以下为磁盘完整内容（offset={offset}，非预览）\n\n{body}"
        );
    }

    if let Some(e) = entry {
        if let Some(ref rel) = e.ref_path
            && let Some(full) = load_persisted_full(working_dir, rel)
        {
            let lines: Vec<&str> = full.lines().collect();
            let total = lines.len();
            let start = offset.min(total);
            let end = (start + limit).min(total);
            let slice: String = lines[start..end]
                .iter()
                .enumerate()
                .map(|(i, line)| format!("{:>4}\t{line}", start + i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            let mut body = format!(
                "✅ 【快照恢复】`{path}` 来自探索存档 `{rel}`（{} 行全文，非预览）\n\n{slice}",
                total
            );
            if end < total {
                body.push_str(&format!(
                        "\n\n💡 续读: file_read {{\"path\":\"{path}\", \"offset\":{end}, \"limit\":{limit}}}"
                    ));
            }
            return body;
        }
        if e.full_chars <= e.content.chars().count().saturating_add(80) {
            return format!("✅ 【缓存】`{path}` 已探索过\n\n{}", e.content);
        }
        return format!(
            "✅ 【缓存预览】`{path}` 已探索过（中间行已省略）。请用 file_read 并指定 offset 续读，或读 `{}`\n\n{}",
            e.ref_path.as_deref().unwrap_or("源文件路径"),
            e.content
        );
    }

    format!("✅ 【缓存】`{path}` 已探索过（无快照条目）")
}

fn should_preview_only(_tool: &str, content: &str, threshold: usize) -> bool {
    content.chars().count() > threshold
}

fn exploration_ref_path(working_dir: &Path, tool: &str, target: &str) -> PathBuf {
    let safe: String = target
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = if safe.is_empty() {
        tool.to_string()
    } else {
        format!("{tool}_{safe}")
    };
    working_dir
        .join(".ox")
        .join("exploration")
        .join(format!("{name}.md"))
}

fn persist_full_content(
    working_dir: &Path,
    tool: &str,
    target: &str,
    content: &str,
) -> Option<String> {
    let path = exploration_ref_path(working_dir, tool, target);
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return None;
    }
    let doc = format!(
        "# Exploration snapshot\n\n**Tool**: {tool}\n**Target**: {target}\n**Size**: {} chars\n\n---\n\n{content}",
        content.chars().count()
    );
    if fs::write(&path, &doc).is_err() {
        return None;
    }
    path.strip_prefix(working_dir)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// Build a prompt-safe preview that preserves structure for code files.
pub fn build_preview(tool: &str, target: &str, content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    match tool {
        "file_read" => build_code_preview(trimmed, target, max_chars),
        _ => head_tail_preview(trimmed, max_chars),
    }
}

fn head_tail_preview(content: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let head: String = lines
        .iter()
        .take(40)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let tail: String = lines
        .iter()
        .rev()
        .take(20)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = format!("({total} lines total)\n【开头】\n{head}");
    if total > 60 {
        out.push_str("\n…\n【末尾】\n");
        out.push_str(&tail);
    }
    truncate_chars(&out, max_chars)
}

fn build_code_preview(content: &str, path: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let is_code = crate::source_paths::is_source_code_path(path);

    let mut parts = vec![format!(
        "({total} lines, {} chars)",
        content.chars().count()
    )];

    if is_code {
        let sig_prefixes = [
            "pub fn ",
            "fn ",
            "pub async fn ",
            "async fn ",
            "pub struct ",
            "struct ",
            "pub enum ",
            "enum ",
            "pub trait ",
            "trait ",
            "impl ",
            "pub mod ",
            "mod ",
            "pub type ",
            "type ",
            "pub const ",
            "const ",
        ];
        let mut sigs: Vec<String> = Vec::new();
        for line in &lines {
            let t = line.trim();
            if sig_prefixes.iter().any(|p| t.starts_with(p)) {
                sigs.push(line.to_string());
                if sigs.len() >= 50 {
                    break;
                }
            }
        }
        if !sigs.is_empty() {
            parts.push("【结构摘要】".into());
            parts.extend(sigs);
        }
    }

    let head: String = lines
        .iter()
        .take(35)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    parts.push("【文件开头】".into());
    parts.push(head);
    if total > 55 {
        let tail: String = lines
            .iter()
            .rev()
            .take(18)
            .rev()
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        parts.push("…".into());
        parts.push("【文件末尾】".into());
        parts.push(tail);
    }

    truncate_chars(&parts.join("\n"), max_chars)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max).collect();
    format!("{t}…")
}

fn entry_key(tool: &str, target: &str) -> String {
    format!("{}:{}", tool.to_lowercase(), target.to_lowercase())
}

/// Merge a new tool result into the snapshot (dedupe by tool+target).
pub fn merge_entry(
    entries: &mut Vec<ExplorationEntry>,
    working_dir: &Path,
    tool: &str,
    target: &str,
    content: &str,
) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return;
    }

    let full_chars = trimmed.chars().count();
    let threshold = inline_threshold(tool);
    let (ref_path, preview) = if should_preview_only(tool, trimmed, threshold) {
        let rel = persist_full_content(working_dir, tool, target, trimmed);
        let preview = build_preview(tool, target, trimmed, PREVIEW_MAX_CHARS);
        (rel, preview)
    } else {
        (None, trimmed.to_string())
    };

    let key = entry_key(tool, target);
    if let Some(existing) = entries
        .iter_mut()
        .find(|e| entry_key(&e.tool, &e.target) == key)
    {
        existing.content = preview;
        existing.ref_path = ref_path.or_else(|| existing.ref_path.clone());
        existing.full_chars = full_chars;
        return;
    }

    if entries.len() >= MAX_ENTRIES {
        entries.remove(0);
    }
    entries.push(ExplorationEntry {
        tool: tool.to_string(),
        target: target.to_string(),
        content: preview,
        ref_path,
        full_chars,
    });
}

pub fn entries_from_json(s: &str) -> Vec<ExplorationEntry> {
    serde_json::from_str(s).unwrap_or_default()
}

pub fn entries_to_json(entries: &[ExplorationEntry]) -> String {
    serde_json::to_string(entries).unwrap_or_else(|_| "[]".to_string())
}

/// Format snapshot for injection into Review / Execute step prompts.
///
/// Every entry is listed; large payloads point to `.ox/exploration/` refs.
pub fn format_summary(entries: &[ExplorationEntry], max_chars: usize) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let budget = max_chars.min(MAX_TOTAL_INJECT_CHARS);

    let mut blocks: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let header = if entry.target.is_empty() {
            format!("### {}", entry.tool)
        } else {
            format!("### {}({})", entry.tool, entry.target)
        };
        let meta = if let Some(ref path) = entry.ref_path {
            format!(
                "💾 完整内容: `{path}`（{} 字符）— 计划阶段已读取，审查/执行勿重复 file_read\n",
                entry.full_chars
            )
        } else if entry.full_chars > entry.content.chars().count() {
            format!("({} 字符)\n", entry.full_chars)
        } else {
            String::new()
        };
        blocks.push(format!("{header}\n{meta}{}", entry.content));
    }

    let mut out = blocks.join("\n\n");
    if out.chars().count() <= budget {
        return out;
    }

    // Shrink previews proportionally — never drop entries.
    let overhead: usize = blocks
        .iter()
        .map(|b| b.len().saturating_sub(500.min(b.len())))
        .sum();
    let preview_budget = budget.saturating_sub(overhead.min(budget / 2));
    let per_entry = (preview_budget / entries.len().max(1)).max(400);

    let mut shrunk = Vec::new();
    for entry in entries {
        let header = if entry.target.is_empty() {
            format!("### {}", entry.tool)
        } else {
            format!("### {}({})", entry.tool, entry.target)
        };
        let meta = if let Some(ref path) = entry.ref_path {
            format!(
                "💾 完整内容: `{path}`（{} 字符）— 计划阶段已读取，审查勿重复 file_read\n",
                entry.full_chars
            )
        } else {
            String::new()
        };
        let preview = build_preview(&entry.tool, &entry.target, &entry.content, per_entry);
        shrunk.push(format!("{header}\n{meta}{preview}"));
    }
    out = shrunk.join("\n\n");
    if out.chars().count() > budget {
        out = truncate_chars(&out, budget);
        out.push_str("\n\n（预览已压缩；大文件请 file_read `.ox/exploration/` 下对应路径）");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn extract_data_block() {
        let raw = "── DATA (file_read) ──\nfn main() {}\n── END DATA ──";
        assert_eq!(extract_data_content(raw), "fn main() {}");
    }

    #[test]
    fn large_file_read_persisted_with_ref() {
        let dir = temp_dir().join(format!("ox_explore_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let body = format!("{}\n", "line\n".repeat(110_000)); // > 512KB
        let mut entries = Vec::new();
        merge_entry(&mut entries, &dir, "file_read", "src/big.rs", &body);

        assert_eq!(entries.len(), 1);
        assert!(entries[0].ref_path.is_some());
        assert!(entries[0].full_chars > FILE_READ_INLINE_MAX_CHARS);
        assert!(entries[0].content.contains("lines"));

        let rel = entries[0].ref_path.as_ref().unwrap();
        let full_path = dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(full_path.exists());
        let stored = fs::read_to_string(&full_path).unwrap();
        assert!(stored.contains("line"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn small_file_stays_inline() {
        let dir = temp_dir().join(format!("ox_explore_inline_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut entries = Vec::new();
        merge_entry(&mut entries, &dir, "file_read", "a.rs", "fn main() {}");
        assert!(entries[0].ref_path.is_none());
        assert_eq!(entries[0].content, "fn main() {}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_preview_keeps_signatures() {
        let src = "use std::io;\n\npub fn handle_key() {}\npub struct App;\n\nfn helper() {\n    loop {}\n}\n";
        let big = src.repeat(200);
        let preview = build_code_preview(&big, "app.rs", 2000);
        assert!(preview.contains("pub fn handle_key"));
        assert!(preview.contains("pub struct App"));
    }

    #[test]
    fn format_summary_lists_all_entries() {
        let entries: Vec<ExplorationEntry> = (0..5)
            .map(|i| ExplorationEntry {
                tool: "file_read".into(),
                target: format!("f{i}.rs"),
                content: "x".repeat(800),
                ref_path: Some(format!(".ox/exploration/file_read_f{i}.rs.md")),
                full_chars: 50_000,
            })
            .collect();
        let s = format_summary(&entries, 3000);
        for i in 0..5 {
            assert!(s.contains(&format!("f{i}.rs")));
        }
        assert!(s.contains(".ox/exploration/"));
    }

    #[test]
    fn medium_java_file_stays_inline_under_512kb() {
        let dir = temp_dir().join(format!("ox_explore_java_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let body = (0..189)
            .map(|i| format!("    line {i} content padding to exceed old inline char threshold;"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.chars().count() < FILE_READ_INLINE_MAX_CHARS);
        assert_eq!(body.lines().count(), 189);

        let mut entries = Vec::new();
        merge_entry(
            &mut entries,
            &dir,
            "file_read",
            "CompleteDocumentStrategy.java@0+200",
            &body,
        );
        assert!(entries[0].ref_path.is_none());
        assert!(entries[0].content.contains("line 26"));
        assert!(entries[0].content.contains("line 100"));
        assert!(!entries[0].content.contains("【文件开头】"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_cache_reads_full_file_from_disk() {
        let dir = temp_dir().join(format!("ox_explore_resolve_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let rel = "src/Middle.java";
        let full_path = dir.join(rel);
        fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        let body = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&full_path, &body).unwrap();

        let mut entries = Vec::new();
        let huge = "x".repeat(crate::tools::file_read::INLINE_CONTENT_THRESHOLD + 1);
        merge_entry(
            &mut entries,
            &dir,
            "file_read",
            "src/Middle.java@0+200",
            &huge,
        );
        assert!(entries[0].ref_path.is_some());

        let resolved = resolve_file_read_cache(
            &dir,
            &entries,
            "src/Middle.java",
            r#"{"path":"src/Middle.java","offset":0,"limit":200}"#,
        );
        assert!(resolved.contains("【快照恢复】"));
        assert!(resolved.contains("line 26"));
        assert!(resolved.contains("line 40"));
        assert!(!resolved.contains("【文件开头】"));

        let _ = fs::remove_dir_all(&dir);
    }
}
