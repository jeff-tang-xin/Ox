//! Tantivy-backed session memory store.
//! Persists LLM's session summaries (learnings, facts, file changes) across sessions.
//! Path: `<project_root>/.ox/memory.db`
//! react_log is stored in Tantivy for zero-truncation full-text retrieval.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use jieba_rs::Jieba;

use crate::agent::unified_action::SessionSummary;
use crate::memory::react_index::ReactIndex;
use crate::memory::session_index::{
    FactRecord, FileModifiedRecord, FileReadRecord, SessionIndex, SessionRecord,
    DOC_TYPE_FACT, DOC_TYPE_FILE_MODIFIED, DOC_TYPE_FILE_READ, DOC_TYPE_SESSION,
};

/// Type alias for a react_log row: (created_at, task_desc, tool, target, outcome, decision, assistant_text, reasoning, tool_result)
type ReactRow = (String, String, String, String, String, String, String, String, String);

pub struct MemoryStore {
    session_index: SessionIndex,
    react_index: ReactIndex,
    meta_store: Mutex<HashMap<String, String>>,
}

/// One clustered memory-graph node produced by the summarizer during offload.
/// `react_ids` are the `react_log.id` rows that this node consolidates.
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub topic: String,
    pub summary: String,
    pub react_ids: Vec<i64>,
}

impl MemoryStore {
    /// Open or create store at the given path (e.g. `<project_root>/.ox/memory.db`).
    /// The path's parent directory is used for Tantivy index storage.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Convert file path to directory path for Tantivy indices
        // e.g., "memory.db" → "memory_index"
        let dir_path = if path.extension().is_some() {
            let stem = path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("memory");
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            parent.join(format!("{}_index", stem))
        } else {
            path.to_path_buf()
        };
        
        std::fs::create_dir_all(&dir_path)?;

        let react_dir = dir_path.join("react_index");
        let react_index = ReactIndex::open(&react_dir)?;

        let session_index = SessionIndex::open(&dir_path)?;

        Ok(Self {
            session_index,
            react_index,
            meta_store: Mutex::new(HashMap::new()),
        })
    }

    /// Save a session summary for a completed session.
    /// Uses Tantivy indices for storage, deletes prior child rows for this session_id
    /// to avoid duplicate accumulation, and merges into the last session if it's about
    /// the same topic.
    pub fn save_session(
        &self,
        session_id: &str,
        task_desc: &str,
        summary: &SessionSummary,
    ) -> Result<()> {
        let merged_id = self.find_merge_target(task_desc, summary);

        let target_id = merged_id.as_deref().unwrap_or(session_id);

        self.session_index.delete_by_session(target_id)?;

        let created_at =
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        let merged_learnings = if merged_id.is_some() {
            if let Ok(Some(old_session)) = self.session_index.get_session(target_id) {
                let mut old = old_session.learnings;
                if !old.is_empty() && !summary.learnings.is_empty() {
                    old.push_str(" → ");
                    old.push_str(&summary.learnings);
                    old
                } else {
                    summary.learnings.clone()
                }
            } else {
                summary.learnings.clone()
            }
        } else {
            summary.learnings.clone()
        };

        let session_record = SessionRecord {
            id: target_id.to_string(),
            task_desc: task_desc.to_string(),
            content_summary: String::new(),
            learnings: merged_learnings,
            created_at,
            doc_type: DOC_TYPE_SESSION.to_string(),
        };
        self.session_index.insert_session(&session_record)?;

        for (idx, f) in summary.key_facts.iter().enumerate() {
            let fact_id = format!("{}_fact_{}", target_id, idx);
            let record = FactRecord {
                id: fact_id,
                session_id: target_id.to_string(),
                fact_text: f.fact.to_string(),
                related_files: f.files.join(", "),
                doc_type: DOC_TYPE_FACT.to_string(),
            };
            self.session_index.insert_fact(&record)?;
        }

        for (idx, r) in summary.files_read.iter().enumerate() {
            let fr_id = format!("{}_read_{}", target_id, idx);
            let record = FileReadRecord {
                id: fr_id,
                session_id: target_id.to_string(),
                file_path: r.path.to_string(),
                purpose: r.purpose.to_string(),
                doc_type: DOC_TYPE_FILE_READ.to_string(),
            };
            self.session_index.insert_file_read(&record)?;
        }

        for (idx, m) in summary.files_modified.iter().enumerate() {
            let fm_id = format!("{}_mod_{}", target_id, idx);
            let record = FileModifiedRecord {
                id: fm_id,
                session_id: target_id.to_string(),
                file_path: m.path.to_string(),
                change_summary: m.summary.to_string(),
                doc_type: DOC_TYPE_FILE_MODIFIED.to_string(),
            };
            self.session_index.insert_file_modified(&record)?;
        }

        for s in &summary.skills {
            tracing::info!("[MEMORY] Skill suggested: {} (scope={})", s.id, s.scope);
        }

        self.session_index.commit()?;
        Ok(())
    }

    /// Find a merge target: the last session within 30 min with high trigram
    /// similarity in learnings, or overlapping file paths.
    fn find_merge_target(
        &self,
        _task_desc: &str,
        summary: &SessionSummary,
    ) -> Option<String> {
        let mut recent = self.session_index.get_recent_sessions(20).ok()?;
        recent.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let now = chrono::Utc::now();
        let thirty_minutes_ago = now - chrono::Duration::minutes(30);

        let last = recent.into_iter().find(|s| {
            if let Ok(dt) =
                chrono::DateTime::parse_from_str(&s.created_at, "%Y-%m-%d %H:%M:%S%.3f")
            {
                dt.with_timezone(&chrono::Utc) >= thirty_minutes_ago
            } else {
                false
            }
        })?;

        if trigram_overlap(&last.learnings, &summary.learnings) > 0.25 {
            return Some(last.id);
        }

        let new_files: Vec<String> = summary
            .files_modified
            .iter()
            .map(|m| m.path.rsplit('/').next().unwrap_or(&m.path).to_lowercase())
            .collect();
        if new_files.is_empty() {
            return None;
        }

        let all_files = self.session_index.get_all_file_modified(10000).ok()?;
        let old_files: Vec<String> = all_files
            .iter()
            .filter(|f| f.session_id == last.id)
            .map(|f| {
                f.file_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&f.file_path)
                    .to_lowercase()
            })
            .collect();

        if new_files.iter().any(|f| old_files.contains(f)) {
            return Some(last.id);
        }

        None
    }

    /// Query recent sessions that touched the given file path.
    /// Normalizes separators + case so Windows/Unix + absolute/relative paths match reliably.
    pub fn query_file_history(&self, file_path: &str, limit: usize) -> Result<String> {
        let mut results = self.session_index.get_sessions_by_file(file_path, limit)?;
        results.sort_by(|a, b| b.0.created_at.cmp(&a.0.created_at));
        results.truncate(limit);

        let mut out = String::new();
        for (session, change) in &results {
            let date: String = session.created_at.chars().take(10).collect();
            let short: String = session.learnings.chars().take(120).collect();
            out.push_str(&format!("  • {} — {}\n", date, short));
            if !change.is_empty() {
                out.push_str(&format!(
                    "    └ {}\n",
                    change.chars().take(80).collect::<String>()
                ));
            }
        }
        Ok(out)
    }

    /// Query relevant history — ranks by file match + task keyword overlap + recency.
    /// Returns empty when nothing scores above threshold.
    pub fn query_relevant_history(
        &self,
        file_paths: &[String],
        current_task: &str,
        max_results: usize,
    ) -> Result<String> {
        let task_keywords: Vec<String> = current_task
            .split(|c: char| !c.is_alphanumeric() && c != '.')
            .filter(|w| w.len() > 2)
            .filter(|w| {
                ![
                    "fix", "改", "继续", "修", "this", "the", "for", "and", "not", "are", "was",
                ]
                .contains(w)
            })
            .map(|w| w.to_lowercase())
            .collect();

        let file_bases: Vec<String> = file_paths
            .iter()
            .map(|p| {
                std::path::Path::new(&p.replace('\\', "/"))
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(p)
                    .to_lowercase()
            })
            .collect();

        let all_modified = self.session_index.get_all_file_modified(10000)?;

        struct Scored {
            learnings: String,
            change_summary: String,
            created_at: String,
            score: f64,
        }

        let mut scored: Vec<Scored> = Vec::new();

        for modified in &all_modified {
            let norm = modified.file_path.replace('\\', "/").to_lowercase();
            let base = std::path::Path::new(&norm)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&norm)
                .to_string();

            let mut score = 0.0f64;

            if file_bases.contains(&base) {
                score += 3.0;
            } else if file_bases
                .iter()
                .any(|b| base.contains(b) || b.contains(&base))
            {
                score += 1.0;
            }

            if let Ok(Some(session)) = self.session_index.get_session(&modified.session_id)
            {
                let task_lower = session.task_desc.to_lowercase();
                let kw_matches = task_keywords
                    .iter()
                    .filter(|k| task_lower.contains(k.as_str()))
                    .count();
                if kw_matches > 0 {
                    score += (kw_matches as f64).min(3.0);
                }

                if score >= 1.5 {
                    scored.push(Scored {
                        learnings: session.learnings,
                        change_summary: modified.change_summary.clone(),
                        created_at: session.created_at,
                        score,
                    });
                }
            }
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(max_results);
        scored.reverse();

        if scored.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::new();
        for s in &scored {
            let date: String = s.created_at.chars().take(16).collect();
            let short: String = s.learnings.chars().take(120).collect();
            out.push_str(&format!("  ─ {} — {}\n", date, short));
            if !s.change_summary.is_empty() {
                out.push_str(&format!(
                    "    └ {}\n",
                    s.change_summary.chars().take(80).collect::<String>()
                ));
            }
        }
        Ok(out)
    }

    /// Record a single ReAct step to the log (with timestamp).
    /// Each tool execution → one row storing the full ReAct tuple so it can be
    /// replayed as `[user(task_desc @ created_at), assistant(reasoning → visible),
    /// tool_call(tool+target), tool_result]`.
    /// - `decision`: short in-turn rationale (why this tool was chosen)
    /// - `assistant_text`: visible assistant reply (striped of think blocks)
    /// - `reasoning`: raw thinking/reasoning content (for replay when visible text alone is insufficient)
    /// - `tool_result`: truncated tool output
    /// All data is stored in Tantivy with ZERO truncation for full-text retrieval.
    #[allow(clippy::too_many_arguments)]
    pub fn record_react(
        &self,
        session_id: &str,
        task_desc: &str,
        tool: &str,
        target: &str,
        outcome: &str,
        decision: &str,
        assistant_text: &str,
        reasoning: &str,
        tool_result: &str,
    ) -> Result<()> {
        let created_at =
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;

        self.react_index.add_record(
            session_id,
            task_desc,
            &created_at,
            timestamp,
            tool,
            target,
            outcome,
            decision,
            assistant_text,
            reasoning,
            tool_result,
            &[],
        )?;

        Ok(())
    }

    /// Get unimpacted ReAct timeline (oldest first) for context injection.
    pub fn get_react_timeline(&self, session_id: &str, limit: usize) -> Result<String> {
        let records = self.react_index.get_active_react_records(session_id, limit)?;

        let mut out = String::new();
        let mut time_group = String::new();
        for r in &records {
            let ts = &r.created_at;
            let date: String = ts.chars().take(16).collect();
            if date != time_group {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("🔄 [{}]\n", date));
                time_group = date;
            }
            let icon = if r.outcome == "ok" || r.outcome.starts_with("ok") {
                "✅"
            } else {
                "⚠️"
            };
            let target_short: String = r.target.chars().take(50).collect();
            out.push_str(&format!("  {} {} {}\n", icon, r.tool, target_short));
            if !r.decision.is_empty() {
                out.push_str(&format!(
                    "    → {}\n",
                    r.decision.chars().take(100).collect::<String>()
                ));
            }
        }
        Ok(out)
    }

    /// Retrieve ReAct history by searching Tantivy with context keywords.
    ///
    /// Instead of linear time-based truncation, this function:
    /// 1. Builds a query from context keywords (files, task, errors)
    /// 2. Searches Tantivy BM25 index
    /// 3. Returns the FULL, UNCUT react_log records for top results
    pub fn get_graph_related_react(
        &self,
        session_id: &str,
        context_files: &[String],
        context_task: &str,
        context_errors: &[String],
        max_records: usize,
    ) -> Result<String> {
        let mut keywords: Vec<String> = Vec::new();

        for file_path in context_files {
            if let Some(base) =
                std::path::Path::new(file_path).file_name().and_then(|s| s.to_str())
            {
                keywords.push(base.to_string());
            }
        }

        if !context_task.trim().is_empty() {
            let words: Vec<&str> = context_task
                .split(|c: char| !c.is_alphanumeric() && c != '.' && c != '_')
                .filter(|w| w.len() > 2)
                .collect();
            for w in words.iter().take(5) {
                keywords.push(w.to_string());
            }
        }

        for err in context_errors {
            if !err.is_empty() {
                keywords.push(err.to_string());
            }
        }

        if keywords.is_empty() {
            return Ok(String::new());
        }

        let search_query = keywords.join(" ");
        let search_results = self
            .react_index
            .search(session_id, &search_query, max_records)?;

        if search_results.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::new();
        out.push_str("📊 Memory Graph (Keyword-based retrieval):\n");
        out.push_str(&format!(
            "  Context: files={:?}, task={}, errors={:?}\n",
            context_files,
            context_task.chars().take(80).collect::<String>(),
            context_errors
        ));
        out.push('\n');

        out.push_str("📜 Related ReAct History (full records, no truncation):\n");
        for result in &search_results {
            let r = &result.record;
            let target_short: String = r.target.chars().take(60).collect();
            let status_icon = if r.outcome == "ok" { "✓" } else { "✗" };
            let task_short: String = r.task_desc.chars().take(80).collect();
            out.push_str(&format!(
                "  [{}] {} {} {}\n",
                task_short, status_icon, r.tool, target_short
            ));
            if !target_short.is_empty() {
                out.push_str(&format!("    target: {}\n", target_short));
            }
            if !r.decision.is_empty() {
                out.push_str(&format!(
                    "    decision: {}\n",
                    r.decision.chars().take(150).collect::<String>()
                ));
            }
            if !r.reasoning.is_empty() {
                out.push_str(&format!(
                    "    reasoning: {}\n",
                    r.reasoning.chars().take(300).collect::<String>()
                ));
            }
            if !r.assistant_text.is_empty() {
                out.push_str(&format!(
                    "    response: {}\n",
                    r.assistant_text.chars().take(200).collect::<String>()
                ));
            }
            if !r.tool_result.is_empty() {
                out.push_str(&format!(
                    "    result: {}\n",
                    r.tool_result.chars().take(500).collect::<String>()
                ));
            }
            out.push('\n');
        }

        Ok(out)
    }

    /// Retrieve relevant ReAct records using Tantivy BM25 full-text search.
    ///
    /// Segments the query with jieba (Chinese-aware tokenization),
    /// searches Tantivy index with BM25 scoring, returns FULL, UNCUT records.
    pub fn get_react_by_bm25(
        &self,
        session_id: &str,
        query: &str,
        top_n: usize,
        min_score: f64,
    ) -> Result<String> {
        let jieba = Jieba::new();
        let tokens = jieba.cut(query, false);
        let joined = tokens.join(" AND ");

        if joined.is_empty() {
            return Ok(String::new());
        }

        let results = self.react_index.search(session_id, &joined, top_n * 3)?;

        let mut out = String::new();
        let mut count = 0;

        for result in results {
            if (result.score as f64) < min_score {
                continue;
            }
            if result.record.tier != 0 {
                continue;
            }

            count += 1;
            let r = &result.record;

            out.push_str(&format!(
                "  [BM25 score={:.2}] {} ({} bytes)\n",
                result.score,
                r.created_at,
                r.task_desc.chars().count()
            ));
            out.push_str(&format!(
                "  📋 {}\n",
                r.task_desc.chars().take(100).collect::<String>()
            ));
            out.push_str(&format!(
                "  🔧 {} {}\n",
                r.tool,
                r.target.chars().take(60).collect::<String>()
            ));
            out.push_str(&format!(
                "  📊 {}\n",
                if r.outcome == "ok" {
                    "✓ success"
                } else {
                    "✗ failed"
                }
            ));

            if !r.decision.is_empty() {
                out.push_str(&format!(
                    "  💭 decision: {}\n",
                    r.decision.chars().take(200).collect::<String>()
                ));
            }
            if !r.reasoning.is_empty() {
                out.push_str(&format!(
                    "  🧠 reasoning: {}\n",
                    r.reasoning.chars().take(300).collect::<String>()
                ));
            }
            if !r.assistant_text.is_empty() {
                out.push_str(&format!(
                    "  💬 response: {}\n",
                    r.assistant_text.chars().take(200).collect::<String>()
                ));
            }
            if !r.tool_result.is_empty() {
                let tr: String = r.tool_result.chars().take(2000).collect();
                out.push_str(&format!("  📄 result: {}\n", tr));
            }
            out.push('\n');
        }

        if count > 0 {
            let header = format!(
                "🔍 BM25 Relevant History ({} records, score >= {:.2}):\n",
                count, min_score
            );
            out.insert_str(0, &header);
        }

        Ok(out)
    }

    /// Get the full ReAct mainline (oldest first) for context injection.
    ///
    /// **Two-tier structure:**
    /// 1. **Recent 3 turns**: full detail (decision + reasoning + result) — ZERO truncation from Tantivy
    /// 2. **Historical turns**: decision graph only — compact dependency chains
    ///
    /// A "turn" is auto-detected by time gaps (>30s between records).
    /// All data is retrieved from Tantivy with complete, uncut content.
    pub fn get_react_mainline(&self, session_id: &str, limit: usize) -> Result<String> {
        let records = self
            .react_index
            .get_session_records_chronological(session_id)?;

        if records.is_empty() {
            return Ok(String::new());
        }

        let rows: Vec<ReactRow> =
            records.into_iter().take(limit).map(|r| {
                (
                    r.created_at,
                    r.task_desc,
                    r.tool,
                    r.target,
                    r.outcome,
                    r.decision,
                    r.assistant_text,
                    r.reasoning,
                    r.tool_result,
                )
            }).collect();

        let mut turns: Vec<&[ReactRow]> = Vec::new();
        let mut turn_start = 0;
        for i in 1..rows.len() {
            let gap = time_gap_seconds(&rows[i - 1].0, &rows[i].0);
            if gap > 30 {
                turns.push(&rows[turn_start..i]);
                turn_start = i;
            }
        }
        turns.push(&rows[turn_start..]);

        let total_turns = turns.len();
        let recent_turns = 3;

        let mut out = String::new();

        for (turn_idx, turn) in turns.iter().copied().enumerate() {
            let is_recent = turn_idx >= total_turns.saturating_sub(recent_turns);
            if is_recent {
                continue;
            }

            let task_header = turn
                .first()
                .map(|r| r.1.chars().take(80).collect::<String>())
                .unwrap_or_default();

            out.push_str(&format!("── Turn {} ──\n", turn_idx + 1));
            if !task_header.is_empty() {
                out.push_str(&format!("📋 {}\n", task_header));
            }

            let mut chain = Vec::new();
            for (_, _task_desc, tool, target, outcome, decision, _, reasoning, _) in turn {
                let sym = if outcome == "ok" || outcome.starts_with("ok") {
                    "→"
                } else {
                    "✗"
                };
                let target_short: String = target.chars().take(40).collect();
                let step = if target_short.is_empty() {
                    format!("{} {}", sym, tool)
                } else {
                    format!("{} {}({})", sym, tool, target_short)
                };
                chain.push(step);

                if !decision.is_empty() || !reasoning.is_empty() {
                    let rationale = if !decision.is_empty() {
                        decision
                    } else {
                        reasoning
                    };
                    let r: String = rationale.chars().take(60).collect();
                    if !r.is_empty() {
                        out.push_str(&format!("  ↳ {}\n", r));
                    }
                }
            }
            out.push_str(&format!("  {}\n", chain.join(" → ")));
            out.push('\n');
        }

        for (turn_idx, turn) in turns.iter().copied().enumerate() {
            let is_recent = turn_idx >= total_turns.saturating_sub(recent_turns);
            if !is_recent {
                continue;
            }

            let task_header = turn
                .first()
                .map(|r| r.1.chars().take(80).collect::<String>())
                .unwrap_or_default();

            if total_turns > recent_turns {
                out.push_str(&format!("── Turn {} (recent) ──\n", turn_idx + 1));
            } else {
                out.push_str(&format!("── Turn {} ──\n", turn_idx + 1));
            }

            if !task_header.is_empty() {
                out.push_str(&format!("📋 {}\n", task_header));
            }

            for (_, _task_desc, tool, target, outcome, decision, assistant_text, reasoning, tool_result) in turn {
                let target_short: String = target.chars().take(60).collect();
                let ok = outcome == "ok" || outcome.starts_with("ok");
                let sym = if ok { "✓" } else { "✗" };
                out.push_str(&format!("  {} {} {}\n", sym, tool, target_short));

                if !decision.is_empty() {
                    out.push_str(&format!("    → {}\n", decision));
                }

                if !reasoning.is_empty() {
                    out.push_str(&format!("    ↳ {}\n", reasoning));
                }

                if !assistant_text.is_empty() {
                    out.push_str(&format!("    💬 {}\n", assistant_text));
                }

                if !tool_result.is_empty() {
                    out.push_str(&format!("    ← {}\n", tool_result));
                }
            }
            out.push('\n');
        }

        Ok(out)
    }

    /// Search react_log by query using Tantivy BM25 scoring.
    /// Returns top-n relevant records with ZERO truncation.
    pub fn search_react(
        &self,
        query: &str,
        session_id: &str,
        top_n: usize,
    ) -> Result<String> {
        let results = self.react_index.search(session_id, query, top_n)?;

        if results.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::new();
        out.push_str(&format!(
            "── ReAct Search Results (query: \"{}\") ──\n\n",
            query
        ));

        for result in results {
            let r = &result.record;
            out.push_str(&format!(
                "[score={:.2}] [{}] {}\n",
                result.score, r.created_at, r.task_desc
            ));
            out.push_str(&format!("  Tool: {} {}\n", r.tool, r.target));
            out.push_str(&format!("  Outcome: {}\n", r.outcome));

            if !r.decision.is_empty() {
                out.push_str(&format!("  Decision: {}\n", r.decision));
            }
            if !r.reasoning.is_empty() {
                out.push_str(&format!("  Reasoning: {}\n", r.reasoning));
            }
            if !r.assistant_text.is_empty() {
                out.push_str(&format!("  Assistant: {}\n", r.assistant_text));
            }
            if !r.tool_result.is_empty() {
                out.push_str(&format!("  Result: {}\n", r.tool_result));
            }
            if !r.keywords.is_empty() {
                out.push_str(&format!("  Keywords: {}\n", r.keywords.join(", ")));
            }
            out.push('\n');
        }

        Ok(out)
    }

    /// Get complete ReactRecords by IDs (zero truncation, from Tantivy).
    pub fn get_react_records_by_ids(
        &self,
        ids: &[i64],
    ) -> Result<Vec<crate::memory::react_index::ReactRecord>> {
        let uids: Vec<u64> = ids.iter().map(|i| *i as u64).collect();
        self.react_index.get_records_by_ids(&uids)
    }

    /// Write extracted keywords back to a Tantivy react_log document.
    pub fn update_react_keywords(
        &self,
        react_id: i64,
        keywords: &[crate::memory::memory_offload::KeywordItem],
    ) -> Result<()> {
        let tokens: Vec<String> = keywords
            .iter()
            .map(|k| format!("{}:{}", k.cat, k.kw))
            .collect();
        self.react_index
            .update_record_keywords(react_id as u64, &tokens)
    }

    /// Build context for injection: ALL un-impacted records (full detail) +
    /// semantically searched impacted graph records, sorted by time + weight.
    pub fn get_context_for_injection(
        &self,
        session_id: &str,
        task_desc: &str,
        current_files: &[String],
    ) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();

        let active_limit = 10_000;
        let active_records = self
            .react_index
            .get_active_react_records(session_id, active_limit)?;

        if !active_records.is_empty() {
            let mut active_text = String::new();
            active_text.push_str("🕐 Active Memory (Un-impacted, Full Detail):\n");

            for r in &active_records {
                active_text.push_str(&format!(
                    "  [{}] {} {} → {}\n",
                    r.created_at.chars().take(16).collect::<String>(),
                    r.tool,
                    r.target.chars().take(60).collect::<String>(),
                    r.outcome
                ));

                if !r.decision.is_empty() {
                    active_text.push_str(&format!("    → {}\n", r.decision));
                }
                if !r.reasoning.is_empty() {
                    active_text.push_str(&format!("    ↳ {}\n", r.reasoning));
                }
                if !r.assistant_text.is_empty() {
                    active_text.push_str(&format!("    💬 {}\n", r.assistant_text));
                }
                if !r.tool_result.is_empty() {
                    active_text.push_str(&format!(
                        "    ← {}\n",
                        r.tool_result.chars().take(300).collect::<String>()
                    ));
                }
                if !r.keywords.is_empty() {
                    active_text.push_str(&format!(
                        "    🏷️ Tags: {}\n",
                        r.keywords.join(", ")
                    ));
                }
                active_text.push('\n');
            }

            parts.push(active_text);
        }

        let query_keywords: Vec<String> = {
            let mut kws = Vec::new();
            if !task_desc.is_empty() {
                let words: Vec<&str> = task_desc
                    .split(|c: char| !c.is_alphanumeric() && c != '.' && c != '_')
                    .filter(|w| w.len() > 2)
                    .collect();
                for w in words.iter().take(5) {
                    kws.push(w.to_string());
                }
            }
            for f in current_files.iter().take(3) {
                if let Some(base) =
                    std::path::Path::new(f).file_name().and_then(|s| s.to_str())
                {
                    let clean = base
                        .trim_end_matches(".rs")
                        .trim_end_matches(".toml")
                        .trim_end_matches(".json");
                    if clean.len() > 2 {
                        kws.push(clean.to_string());
                    }
                }
            }
            kws
        };

        if !query_keywords.is_empty() {
            let limit = 20;
            let graph_hits = self
                .react_index
                .search_by_keywords(session_id, &query_keywords, limit)?;

            let mut graph_records: Vec<crate::memory::react_index::ReactRecord> =
                graph_hits
                    .into_iter()
                    .filter(|r| {
                        r.doc_type == crate::memory::react_index::DOC_TYPE_GRAPH
                    })
                    .collect();

            if !graph_records.is_empty() {
                graph_records.sort_by(|a, b| {
                    b.weight
                        .partial_cmp(&a.weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(b.timestamp.cmp(&a.timestamp))
                });
                graph_records.truncate(10);

                let mut graph_text = String::new();
                graph_text
                    .push_str("📚 Archived Memory (Impact Graphs, Semantic Match):\n");

                for r in &graph_records {
                    let summary = if r.summary.is_empty() {
                        &r.task_desc
                    } else {
                        &r.summary
                    };
                    let tier_label = match r.tier {
                        3 => "◆◆ L3",
                        2 => "◆ L2",
                        1 => "◇ L1",
                        _ => "○ L0",
                    };
                    graph_text.push_str(&format!(
                        "  {} [{}] (weight={:.2}, hits={}) {}\n",
                        tier_label,
                        r.created_at.chars().take(16).collect::<String>(),
                        r.weight,
                        r.hit_count,
                        summary.chars().take(100).collect::<String>()
                    ));
                    if !r.keywords.is_empty() {
                        graph_text.push_str(&format!(
                            "    🏷️ Tags: {}\n",
                            r.keywords.join(", ")
                        ));
                    }
                    if !r.detail.is_empty() {
                        graph_text.push_str(&format!(
                            "    📋 {}\n",
                            r.detail.chars().take(200).collect::<String>()
                        ));
                    }
                    graph_text.push('\n');
                }

                parts.push(graph_text);
            }
        }

        if parts.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::new();
        out.push_str("📜 Memory System (Active + Archived Graphs):\n");
        for part in &parts {
            out.push_str(part);
        }

        Ok(out)
    }

    /// Search react_log by keyword tag (e.g. cat="problem", kw="内存泄漏").
    pub fn search_react_by_keyword(
        &self,
        session_id: &str,
        cat: Option<&str>,
        kw: &str,
        top_n: usize,
    ) -> Result<Vec<crate::memory::react_index::SearchResult>> {
        let token = match cat {
            Some(c) => format!("{}:{}", c, kw),
            None => format!(":{}", kw),
        };
        if cat.is_none() {
            let categories = ["problem", "conclusion", "fix", "file", "error", "concept"];
            let mut merged: Vec<crate::memory::react_index::SearchResult> = Vec::new();
            for c in categories {
                let t = format!("{}:{}", c, kw);
                if let Ok(hits) =
                    self.react_index.search_by_keywords(session_id, &[t.clone()], top_n)
                {
                    for record in hits {
                        merged.push(crate::memory::react_index::SearchResult {
                            record,
                            score: 1.0,
                        });
                    }
                }
            }
            merged.truncate(top_n);
            Ok(merged)
        } else {
            let records = self
                .react_index
                .search_by_keywords(session_id, &[token.clone()], top_n)?;
            Ok(records
                .into_iter()
                .map(|record| crate::memory::react_index::SearchResult {
                    record,
                    score: 1.0,
                })
                .collect())
        }
    }

    /// Get memory_graphs (id, summary, detail) for LLM keyword extraction.
    pub fn get_memory_graphs_for_extraction(
        &self,
        session_id: &str,
    ) -> Result<Vec<(i64, String, String)>> {
        let records = self
            .react_index
            .get_all_graphs_for_session(session_id, 10000)?;
        let mut out = Vec::new();
        for r in records {
            if r.merged_into.is_none() {
                out.push((
                    r.id.parse::<i64>().unwrap_or(0),
                    r.summary,
                    r.detail,
                ));
            }
        }
        Ok(out)
    }

    /// Write extracted keywords back to a Tantivy graph record.
    pub fn update_graph_keywords(
        &self,
        graph_id: i64,
        keywords: &[crate::memory::memory_offload::KeywordItem],
    ) -> Result<()> {
        let tokens: Vec<String> = keywords
            .iter()
            .map(|k| format!("{}:{}", k.cat, k.kw))
            .collect();
        self.react_index
            .update_record_keywords(graph_id as u64, &tokens)
    }

    /// Build a temporal-spatial memory graph for context injection.
    pub fn get_memory_graph(
        &self,
        session_id: &str,
        query_keywords: &[String],
        limit: usize,
    ) -> Result<String> {
        let all_records = self
            .react_index
            .get_all_records_for_graph(session_id, limit)?;

        if all_records.is_empty() {
            return Ok(String::new());
        }

        let related_by_kw = if query_keywords.is_empty() {
            Vec::new()
        } else {
            self.react_index
                .search_by_keywords(session_id, query_keywords, 20)?
        };
        let mut related_ids = std::collections::HashSet::new();
        for r in &related_by_kw {
            related_ids.insert(r.id.clone());
        }

        let mut kw_index: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for r in &all_records {
            for kw in &r.keywords {
                kw_index.entry(kw.clone()).or_default().push(r.id.clone());
            }
        }

        let mut connections: Vec<(String, String, String)> = Vec::new();
        let mut seen_edges = std::collections::HashSet::new();

        for (kw, ids) in &kw_index {
            if ids.len() > 1 {
                for i in 0..ids.len() {
                    for j in (i + 1)..ids.len() {
                        let edge_key = format!("{}-{}-{}", ids[i], ids[j], kw);
                        if seen_edges.insert(edge_key) {
                            let (from, to) = if ids[i] < ids[j] {
                                (&ids[i], &ids[j])
                            } else {
                                (&ids[j], &ids[i])
                            };
                            connections.push((from.clone(), to.clone(), kw.clone()));
                        }
                    }
                }
            }
        }

        let mut output = String::new();
        output.push_str("📊 Memory Graph (Temporal + Keyword Connections):\n\n");

        let active_records: Vec<&crate::memory::react_index::ReactRecord> = all_records
            .iter()
            .filter(|r| r.doc_type == crate::memory::react_index::DOC_TYPE_REACT)
            .collect();
        let archived_records: Vec<&crate::memory::react_index::ReactRecord> = all_records
            .iter()
            .filter(|r| r.doc_type == crate::memory::react_index::DOC_TYPE_GRAPH)
            .collect();

        if !active_records.is_empty() {
            output.push_str("**Active Memory (Recent Steps):**\n");
            for r in &active_records {
                let marker = if related_ids.contains(&r.id) {
                    "🎯 "
                } else {
                    "  "
                };
                output.push_str(&format!(
                    "{} [{}] {} {} → {}\n",
                    marker,
                    r.created_at.chars().take(16).collect::<String>(),
                    r.tool,
                    r.target.chars().take(40).collect::<String>(),
                    r.outcome
                ));
                if !r.keywords.is_empty() {
                    output.push_str(&format!("     Tags: {}\n", r.keywords.join(", ")));
                }
            }
            output.push('\n');
        }

        if !archived_records.is_empty() {
            output.push_str("**Archived Memory (Summaries):**\n");
            for r in &archived_records {
                let marker = if related_ids.contains(&r.id) {
                    "🎯 "
                } else {
                    "  "
                };
                let summary = if r.summary.is_empty() {
                    &r.task_desc
                } else {
                    &r.summary
                };
                output.push_str(&format!(
                    "{} [{}] {}\n",
                    marker,
                    r.created_at.chars().take(16).collect::<String>(),
                    summary.chars().take(80).collect::<String>()
                ));
                if !r.keywords.is_empty() {
                    output.push_str(&format!("     Tags: {}\n", r.keywords.join(", ")));
                }
            }
            output.push('\n');
        }

        if !connections.is_empty() {
            let mut unique_connections: Vec<(String, String)> = Vec::new();
            let mut seen_pairs = std::collections::HashSet::new();

            for (from, to, _kw) in &connections {
                let pair = if from < to {
                    format!("{}-{}", from, to)
                } else {
                    format!("{}-{}", to, from)
                };
                if seen_pairs.insert(pair) {
                    unique_connections.push((from.clone(), to.clone()));
                }
            }

            if !unique_connections.is_empty() {
                output.push_str("**Connections (shared keywords):**\n");
                for (from, to) in unique_connections.iter().take(10) {
                    let shared_kws: Vec<String> = connections
                        .iter()
                        .filter(|(f, t, _)| {
                            (f == from && t == to) || (f == to && t == from)
                        })
                        .map(|(_, _, k)| k.clone())
                        .collect();
                    let label = if let Some(first_kw) = shared_kws.first() {
                        first_kw.replace(":", "=")
                    } else {
                        "related".to_string()
                    };
                    output.push_str(&format!(
                        "  [{}] ↔ [{}] (shared: {})\n",
                        from, to, label
                    ));
                }
                output.push('\n');
            }
        }

        output.push_str("---\n");
        Ok(output)
    }

    /// Search react_log by keyword tag for graph building
    pub fn search_react_by_keywords(
        &self,
        session_id: &str,
        keyword_tokens: &[String],
        top_n: usize,
    ) -> Result<Vec<crate::memory::react_index::ReactRecord>> {
        self.react_index
            .search_by_keywords(session_id, keyword_tokens, top_n)
    }

    /// Like `get_react_timeline` but prefixes each row with its `react_log.id`
    /// (`[id=N]`), so the summarizer can reference rows in its cluster output.
    pub fn get_react_timeline_with_ids(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<String> {
        let records = self.react_index.get_active_react_records(session_id, limit)?;

        let mut out = String::new();
        for r in &records {
            let id = r.id.parse::<i64>().unwrap_or(0);
            let ts = &r.created_at;
            let date: String = ts.chars().take(16).collect();
            let target_short: String = r.target.chars().take(60).collect();
            out.push_str(&format!(
                "[id={id}] [{date}] {} {target_short} → {}\n",
                r.tool, r.outcome
            ));
            if !r.decision.is_empty() {
                out.push_str(&format!(
                    "    判断: {}\n",
                    r.decision.chars().take(120).collect::<String>()
                ));
            }
        }
        Ok(out)
    }

    /// Archive a batch of ReAct rows into clustered memory-graph nodes (Tantivy).
    pub fn archive_react_batch(
        &self,
        session_id: &str,
        clusters: &[GraphNode],
    ) -> Result<()> {
        if clusters.is_empty() {
            return Ok(());
        }
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;

        for node in clusters {
            let weight = if node.topic.contains("[IMPACT]") {
                2.0
            } else {
                1.0
            };
            let tier = 1;

            let detail_with_refs = if node.react_ids.is_empty() {
                node.summary.clone()
            } else {
                format!("{} [react_ids: {:?}]", node.summary, node.react_ids)
            };

            let keywords = vec![format!("topic:{}", node.topic)];

            self.react_index.add_graph_record(
                session_id,
                &node.topic,
                &detail_with_refs,
                timestamp,
                &keywords,
                tier,
                weight,
            )?;
        }
        self.react_index.commit()?;
        Ok(())
    }

    /// Get memory-graph node titles for this session.
    pub fn get_memory_graphs(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, i64, f64)>> {
        let records = self
            .react_index
            .get_all_graphs_for_session(session_id, limit * 10)?;
        let mut out: Vec<(i64, String, i64, f64)> = records
            .iter()
            .filter(|r| r.tier > 0 && r.merged_into.is_none())
            .map(|r| {
                (
                    r.id.parse::<i64>().unwrap_or(0),
                    r.summary.clone(),
                    r.tier,
                    r.weight,
                )
            })
            .collect();
        out.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then(b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
                .then(b.0.cmp(&a.0))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Record a recall hit on a graph node (drives L2→L3 promotion + anti-downgrade).
    pub fn touch_graph_hit(&self, graph_id: i64) -> Result<()> {
        let record = self.react_index.get_record_by_id(graph_id as u64)?;
        if let Some(mut r) = record {
            r.hit_count += 1;
            r.last_hit_at =
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
            self.react_index.update_graph_metadata(
                graph_id as u64,
                None,
                None,
                None,
                Some(r.hit_count),
                Some(r.last_hit_at),
            )?;
            self.react_index.commit()?;
        }
        Ok(())
    }

    /// Read a `meta` key (e.g. `last_l1l2_consolidation`).
    pub fn meta_get(&self, key: &str) -> Option<String> {
        let meta = self.meta_store.lock().unwrap();
        meta.get(key).cloned()
    }

    /// Write a `meta` key.
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let mut meta = self.meta_store.lock().unwrap();
        meta.insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Load active tier-1 nodes (candidates for L1→L2 consolidation).
    pub fn get_l1_nodes(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, f64)>> {
        let records = self.react_index.get_graph_records(session_id, limit)?;
        let mut out = Vec::new();
        for r in records {
            if r.tier == 1 && r.merged_into.is_none() {
                let id = r.id.parse::<i64>().unwrap_or(0);
                out.push((id, r.summary, r.weight));
            }
        }
        Ok(out)
    }

    /// Apply one L1→L2 merge group.
    pub fn apply_l1_l2_merge(
        &self,
        session_id: &str,
        topic: &str,
        summary: &str,
        member_ids: &[i64],
        weight: f64,
    ) -> Result<i64> {
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;

        let new_id = self.react_index.add_graph_record(
            session_id,
            topic,
            summary,
            timestamp,
            &[],
            2,
            weight,
        )?;

        let new_id_str = new_id.to_string();
        for mid in member_ids {
            self.react_index.update_graph_metadata(
                *mid as u64,
                None,
                None,
                Some(new_id_str.clone()),
                None,
                None,
            )?;
        }
        self.react_index.commit()?;
        Ok(new_id as i64)
    }

    /// Downgrade stale tier-2/tier-1 nodes.
    pub fn downgrade_stale_nodes(
        &self,
        session_id: &str,
        stale_days: u32,
    ) -> Result<usize> {
        let records = self
            .react_index
            .get_all_graphs_for_session(session_id, 10000)?;
        let now = chrono::Utc::now();
        let stale_threshold = now - chrono::Duration::days(stale_days as i64);
        let stale_threshold_2x =
            now - chrono::Duration::days((stale_days * 2) as i64);

        let mut downgraded = 0;
        for r in records {
            let last_hit = if r.last_hit_at.is_empty() {
                chrono::DateTime::from_timestamp_millis(r.timestamp as i64)
                    .unwrap_or(now)
            } else {
                chrono::DateTime::parse_from_str(
                    &r.last_hit_at,
                    "%Y-%m-%d %H:%M:%S%.3f",
                )
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or(now)
            };

            let last_hit_timestamp = last_hit.timestamp();
            let stale_seconds = stale_threshold.timestamp();
            let stale_2x_seconds = stale_threshold_2x.timestamp();

            let id = r.id.parse::<u64>().unwrap_or(0);

            if r.tier == 2 && r.merged_into.is_none() && last_hit_timestamp < stale_seconds
            {
                self.react_index
                    .update_graph_metadata(id, Some(1), None, None, None, None)?;
                downgraded += 1;
            } else if r.tier == 1
                && r.merged_into.is_none()
                && last_hit_timestamp < stale_2x_seconds
            {
                self.react_index
                    .update_graph_metadata(id, Some(0), None, None, None, None)?;
                downgraded += 1;
            }
        }
        Ok(downgraded)
    }

    /// L2→L3 promotion candidates.
    pub fn get_l3_candidates(
        &self,
        session_id: &str,
        min_hits: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        let records = self
            .react_index
            .get_all_graphs_for_session(session_id, limit * 10)?;
        let mut out: Vec<(i64, String)> = records
            .iter()
            .filter(|r| r.tier == 2 && r.merged_into.is_none() && r.hit_count >= min_hits)
            .map(|r| (r.id.parse::<i64>().unwrap_or(0), r.summary.clone()))
            .collect();
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.truncate(limit);
        Ok(out)
    }

    /// Mark a node as promoted to L3 (tier=3).
    pub fn mark_promoted_l3(&self, graph_id: i64) -> Result<()> {
        self.react_index
            .update_graph_metadata(graph_id as u64, Some(3), None, None, None, None)?;
        self.react_index.commit()?;
        Ok(())
    }

    /// Node replay: reconstruct the full ReAct trace consolidated into one graph node.
    pub fn get_react_batch_by_graph(&self, graph_id: i64) -> Result<String> {
        let graph_record = self.react_index.get_record_by_id(graph_id as u64)?;

        let mut out = String::new();

        if let Some(record) = graph_record {
            let topic = &record.summary;
            let detail = &record.detail;

            if !topic.is_empty() {
                out.push_str(&format!(
                    "📊 记忆图谱节点 #{graph_id}: {}\n",
                    topic
                ));
            }
            if !detail.is_empty() {
                out.push_str(&format!("{}\n", detail));
            }
            out.push_str(
                "┈┈┈ 原始 ReAct（[user] → [assistant] → [tool_result]）┈┈┈\n",
            );

            let react_ids = Self::extract_react_ids_from_detail(detail);

            if !react_ids.is_empty() {
                let react_records = self.react_index.get_records_by_ids(&react_ids)?;

                for r in &react_records {
                    let ts = &r.created_at;
                    let date: String = ts.chars().take(19).collect();
                    let icon =
                        if r.outcome == "ok" || r.outcome.starts_with("ok") {
                            "✅"
                        } else {
                            "⚠️"
                        };

                    out.push_str(&format!(
                        "[user] {date} {}\n",
                        r.task_desc.chars().take(200).collect::<String>()
                    ));

                    let think = if !r.assistant_text.trim().is_empty() {
                        r.assistant_text.clone()
                    } else {
                        r.decision.clone()
                    };
                    if !think.trim().is_empty() {
                        out.push_str(&format!(
                            "[assistant] {}\n",
                            think.chars().take(400).collect::<String>()
                        ));
                    }

                    let target_short: String = r.target.chars().take(60).collect();
                    out.push_str(&format!(
                        "[tool_result] {icon} {}({})\n",
                        r.tool, target_short
                    ));
                    if !r.tool_result.trim().is_empty() {
                        out.push_str(&format!(
                            "  {}\n",
                            r.tool_result.chars().take(500).collect::<String>()
                        ));
                    }
                    out.push('\n');
                }
            }
        }

        Ok(out)
    }

    fn extract_react_ids_from_detail(detail: &str) -> Vec<u64> {
        if let Some(start) = detail.find("[react_ids:") {
            let rest = &detail[start..];
            if let Some(bracket_start) = rest.find('[') {
                let ids_str = &rest[bracket_start + 1..];
                if let Some(bracket_end) = ids_str.find(']') {
                    let ids_str = &ids_str[..bracket_end];
                    return ids_str
                        .split(',')
                        .filter_map(|s| s.trim().parse::<u64>().ok())
                        .collect();
                }
            }
        }
        Vec::new()
    }
}

/// Compute time gap in seconds between two datetime strings.
fn time_gap_seconds(ts1: &str, ts2: &str) -> i64 {
    fn to_seconds(ts: &str) -> Option<i64> {
        let s = ts.trim();
        let time_part = if s.len() >= 19 && s.as_bytes()[10] == b' ' {
            &s[11..19]
        } else if s.len() >= 8 {
            &s[s.len() - 8..]
        } else {
            return None;
        };
        let h: i64 = time_part[0..2].parse().ok()?;
        let m: i64 = time_part[3..5].parse().ok()?;
        let sec: i64 = time_part[6..8].parse().ok()?;
        Some(h * 3600 + m * 60 + sec)
    }

    fn to_days(ts: &str) -> Option<i64> {
        let s = ts.trim();
        if s.len() >= 10 {
            let y: i64 = s[0..4].parse().ok()?;
            let mo: i64 = s[5..7].parse().ok()?;
            let d: i64 = s[8..10].parse().ok()?;
            Some(y * 365 + mo * 31 + d)
        } else {
            None
        }
    }

    let Some(t1) = to_seconds(ts1) else { return 0 };
    let Some(t2) = to_seconds(ts2) else { return 0 };
    let Some(d1) = to_days(ts1) else { return 0 };
    let Some(d2) = to_days(ts2) else { return 0 };

    let day_gap = (d2 - d1) * 86400;
    let sec_gap = t2 - t1;
    (day_gap + sec_gap).abs()
}

/// Check if a target string looks like a file path.
pub fn is_file_path(target: &str) -> bool {
    let t = target.trim();
    if t.is_empty() {
        return false;
    }
    let has_separator = t.contains('/') || t.contains('\\') || t.starts_with('.');
    let has_extension = t.contains('.') && !t.starts_with('.') && !t.ends_with('.');
    let known_ext = matches!(
        t.rsplit('.').next().unwrap_or(""),
        "rs" | "toml" | "json" | "md" | "txt" | "py" | "js" | "ts" | "tsx" | "jsx"
        | "css" | "html" | "yml" | "yaml" | "xml" | "lock" | "cfg" | "ini" | "log"
        | "sql" | "sh" | "bat" | "ps1" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
        | "rb" | "php" | "swift" | "kt" | "scala" | "lua" | "vim"
    );
    has_separator || (has_extension && known_ext)
}

/// Extract a stable error signature from tool_result for graph linking.
fn extract_error_signature(tool_result: &str, outcome: &str) -> String {
    if let Some(start) = tool_result.find("error[E") {
        let chunk = &tool_result[start..];
        if let Some(end) = chunk.find('\n') {
            let line = &chunk[..end].trim();
            if let Some(id_start) = line.find("error[E") {
                let rest = &line[id_start + 7..];
                if let Some(bracket_end) = rest.find(']') {
                    let code = &rest[..bracket_end];
                    let msg = rest[bracket_end + 1..].trim_start_matches(':').trim();
                    let short_msg: String = msg.chars().take(40).collect();
                    return format!("E{}: {}", code, short_msg);
                }
            }
        }
    }

    let lower = tool_result.to_lowercase();
    if lower.contains("error:") || lower.contains("error[") {
        for line in tool_result.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("error:") || line_lower.contains("error[") {
                let sig: String = line.chars().take(60).collect();
                return sig;
            }
        }
    }

    if lower.contains("panicked") || lower.contains("panic") {
        return "panic".to_string();
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return "timeout".to_string();
    }
    if lower.contains("permission denied") || lower.contains("access denied") {
        return "permission denied".to_string();
    }
    if lower.contains("not found") || lower.contains("no such") {
        return "not found".to_string();
    }

    if !outcome.is_empty() && outcome != "ok" {
        let sig: String = outcome.chars().take(40).collect();
        return sig;
    }

    String::new()
}

/// without tokenization or embedding.
fn trigram_overlap(a: &str, b: &str) -> f64 {
    fn trigrams(s: &str) -> std::collections::HashSet<String> {
        s.chars()
            .collect::<Vec<_>>()
            .windows(3)
            .map(|w| w.iter().collect())
            .collect()
    }
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    ta.intersection(&tb).count() as f64 / ta.union(&tb).count() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::unified_action::{FileModifiedRecord, KeyFact};

    #[test]
    fn test_save_and_query() {
        let dir = std::env::temp_dir().join("ox_memory_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let store = MemoryStore::open(&dir.join("test.db")).unwrap();

        let mut summary = SessionSummary::default();
        summary.learnings = "订单系统用策略工厂".into();
        summary.key_facts.push(KeyFact {
            fact: "策略工厂负责状态转换".into(),
            files: vec!["X.java".into()],
        });
        summary.files_modified.push(FileModifiedRecord {
            path: "src/X.java".into(),
            summary: "加了null检查".into(),
        });

        store.save_session("test-1", "测试任务", &summary).unwrap();

        let result = store.query_file_history("X.java", 5).unwrap();
        assert!(result.contains("订单系统"));
        assert!(result.contains("null检查"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tiering_lifecycle() {
        let unique_id = format!("tier_test_{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("ox_tier_test_{}", unique_id));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MemoryStore::open(&dir.join("test.db")).unwrap();

        store.record_react(
            &unique_id,
            "task about module",
            "read",
            "src/main.rs",
            "ok",
            "need to read main",
            "reading file",
            "found the entry point",
            "file contents...",
        ).unwrap();

        store.record_react(
            &unique_id,
            "task about module",
            "edit",
            "src/main.rs",
            "ok",
            "need to fix",
            "editing file",
            "fixed the bug",
            "file modified successfully",
        ).unwrap();

        let graph = store.get_memory_graph(&unique_id, &[], 10).unwrap();
        assert!(!graph.is_empty());

        let batch = store.get_react_batch_by_graph(1).unwrap();
        assert!(!batch.is_empty());

        let nodes = store.get_memory_graphs(&unique_id, 10).unwrap();
        assert!(nodes.is_empty());

        let clusters = vec![GraphNode {
            topic: "test_topic".to_string(),
            summary: "test summary".to_string(),
            react_ids: vec![1, 2],
        }];
        store.archive_react_batch(&unique_id, &clusters).unwrap();

        let nodes = store.get_memory_graphs(&unique_id, 10).unwrap();
        assert_eq!(nodes.len(), 1);

        store.touch_graph_hit(nodes[0].0).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_meta_store() {
        let dir = std::env::temp_dir().join("ox_meta_test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = MemoryStore::open(&dir.join("test.db")).unwrap();

        assert!(store.meta_get("key1").is_none());
        store.meta_set("key1", "value1").unwrap();
        assert_eq!(store.meta_get("key1"), Some("value1".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_react_mainline() {
        let dir = std::env::temp_dir().join("ox_mainline_test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = MemoryStore::open(&dir.join("test.db")).unwrap();

        store.record_react(
            "s1",
            "task desc",
            "read",
            "file.rs",
            "ok",
            "decision1",
            "assistant text",
            "reasoning here",
            "tool result",
        ).unwrap();

        let mainline = store.get_react_mainline("s1", 10).unwrap();
        assert!(!mainline.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_bm25_search() {
        let dir = std::env::temp_dir().join("ox_bm25_test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = MemoryStore::open(&dir.join("test.db")).unwrap();

        store.record_react(
            "s1",
            "refactor module system",
            "edit",
            "module.rs",
            "ok",
            "need refactor",
            "assistant",
            "reasoning",
            "done",
        ).unwrap();

        let result = store.get_react_by_bm25("s1", "refactor", 5, 0.1).unwrap();
        assert!(!result.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}