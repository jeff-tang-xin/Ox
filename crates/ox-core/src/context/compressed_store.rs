use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};

use crate::message::Message;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS compressed_context (
    session_id      TEXT PRIMARY KEY,
    messages_json   TEXT NOT NULL,
    source_msg_count INTEGER NOT NULL,
    created_at      TEXT NOT NULL
);
"#;

/// Stores compressed conversation context in SQLite.
/// JSONL keeps the full chat log; this table holds the compressed snapshot
/// so that context building uses compressed + new messages.
pub struct CompressedContextStore {
    conn: Mutex<Connection>,
}

impl CompressedContextStore {
    /// Open or create store at the given path
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory store for fallback when disk is unavailable
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Load compressed context for a session.
    /// Returns (compressed_messages, source_msg_count) or None if not found.
    pub fn load(&self, session_id: &str) -> anyhow::Result<Option<(Vec<Message>, usize)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT messages_json, source_msg_count FROM compressed_context WHERE session_id = ?1",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            let count: usize = row.get::<_, i64>(1)? as usize;
            let messages: Vec<Message> = serde_json::from_str(&json)?;
            Ok(Some((messages, count)))
        } else {
            Ok(None)
        }
    }

    /// Save compressed context for a session (upserts).
    pub fn save(
        &self,
        session_id: &str,
        messages: &[Message],
        source_msg_count: usize,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(messages)?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO compressed_context
             (session_id, messages_json, source_msg_count, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, json, source_msg_count as i64, now],
        )?;
        Ok(())
    }

    /// Delete compressed context for a session (e.g. on /new).
    pub fn delete(&self, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM compressed_context WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }
}
