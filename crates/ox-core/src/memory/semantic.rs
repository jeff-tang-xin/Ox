use rusqlite::{params, Connection};
use std::sync::Arc;
use tracing;

/// 语义关联类型
#[derive(Debug, Clone, PartialEq)]
pub enum AssociationType {
    Synonym,        // 同义词（用户搜索 A 后也搜索 B）
    CoOccurrence,   // 共现（A 和 B 经常在同一会话中出现）
    Hierarchical,   // 层级关系（"认证" 是 "安全" 的子类）
    UserDefined,    // 用户显式定义
}

impl AssociationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Synonym => "synonym",
            Self::CoOccurrence => "co_occurrence",
            Self::Hierarchical => "hierarchy",
            Self::UserDefined => "user_defined",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "synonym" => Some(Self::Synonym),
            "co_occurrence" => Some(Self::CoOccurrence),
            "hierarchy" => Some(Self::Hierarchical),
            "user_defined" => Some(Self::UserDefined),
            _ => None,
        }
    }
}

/// 语义关联记录
#[derive(Debug, Clone)]
pub struct SemanticAssociation {
    pub source_term: String,
    pub target_term: String,
    pub association_type: AssociationType,
    pub strength: f32,
    pub co_occurrence_count: u32,
}

/// 从 LLM 响应中提取的关键词
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeywordExtraction {
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub related_files: Vec<String>,
}

/// 语义关联管理器
pub struct SemanticAssociationManager {
    // Use a Mutex to make it Sync (rusqlite Connection is not Sync)
    conn: Arc<std::sync::Mutex<Connection>>,
}

impl Clone for SemanticAssociationManager {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
        }
    }
}

impl SemanticAssociationManager {
    pub fn new(conn: Arc<std::sync::Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// 记录语义关联（LLM 提取的关键词）
    pub fn record_llm_keywords(
        &self,
        user_query: &str,
        extracted: &KeywordExtraction,
    ) -> anyhow::Result<()> {
        let query_terms = self.extract_terms(user_query);
        
        for query_term in &query_terms {
            // 关联 keywords
            for keyword in &extracted.keywords {
                if query_term != keyword {
                    self.strengthen_association(
                        query_term,
                        keyword,
                        AssociationType::Synonym,
                    )?;
                }
            }
            
            // 关联 topics
            for topic in &extracted.topics {
                self.strengthen_association(
                    query_term,
                    topic,
                    AssociationType::CoOccurrence,
                )?;
            }
        }
        
        tracing::debug!(
            "[SEMANTIC LEARNING] Recorded {} keywords for query '{}'",
            extracted.keywords.len(),
            user_query
        );
        
        Ok(())
    }

    /// 增强关联强度
    fn strengthen_association(
        &self,
        source: &str,
        target: &str,
        assoc_type: AssociationType,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        
        conn.execute(
            "INSERT INTO semantic_associations 
             (source_term, target_term, association_type, strength, co_occurrence_count, last_updated)
             VALUES (?1, ?2, ?3, 0.5, 1, ?4)
             ON CONFLICT(source_term, target_term) DO UPDATE SET
                co_occurrence_count = co_occurrence_count + 1,
                strength = MIN(strength + 0.1, 1.0),
                last_updated = ?4",
            params![
                source,
                target,
                assoc_type.as_str(),
                now
            ],
        )?;
        
        Ok(())
    }

    /// 查询相关术语（用于查询扩展）
    pub fn get_related_terms(&self, term: &str, min_strength: f32) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT target_term FROM semantic_associations 
             WHERE source_term = ?1 AND strength >= ?2
             ORDER BY strength DESC
             LIMIT 10"
        )?;
        
        let rows = stmt.query_map(params![term, min_strength], |row| {
            row.get::<_, String>(0)
        })?;
        
        let mut terms = Vec::new();
        for row in rows {
            terms.push(row?);
        }
        
        Ok(terms)
    }

    /// 记录搜索历史
    pub fn record_search(
        &self,
        query: &str,
        results_count: usize,
        clicked_result_id: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        
        conn.execute(
            "INSERT INTO search_history (query, timestamp, results_count, clicked_result_id, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                query,
                now,
                results_count as i64,
                clicked_result_id,
                session_id
            ],
        )?;
        
        Ok(())
    }

    /// 提取查询中的术语（简单实现：按空格和标点分割）
    fn extract_terms(&self, query: &str) -> Vec<String> {
        query
            .split(|c: char| c.is_whitespace() || c == '？' || c == '?' || c == '，' || c == ',')
            .filter(|s| !s.is_empty() && s.len() > 1)
            .map(|s| s.to_string())
            .collect()
    }

    /// 获取所有语义关联（用于调试）
    pub fn get_all_associations(&self) -> anyhow::Result<Vec<SemanticAssociation>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT source_term, target_term, association_type, strength, co_occurrence_count
             FROM semantic_associations
             ORDER BY strength DESC"
        )?;
        
        let rows = stmt.query_map([], |row| {
            Ok(SemanticAssociation {
                source_term: row.get(0)?,
                target_term: row.get(1)?,
                association_type: AssociationType::from_str(&row.get::<_, String>(2)?)
                    .unwrap_or(AssociationType::CoOccurrence),
                strength: row.get(3)?,
                co_occurrence_count: row.get(4)?,
            })
        })?;
        
        let mut associations = Vec::new();
        for row in rows {
            associations.push(row?);
        }
        
        Ok(associations)
    }
}
