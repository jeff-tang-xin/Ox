use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, Result as SqlResult, Statement, params};

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
    language_weight   REAL NOT NULL DEFAULT 0.5,
    
    -- 🆕 LLM Judge feedback tracking
    avg_llm_score     REAL NOT NULL DEFAULT 0.0,
    judge_eval_count  INTEGER NOT NULL DEFAULT 0,
    recent_score_0    REAL NOT NULL DEFAULT 0.0,
    recent_score_1    REAL NOT NULL DEFAULT 0.0,
    recent_score_2    REAL NOT NULL DEFAULT 0.0,
    recent_score_3    REAL NOT NULL DEFAULT 0.0,
    recent_score_4    REAL NOT NULL DEFAULT 0.0,
    
    -- 🆕 File association (JSON array)
    related_files     TEXT NOT NULL DEFAULT '[]'
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

-- EMA trend tracking for implicit feedback metrics
CREATE TABLE IF NOT EXISTS ema_trends (
    metric_name         TEXT NOT NULL,
    current_value       REAL NOT NULL DEFAULT 0.5,
    trend               REAL NOT NULL DEFAULT 0.0,
    sample_count        INTEGER NOT NULL DEFAULT 0,
    last_updated        INTEGER NOT NULL,
    PRIMARY KEY (metric_name)
);

-- 🆕 Semantic associations for dynamic query expansion
CREATE TABLE IF NOT EXISTS semantic_associations (
    source_term         TEXT NOT NULL,
    target_term         TEXT NOT NULL,
    association_type    TEXT NOT NULL CHECK(association_type IN ('synonym', 'co_occurrence', 'hierarchy', 'user_defined')),
    strength            REAL NOT NULL DEFAULT 0.5 CHECK(strength >= 0 AND strength <= 1.0),
    co_occurrence_count INTEGER NOT NULL DEFAULT 1,
    created_at          INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    last_updated        INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    PRIMARY KEY (source_term, target_term)
);

CREATE INDEX IF NOT EXISTS idx_semantic_source ON semantic_associations(source_term);
CREATE INDEX IF NOT EXISTS idx_semantic_target ON semantic_associations(target_term);

-- 🆕 Search history for learning user behavior
CREATE TABLE IF NOT EXISTS search_history (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    query               TEXT NOT NULL,
    timestamp           INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    results_count       INTEGER NOT NULL,
    clicked_result_id   TEXT,
    session_id          TEXT
);

CREATE INDEX IF NOT EXISTS idx_search_timestamp ON search_history(timestamp);"#;

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

    /// Get a clone of the Arc<Connection> for semantic manager
    pub fn get_connection(&self) -> Arc<Connection> {
        Arc::clone(&self.conn)
    }

    /// Get or create precompiled insert statement
    fn get_insert_stmt(
        &self,
    ) -> anyhow::Result<std::sync::MutexGuard<'_, Option<Statement<'static>>>> {
        let mut stmt_guard = self.insert_stmt.lock().unwrap();
        if stmt_guard.is_none() {
            // Prepare statement - note: we leak the statement to get 'static lifetime
            // This is safe because the connection lives as long as the store
            let raw_conn = Arc::as_ptr(&self.conn) as *mut Connection;
            let stmt = unsafe {
                (&*raw_conn).prepare(
                    "INSERT OR REPLACE INTO memories
                 (id, content, node_type, depth, project_id, language, source,
                  created_at, last_accessed, is_project_critical,
                  trace_0, trace_1, trace_2, trace_3, trace_4, language_weight,
                  avg_llm_score, judge_eval_count,
                  recent_score_0, recent_score_1, recent_score_2, recent_score_3, recent_score_4,
                  related_files)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
                )?
            };
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
        
        // Serialize related_files to JSON
        let related_files_json = serde_json::to_string(&node.related_files).unwrap_or_else(|_| "[]".to_string());
        
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
            node.avg_llm_score,
            node.judge_eval_count,
            node.recent_scores[0],
            node.recent_scores[1],
            node.recent_scores[2],
            node.recent_scores[3],
            node.recent_scores[4],
            related_files_json,
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
                  trace_0, trace_1, trace_2, trace_3, trace_4, language_weight,
                  avg_llm_score, judge_eval_count,
                  recent_score_0, recent_score_1, recent_score_2, recent_score_3, recent_score_4,
                  related_files)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
            )?;
            for node in nodes {
                let related_files_json = serde_json::to_string(&node.related_files).unwrap_or_else(|_| "[]".to_string());
                
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
                    node.avg_llm_score,
                    node.judge_eval_count,
                    node.recent_scores[0],
                    node.recent_scores[1],
                    node.recent_scores[2],
                    node.recent_scores[3],
                    node.recent_scores[4],
                    related_files_json,
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
        let rows = stmt.query_map(params![project_id, limit as i64], |row| {
            self.row_to_node(row)
        })?;
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

    pub fn search(
        &self,
        keyword: &str,
        project_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryNode>> {
        let pattern = format!("%{}%", keyword);
        let nodes = match project_id {
            Some(pid) => {
                let sql = "SELECT * FROM memories WHERE content LIKE ?1 AND project_id = ?2 ORDER BY last_accessed DESC LIMIT ?3";
                let mut stmt = self.conn.prepare(sql)?;
                stmt.query_map(params![pattern, pid, limit as i64], |row| {
                    self.row_to_node(row)
                })?
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
        self.conn.execute(
            "UPDATE memories SET depth = MIN(depth + 1, 5) WHERE id = ?1",
            params![id],
        )?;
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
        self.conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])?;
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

    pub fn load_ema_trend(&self, metric_name: &str) -> anyhow::Result<Option<(f64, f64, u32)>> {
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

    fn row_to_node(&self, row: &rusqlite::Row) -> SqlResult<MemoryNode> {
        // Try to get related_files from column 23 (if exists), otherwise default to empty vec
        let related_files_json: String = row.get(23).unwrap_or_else(|_| "[]".to_string());
        let related_files: Vec<String> = serde_json::from_str(&related_files_json).unwrap_or_default();
        
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
            traces: [
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
            ],
            language_weight: row.get(15)?,
            // 🆕 LLM Judge feedback fields
            avg_llm_score: row.get(16).unwrap_or(0.0),
            judge_eval_count: row.get(17).unwrap_or(0),
            recent_scores: [
                row.get(18).unwrap_or(0.0),
                row.get(19).unwrap_or(0.0),
                row.get(20).unwrap_or(0.0),
                row.get(21).unwrap_or(0.0),
                row.get(22).unwrap_or(0.0),
            ],
            // 🆕 File association
            related_files,
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

        let results = store
            .query_by_project("proj123", &[MemoryNodeType::Architectural], 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Uses tokio for async");
        assert_eq!(results[0].depth, 2);
    }

    #[test]
    fn insert_batch_and_search() {
        let dir = temp_dir();
        let path = dir.path().join("test.db");
        let store = MemoryStore::open(&path).unwrap();

        let nodes: Vec<MemoryNode> = (0..5)
            .map(|i| {
                MemoryNode::new(
                    format!("Memory item {}", i),
                    MemoryNodeType::Fact,
                    Some("proj".into()),
                    "rust".into(),
                    MemorySource::ToolObservation,
                )
            })
            .collect();

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

        let results = store
            .query_overall(&[MemoryNodeType::BestPractice], 10)
            .unwrap();
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

        let results = store
            .query_by_project("p", &[MemoryNodeType::Fact], 10)
            .unwrap();
        assert_eq!(results[0].depth, 5);
    }
}
