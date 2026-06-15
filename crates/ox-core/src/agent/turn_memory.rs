//! In-turn durable memory — survives message compaction within a single agent turn.

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 80;
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
    pub iterations: u32,
}

impl TurnMemory {
    pub fn new(user_task: impl Into<String>) -> Self {
        Self {
            user_task: user_task.into(),
            entries: Vec::new(),
            iterations: 0,
            }
    }

    pub fn record(&mut self, tool: &str, target: &str, outcome: &str) {
        let key = format!("{}:{}", tool, target);
        if let Some(existing) = self.entries.iter_mut().find(|e| {
            format!("{}:{}", e.tool, e.target) == key
        }) {
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
        let target = crate::agent::exploration_snapshot::target_from_tool_args(tool, arguments);
        self.record(tool, &target, if ok { "ok" } else { "error" });
    }

    pub fn merge_from(&mut self, other: TurnMemory) {
        if self.user_task.is_empty() && !other.user_task.is_empty() {
            self.user_task = other.user_task;
        }
        self.iterations = self.iterations.max(other.iterations);
        for e in other.entries {
            self.record(&e.tool, &e.target, &e.outcome);
        }
    }

    pub fn bump_iteration(&mut self) {
        self.iterations += 1;
    }

    pub fn format_injection(&self, iteration: u32) -> String {
        let mut out = format!(
            "[TURN_MEMORY]\n🔄 本轮第 {} 次 LLM 调用（turn iteration {}）",
            iteration + 1,
            self.iterations
        );
        if !self.user_task.is_empty() {
            let task: String = self.user_task.chars().take(300).collect();
            out.push_str(&format!("\n📋 任务: {task}"));
        }
        if self.entries.is_empty() {
            out.push_str("\n（尚无工具记录）");
        } else {
            out.push_str("\n【本轮已完成 — 勿重复】");
            for e in &self.entries {
                let icon = if e.outcome == "ok" { "✅" } else { "⚠️" };
                out.push_str(&format!(
                    "\n  {icon} {}({}) — {}",
                    e.tool, e.target, e.outcome
                ));
            }
        }
        out.push_str("\n基于以上记录继续，不要重复相同工具调用。");
        if out.len() > MAX_SUMMARY_CHARS {
            out.chars().take(MAX_SUMMARY_CHARS).collect()
        } else {
            out
        }
    }

    /// Rebuild entries from message history (fixes amnesia when compaction drops tool results).
    pub fn sync_from_messages(&mut self, messages: &[crate::message::Message], include_writes: bool) {
        let progress = crate::agent::context_injector::build_tool_progress(messages, include_writes);
        for line in progress.lines() {
            let line = line.trim();
            if let Some((tool, target, ok)) = parse_progress_line(line) {
                self.record(&tool, &target, if ok { "ok" } else { "error" });
            }
        }
    }
}

fn parse_progress_line(line: &str) -> Option<(String, String, bool)> {
    if let Some(outcome) = line.strip_prefix("project_detect → ") {
        return Some((
            "project_detect".into(),
            "-".into(),
            outcome == "成功",
        ));
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
        if let Some(rest) = line.strip_prefix(&prefix) {
            if let Some((target, outcome)) = rest.split_once(") → ") {
                return Some((
                    tool.to_string(),
                    target.to_string(),
                    outcome == "成功",
                ));
            }
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

/// Compact in-turn message list when it grows too large.
/// Keeps: system prompt, anchor user message, recent tail, strips middle bulk.
pub fn compact_turn_messages(messages: &mut Vec<crate::message::Message>, keep_tail: usize) {
    if messages.len() <= keep_tail + 4 {
        return;
    }

    let system = messages.first().cloned();
    let anchor_user = messages
        .iter()
        .find(|m| matches!(m, crate::message::Message::User { .. }))
        .cloned();

    let tail_start = messages.len().saturating_sub(keep_tail);
    let mut tail: Vec<_> = messages[tail_start..].to_vec();

    // Ensure tail starts with a valid assistant+tool pair boundary
    while !tail.is_empty() {
        if matches!(tail.first(), Some(crate::message::Message::ToolResult { .. })) {
            tail.remove(0);
        } else {
            break;
        }
    }

    let dropped = messages.len().saturating_sub(keep_tail + 2);
    let mut compacted = Vec::new();
    if let Some(s) = system {
        compacted.push(s);
    }
    compacted.push(crate::message::Message::system(&format!(
        "[CONTEXT_COMPACTED]\n为控制上下文长度，已压缩本轮较早的 {dropped} 条消息。\n\
         完整工具记录见 [TURN_MEMORY] / [STEP_MEMORY]。请基于最近消息和记忆块继续，勿从头探索。"
    )));
    if let Some(u) = anchor_user {
        compacted.push(u);
    }
    compacted.append(&mut tail);

    crate::context::sanitize_tool_pairs(&mut compacted);
    *messages = compacted;
    tracing::info!(
        "[TURN_COMPACT] Reduced messages to {} (dropped ~{})",
        messages.len(),
        dropped
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;

    #[test]
    fn turn_memory_dedupes() {
        let mut tm = TurnMemory::new("fix bug");
        tm.record("file_read", "a.rs", "ok");
        tm.record("file_read", "a.rs", "ok");
        assert_eq!(tm.entries.len(), 1);
    }

    #[test]
    fn compact_preserves_tail() {
        let mut msgs = vec![Message::system("sys")];
        for i in 0..30 {
            msgs.push(Message::user(format!("u{i}")));
            msgs.push(Message::Assistant {
                content: format!("a{i}"),
                tool_calls: vec![],
                reasoning_content: None,
            });
        }
        compact_turn_messages(&mut msgs, 8);
        assert!(msgs.len() < 30);
        assert!(msgs.iter().any(|m| matches!(m, Message::System { content } if content.contains("COMPACTED"))));
    }
}
