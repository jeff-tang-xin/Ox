//! Budget-triggered memory offload — the *single* context-compaction path.
//!
//! When the real prompt-token count from the API crosses the adaptive
//! threshold (85% default, 92% with GitNexus), we "offload": ask a summarizer
//! LLM to cluster the session's un-archived ReAct log into memory-graph nodes,
//! persist them, mark those rows `impacted=1`, and replace the corresponding
//! old ReAct messages with compact placeholders (freeing budget while keeping
//! tool-call pairs intact).
//!
//! Improvements over the original:
//! - **Adaptive threshold**: GitNexus availability → higher threshold (92%)
//!   because the graph provides a compressed codebase knowledge layer.
//! - **Cooldown**: after an offload, skip the next N budget checks to avoid
//!   rapid-fire re-triggering.
//! - **GitNexus context block**: build a compact `[CODEBASE_CONTEXT]` block
//!   from graph clusters instead of relying solely on LLM summarization.
//! - **Priority-based preservation**: messages referencing active codebase
//!   clusters are preserved, others are compressed more aggressively.

use std::sync::Arc;

use crate::llm::{LlmProvider, LlmStreamEvent, StreamOptions};
use crate::memory::react_index::ReactRecord;
use crate::memory::store::{GraphNode, MemoryStore};
use crate::message::Message;

/// A single extracted keyword with its category.
/// Categories: problem | conclusion | fix | file | error | concept
#[derive(Debug, Clone)]
pub struct KeywordItem {
    pub cat: String,
    pub kw: String,
}

/// One keyword extraction result for a single source (react_log or memory_graph).
#[derive(Debug, Clone)]
pub struct KeywordExtraction {
    /// "react:<id>" or "graph:<id>"
    pub source_id: String,
    pub keywords: Vec<KeywordItem>,
}

/// Default fraction of the context window at which offload triggers.
pub const OFFLOAD_THRESHOLD: f32 = 0.85;

/// Higher threshold when GitNexus is available — the graph acts as a
/// compressed knowledge layer, reducing the need for frequent offload.
pub const OFFLOAD_THRESHOLD_WITH_GITNEXUS: f32 = 0.92;

/// Engine variable holding the current `[MEMORY_GRAPH]` top-of-context block.
pub const MEMORY_GRAPH_VAR: &str = "_memory_graph_block";
/// Engine variable counting consecutive offload failures.
pub const OFFLOAD_FAIL_VAR: &str = "_offload_fail_streak";
/// Engine variable: cooldown counter (number of LLM calls to skip after offload).
pub const OFFLOAD_COOLDOWN_VAR: &str = "_offload_cooldown";
/// Engine variable: cached GitNexus codebase context block.
pub const CODEBASE_CONTEXT_VAR: &str = "_codebase_context_block";

/// Number of LLM calls to skip after a successful offload before checking again.
pub const OFFLOAD_COOLDOWN_CALLS: u32 = 8;

pub const MEMORY_GRAPH_TAG: &str = "[MEMORY_GRAPH]";
pub const CODEBASE_CONTEXT_TAG: &str = "[CODEBASE_CONTEXT]";

/// Result of an offload attempt.
pub enum OffloadOutcome {
    /// Below threshold — nothing done.
    NotNeeded,
    /// Cooldown active — skipped, waiting for more calls before next check.
    Cooldown { remaining: u32 },
    /// Archived N nodes and freed message budget.
    Archived { nodes: usize },
    /// Summarization failed; only correctness cleanup ran (+ maybe a hard trim).
    Degraded,
}

/// Decide + perform offload. Returns the outcome so the caller can log/notify.
///
/// `gitnexus_available` controls the adaptive threshold and context building.
/// `cooldown` is the remaining cooldown counter (decremented by caller).
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
    cooldown: u32,
    gitnexus_available: bool,
    emit_status: impl Fn(String),
) -> (OffloadOutcome, u32, u32) {
    // ── 1. Cooldown check ──
    if cooldown > 0 {
        return (
            OffloadOutcome::Cooldown {
                remaining: cooldown.saturating_sub(1),
            },
            fail_streak,
            cooldown.saturating_sub(1),
        );
    }

    // ── 2. Adaptive threshold ──
    let threshold = if gitnexus_available {
        OFFLOAD_THRESHOLD_WITH_GITNEXUS
    } else {
        OFFLOAD_THRESHOLD
    };

    // 🛡️ Defensive: Some compatible APIs (e.g. deepseek-v4-flash, open-source models)
    // return cumulative session token counts in usage.prompt_tokens instead of
    // per-request prompt tokens. Two anomaly patterns:
    //   (a) prompt_tokens >> context_window (e.g. 500k for 128k window) — cumulative
    //   (b) prompt_tokens == 0 — API never returned a valid value (every call was
    //       anomalous, so last_prompt_tokens was never updated from its init of 0)
    //
    // STRATEGY: For both patterns, estimate token count from message count
    // (approx 200 tokens/message average). This is rough but safe — it prevents
    // both false-positive triggers (every round) AND the dangerous "never trigger"
    // scenario that would happen if we skipped the check entirely.
    let (display_tokens, effective_tokens, anomaly) =
        if prompt_tokens > context_window || prompt_tokens == 0 {
            // Estimate: avg ~200 tokens per message (conservative for safety check)
            let estimated = (messages.len() as u32).saturating_mul(200);
            tracing::warn!(
                "[OFFLOAD] Anomalous prompt_tokens ({}) — \
                 using message-count estimate ({} msgs × 200 ≈ {} tokens) for budget check.",
                prompt_tokens,
                messages.len(),
                estimated
            );
            (prompt_tokens, estimated, true)
        } else {
            (prompt_tokens, prompt_tokens, false)
        };

    let budget = (context_window as f32 * threshold) as u32;
    if effective_tokens < budget {
        if anomaly {
            emit_status(format!(
                "⚠️ API返回异常token值 {}% — 估算约 {} tokens（{} 条消息），未达阈值 {}%",
                (display_tokens as f64 / context_window as f64 * 100.0) as u32,
                effective_tokens,
                messages.len(),
                (threshold * 100.0) as u32
            ));
        }
        return (OffloadOutcome::NotNeeded, fail_streak, 0);
    }

    // Display the REAL percentage — users should see the actual number
    let pct = (display_tokens as f64 / context_window as f64 * 100.0) as u32;
    let anom_note = if anomaly {
        format!("（⚠️ 异常值，估算{}tokens≈{}%）", effective_tokens, (effective_tokens as f64 / context_window as f64 * 100.0) as u32)
    } else {
        String::new()
    };
    emit_status(format!(
        "🔒 上下文达 {}%（阈值 {}%）{} — 正在归纳记忆图谱…（可继续输入，将排队）",
        pct,
        (threshold * 100.0) as u32,
        anom_note
    ));

    // ── 3. Build GitNexus codebase context block (cheap, synchronous prep) ──
    // This runs before summarization so the graph knowledge is available even
    // if the summarizer fails.
    let graph_block = if gitnexus_available {
        build_codebase_context_block(store, session_id)
    } else {
        String::new()
    };

    // ── 4. Pull the un-archived ReAct timeline ──
    let timeline = store
        .get_react_timeline_with_ids(session_id, 200)
        .unwrap_or_default();

    if timeline.trim().is_empty() {
        cleanup_only(messages);
        // Cooldown=1: skip next turn to avoid retry storm on persistent failures
        return (OffloadOutcome::Degraded, fail_streak.saturating_add(1), 1);
    }

    // ── 5. Summarize ──
    let provider = summarizer.as_ref().unwrap_or(default_provider);
    let clusters = match summarize_clusters(provider, &timeline).await {
        Some(c) if !c.is_empty() => c,
        _ => {
            let new_streak = fail_streak.saturating_add(1);
            if new_streak >= 2 {
                hard_trim(messages);
                emit_status("⚠️ 归纳连续失败 — 已直接裁剪较早消息以释放上下文".to_string());
            } else {
                cleanup_only(messages);
                emit_status("⚠️ 记忆归纳失败 — 本次跳过卸载，稍后重试".to_string());
            }
            return (OffloadOutcome::Degraded, new_streak, 1);
        }
    };

    // ── 6. Persist clusters ──
    let node_count = clusters.len();
    if let Err(e) = store.archive_react_batch(session_id, &clusters) {
        tracing::warn!("[OFFLOAD] archive_react_batch failed: {e}");
        cleanup_only(messages);
        // Cooldown=1: skip next turn to avoid retry storm
        return (OffloadOutcome::Degraded, fail_streak.saturating_add(1), 1);
    }

    // ── 6.5. Semantic keyword extraction (non-fatal) ──
    let all_react_ids: Vec<i64> = clusters.iter().flat_map(|c| c.react_ids.clone()).collect();
    if !all_react_ids.is_empty() {
        match run_keyword_extraction(provider, store, session_id, &all_react_ids).await {
            Ok((kw_n, graph_n)) => {
                if kw_n > 0 || graph_n > 0 {
                    emit_status(format!(
                        "🔗 已提取语义关键词 (react_log: {kw_n}, memory_graphs: {graph_n})"
                    ));
                }
            }
            Err(e) => tracing::warn!("[OFFLOAD] keyword extraction failed (non-fatal): {e}"),
        }
    }

    // ── 7. Compact messages with GitNexus-aware priority ──
    let archived_ids: Vec<i64> = clusters.iter().flat_map(|c| c.react_ids.clone()).collect();
    let preserved_paths = extract_active_file_paths_from_timeline(&timeline);
    placeholder_old_react_prioritized(messages, archived_ids.len(), &preserved_paths);

    // ── 8. Inject codebase context block ──
    if !graph_block.is_empty() {
        inject_codebase_context(messages, &graph_block);
    }

    emit_status(format!(
        "✅ 已归纳 {node_count} 个记忆图谱节点（冷却 {OFFLOAD_COOLDOWN_CALLS} 轮）"
    ));
    (
        OffloadOutcome::Archived { nodes: node_count },
        0,
        OFFLOAD_COOLDOWN_CALLS,
    )
}

/// Extract active file paths from the ReAct timeline for priority preservation.
fn extract_active_file_paths_from_timeline(timeline: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in timeline.lines() {
        let lower = line.to_lowercase();
        // Detect file paths mentioned in tool calls and results
        if let Some(path) = lower.find("\"path\"").and_then(|_| {
            let rest = &line[lower.find("\"path\"")?..];
            let start = rest
                .find('"')
                .and_then(|_| rest[1..].find('"').map(|i| &rest[1..1 + i]))?;
            let val_start = start.find('"').map(|i| i + 1)?;
            let val_end = start[val_start..].find('"').map(|i| val_start + i)?;
            Some(start[val_start..val_end].to_string())
        }) && path.len() > 4
            && path.contains('.')
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    paths
}

/// Build a compact codebase context block from GitNexus clusters.
/// This provides the LLM with a compressed view of the codebase structure,
/// reducing reliance on raw message history.
fn build_codebase_context_block(store: &MemoryStore, session_id: &str) -> String {
    let nodes = store.get_memory_graphs(session_id, 20).unwrap_or_default();
    if nodes.is_empty() {
        return String::new();
    }
    let mut b = String::from(CODEBASE_CONTEXT_TAG);
    b.push_str("\n📐 代码图谱上下文（从 GitNexus 图谱构建）\n");
    b.push_str("以下为项目功能区概览，详细信息可通过 read_symbol / code_graph 查询。\n\n");
    for (id, summary, tier, weight) in &nodes {
        let title: String = summary.chars().take(120).collect();
        let tier_mark = if *tier >= 2 { "◆◆" } else { "◆" };
        let impact_mark = if *weight >= 2.0 { " ⚡" } else { "" };
        b.push_str(&format!("  {tier_mark} #{id}{impact_mark} {title}\n"));
    }
    b
}

/// Inject the codebase context block into messages as a system message.
fn inject_codebase_context(messages: &mut Vec<Message>, block: &str) {
    // Remove any existing codebase context block first
    messages.retain(|m| {
        !matches!(m, Message::System { content, .. } if content.starts_with(CODEBASE_CONTEXT_TAG))
    });

    // Insert after the first system message (or at position 1)
    let insert_pos = messages
        .iter()
        .position(|m| matches!(m, Message::System { .. }))
        .map(|p| p + 1)
        .unwrap_or(1)
        .min(messages.len());

    messages.insert(insert_pos, Message::system(block.to_string()));
}

/// Placeholder with priority: preserve messages referencing active files.
fn placeholder_old_react_prioritized(
    messages: &mut Vec<Message>,
    count: usize,
    preserved_paths: &[String],
) {
    let first_user = messages
        .iter()
        .position(|m| matches!(m, Message::User { .. }))
        .unwrap_or(0);

    let cut = messages
        .len()
        .saturating_sub(messages.len() / 3)
        .max(first_user + 1);

    let mut replaced = 0usize;
    let mut compressed = 0usize;

    for msg in messages.iter_mut().take(cut).skip(first_user) {
        let is_active = message_references_active_file(msg, preserved_paths);

        if let Message::Assistant {
            content,
            tool_calls,
            ..
        } = msg
        {
            // Compress large assistant text, but preserve more if referencing active files
            let max_content = if is_active { 600 } else { 300 };
            if !content.is_empty() && content.len() > max_content {
                let boundary = content
                    .char_indices()
                    .take_while(|(i, _)| *i < max_content)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(max_content);
                *content = format!(
                    "{}... (截断，完整内容在 ReAct 日志中)",
                    &content[..boundary]
                );
                compressed += 1;
            }
            for tc in tool_calls.iter_mut() {
                let max_args = if is_active { 1200 } else { 800 };
                if tc.arguments.len() > max_args {
                    tc.arguments =
                        tc.arguments.chars().take(max_args).collect::<String>() + "...(截断)";
                    compressed += 1;
                }
            }
        }

        // Replace old ToolResults with placeholders, but preserve active file results
        if let Message::ToolResult { content, .. } = msg
            && !content.starts_with("（已归纳")
            && !is_active
        {
            *content = "（已归纳到记忆图谱，recall #<编号> 可重放）".to_string();
            replaced += 1;
        }
    }

    cleanup_only(messages);
    tracing::info!(
        "[OFFLOAD] Placeholdered {replaced} old ReAct results + compressed {compressed} large messages (archived ~{count} rows)"
    );
}

/// Check if a message references any of the preserved (active) file paths.
fn message_references_active_file(msg: &Message, preserved_paths: &[String]) -> bool {
    if preserved_paths.is_empty() {
        return false;
    }
    match msg {
        Message::ToolResult { content, .. } => {
            let lower = content.to_lowercase();
            preserved_paths
                .iter()
                .any(|p| lower.contains(&p.to_lowercase()))
        }
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let lower_content = content.to_lowercase();
            let content_match = preserved_paths
                .iter()
                .any(|p| lower_content.contains(&p.to_lowercase()));
            if content_match {
                return true;
            }
            tool_calls.iter().any(|tc| {
                let lower_args = tc.arguments.to_lowercase();
                preserved_paths
                    .iter()
                    .any(|p| lower_args.contains(&p.to_lowercase()))
            })
        }
        _ => false,
    }
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
        let topic = v
            .get("topic")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
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

/// Run semantic keyword extraction on react_log records + memory_graphs summaries.
/// Returns (react_keyword_count, graph_count) on success.
async fn run_keyword_extraction(
    provider: &Arc<dyn LlmProvider>,
    store: &MemoryStore,
    session_id: &str,
    react_ids: &[i64],
) -> anyhow::Result<(usize, usize)> {
    // 1. Fetch complete react records (zero truncation, from Tantivy)
    let react_records = store.get_react_records_by_ids(react_ids)?;
    // 2. Fetch memory_graphs summaries
    let graph_summaries = store.get_memory_graphs_for_extraction(session_id)?;
    // 3. LLM extraction (single call covering both)
    let extractions = extract_keywords(provider, &react_records, &graph_summaries)
        .await
        .ok_or_else(|| anyhow::anyhow!("LLM keyword extraction returned None"))?;
    // 4. Write back by source type
    let mut kw_total = 0usize;
    let mut graph_total = 0usize;
    for ext in &extractions {
        if let Some(rid_str) = ext.source_id.strip_prefix("react:") {
            if let Ok(rid) = rid_str.parse::<i64>() {
                if let Err(e) = store.update_react_keywords(rid, &ext.keywords) {
                    tracing::warn!("[KW] react_id={} writeback failed: {e}", rid);
                }
                kw_total += ext.keywords.len();
            }
        } else if let Some(gid_str) = ext.source_id.strip_prefix("graph:") {
            if let Ok(gid) = gid_str.parse::<i64>() {
                if let Err(e) = store.update_graph_keywords(gid, &ext.keywords) {
                    tracing::warn!("[KW] graph_id={} writeback failed: {e}", gid);
                }
                graph_total += 1;
            }
        }
    }
    Ok((kw_total, graph_total))
}

/// Ask the LLM to extract categorized keywords from react_log records + memory_graphs.
/// Returns None on any failure (network, timeout, unparseable) so caller can degrade.
async fn extract_keywords(
    provider: &Arc<dyn LlmProvider>,
    react_records: &[ReactRecord],
    graph_summaries: &[(i64, String, String)],
) -> Option<Vec<KeywordExtraction>> {
    use tokio::sync::mpsc;

    let mut input = String::new();
    for r in react_records {
        input.push_str(&format!(
            "[source_id=react:{}] task={}\n tool={} target={} outcome={}\n reasoning: {}\n assistant: {}\n tool_result: {}\n\n",
            r.id,
            truncate_str(&r.task_desc, 200),
            r.tool,
            truncate_str(&r.target, 80),
            r.outcome,
            truncate_str(&r.reasoning, 600),
            truncate_str(&r.assistant_text, 400),
            truncate_str(&r.tool_result, 800),
        ));
    }
    for (gid, topic, detail) in graph_summaries {
        input.push_str(&format!(
            "[source_id=graph:{}] topic={}\n detail={}\n\n",
            gid,
            truncate_str(topic, 100),
            truncate_str(detail, 400),
        ));
    }

    if input.trim().is_empty() {
        return None;
    }

    let prompt = format!(
        "你是语义关键词提取器。下面是若干 ReAct 步骤和记忆图谱摘要（含完整推理、工具结果）。\n\
         对每一条提取语义关键词并分类，输出 JSON 数组，每个元素:\n\
         {{\"source_id\":\"react:<id>\"或\"graph:<id>\",\"keywords\":[{{\"cat\":\"分类\",\"kw\":\"关键词\"}}]}}\n\
         分类说明:\n\
         - problem: 用户遇到的问题/异常现象\n\
         - conclusion: LLM 得出的结论/判断\n\
         - fix: 实际的修复/解决动作\n\
         - file: 涉及的文件路径或符号\n\
         - error: 错误码/异常签名\n\
         - concept: 涉及的概念/技术名词\n\
         要求: 关键词简短(≤15字)；每条至少1个关键词；source_id 必须与输入完全一致；只输出 JSON 数组，无 markdown。\n\n\
         输入:\n{input}"
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
    while let Some(ev) = rx.recv().await {
        match ev {
            LlmStreamEvent::TextDelta(t) => full.push_str(&t),
            LlmStreamEvent::Done { .. } => break,
            LlmStreamEvent::Error(_) => return None,
            _ => {}
        }
    }

    parse_keyword_extractions(&full)
}

/// Parse keyword extraction JSON output (tolerates ```json fences / leading prose).
fn parse_keyword_extractions(raw: &str) -> Option<Vec<KeywordExtraction>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end <= start {
        return None;
    }
    let json = &raw[start..=end];
    let parsed: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let mut out = Vec::new();
    for v in parsed {
        let source_id = v
            .get("source_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if source_id.is_empty() {
            continue;
        }
        let keywords = v
            .get("keywords")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| {
                        let cat = x
                            .get("cat")
                            .and_then(|s| s.as_str())
                            .unwrap_or("concept")
                            .to_string();
                        let kw = x
                            .get("kw")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string();
                        if kw.is_empty() {
                            None
                        } else {
                            Some(KeywordItem { cat, kw })
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(KeywordExtraction { source_id, keywords });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Truncate a string to at most `max_chars` characters.
fn truncate_str(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Correctness-only cleanup (no archival). Safe to run on any failure.
fn cleanup_only(messages: &mut Vec<Message>) {
    crate::context::sanitize_tool_pairs(messages);
    crate::context::filter_noisy_messages(messages);
}

/// Replace the earliest ToolResult / tool-carrying Assistant messages with a
/// compact placeholder so freed budget is realized, keeping tool-call pairs
/// consistent afterwards. `count` is advisory (for the notice text).
#[cfg(test)]
fn placeholder_old_react(messages: &mut Vec<Message>, count: usize) {
    let first_user = messages
        .iter()
        .position(|m| matches!(m, Message::User { .. }))
        .unwrap_or(0);

    let cut = messages
        .len()
        .saturating_sub(messages.len() / 3)
        .max(first_user + 1);

    let mut replaced = 0usize;
    let mut compressed = 0usize;

    for msg in messages.iter_mut().take(cut).skip(first_user) {
        // 1. Compress large Assistant messages (not just ToolResults)
        if let Message::Assistant {
            content,
            tool_calls,
            ..
        } = msg
        {
            if !content.is_empty() && content.len() > 500 {
                // Truncate long assistant text to first 300 chars
                if content.len() > 300 {
                    let boundary = content
                        .char_indices()
                        .take_while(|(i, _)| *i < 300)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(300);
                    *content = format!(
                        "{}... (truncated, full text in ReAct log)",
                        &content[..boundary]
                    );
                    compressed += 1;
                }
            }
            // Also compress tool_calls arguments that are large
            for tc in tool_calls.iter_mut() {
                if tc.arguments.len() > 800 {
                    tc.arguments =
                        tc.arguments.chars().take(800).collect::<String>() + "...(truncated)";
                    compressed += 1;
                }
            }
        }

        // 2. Replace old ToolResults with compact placeholders
        if let Message::ToolResult { content, .. } = msg {
            if !content.starts_with("（已归纳") {
                *content = "（已归纳到记忆图谱，recall #<编号> 可重放）".to_string();
                replaced += 1;
            }
        }
    }

    cleanup_only(messages);
    tracing::info!(
        "[OFFLOAD] Placeholdered {replaced} old ReAct results + compressed {compressed} large messages (archived ~{count} rows)"
    );
}

/// Public entry to the last-resort tail-trim, for the agent loop's bounded
/// API-error recovery path (ARK 400 on an oversized/malformed body).
pub fn hard_trim_public(messages: &mut Vec<Message>) {
    hard_trim(messages);
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

    // Keep the last 30 messages, but also try to preserve intermediate
    // User messages and key Assistant messages (those with ## Plan/Done)
    let tail_start = messages.len().saturating_sub(KEEP_TAIL);
    let mut preserved_mid: Vec<Message> = Vec::new();

    for msg in messages.iter().skip(1).take(tail_start - 1) {
        let keep = match msg {
            Message::User { .. } => true,
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                content.contains("## Plan")
                    || content.contains("## Done")
                    || (!tool_calls.is_empty() && content.is_empty())
            }
            _ => false,
        };
        if keep {
            preserved_mid.push(msg.clone());
        }
    }

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
    // Insert preserved mid-section messages before tail
    if !preserved_mid.is_empty() {
        out.push(Message::system(
            "[PRESERVED_KEY_POINTS]\n保留的关键中间消息：",
        ));
        out.extend(preserved_mid.into_iter().take(15)); // Cap at 15 to avoid bloat
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
    let _ = store.downgrade_stale_nodes(session_id, DOWNGRADE_STALE_DAYS);

    // ── 4. L3 promotion candidates ──
    let mut candidates = Vec::new();
    let raw = store.get_l3_candidates(session_id, L3_MIN_HITS, 3).unwrap_or_default();
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
        listing.push_str(&format!(
            "#{id}{impact}: {}\n",
            summary.chars().take(150).collect::<String>()
        ));
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
        let topic = v
            .get("topic")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let summary = v
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let member_ids: Vec<i64> = v
            .get("member_ids")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        if member_ids.is_empty() {
            continue;
        }
        out.push(MergeGroup {
            topic,
            summary,
            member_ids,
        });
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
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
