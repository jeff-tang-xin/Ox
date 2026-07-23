//! SQLite-backed session memory store.
//! Persists LLM's session summaries (learnings, facts, file changes) across sessions.
//! Path: `<project_root>/.ox/memory.db`

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::agent::unified_action::SessionSummary;

pub struct MemoryStore {
    conn: Mutex<Connection>,
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
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                task_desc TEXT NOT NULL DEFAULT '',
                content_summary TEXT NOT NULL DEFAULT '',
                learnings TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS key_facts (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                fact_text TEXT NOT NULL,
                related_files TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS files_read (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                file_path TEXT NOT NULL,
                purpose TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS files_modified (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                file_path TEXT NOT NULL,
                change_summary TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS react_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                task_desc TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                tool TEXT NOT NULL,
                target TEXT NOT NULL DEFAULT '',
                outcome TEXT NOT NULL DEFAULT '',
                decision TEXT NOT NULL DEFAULT '',
                assistant_text TEXT NOT NULL DEFAULT '',
                reasoning TEXT NOT NULL DEFAULT '',
                tool_result TEXT NOT NULL DEFAULT '',
                impacted INTEGER NOT NULL DEFAULT 0,
                graph_id INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_react_session ON react_log(session_id);
            CREATE INDEX IF NOT EXISTS idx_react_impacted ON react_log(impacted);
            CREATE TABLE IF NOT EXISTS memory_graphs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                summary TEXT NOT NULL,
                detail TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_facts_text ON key_facts(fact_text);
            CREATE INDEX IF NOT EXISTS idx_modified_path ON files_modified(file_path);
            CREATE INDEX IF NOT EXISTS idx_key_facts_session ON key_facts(session_id);
            CREATE INDEX IF NOT EXISTS idx_files_read_session ON files_read(session_id);
            CREATE INDEX IF NOT EXISTS idx_files_read_path ON files_read(file_path);
            CREATE INDEX IF NOT EXISTS idx_files_modified_session ON files_modified(session_id);",
        )?;
        // Migrate pre-existing DBs: add the ReAct-triple columns if missing.
        // ADD COLUMN errors when the column already exists — ignore that.
        let _ = conn.execute(
            "ALTER TABLE react_log ADD COLUMN assistant_text TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE react_log ADD COLUMN tool_result TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE react_log ADD COLUMN reasoning TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Memory-graph tiering columns (L1/L2/L3 + downgrade). Idempotent.
        let _ = conn.execute(
            "ALTER TABLE memory_graphs ADD COLUMN tier INTEGER NOT NULL DEFAULT 1",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE memory_graphs ADD COLUMN weight REAL NOT NULL DEFAULT 1.0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE memory_graphs ADD COLUMN hit_count INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE memory_graphs ADD COLUMN last_hit_at TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE memory_graphs ADD COLUMN merged_into INTEGER",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Save a session summary for a completed session.
    /// Uses a single transaction so partial failures don't leave inconsistent state,
    /// and deletes prior child rows for this session_id to avoid duplicate accumulation
    /// across INSERT OR REPLACE on `sessions`.
    /// Merges into the last session if it's about the same topic (same task_desc keywords
    /// AND overlapping modified files), so 审核→修正→修复 不会变成三个独立批次.
    pub fn save_session(
        &self,
        session_id: &str,
        task_desc: &str,
        summary: &SessionSummary,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();

        // Check if last session is about the same topic → merge instead of insert
        let merged_id = self.find_merge_target(&conn, task_desc, summary);

        let target_id = merged_id.as_deref().unwrap_or(session_id);
        let tx = conn.transaction()?;

        tx.execute(
            "DELETE FROM key_facts WHERE session_id = ?1",
            params![target_id],
        )?;
        tx.execute(
            "DELETE FROM files_read WHERE session_id = ?1",
            params![target_id],
        )?;
        tx.execute(
            "DELETE FROM files_modified WHERE session_id = ?1",
            params![target_id],
        )?;

        // If merging, append learnings; otherwise use as-is
        let merged_learnings = if merged_id.is_some() {
            let old: String = tx
                .query_row(
                    "SELECT learnings FROM sessions WHERE id = ?1",
                    params![target_id],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            if !old.is_empty() && !summary.learnings.is_empty() {
                format!("{} → {}", old, summary.learnings)
            } else {
                summary.learnings.clone()
            }
        } else {
            summary.learnings.clone()
        };

        tx.execute(
            "INSERT OR REPLACE INTO sessions (id, task_desc, content_summary, learnings)
             VALUES (?1, ?2, ?3, ?4)",
            params![target_id, task_desc, "", merged_learnings],
        )?;

        for f in &summary.key_facts {
            tx.execute(
                "INSERT INTO key_facts (session_id, fact_text, related_files) VALUES (?1, ?2, ?3)",
                params![target_id, f.fact, f.files.join(", ")],
            )?;
        }
        for r in &summary.files_read {
            tx.execute(
                "INSERT INTO files_read (session_id, file_path, purpose) VALUES (?1, ?2, ?3)",
                params![target_id, r.path, r.purpose],
            )?;
        }
        for m in &summary.files_modified {
            tx.execute(
                "INSERT INTO files_modified (session_id, file_path, change_summary) VALUES (?1, ?2, ?3)",
                params![target_id, m.path, m.summary],
            )?;
        }
        for s in &summary.skills {
            tracing::info!("[MEMORY] Skill suggested: {} (scope={})", s.id, s.scope);
        }

        tx.commit()?;
        Ok(())
    }

    /// Find a merge target: the last session within 30 min with high trigram
    /// similarity in learnings, or overlapping file paths.
    fn find_merge_target(
        &self,
        conn: &rusqlite::Connection,
        _task_desc: &str,
        summary: &SessionSummary,
    ) -> Option<String> {
        let last: Result<(String, String), _> = conn.query_row(
            "SELECT id, learnings FROM sessions
             WHERE created_at >= datetime('now', '-30 minutes')
             ORDER BY created_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        let Ok((last_id, last_learnings)) = last else {
            return None;
        };

        // Trigram similarity >25% → same topic, merge
        if trigram_overlap(&last_learnings, &summary.learnings) > 0.25 {
            return Some(last_id);
        }

        // Fallback: same modified files → same batch
        let new_files: Vec<String> = summary
            .files_modified
            .iter()
            .map(|m| m.path.rsplit('/').next().unwrap_or(&m.path).to_lowercase())
            .collect();
        if new_files.is_empty() {
            return None;
        }
        let mut stmt = conn
            .prepare("SELECT file_path FROM files_modified WHERE session_id = ?1")
            .ok()?;
        let old_files: Vec<String> = stmt
            .query_map(params![last_id], |row| {
                row.get::<_, String>(0)
                    .map(|p| p.rsplit('/').next().unwrap_or(&p).to_lowercase())
            })
            .ok()?
            .filter_map(|r| r.ok())
            .collect();
        if new_files.iter().any(|f| old_files.contains(f)) {
            return Some(last_id);
        }

        None
    }

    /// Query recent sessions that touched the given file path.
    /// Normalizes separators + case so Windows/Unix + absolute/relative paths match reliably.
    pub fn query_file_history(&self, file_path: &str, limit: usize) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.learnings, m.change_summary, s.created_at
             FROM sessions s
             JOIN files_modified m ON m.session_id = s.id
             WHERE LOWER(REPLACE(m.file_path, '\\', '/')) = ?1
                OR LOWER(REPLACE(m.file_path, '\\', '/')) LIKE ?2
             ORDER BY s.created_at DESC
             LIMIT ?3",
        )?;

        let norm: String = file_path.replace('\\', "/").to_lowercase();
        let base: String = std::path::Path::new(&norm)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&norm)
            .to_string();
        let like_suffix = format!("%/{}", base);
        let rows = stmt.query_map(params![norm, like_suffix, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut out = String::new();
        for row in rows {
            let (learnings, change, created) = row?;
            let date: String = created.chars().take(10).collect();
            let short: String = learnings.chars().take(120).collect();
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
        let conn = self.conn.lock().unwrap();

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

        let mut stmt = conn.prepare(
            "SELECT s.task_desc, s.learnings, m.file_path, m.change_summary, s.created_at
             FROM sessions s
             JOIN files_modified m ON m.session_id = s.id
             ORDER BY s.created_at DESC
             LIMIT 20",
        )?;

        struct Scored {
            learnings: String,
            change_summary: String,
            created_at: String,
            score: f64,
        }

        let mut scored: Vec<Scored> = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        for row in rows {
            let (task_desc, learnings, file_path, change_summary, created_at) = row?;
            let norm = file_path.replace('\\', "/").to_lowercase();
            let base = std::path::Path::new(&norm)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&norm)
                .to_string();

            let mut score = 0.0f64;

            if file_bases.iter().any(|b| base == *b) {
                score += 3.0;
            } else if file_bases
                .iter()
                .any(|b| base.contains(b) || b.contains(&base))
            {
                score += 1.0;
            }

            let task_lower = task_desc.to_lowercase();
            let kw_matches = task_keywords
                .iter()
                .filter(|k| task_lower.contains(k.as_str()))
                .count();
            if kw_matches > 0 {
                score += (kw_matches as f64).min(3.0);
            }

            if score < 1.5 {
                continue;
            }

            scored.push(Scored {
                learnings,
                change_summary,
                created_at,
                score,
            });
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(max_results);
        // Reverse: oldest first (top of context = least attention), newest last
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
        let conn = self.conn.lock().unwrap();
        let assistant_text: String = assistant_text.chars().take(4000).collect();
        let reasoning: String = reasoning.chars().take(4000).collect();
        let tool_result: String = tool_result.chars().take(6000).collect();
        conn.execute(
            "INSERT INTO react_log
                (session_id, task_desc, tool, target, outcome, decision, assistant_text, reasoning, tool_result)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![session_id, task_desc, tool, target, outcome, decision, assistant_text, reasoning, tool_result],
        )?;
        Ok(())
    }

    /// Get unimpacted ReAct timeline (oldest first) for context injection.
    pub fn get_react_timeline(&self, session_id: &str, limit: usize) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT created_at, tool, target, outcome, decision
             FROM react_log
             WHERE impacted = 0 AND session_id = ?1
             ORDER BY created_at ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        let mut out = String::new();
        let mut time_group = String::new();
        for row in rows {
            let (ts, tool, target, outcome, decision) = row?;
            let date: String = ts.chars().take(16).collect();
            if date != time_group {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("🔄 [{}]\n", date));
                time_group = date;
            }
            let icon = if outcome == "ok" || outcome.starts_with("ok") {
                "✅"
            } else {
                "⚠️"
            };
            let target_short: String = target.chars().take(50).collect();
            out.push_str(&format!("  {} {} {}\n", icon, tool, target_short));
            if !decision.is_empty() {
                out.push_str(&format!(
                    "    → {}\n",
                    decision.chars().take(100).collect::<String>()
                ));
            }
        }
        Ok(out)
    }

    /// Get the full ReAct mainline (oldest first) for context injection.
    /// This is the **primary memory source** for the LLM — it includes
    /// the complete tool execution trace with assistant reasoning and results.
    /// Returns formatted text grouped by time, with summaries of each step.
    pub fn get_react_mainline(&self, session_id: &str, limit: usize) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT created_at, task_desc, tool, target, outcome, decision, assistant_text, reasoning, tool_result
             FROM react_log
             WHERE impacted = 0 AND session_id = ?1
             ORDER BY created_at ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut out = String::new();
        let mut prev_date = String::new();
        let mut prev_task = String::new();
        for row in rows {
            let (ts, task_desc, tool, target, outcome, decision, assistant_text, reasoning, tool_result) = row?;
            let date: String = ts.chars().take(16).collect();
            let target_short: String = target.chars().take(80).collect();

            if date != prev_date {
                if !out.is_empty() {
                    out.push_str("\n");
                }
                out.push_str(&format!("── {} ──\n", date));
                prev_date = date;
            }

            if task_desc != prev_task {
                out.push_str(&format!("📋 Task: {}\n", task_desc.chars().take(200).collect::<String>()));
                prev_task = task_desc;
            }

            let icon = if outcome == "ok" || outcome.starts_with("ok") { "✅" } else { "⚠️" };
            out.push_str(&format!("  {} [{}] {}\n", icon, tool, target_short));

            if !decision.is_empty() {
                out.push_str(&format!("    💭 Decision: {}\n", decision.chars().take(200).collect::<String>()));
            }

            if !reasoning.is_empty() {
                let r: String = reasoning.chars().take(300).collect();
                if !r.is_empty() {
                    out.push_str(&format!("    🧠 Reasoning: {}\n", r));
                }
            }

            if !assistant_text.is_empty() {
                let a: String = assistant_text.chars().take(300).collect();
                if !a.is_empty() {
                    out.push_str(&format!("    💬 Assistant: {}\n", a));
                }
            }

            if !tool_result.is_empty() {
                let r: String = tool_result.chars().take(500).collect();
                if !r.is_empty() {
                    out.push_str(&format!("    📄 Result: {}\n", r));
                }
            }
        }
        Ok(out)
    }

    /// Like `get_react_timeline` but prefixes each row with its `react_log.id`
    /// (`[id=N]`), so the summarizer can reference rows in its cluster output.
    /// Used only for offload summarization, not context injection.
    pub fn get_react_timeline_with_ids(&self, session_id: &str, limit: usize) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, tool, target, outcome, decision
             FROM react_log
             WHERE impacted = 0 AND session_id = ?1
             ORDER BY created_at ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut out = String::new();
        for row in rows {
            let (id, ts, tool, target, outcome, decision) = row?;
            let date: String = ts.chars().take(16).collect();
            let target_short: String = target.chars().take(60).collect();
            out.push_str(&format!(
                "[id={id}] [{date}] {tool} {target_short} → {outcome}\n"
            ));
            if !decision.is_empty() {
                out.push_str(&format!(
                    "    判断: {}\n",
                    decision.chars().take(120).collect::<String>()
                ));
            }
        }
        Ok(out)
    }

    /// Archive a batch of ReAct rows into clustered memory-graph nodes.
    /// One transaction: each cluster becomes a `memory_graphs` row, then the
    /// referenced `react_log` rows are stamped `impacted=1, graph_id=<new id>`.
    /// This is the "offload" write — after it, those rows drop out of
    /// `get_react_timeline` (which filters `impacted=0`) and live on only as
    /// graph nodes, retrievable via `get_react_batch_by_graph`.
    pub fn archive_react_batch(&self, session_id: &str, clusters: &[GraphNode]) -> Result<()> {
        if clusters.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for node in clusters {
            // impact clusters (their summary flags [IMPACT]) carry more weight so
            // they rank higher in context injection and survive downgrade longer.
            let weight: f64 = if node.topic.contains("[IMPACT]")
                || node.summary.contains("[IMPACT]")
                || node.summary.contains("impact")
            {
                2.0
            } else {
                1.0
            };
            tx.execute(
                "INSERT INTO memory_graphs (session_id, summary, detail, tier, weight)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![session_id, node.topic, node.summary, weight],
            )?;
            let gid = tx.last_insert_rowid();
            for rid in &node.react_ids {
                tx.execute(
                    "UPDATE react_log SET impacted = 1, graph_id = ?1 WHERE id = ?2",
                    params![gid, rid],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Get memory-graph node titles for this session for the top-of-context
    /// `[MEMORY_GRAPH]` block + recall index. Returns `(id, summary, tier, weight)`.
    /// Excludes cold-archived (tier=0) and already-merged (superseded) nodes.
    /// Higher tier + weight first (L2 above L1, impact above regular).
    pub fn get_memory_graphs(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, i64, f64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, summary, tier, weight FROM memory_graphs
             WHERE session_id = ?1 AND tier > 0 AND merged_into IS NULL
             ORDER BY tier DESC, weight DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Record a recall hit on a graph node (drives L2→L3 promotion + anti-downgrade).
    pub fn touch_graph_hit(&self, graph_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE memory_graphs
             SET hit_count = hit_count + 1, last_hit_at = datetime('now')
             WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(())
    }

    /// Read a `meta` key (e.g. `last_l1l2_consolidation`).
    pub fn meta_get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Write a `meta` key.
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Load active tier-1 nodes (candidates for L1→L2 consolidation).
    /// Returns `(id, summary, weight)`, newest first.
    pub fn get_l1_nodes(&self, session_id: &str, limit: usize) -> Result<Vec<(i64, String, f64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, summary, weight FROM memory_graphs
             WHERE session_id = ?1 AND tier = 1 AND merged_into IS NULL
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Apply one L1→L2 merge group: create a tier-2 node consolidating the given
    /// tier-1 node ids, re-parent their react_log rows to the new node, and mark
    /// the old nodes `merged_into` the new one (so they drop out of injection but
    /// remain replayable). `weight` = max weight of members (impact preserved).
    pub fn apply_l1_l2_merge(
        &self,
        session_id: &str,
        topic: &str,
        summary: &str,
        member_ids: &[i64],
        weight: f64,
    ) -> Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO memory_graphs (session_id, summary, detail, tier, weight)
             VALUES (?1, ?2, ?3, 2, ?4)",
            params![session_id, topic, summary, weight],
        )?;
        let new_id = tx.last_insert_rowid();
        for mid in member_ids {
            // Re-point the original ReAct rows so replay of the L2 node shows all.
            tx.execute(
                "UPDATE react_log SET graph_id = ?1 WHERE graph_id = ?2",
                params![new_id, mid],
            )?;
            tx.execute(
                "UPDATE memory_graphs SET merged_into = ?1 WHERE id = ?2",
                params![new_id, mid],
            )?;
        }
        tx.commit()?;
        Ok(new_id)
    }

    /// Downgrade stale tier-2/tier-1 nodes (forgetting = demotion, not deletion).
    /// tier-2 with no hit in `stale_days` → tier-1; tier-1 (never L2, cold) with
    /// no hit in `2*stale_days` → tier-0 (archived, excluded from injection).
    pub fn downgrade_stale_nodes(&self, stale_days: u32) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let t2 = conn.execute(
            &format!(
                "UPDATE memory_graphs SET tier = 1
                 WHERE tier = 2 AND merged_into IS NULL
                   AND COALESCE(last_hit_at, created_at) < datetime('now', '-{} days')",
                stale_days
            ),
            [],
        )?;
        let t1 = conn.execute(
            &format!(
                "UPDATE memory_graphs SET tier = 0
                 WHERE tier = 1 AND merged_into IS NULL
                   AND COALESCE(last_hit_at, created_at) < datetime('now', '-{} days')",
                stale_days * 2
            ),
            [],
        )?;
        Ok(t2 + t1)
    }

    /// L2→L3 promotion candidates: tier-2 nodes hit at least `min_hits` times.
    /// Returns `(id, summary)`. Caller abstracts these into Skill drafts.
    pub fn get_l3_candidates(&self, min_hits: i64, limit: usize) -> Result<Vec<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, summary FROM memory_graphs
             WHERE tier = 2 AND merged_into IS NULL AND hit_count >= ?1
             ORDER BY hit_count DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![min_hits, limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark a node as promoted to L3 (tier=3) so it isn't re-suggested.
    pub fn mark_promoted_l3(&self, graph_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE memory_graphs SET tier = 3 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(())
    }

    /// Node replay: reconstruct the full ReAct trace consolidated into one graph
    /// node, oldest first. Used by `recall #<id>` to re-expand an offloaded node.
    pub fn get_react_batch_by_graph(&self, graph_id: i64) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        // Node header (topic + summary) then its constituent ReAct rows.
        let (topic, detail): (String, String) = conn
            .query_row(
                "SELECT summary, detail FROM memory_graphs WHERE id = ?1",
                params![graph_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|_| (String::new(), String::new()));

        let mut out = String::new();
        if !topic.is_empty() {
            out.push_str(&format!("📊 记忆图谱节点 #{graph_id}: {topic}\n"));
        }
        if !detail.is_empty() {
            out.push_str(&format!("{detail}\n"));
        }
        out.push_str("┈┈┈ 原始 ReAct（[user] → [assistant] → [tool_result]）┈┈┈\n");

        let mut stmt = conn.prepare(
            "SELECT created_at, task_desc, tool, target, outcome, decision, assistant_text, tool_result
             FROM react_log
             WHERE graph_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for row in rows {
            let (ts, task, tool, target, outcome, decision, assistant, tool_result) = row?;
            let date: String = ts.chars().take(19).collect();
            let icon = if outcome == "ok" || outcome.starts_with("ok") {
                "✅"
            } else {
                "⚠️"
            };
            // [1] user (timestamped)
            out.push_str(&format!(
                "[user] {date} {}\n",
                task.chars().take(200).collect::<String>()
            ));
            // [2] assistant — prefer the fuller assistant_text, fall back to decision
            let think = if !assistant.trim().is_empty() {
                assistant
            } else {
                decision
            };
            if !think.trim().is_empty() {
                out.push_str(&format!(
                    "[assistant] {}\n",
                    think.chars().take(400).collect::<String>()
                ));
            }
            // [3] tool_result
            let target_short: String = target.chars().take(60).collect();
            out.push_str(&format!("[tool_result] {icon} {tool}({target_short})\n"));
            if !tool_result.trim().is_empty() {
                out.push_str(&format!(
                    "  {}\n",
                    tool_result.chars().take(500).collect::<String>()
                ));
            }
            out.push('\n');
        }
        Ok(out)
    }
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
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.db");
        let store = MemoryStore::open(&path).unwrap();

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

    /// End-to-end tiering lifecycle: archive L1 → merge to L2 → recall hits →
    /// L3 candidate surfaces → promote → stale L1 downgrades to L0.
    #[test]
    fn test_tiering_lifecycle() {
        let dir = std::env::temp_dir().join("ox_memory_tier_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let store = MemoryStore::open(&dir.join("tier.db")).unwrap();
        let sid = "tier-session";

        // Seed react_log rows so archival has something to re-parent.
        for i in 0..4 {
            store
                .record_react(
                    sid,
                    "重构记忆分层",
                    "file_read",
                    &format!("f{i}.rs"),
                    "ok",
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        // Archive into 3 tier-1 nodes; one carries an [IMPACT] weight.
        let clusters = vec![
            GraphNode {
                topic: "[IMPACT] 分层设计".into(),
                summary: "L0-L3 tiering".into(),
                react_ids: vec![1],
            },
            GraphNode {
                topic: "存储层".into(),
                summary: "sqlite schema".into(),
                react_ids: vec![2],
            },
            GraphNode {
                topic: "归并策略".into(),
                summary: "LLM merge".into(),
                react_ids: vec![3],
            },
        ];
        store.archive_react_batch(sid, &clusters).unwrap();

        let l1 = store.get_l1_nodes(sid, 60).unwrap();
        assert_eq!(l1.len(), 3, "3 tier-1 nodes archived");
        // Impact node weight preserved.
        assert!(
            l1.iter().any(|(_, _, w)| *w >= 2.0),
            "[IMPACT] node weighted 2.0"
        );

        // Merge two tier-1 nodes into one tier-2 node (impact weight carries over).
        let member_ids: Vec<i64> = l1.iter().take(2).map(|(id, _, _)| *id).collect();
        let l2_id = store
            .apply_l1_l2_merge(sid, "分层+存储", "merged knowledge", &member_ids, 2.0)
            .unwrap();

        // Merged members drop out of L1 candidate set.
        let l1_after = store.get_l1_nodes(sid, 60).unwrap();
        assert_eq!(l1_after.len(), 1, "2 merged, 1 tier-1 remains");

        // Injection view: L2 ranks above L1 (tier DESC).
        let graphs = store.get_memory_graphs(sid, 20).unwrap();
        assert_eq!(
            graphs.first().map(|(_, _, t, _)| *t),
            Some(2),
            "L2 pinned on top"
        );

        // No L3 candidate before recall hits.
        assert!(store.get_l3_candidates(3, 5).unwrap().is_empty());

        // Three recall hits on the L2 node → crosses the L3 threshold.
        for _ in 0..3 {
            store.touch_graph_hit(l2_id).unwrap();
        }
        let l3 = store.get_l3_candidates(3, 5).unwrap();
        assert_eq!(l3.len(), 1, "L2 node with 3 hits is an L3 candidate");
        assert_eq!(l3[0].0, l2_id);

        // Promote to L3 → no longer re-suggested as an L2 candidate.
        store.mark_promoted_l3(l2_id).unwrap();
        assert!(
            store.get_l3_candidates(3, 5).unwrap().is_empty(),
            "promoted node not re-offered"
        );

        // Downgrade is a safe no-op for fresh nodes (created within the current
        // second, so not yet `< now`) and never touches the promoted L3 node.
        let _ = store.downgrade_stale_nodes(0).unwrap();
        let graphs_after = store.get_memory_graphs(sid, 20).unwrap();
        assert!(
            graphs_after
                .iter()
                .any(|(id, _, t, _)| *id == l2_id && *t == 3),
            "promoted L3 node survives downgrade"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
