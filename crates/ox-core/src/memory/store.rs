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
            CREATE INDEX IF NOT EXISTS idx_modified_path ON files_modified(file_path);"
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Save a session summary for a completed session.
    pub fn save_session(
        &self,
        session_id: &str,
        task_desc: &str,
        summary: &SessionSummary,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions (id, task_desc, content_summary, learnings)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, task_desc, "", summary.learnings],
        )?;

        for f in &summary.key_facts {
            conn.execute(
                "INSERT INTO key_facts (session_id, fact_text, related_files) VALUES (?1, ?2, ?3)",
                params![session_id, f.fact, f.files.join(", ")],
            )?;
        }
        for r in &summary.files_read {
            conn.execute(
                "INSERT INTO files_read (session_id, file_path, purpose) VALUES (?1, ?2, ?3)",
                params![session_id, r.path, r.purpose],
            )?;
        }
        for m in &summary.files_modified {
            conn.execute(
                "INSERT INTO files_modified (session_id, file_path, change_summary) VALUES (?1, ?2, ?3)",
                params![session_id, m.path, m.summary],
            )?;
        }
        for s in &summary.skills {
            tracing::info!("[MEMORY] Skill suggested: {} (scope={})", s.id, s.scope);
        }
        Ok(())
    }

    /// Query recent sessions that touched the given file path.
    pub fn query_file_history(&self, file_path: &str, limit: usize) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.learnings, m.change_summary, s.created_at
             FROM sessions s
             JOIN files_modified m ON m.session_id = s.id
             WHERE m.file_path LIKE ?1
             ORDER BY s.created_at DESC
             LIMIT ?2"
        )?;

        let pattern = format!("%{}%", file_path.rsplit('/').next().unwrap_or(file_path));
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
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