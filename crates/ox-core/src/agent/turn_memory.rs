//! In-turn durable memory — survives message compaction within a single agent turn.

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 80;
const MAX_DECISIONS: usize = 24;
const MAX_SUMMARY_CHARS: usize = 6_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMemoryEntry {
    pub tool: String,
    pub target: String,
    pub outcome: String, // "ok" | "error" | brief note
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnMemory {
    pub user_task: String,
    pub entries: Vec<TurnMemoryEntry>,
    #[serde(default)]
    pub decisions: Vec<String>,
    pub iterations: u32,
}

impl TurnMemory {
    pub fn new(user_task: impl Into<String>) -> Self {
        Self {
            user_task: user_task.into(),
            entries: Vec::new(),
            decisions: Vec::new(),
            iterations: 0,
        }
    }

    pub fn record(&mut self, tool: &str, target: &str, outcome: &str) {
        let key = format!("{}:{}", tool, target);
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| format!("{}:{}", e.tool, e.target) == key)
        {
            // Do not let coarse progress reconstruction ("ok") erase a richer
            // result excerpt captured from the actual ToolResult.
            if outcome == "ok" && existing.outcome.starts_with("ok — ") {
                return;
            }
            existing.outcome = outcome.to_string();
            return;
        }
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push(TurnMemoryEntry {
            tool: tool.to_string(),
            target: target.to_string(),
            outcome: outcome.to_string(),
        });
    }

    pub fn record_tool(&mut self, tool: &str, arguments: &str, ok: bool) {
        self.record_tool_with_result(tool, arguments, ok, None);
    }

    pub fn record_tool_with_result(
        &mut self,
        tool: &str,
        arguments: &str,
        ok: bool,
        result_content: Option<&str>,
    ) {
        let target = crate::agent::exploration_snapshot::target_from_tool_args(tool, arguments);
        let outcome = Self::outcome_label(tool, ok, result_content);
        self.record(tool, &target, &outcome);
    }

    fn outcome_label(tool: &str, ok: bool, result_content: Option<&str>) -> String {
        if !ok {
            return "error".to_string();
        }
        if let Some(raw) = result_content
            && matches!(
                tool,
                "file_read"
                    | "find_symbol"
                    | "code_search"
                    | "file_search"
                    | "code_graph"
                    | "shell_exec"
                    | "git_status"
                    | "git_diff"
            )
        {
            let content = crate::agent::exploration_snapshot::extract_data_content(raw);
            let excerpt = compact_result_excerpt(&content, 360);
            if !excerpt.is_empty() {
                return format!("ok — {excerpt}");
            }
        }
        "ok".to_string()
    }

    pub fn record_decision(&mut self, note: impl AsRef<str>) {
        let note = compact_result_excerpt(note.as_ref(), 420);
        if note.trim().is_empty() {
            return;
        }
        if self.decisions.last().is_some_and(|last| last == &note) {
            return;
        }
        if self.decisions.len() >= MAX_DECISIONS {
            self.decisions.remove(0);
        }
        self.decisions.push(note);
    }
    pub fn merge_from(&mut self, other: TurnMemory) {
        if self.user_task.is_empty() && !other.user_task.is_empty() {
            self.user_task = other.user_task;
        }
        self.iterations = self.iterations.max(other.iterations);
        for e in other.entries {
            self.record(&e.tool, &e.target, &e.outcome);
        }
        for d in other.decisions {
            self.record_decision(d);
        }
    }

    pub fn had_code_changes(&self) -> bool {
        self.entries.iter().any(|e| {
            matches!(e.tool.as_str(), "file_write" | "edit_file" | "delete_range")
                && e.outcome != "error"
        })
    }

    /// Unique tool names used this turn (for round memory checkpoint).
    pub fn tool_names_summary(&self) -> Vec<String> {
        let mut names: Vec<String> = self.entries.iter().map(|e| e.tool.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn bump_iteration(&mut self) {
        self.iterations += 1;
    }

    pub fn format_injection_slim(&self, iteration: u32) -> String {
        let mut out = format!(
            "[TURN_MEMORY — IMPLEMENT]\n\
             这是你在当前用户请求中的连续工作记忆，不是新任务，也不是外部建议。\n\
             iteration {} | 你已执行工具 {} 次",
            iteration + 1,
            self.entries.len()
        );
        for e in self.entries.iter().rev().take(8).rev() {
            out.push_str(&format!("\n  • {}({}) → {}", e.tool, e.target, e.outcome));
        }
        if !self.decisions.is_empty() {
            out.push_str("\n你刚才形成的判断（非原始 think 摘要）:");
            for d in self.decisions.iter().rev().take(4).rev() {
                out.push_str(&format!("\n  - {d}"));
            }
        }
        out
    }

    pub fn format_injection(&self, iteration: u32) -> String {
        let mut out = format!(
            "[TURN_MEMORY — CURRENT ROUND ONLY]\n\
             🔄 本轮第 {} 次 LLM 调用（iteration {}）",
            iteration + 1,
            self.iterations
        );
        if !self.user_task.is_empty() {
            let task: String = self.user_task.chars().take(300).collect();
            out.push_str(&format!("\n✉️ 本轮用户输入: {task}"));
        }
        if self.entries.is_empty() {
            out.push_str("\n（本轮尚无工具记录）");
        } else {
            out.push_str("\n【你在本轮已经执行过 — 勿重复】");
            for e in &self.entries {
                let icon = if e.outcome == "ok" { "✅" } else { "⚠️" };
                out.push_str(&format!(
                    "\n  {icon} {}({}) — {}",
                    e.tool, e.target, e.outcome
                ));
            }
        }
        if !self.decisions.is_empty() {
            out.push_str("\n【你在本轮已经形成的判断 — 用于承接上下文，非原始 think】");
            for d in self.decisions.iter().rev().take(8).rev() {
                out.push_str(&format!("\n  - {d}"));
            }
        }
        out.push_str("\n基于以上你自己在**本轮**已经完成的动作和判断继续；不要把它当成外部建议，也不要重复执行。");
        if out.len() > MAX_SUMMARY_CHARS {
            out.chars().take(MAX_SUMMARY_CHARS).collect()
        } else {
            out
        }
    }

    /// Rebuild entries from message history (fixes amnesia when compaction drops tool results).
    pub fn sync_from_messages(
        &mut self,
        messages: &[crate::message::Message],
        include_writes: bool,
    ) {
        let progress =
            crate::agent::context_injector::build_tool_progress(messages, include_writes);
        for line in progress.lines() {
            let line = line.trim();
            if let Some((tool, target, ok)) = parse_progress_line(line) {
                self.record(&tool, &target, if ok { "ok" } else { "error" });
            }
        }
    }
}

fn compact_result_excerpt(content: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("──") {
            continue;
        }
        if !out.is_empty() {
            out.push_str(" | ");
        }
        out.push_str(trimmed);
        if out.chars().count() >= max_chars {
            break;
        }
    }
    if out.chars().count() > max_chars {
        let mut clipped: String = out.chars().take(max_chars).collect();
        clipped.push('…');
        clipped
    } else {
        out
    }
}
fn parse_progress_line(line: &str) -> Option<(String, String, bool)> {
    if let Some(outcome) = line.strip_prefix("project_detect → ") {
        return Some(("project_detect".into(), "-".into(), outcome == "成功"));
    }
    for tool in [
        "file_list",
        "file_read",
        "file_write",
        "edit_file",
        "delete_range",
        "shell_exec",
    ] {
        let prefix = format!("{tool}(");
        if let Some(rest) = line.strip_prefix(&prefix)
            && let Some((target, outcome)) = rest.split_once(") → ")
        {
            return Some((tool.to_string(), target.to_string(), outcome == "成功"));
        }
    }
    if let Some((left, outcome)) = line.rsplit_once(" → ") {
        let ok = outcome == "成功";
        if let Some((tool, args)) = left.split_once(':') {
            return Some((tool.trim().to_string(), args.trim().to_string(), ok));
        }
    }
    None
}

pub fn turn_memory_from_json(s: &str) -> Option<TurnMemory> {
    serde_json::from_str(s).ok()
}

pub fn turn_memory_to_json(tm: &TurnMemory) -> String {
    serde_json::to_string(tm).unwrap_or_else(|_| "{}".to_string())
}

pub const TURN_MEMORY_TAG: &str = "[TURN_MEMORY]";

pub fn strip_turn_memory(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        !matches!(m, crate::message::Message::System { content } if content.starts_with(TURN_MEMORY_TAG))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_memory_dedupes() {
        let mut tm = TurnMemory::new("fix bug");
        tm.record("file_read", "a.rs", "ok");
        tm.record("file_read", "a.rs", "ok");
        assert_eq!(tm.entries.len(), 1);
    }

    #[test]
    fn decision_memory_records_and_injects() {
        let mut tm = TurnMemory::new("fix bug");
        tm.record_decision("选择动作: file_read; 可见依据: 需要确认调用关系");
        tm.record_decision("工具观察: file_read(a.rs) 成功; 关键结果: fn handle");
        let injected = tm.format_injection(0);
        assert!(injected.contains("你在本轮已经形成的判断"));
        assert!(injected.contains("file_read"));
    }

    #[test]
    fn legacy_turn_memory_json_defaults_decisions() {
        let legacy = r#"{"user_task":"fix bug","entries":[],"iterations":2}"#;
        let tm = turn_memory_from_json(legacy).expect("legacy turn memory should load");
        assert!(tm.decisions.is_empty());
        assert_eq!(tm.iterations, 2);
    }
}
