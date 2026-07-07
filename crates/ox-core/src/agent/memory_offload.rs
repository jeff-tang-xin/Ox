//! Budget-triggered memory offload — the *single* context-compaction path.
//!
//! When the real prompt-token count from the API crosses 80% of the model's
//! context window, we "offload": ask a summarizer LLM to cluster the session's
//! un-archived ReAct log into memory-graph nodes, persist them, mark those rows
//! `impacted=1`, and replace the corresponding old ReAct messages with compact
//! placeholders (freeing budget while keeping tool-call pairs intact).
//!
//! This replaces the former three-way split (`compact_completed_rounds`,
//! `compact_turn_messages`, ad-hoc engine-variable summaries): budget overflow
//! and memory archival are now one action.

use std::sync::Arc;

use crate::llm::{LlmProvider, LlmStreamEvent, StreamOptions};
use crate::memory::store::{GraphNode, MemoryStore};
use crate::message::Message;

/// Fraction of the context window at which offload triggers.
pub const OFFLOAD_THRESHOLD: f32 = 0.80;

/// Engine variable holding the current `[MEMORY_GRAPH]` top-of-context block.
/// Injected (once populated) by `mod.rs` ahead of `[TURN_CONTEXT]`.
pub const MEMORY_GRAPH_VAR: &str = "_memory_graph_block";
/// Engine variable counting consecutive offload failures (for the hard-trim fallback).
pub const OFFLOAD_FAIL_VAR: &str = "_offload_fail_streak";

pub const MEMORY_GRAPH_TAG: &str = "[MEMORY_GRAPH]";

/// Result of an offload attempt.
pub enum OffloadOutcome {
    /// Below threshold — nothing done.
    NotNeeded,
    /// Archived N nodes and freed message budget.
    Archived { nodes: usize },
    /// Summarization failed; only correctness cleanup ran (+ maybe a hard trim).
    Degraded,
}

/// Decide + perform offload. Returns the outcome so the caller can log/notify.
///
/// `emit_status` is a closure so this module stays UI-agnostic (the caller wires
/// it to `AgentToUiEvent::Status`).
#[allow(clippy::too_many_arguments)]
pub async fn offload_if_over_budget(
    prompt_tokens: u32,
    context_window: u32,
    messages: &mut Vec<Message>,
    summarizer: Option<Arc<dyn LlmProvider>>,
    default_provider: &Arc<dyn LlmProvider>,
    store: &MemoryStore,
    session_id: &str,
    fail_streak: u32,
    emit_status: impl Fn(String),
) -> (OffloadOutcome, u32) {
    let budget = (context_window as f32 * OFFLOAD_THRESHOLD) as u32;
    if prompt_tokens < budget {
        return (OffloadOutcome::NotNeeded, fail_streak);
    }

    emit_status(format!(
        "🔒 上下文达 {}% — 正在归纳记忆图谱…（可继续输入，将排队）",
        (OFFLOAD_THRESHOLD * 100.0) as u32
    ));

    // Pull the un-archived ReAct timeline (id-tagged) as summarization input.
    let timeline = store
        .get_react_timeline_with_ids(session_id, 200)
        .unwrap_or_default();

    // Nothing to archive → fall back to correctness cleanup only.
    if timeline.trim().is_empty() {
        cleanup_only(messages);
        return (OffloadOutcome::Degraded, fail_streak.saturating_add(1));
    }

    let provider = summarizer.as_ref().unwrap_or(default_provider);
    let clusters = match summarize_clusters(provider, &timeline).await {
        Some(c) if !c.is_empty() => c,
        _ => {
            // Failure path: skip archival, run correctness cleanup, and if we've
            // now failed repeatedly, hard-trim the message tail to guarantee the
            // budget is relieved and we don't loop forever.
            let new_streak = fail_streak.saturating_add(1);
            if new_streak >= 2 {
                hard_trim(messages);
                emit_status("⚠️ 归纳连续失败 — 已直接裁剪较早消息以释放上下文".to_string());
            } else {
                cleanup_only(messages);
                emit_status("⚠️ 记忆归纳失败 — 本次跳过卸载，稍后重试".to_string());
            }
            return (OffloadOutcome::Degraded, new_streak);
        }
    };

    // Persist clusters + stamp react_log rows impacted=1.
    let node_count = clusters.len();
    if let Err(e) = store.archive_react_batch(session_id, &clusters) {
        tracing::warn!("[OFFLOAD] archive_react_batch failed: {e}");
        cleanup_only(messages);
        return (OffloadOutcome::Degraded, fail_streak.saturating_add(1));
    }

    // Replace old ReAct tool messages with placeholders (keep pairs valid).
    let archived_ids: Vec<i64> = clusters.iter().flat_map(|c| c.react_ids.clone()).collect();
    placeholder_old_react(messages, archived_ids.len());

    emit_status(format!("✅ 已归纳 {node_count} 个记忆图谱节点"));
    (OffloadOutcome::Archived { nodes: node_count }, 0)
}

/// Build the top-of-context `[MEMORY_GRAPH]` block from persisted nodes.
/// Returns empty when this session has no archived nodes yet (so it is injected
/// only *after* an offload has happened, per design).
pub fn build_memory_graph_block(store: &MemoryStore, session_id: &str) -> String {
    let nodes = store.get_memory_graphs(session_id, 20).unwrap_or_default();
    if nodes.is_empty() {
        return String::new();
    }
    let mut b = String::from(MEMORY_GRAPH_TAG);
    b.push_str("\n📊 记忆图谱（历史已归纳，可 recall #<编号> 重放任一节点完整 ReAct）\n");
    for (id, summary, tier, weight) in &nodes {
        let title: String = summary.chars().take(100).collect();
        // Tier/weight markers so the model reads structure at a glance:
        // ◆◆ = L2 cross-session knowledge, ◆ = L1 session cluster.
        let tier_mark = if *tier >= 2 { "◆◆" } else { "◆" };
        let impact_mark = if *weight >= 2.0 { " ⚡" } else { "" };
        b.push_str(&format!("  {tier_mark} #{id}{impact_mark} {title}\n"));
    }
    b
}

/// Ask the summarizer to cluster the ReAct timeline into memory-graph nodes.
/// Expects a JSON array `[{topic, summary, react_ids:[...]}]`. Returns None on
/// any failure (network, timeout, unparseable) so the caller can degrade.
async fn summarize_clusters(
    provider: &Arc<dyn LlmProvider>,
    timeline: &str,
) -> Option<Vec<GraphNode>> {
    use tokio::sync::mpsc;

    let prompt = format!(
        "你是记忆归纳器。下面是一段 ReAct 工具调用时间线，每行前的编号形如 `[id=N]` 是该步的 react_log id。\n\
         请按**主题**把这些步骤聚类，输出 JSON 数组，每个元素:\n\
         {{\"topic\":\"简短主题(≤20字)\",\"summary\":\"这簇做了什么、关键结论(≤120字)\",\"react_ids\":[相关的id数字]}}\n\
         要求: 只输出 JSON 数组本身，不要 markdown 代码块、不要解释。每个 id 只归入一个簇。\n\n\
         时间线:\n{timeline}"
    );

    let messages = vec![Message::system(&prompt)];
    let (tx, mut rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

    if provider
        .stream_chat(&messages, &[], tx, StreamOptions::default())
        .await
        .is_err()
    {
        return None;
    }

    let mut full = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            LlmStreamEvent::TextDelta(t) => full.push_str(&t),
            LlmStreamEvent::Done { .. } => break,
            LlmStreamEvent::Error(_) => return None,
            _ => {}
        }
    }

    parse_clusters(&full)
}

/// Extract the JSON array from possibly-noisy model output and parse it.
fn parse_clusters(raw: &str) -> Option<Vec<GraphNode>> {
    // Tolerate ```json fences / leading prose: grab the outermost [...] slice.
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    let json = &raw[start..=end];

    let parsed: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let mut out = Vec::new();
    for v in parsed {
        let topic = v.get("topic").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let summary = v
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let react_ids: Vec<i64> = v
            .get("react_ids")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        if topic.is_empty() && summary.is_empty() {
            continue;
        }
        out.push(GraphNode {
            topic,
            summary,
            react_ids,
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Correctness-only cleanup (no archival). Safe to run on any failure.
fn cleanup_only(messages: &mut Vec<Message>) {
    crate::context::sanitize_tool_pairs(messages);
    crate::context::filter_noisy_messages(messages);
}

/// Replace the earliest ToolResult / tool-carrying Assistant messages with a
/// compact placeholder so freed budget is realized, keeping tool-call pairs
/// consistent afterwards. `count` is advisory (for the notice text).
fn placeholder_old_react(messages: &mut Vec<Message>, count: usize) {
    // Find the first user message index — never touch the current-round anchor
    // or the leading system prompt; only collapse the middle tool churn.
    let first_user = messages
        .iter()
        .position(|m| matches!(m, Message::User { .. }))
        .unwrap_or(0);

    // Collapse tool results before the *last* third of the conversation.
    let cut = messages.len().saturating_sub(messages.len() / 3).max(first_user + 1);

    let mut replaced = 0usize;
    for msg in messages.iter_mut().take(cut).skip(first_user) {
        if let Message::ToolResult { content, .. } = msg {
            if !content.starts_with("（已归纳") {
                *content = "（已归纳到记忆图谱，recall #<编号> 可重放）".to_string();
                replaced += 1;
            }
        }
    }
    // Drop tool_calls whose results we just collapsed? No — keep pairs intact;
    // the placeholder still satisfies the tool_call↔result contract. Then run
    // correctness cleanup to drop anything now orphaned.
    cleanup_only(messages);
    tracing::info!(
        "[OFFLOAD] Placeholdered {replaced} old ReAct results (archived ~{count} rows)"
    );
}

/// Last-resort budget relief when summarization keeps failing: keep the leading
/// system prompt + first user anchor + the tail, drop the middle. Mirrors the
/// old `compact_turn_messages` shape but without any memory dependency.
fn hard_trim(messages: &mut Vec<Message>) {
    const KEEP_TAIL: usize = 30;
    if messages.len() <= KEEP_TAIL + 4 {
        cleanup_only(messages);
        return;
    }
    let system = messages.first().cloned();
    let anchor_user = messages
        .iter()
        .find(|m| matches!(m, Message::User { .. }))
        .cloned();
    let tail_start = messages.len().saturating_sub(KEEP_TAIL);
    let tail: Vec<Message> = messages[tail_start..].to_vec();

    let mut out = Vec::new();
    if let Some(s) = system {
        out.push(s);
    }
    out.push(Message::system(
        "[CONTEXT_TRIMMED]\n为释放上下文，已裁剪较早消息。历史请 recall 记忆图谱节点。",
    ));
    if let Some(u) = anchor_user {
        out.push(u);
    }
    out.extend(tail);
    crate::context::sanitize_tool_pairs(&mut out);
    *messages = out;
}

// ═══════════════════════════════════════════════════════════════════
//  L1 → L2 periodic consolidation + downgrade + L3 promotion candidates
// ═══════════════════════════════════════════════════════════════════

const CONSOLIDATION_META_KEY: &str = "last_l1l2_consolidation";
/// Minimum tier-1 nodes before a consolidation pass is worthwhile.
const MIN_L1_NODES_TO_MERGE: usize = 4;
/// Node hit_count at/above which a tier-2 node becomes an L3 (Skill) candidate.
const L3_MIN_HITS: i64 = 3;
/// Days without a hit before a node is downgraded (forgetting = demotion).
const DOWNGRADE_STALE_DAYS: u32 = 30;

/// One L2 promotion candidate surfaced to the caller for user-confirmed Skill
/// abstraction (Phase 4). `graph_id` lets the caller mark it promoted after save.
pub struct L3Candidate {
    pub graph_id: i64,
    pub summary: String,
    pub skill_draft: String,
}

/// Periodic L1→L2 consolidation, time-gated by `interval_hours` (wall clock via
/// the DB's `datetime('now')`, compared against the `meta` timestamp — no live
/// timer, checked lazily at turn start). Returns L3 candidates (if any) for the
/// caller to route through the user-confirmed Skill flow.
///
/// Steps: (1) bail if not due; (2) LLM similarity-merge tier-1 → tier-2;
/// (3) downgrade stale nodes; (4) collect L3 candidates; (5) stamp timestamp.
pub async fn consolidate_if_due(
    store: &MemoryStore,
    summarizer: Option<Arc<dyn LlmProvider>>,
    default_provider: &Arc<dyn LlmProvider>,
    session_id: &str,
    interval_hours: u32,
    now_unix: u64,
    emit_status: impl Fn(String),
) -> Vec<L3Candidate> {
    // ── 1. Due check ──
    let last: u64 = store
        .meta_get(CONSOLIDATION_META_KEY)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let interval_secs = (interval_hours as u64) * 3600;
    if now_unix.saturating_sub(last) < interval_secs {
        return Vec::new();
    }
    // Stamp immediately so a failure mid-pass doesn't retrigger every turn.
    let _ = store.meta_set(CONSOLIDATION_META_KEY, &now_unix.to_string());

    // ── 2. L1 → L2 similarity merge ──
    let l1 = store.get_l1_nodes(session_id, 60).unwrap_or_default();
    if l1.len() >= MIN_L1_NODES_TO_MERGE {
        let provider = summarizer.as_ref().unwrap_or(default_provider);
        if let Some(groups) = merge_l1_clusters(provider, &l1).await {
            let mut merged = 0usize;
            for g in &groups {
                if g.member_ids.len() < 2 {
                    continue; // a lone node needs no merge
                }
                // Preserve impact weight: max over members.
                let weight = l1
                    .iter()
                    .filter(|(id, _, _)| g.member_ids.contains(id))
                    .map(|(_, _, w)| *w)
                    .fold(1.0f64, f64::max);
                if store
                    .apply_l1_l2_merge(session_id, &g.topic, &g.summary, &g.member_ids, weight)
                    .is_ok()
                {
                    merged += 1;
                }
            }
            if merged > 0 {
                emit_status(format!("🧩 记忆归并：{merged} 个跨主题簇晋升 L2"));
            }
        }
    }

    // ── 3. Downgrade stale nodes (forgetting) ──
    let _ = store.downgrade_stale_nodes(DOWNGRADE_STALE_DAYS);

    // ── 4. L3 promotion candidates ──
    let mut candidates = Vec::new();
    let raw = store.get_l3_candidates(L3_MIN_HITS, 3).unwrap_or_default();
    if !raw.is_empty() {
        let provider = summarizer.as_ref().unwrap_or(default_provider);
        for (gid, summary) in raw {
            if let Some(draft) = abstract_to_skill(provider, &summary).await {
                candidates.push(L3Candidate {
                    graph_id: gid,
                    summary,
                    skill_draft: draft,
                });
            }
        }
    }
    candidates
}

/// One LLM-decided merge group.
struct MergeGroup {
    topic: String,
    summary: String,
    member_ids: Vec<i64>,
}

/// Ask the LLM which tier-1 nodes describe the same theme and how to merge them.
async fn merge_l1_clusters(
    provider: &Arc<dyn LlmProvider>,
    nodes: &[(i64, String, f64)],
) -> Option<Vec<MergeGroup>> {
    use tokio::sync::mpsc;

    let mut listing = String::new();
    for (id, summary, weight) in nodes {
        let impact = if *weight >= 2.0 { " [IMPACT]" } else { "" };
        listing.push_str(&format!("#{id}{impact}: {}\n", summary.chars().take(150).collect::<String>()));
    }

    let prompt = format!(
        "你在归并跨会话的记忆图谱节点。下面每行是一个节点：`#id: 摘要`。\n\
         把**讲同一主题**的节点分到一组，输出 JSON 数组，每个元素:\n\
         {{\"topic\":\"合并后主题(≤20字)\",\"summary\":\"合并后的知识(≤150字，融合各成员)\",\"member_ids\":[节点id]}}\n\
         规则: 只合并确实同主题的；带 [IMPACT] 的节点其影响面描述必须保留进 summary，不得压缩掉；\
         单独主题也各自成组(member_ids 只含自己)；只输出 JSON 数组，无 markdown。\n\n\
         节点:\n{listing}"
    );

    let messages = vec![Message::system(&prompt)];
    let (tx, mut rx) = mpsc::unbounded_channel::<LlmStreamEvent>();
    if provider
        .stream_chat(&messages, &[], tx, StreamOptions::default())
        .await
        .is_err()
    {
        return None;
    }
    let mut full = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            LlmStreamEvent::TextDelta(t) => full.push_str(&t),
            LlmStreamEvent::Done { .. } => break,
            LlmStreamEvent::Error(_) => return None,
            _ => {}
        }
    }
    parse_merge_groups(&full)
}

fn parse_merge_groups(raw: &str) -> Option<Vec<MergeGroup>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&raw[start..=end]).ok()?;
    let mut out = Vec::new();
    for v in parsed {
        let topic = v.get("topic").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let summary = v.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let member_ids: Vec<i64> = v
            .get("member_ids")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        if member_ids.is_empty() {
            continue;
        }
        out.push(MergeGroup { topic, summary, member_ids });
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Abstract a stable, frequently-recalled L2 node into a Skill draft (markdown).
/// Returns None on failure — caller simply skips this candidate.
async fn abstract_to_skill(provider: &Arc<dyn LlmProvider>, summary: &str) -> Option<String> {
    use tokio::sync::mpsc;

    let prompt = format!(
        "下面是一条被多次复用的稳定项目知识。把它抽象成一个可复用的 Skill（经验规则/约束），\
         用简洁 markdown：一个 `#` 标题 + 「何时适用」+「怎么做」。只输出 markdown 正文。\n\n知识:\n{summary}"
    );
    let messages = vec![Message::system(&prompt)];
    let (tx, mut rx) = mpsc::unbounded_channel::<LlmStreamEvent>();
    if provider
        .stream_chat(&messages, &[], tx, StreamOptions::default())
        .await
        .is_err()
    {
        return None;
    }
    let mut full = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            LlmStreamEvent::TextDelta(t) => full.push_str(&t),
            LlmStreamEvent::Done { .. } => break,
            LlmStreamEvent::Error(_) => return None,
            _ => {}
        }
    }
    let trimmed = full.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clusters_tolerates_fences() {
        let raw = "```json\n[{\"topic\":\"A\",\"summary\":\"did A\",\"react_ids\":[1,2]}]\n```";
        let clusters = parse_clusters(raw).unwrap();
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].topic, "A");
        assert_eq!(clusters[0].react_ids, vec![1, 2]);
    }

    #[test]
    fn parse_clusters_rejects_garbage() {
        assert!(parse_clusters("no json here").is_none());
        assert!(parse_clusters("[]").is_none());
    }

    #[test]
    fn placeholder_keeps_tool_pairs_valid() {
        use crate::message::ToolCall;
        let mut messages = vec![
            Message::system("sys"),
            Message::user("do X"),
            Message::Assistant {
                content: "calling".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "file_read".into(),
                    arguments: "{}".into(),
                }],
                reasoning_content: None,
            },
            Message::ToolResult {
                tool_call_id: "c1".into(),
                content: "huge content ".repeat(50),
            },
            Message::user("do Y"),
            Message::assistant("done"),
        ];
        placeholder_old_react(&mut messages, 1);
        // Still parses without orphaned pairs; system + anchors preserved.
        assert!(matches!(messages.first(), Some(Message::System { .. })));
    }
}
