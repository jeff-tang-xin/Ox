use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection, Result as SqlResult};

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
"#;

pub struct MemoryStore {
    conn: Arc<Connection>,
}

impl Clone for MemoryStore {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
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
        let mut store = Self { conn: Arc::new(conn) };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&mut self) -> anyhow::Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        Ok(())
    }

    pub fn insert(&self, node: &MemoryNode) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO memories
             (id, content, node_type, depth, project_id, language, source,
              created_at, last_accessed, is_project_critical,
              trace_0, trace_1, trace_2, trace_3, trace_4, language_weight)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
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
            ],
        )?;
        Ok(())
    }

    pub fn insert_batch(&self, nodes: &[MemoryNode]) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for node in nodes {
            tx.execute(
                "INSERT OR REPLACE INTO memories
                 (id, content, node_type, depth, project_id, language, source,
                  created_at, last_accessed, is_project_critical,
                  trace_0, trace_1, trace_2, trace_3, trace_4, language_weight)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
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
                ],
            )?;
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
