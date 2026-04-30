use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, Result as SqlResult, Statement};

use super::{MemoryNode, MemoryNodeType, MemorySource};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS memories (
    id                TEXT PRIMARY KEY,
    content           TEXT NOT NULL,
    node_type         TEXT NOT NULL,
    depth             INTEGER NOT NULL DEFAULT 0,
    project_id        TEXT,
    language          TEXT NOT NULL DEFAULT '',
    source            TEXT NOT NULL,
    created_at        INTEGER NOT NULL,
    last_accessed     INTEGER NOT NULL,
    is_project_critical INTEGER NOT NULL DEFAULT 0,
    trace_0           REAL NOT NULL DEFAULT 0.0,
    trace_1           REAL NOT NULL DEFAULT 0.0,
    trace_2           REAL NOT NULL DEFAULT 0.0,
    trace_3           REAL NOT NULL DEFAULT 0.0,
    trace_4           REAL NOT NULL DEFAULT 0.0,
    language_weight   REAL NOT NULL DEFAULT 0.5
);

CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);
CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(node_type);
CREATE INDEX IF NOT EXISTS idx_memories_accessed ON memories(last_accessed);

-- Council model capability scores
CREATE TABLE IF NOT EXISTS model_capabilities (
    provider          TEXT NOT NULL,
    model             TEXT NOT NULL,
    topic_category    TEXT NOT NULL,
    proposal_adopted_rate REAL NOT NULL DEFAULT 0.5,
    review_quality    REAL NOT NULL DEFAULT 0.5,
    session_count     INTEGER NOT NULL DEFAULT 0,
    last_updated      INTEGER NOT NULL,
    PRIMARY KEY (provider, model, topic_category)
);

CREATE INDEX IF NOT EXISTS idx_model_caps_topic ON model_capabilities(topic_category);

-- Persona vectors persistence
CREATE TABLE IF NOT EXISTS persona_vectors (
    language              TEXT PRIMARY KEY,
    safety_over_speed     REAL NOT NULL DEFAULT 0.7,
    prefers_conciseness   REAL NOT NULL DEFAULT 0.7,
    code_style_strictness REAL NOT NULL DEFAULT 0.7,
    refuses_unsafe_code   INTEGER NOT NULL DEFAULT 1,
    frozen                INTEGER NOT NULL DEFAULT 0,
    forbidden_phrases     TEXT NOT NULL DEFAULT '',
    moral_priorities      TEXT NOT NULL DEFAULT '',
    last_updated          INTEGER NOT NULL
);

-- EMA trend tracking for implicit feedback metrics
CREATE TABLE IF NOT EXISTS ema_trends (
    metric_name         TEXT NOT NULL,
    current_value       REAL NOT NULL DEFAULT 0.5,
    trend               REAL NOT NULL DEFAULT 0.0,
    sample_count        INTEGER NOT NULL DEFAULT 0,
    last_updated        INTEGER NOT NULL,
    PRIMARY KEY (metric_name)
);

-- Persona snapshots for rollback mechanism
CREATE TABLE IF NOT EXISTS persona_snapshots (
    snapshot_id         TEXT PRIMARY KEY,
    language            TEXT NOT NULL,
    safety_over_speed   REAL NOT NULL,
    prefers_conciseness REAL NOT NULL,
    code_style_strictness REAL NOT NULL,
    refuses_unsafe_code INTEGER NOT NULL,
    frozen              INTEGER NOT NULL DEFAULT 0,
    forbidden_phrases   TEXT NOT NULL,
    moral_priorities    TEXT NOT NULL,
    satisfaction_score  REAL NOT NULL,
    created_at          INTEGER NOT NULL,
    is_active           INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_persona_snapshots_lang ON persona_snapshots(language);
CREATE INDEX IF NOT EXISTS idx_persona_snapshots_active ON persona_snapshots(is_active);
"#;

pub struct MemoryStore {
    conn: Arc<Connection>,
    // Precompiled statements for better performance
    insert_stmt: Mutex<Option<Statement<'static>>>,
}

// Safety: Statements are tied to connection lifetime, but we use 'static for simplicity
// since the connection is Arc'd and lives as long as the store.
unsafe impl Send for MemoryStore {}
unsafe impl Sync for MemoryStore {}

impl Clone for MemoryStore {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            insert_stmt: Mutex::new(None), // Don't clone precompiled statements
        }
    }
}

impl MemoryStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        let mut store = Self { 
            conn: Arc::new(conn),
            insert_stmt: Mutex::new(None),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&mut self) -> anyhow::Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        Ok(())
    }

    /// Get or create precompiled insert statement
    fn get_insert_stmt(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Option<Statement<'static>>>> {
        let mut stmt_guard = self.insert_stmt.lock().unwrap();
        if stmt_guard.is_none() {
            // Prepare statement - note: we leak the statement to get 'static lifetime
            // This is safe because the connection lives as long as the store
            let raw_conn = Arc::as_ptr(&self.conn) as *mut Connection;
            let stmt = unsafe { (&*raw_conn).prepare(
                "INSERT OR REPLACE INTO memories
                 (id, content, node_type, depth, project_id, language, source,
                  created_at, last_accessed, is_project_critical,
                  trace_0, trace_1, trace_2, trace_3, trace_4, language_weight)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"
            )? };
            // Safety: We're transmuting the statement to 'static lifetime.
            // This is safe because: 1) The connection is Arc'd and won't be dropped
            // while the store exists. 2) We only drop statements when the store drops.
            let static_stmt: Statement<'static> = unsafe { std::mem::transmute(stmt) };
            *stmt_guard = Some(static_stmt);
        }
        Ok(stmt_guard)
    }

    pub fn insert(&self, node: &MemoryNode) -> anyhow::Result<()> {
        let mut stmt_guard = self.get_insert_stmt()?;
        let stmt = stmt_guard.as_mut().unwrap();
        stmt.execute(params![
            node.id,
            node.content,
            node.node_type.as_str(),
            node.depth,
            node.project_id,
            node.language,
            node.source.as_str(),
            node.created_at,
            node.last_accessed,
            node.is_project_critical as i32,
            node.traces[0],
            node.traces[1],
            node.traces[2],
            node.traces[3],
            node.traces[4],
            node.language_weight,
        ])?;
        Ok(())
    }

    pub fn insert_batch(&self, nodes: &[MemoryNode]) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            // Use a local prepared statement for the transaction
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO memories
                 (id, content, node_type, depth, project_id, language, source,
                  created_at, last_accessed, is_project_critical,
                  trace_0, trace_1, trace_2, trace_3, trace_4, language_weight)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"
            )?;
            for node in nodes {
                stmt.execute(params![
                    node.id,
                    node.content,
                    node.node_type.as_str(),
                    node.depth,
                    node.project_id,
                    node.language,
                    node.source.as_str(),
                    node.created_at,
                    node.last_accessed,
                    node.is_project_critical as i32,
                    node.traces[0],
                    node.traces[1],
                    node.traces[2],
                    node.traces[3],
                    node.traces[4],
                    node.language_weight,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn query_by_project(
        &self,
        project_id: &str,
        types: &[MemoryNodeType],
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryNode>> {
        let type_strs: Vec<String> = types.iter().map(|t| t.as_str().to_string()).collect();
        let type_clause = type_strs.join_for_sql();
        let sql = format!(
            "SELECT * FROM memories WHERE project_id = ?1 AND node_type IN ({}) ORDER BY last_accessed DESC LIMIT ?2",
            type_clause
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![project_id, limit as i64], |row| self.row_to_node(row))?;
        rows.collect::<SqlResult<Vec<_>>>().map_err(Into::into)
    }

    pub fn query_overall(
        &self,
        types: &[MemoryNodeType],
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryNode>> {
        let type_strs: Vec<String> = types.iter().map(|t| t.as_str().to_string()).collect();
        let type_clause = type_strs.join_for_sql();
        let sql = format!(
            "SELECT * FROM memories WHERE project_id IS NULL AND node_type IN ({}) ORDER BY last_accessed DESC LIMIT ?1",
            type_clause
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit as i64], |row| self.row_to_node(row))?;
        rows.collect::<SqlResult<Vec<_>>>().map_err(Into::into)
    }

    pub fn search(&self, keyword: &str, project_id: Option<&str>, limit: usize) -> anyhow::Result<Vec<MemoryNode>> {
        let pattern = format!("%{}%", keyword);
        let nodes = match project_id {
            Some(pid) => {
                let sql = "SELECT * FROM memories WHERE content LIKE ?1 AND project_id = ?2 ORDER BY last_accessed DESC LIMIT ?3";
                let mut stmt = self.conn.prepare(sql)?;
                stmt.query_map(params![pattern, pid, limit as i64], |row| self.row_to_node(row))?
                    .collect::<SqlResult<Vec<_>>>()?
            }
            None => {
                let sql = "SELECT * FROM memories WHERE content LIKE ?1 ORDER BY last_accessed DESC LIMIT ?2";
                let mut stmt = self.conn.prepare(sql)?;
                stmt.query_map(params![pattern, limit as i64], |row| self.row_to_node(row))?
                    .collect::<SqlResult<Vec<_>>>()?
            }
        };
        Ok(nodes)
    }

    pub fn update_depth(&self, id: &str, new_depth: u8) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE memories SET depth = ?1 WHERE id = ?2",
            params![new_depth, id],
        )?;
        Ok(())
    }

    pub fn increment_depth(&self, id: &str) -> anyhow::Result<()> {
        self.conn.execute("UPDATE memories SET depth = MIN(depth + 1, 5) WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn update_last_accessed(&self, id: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "UPDATE memories SET last_accessed = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn count_by_project(&self, project_id: &str) -> anyhow::Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn count_overall(&self) -> anyhow::Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn checkpoint(&self) -> anyhow::Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    // ── Model Capability Score persistence ──

    pub fn save_model_capability(
        &self,
        provider: &str,
        model: &str,
        topic: &str,
        adopted_rate: f32,
        quality: f32,
        count: u32,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO model_capabilities 
             (provider, model, topic_category, proposal_adopted_rate, review_quality, session_count, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![provider, model, topic, adopted_rate, quality, count, now],
        )?;
        Ok(())
    }

    pub fn load_model_capability(
        &self,
        provider: &str,
        model: &str,
        topic: &str,
    ) -> anyhow::Result<Option<(f32, f32, u32)>> {
        let row = self.conn.query_row(
            "SELECT proposal_adopted_rate, review_quality, session_count FROM model_capabilities
             WHERE provider = ?1 AND model = ?2 AND topic_category = ?3",
            params![provider, model, topic],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        );
        match row {
            Ok(data) => Ok(Some(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_all_model_capabilities(
        &self,
    ) -> anyhow::Result<Vec<(String, String, String, f32, f32, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, model, topic_category, proposal_adopted_rate, review_quality, session_count
             FROM model_capabilities ORDER BY provider, model, topic_category"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?;
        rows.collect::<SqlResult<Vec<_>>>().map_err(Into::into)
    }

    // ── Persona Vector persistence ──

    pub fn save_persona_vector(
        &self,
        language: &str,
        safety: f64,
        conciseness: f64,
        style_strictness: f64,
        refuses_unsafe: bool,
        frozen: bool,
        forbidden: &[String],
        priorities: &[String],
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let forbidden_str = forbidden.join("|");
        let priorities_str = priorities.join("|");
        
        self.conn.execute(
            "INSERT OR REPLACE INTO persona_vectors 
             (language, safety_over_speed, prefers_conciseness, code_style_strictness,
              refuses_unsafe_code, frozen, forbidden_phrases, moral_priorities, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                language, safety, conciseness, style_strictness,
                refuses_unsafe as i32, frozen as i32,
                forbidden_str, priorities_str, now
            ],
        )?;
        Ok(())
    }

    pub fn load_persona_vector(
        &self,
        language: &str,
    ) -> anyhow::Result<Option<(f64, f64, f64, bool, bool, Vec<String>, Vec<String>)>> {
        let row = self.conn.query_row(
            "SELECT safety_over_speed, prefers_conciseness, code_style_strictness,
                    refuses_unsafe_code, frozen, forbidden_phrases, moral_priorities
             FROM persona_vectors WHERE language = ?1",
            params![language],
            |row| {
                let forbidden: String = row.get(5)?;
                let priorities: String = row.get(6)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, i32>(3)? != 0,
                    row.get::<_, i32>(4)? != 0,
                    if forbidden.is_empty() { vec![] } else { forbidden.split('|').map(|s| s.to_string()).collect() },
                    if priorities.is_empty() { vec![] } else { priorities.split('|').map(|s| s.to_string()).collect() },
                ))
            },
        );
        match row {
            Ok(data) => Ok(Some(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── EMA Trend Tracking persistence ──

    pub fn save_ema_trend(
        &self,
        metric_name: &str,
        current_value: f64,
        trend: f64,
        sample_count: u32,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO ema_trends 
             (metric_name, current_value, trend, sample_count, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![metric_name, current_value, trend, sample_count, now],
        )?;
        Ok(())
    }

    pub fn load_ema_trend(
        &self,
        metric_name: &str,
    ) -> anyhow::Result<Option<(f64, f64, u32)>> {
        let row = self.conn.query_row(
            "SELECT current_value, trend, sample_count FROM ema_trends
             WHERE metric_name = ?1",
            params![metric_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        );
        match row {
            Ok(data) => Ok(Some(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Persona Snapshot persistence for rollback ──

    pub fn save_persona_snapshot(
        &self,
        snapshot_id: &str,
        language: &str,
        safety: f64,
        conciseness: f64,
        style_strictness: f64,
        refuses_unsafe: bool,
        frozen: bool,
        forbidden: &[String],
        priorities: &[String],
        satisfaction_score: f64,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let forbidden_str = forbidden.join("|");
        let priorities_str = priorities.join("|");
        
        self.conn.execute(
            "INSERT OR REPLACE INTO persona_snapshots 
             (snapshot_id, language, safety_over_speed, prefers_conciseness, 
              code_style_strictness, refuses_unsafe_code, frozen, forbidden_phrases, 
              moral_priorities, satisfaction_score, created_at, is_active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                snapshot_id, language, safety, conciseness, style_strictness,
                refuses_unsafe as i32, frozen as i32,
                forbidden_str, priorities_str, satisfaction_score, now, 0
            ],
        )?;
        Ok(())
    }

    pub fn load_persona_snapshot(
        &self,
        snapshot_id: &str,
    ) -> anyhow::Result<Option<(String, f64, f64, f64, bool, bool, Vec<String>, Vec<String>, f64)>> {
        let row = self.conn.query_row(
            "SELECT language, safety_over_speed, prefers_conciseness, code_style_strictness,
                    refuses_unsafe_code, frozen, forbidden_phrases, moral_priorities, satisfaction_score
             FROM persona_snapshots WHERE snapshot_id = ?1",
            params![snapshot_id],
            |row| {
                let forbidden: String = row.get(6)?;
                let priorities: String = row.get(7)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, i32>(4)? != 0,
                    row.get::<_, i32>(5)? != 0,
                    if forbidden.is_empty() { vec![] } else { forbidden.split('|').map(|s| s.to_string()).collect() },
                    if priorities.is_empty() { vec![] } else { priorities.split('|').map(|s| s.to_string()).collect() },
                    row.get(8)?,
                ))
            },
        );
        match row {
            Ok(data) => Ok(Some(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn activate_snapshot(&self, snapshot_id: &str) -> anyhow::Result<()> {
        // Deactivate all snapshots first
        self.conn.execute(
            "UPDATE persona_snapshots SET is_active = 0 WHERE is_active = 1",
            [],
        )?;
        // Activate the target snapshot
        self.conn.execute(
            "UPDATE persona_snapshots SET is_active = 1 WHERE snapshot_id = ?1",
            params![snapshot_id],
        )?;
        Ok(())
    }

    pub fn get_active_snapshot_id(&self, language: &str) -> anyhow::Result<Option<String>> {
        let row = self.conn.query_row(
            "SELECT snapshot_id FROM persona_snapshots 
             WHERE language = ?1 AND is_active = 1 
             ORDER BY created_at DESC LIMIT 1",
            params![language],
            |row| row.get(0),
        );
        match row {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn row_to_node(&self, row: &rusqlite::Row) -> SqlResult<MemoryNode> {
        Ok(MemoryNode {
            id: row.get(0)?,
            content: row.get(1)?,
            node_type: MemoryNodeType::from_str(&row.get::<_, String>(2)?)
                .unwrap_or(MemoryNodeType::Fact),
            depth: row.get(3)?,
            project_id: row.get(4)?,
            language: row.get(5)?,
            source: MemorySource::from_str(&row.get::<_, String>(6)?)
                .unwrap_or(MemorySource::LlmExtraction),
            created_at: row.get(7)?,
            last_accessed: row.get(8)?,
            is_project_critical: row.get::<_, i32>(9)? != 0,
            traces: [row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?],
            language_weight: row.get(15)?,
        })
    }
}

trait JoinForSql {
    fn join_for_sql(&self) -> String;
}

impl JoinForSql for Vec<String> {
    fn join_for_sql(&self) -> String {
        self.iter()
            .map(|s| format!("'{}'", s))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn open_creates_schema() {
        let dir = temp_dir();
        let path = dir.path().join("test.db");
        let store = MemoryStore::open(&path).unwrap();
        let count = store.count_overall().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_and_query() {
        let dir = temp_dir();
        let path = dir.path().join("test.db");
        let store = MemoryStore::open(&path).unwrap();

        let node = MemoryNode::new(
            "Uses tokio for async".into(),
            MemoryNodeType::Architectural,
            Some("proj123".into()),
            "rust".into(),
            MemorySource::LlmExtraction,
        );
        store.insert(&node).unwrap();

        let results = store.query_by_project("proj123", &[MemoryNodeType::Architectural], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Uses tokio for async");
        assert_eq!(results[0].depth, 2);
    }

    #[test]
    fn insert_batch_and_search() {
        let dir = temp_dir();
        let path = dir.path().join("test.db");
        let store = MemoryStore::open(&path).unwrap();

        let nodes: Vec<MemoryNode> = (0..5).map(|i| {
            MemoryNode::new(
                format!("Memory item {}", i),
                MemoryNodeType::Fact,
                Some("proj".into()),
                "rust".into(),
                MemorySource::ToolObservation,
            )
        }).collect();

        store.insert_batch(&nodes).unwrap();
        assert_eq!(store.count_by_project("proj").unwrap(), 5);

        let results = store.search("item 3", Some("proj"), 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn overall_memories() {
        let dir = temp_dir();
        let path = dir.path().join("test.db");
        let store = MemoryStore::open(&path).unwrap();

        let node = MemoryNode::new(
            "Prefer small functions".into(),
            MemoryNodeType::BestPractice,
            None,
            "rust".into(),
            MemorySource::LlmExtraction,
        );
        store.insert(&node).unwrap();

        let results = store.query_overall(&[MemoryNodeType::BestPractice], 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn update_depth_and_access() {
        let dir = temp_dir();
        let path = dir.path().join("test.db");
        let store = MemoryStore::open(&path).unwrap();

        let node = MemoryNode::new(
            "Test".into(),
            MemoryNodeType::Fact,
            Some("p".into()),
            "rust".into(),
            MemorySource::ToolObservation,
        );
        store.insert(&node).unwrap();
        store.update_depth(&node.id, 5).unwrap();

        let results = store.query_by_project("p", &[MemoryNodeType::Fact], 10).unwrap();
        assert_eq!(results[0].depth, 5);
    }
}
