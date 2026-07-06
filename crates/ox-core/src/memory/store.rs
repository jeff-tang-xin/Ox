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
            CREATE INDEX IF NOT EXISTS idx_facts_text ON key_facts(fact_text);
            CREATE INDEX IF NOT EXISTS idx_modified_path ON files_modified(file_path);
            CREATE INDEX IF NOT EXISTS idx_key_facts_session ON key_facts(session_id);
            CREATE INDEX IF NOT EXISTS idx_files_read_session ON files_read(session_id);
            CREATE INDEX IF NOT EXISTS idx_files_read_path ON files_read(file_path);
            CREATE INDEX IF NOT EXISTS idx_files_modified_session ON files_modified(session_id);"
        )?;
        Ok(Self { conn: Mutex::new(conn) })
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

        tx.execute("DELETE FROM key_facts WHERE session_id = ?1", params![target_id])?;
        tx.execute("DELETE FROM files_read WHERE session_id = ?1", params![target_id])?;
        tx.execute("DELETE FROM files_modified WHERE session_id = ?1", params![target_id])?;

        // If merging, append learnings; otherwise use as-is
        let merged_learnings = if merged_id.is_some() {
            let old: String = tx.query_row(
                "SELECT learnings FROM sessions WHERE id = ?1", params![target_id],
                |row| row.get(0),
            ).unwrap_or_default();
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
        let new_files: Vec<String> = summary.files_modified.iter()
            .map(|m| m.path.rsplit('/').next().unwrap_or(&m.path).to_lowercase())
            .collect();
        if new_files.is_empty() {
            return None;
        }
        let mut stmt = conn.prepare(
            "SELECT file_path FROM files_modified WHERE session_id = ?1"
        ).ok()?;
        let old_files: Vec<String> = stmt.query_map(params![last_id], |row| {
            row.get::<_, String>(0).map(|p| {
                p.rsplit('/').next().unwrap_or(&p).to_lowercase()
            })
        }).ok()?.filter_map(|r| r.ok()).collect();
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
             LIMIT ?3"
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
                out.push_str(&format!("    └ {}\n", change.chars().take(80).collect::<String>()));
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
            .filter(|w| !["fix", "改", "继续", "修", "this", "the", "for", "and", "not", "are", "was"].contains(w))
            .map(|w| w.to_lowercase())
            .collect();

        let file_bases: Vec<String> = file_paths.iter().map(|p| {
            std::path::Path::new(&p.replace('\\', "/"))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(p)
                .to_lowercase()
        }).collect();

        let mut stmt = conn.prepare(
            "SELECT s.task_desc, s.learnings, m.file_path, m.change_summary, s.created_at
             FROM sessions s
             JOIN files_modified m ON m.session_id = s.id
             ORDER BY s.created_at DESC
             LIMIT 20"
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
            } else if file_bases.iter().any(|b| base.contains(b) || b.contains(&base)) {
                score += 1.0;
            }

            let task_lower = task_desc.to_lowercase();
            let kw_matches = task_keywords.iter()
                .filter(|k| task_lower.contains(k.as_str()))
                .count();
            if kw_matches > 0 {
                score += (kw_matches as f64).min(3.0);
            }

            if score < 1.5 {
                continue;
            }

            scored.push(Scored { learnings, change_summary, created_at, score });
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(max_results);

        if scored.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::from("📦 历史批次:\n");
        for s in &scored {
            let date: String = s.created_at.chars().take(10).collect();
            let short: String = s.learnings.chars().take(120).collect();
            out.push_str(&format!("  • {} — {}\n", date, short));
            if !s.change_summary.is_empty() {
                out.push_str(&format!("    └ {}\n", s.change_summary.chars().take(80).collect::<String>()));
            }
        }
        Ok(out)
    }
}

/// Trigram (3-gram) Jaccard similarity. Works for Chinese/English mixed text
/// without tokenization or embedding.
fn trigram_overlap(a: &str, b: &str) -> f64 {
    fn trigrams(s: &str) -> std::collections::HashSet<String> {
        s.chars().collect::<Vec<_>>().windows(3).map(|w| w.iter().collect()).collect()
    }
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() { return 0.0; }
    ta.intersection(&tb).count() as f64 / ta.union(&tb).count() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::unified_action::{KeyFact, FileModifiedRecord};

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
}