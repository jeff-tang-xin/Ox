use anyhow::Result;
use anyhow::Context;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, QueryParser, TermQuery, AllQuery, Occur};
use tantivy::schema::*;
use tantivy::{Index, IndexWriter};
use tantivy::schema::Document;
use tantivy::Term;

/// Document type tag
pub const DOC_TYPE_REACT: &str = "react";
pub const DOC_TYPE_GRAPH: &str = "graph";

/// A single react_log record stored in Tantivy
#[derive(Debug, Clone)]
pub struct ReactRecord {
    pub id: String,
    pub session_id: String,
    pub doc_type: String, // "react" or "graph"
    pub task_desc: String,
    pub created_at: String,
    pub timestamp: u64,
    pub tool: String,
    pub target: String,
    pub outcome: String,
    pub decision: String,
    pub assistant_text: String,
    pub reasoning: String,
    pub tool_result: String,
    pub summary: String,
    pub detail: String,
    pub keywords: Vec<String>,
    // Graph-specific fields
    pub tier: i64,
    pub weight: f64,
    pub merged_into: Option<String>,
    pub hit_count: i64,
    pub last_hit_at: String,
}

/// A graph summary record (archived memory)
#[derive(Debug, Clone)]
pub struct GraphRecord {
    pub id: String,
    pub session_id: String,
    pub summary: String,
    pub detail: String,
    pub timestamp: u64,
    pub keywords: Vec<String>,
}

/// Search result with score
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub record: ReactRecord,
    pub score: f32,
}

/// Tantivy index for agent memory retrieval
pub struct ReactIndex {
    index: Index,
    index_path: std::path::PathBuf,
    writer: std::sync::Mutex<IndexWriter>,
    id_counter: AtomicU64,
    id_field: Field,
    session_id_field: Field,
    doc_type_field: Field,
    task_desc_field: Field,
    created_at_field: Field,
    timestamp_field: Field,
    tool_field: Field,
    target_field: Field,
    outcome_field: Field,
    decision_field: Field,
    assistant_text_field: Field,
    reasoning_field: Field,
    tool_result_field: Field,
    summary_field: Field,
    detail_field: Field,
    content_field: Field,
    keywords_field: Field,
    // Graph-specific fields
    tier_field: Field,
    weight_field: Field,
    merged_into_field: Field,
    hit_count_field: Field,
    last_hit_at_field: Field,
}

impl ReactIndex {
    /// Open or create Tantivy index at given path.
    /// Migrates old indexes by recreating the index when schema is incompatible.
    pub fn open(path: &Path) -> Result<Self> {
        let schema = Self::build_schema();

        let needs_recreate = if path.exists() {
            match Index::open_in_dir(path) {
                Ok(idx) => {
                    // Check schema compatibility: must have all required fields
                    let required_fields = ["keywords", "doc_type", "tier", "weight", "merged_into", "hit_count", "last_hit_at"];
                    let missing: Vec<&str> = required_fields.iter()
                        .filter(|f| idx.schema().get_field(f).is_none())
                        .cloned()
                        .collect();
                    if !missing.is_empty() {
                        tracing::warn!("[TANTIVY] Schema missing fields: {:?}, recreating index", missing);
                        true // Need to recreate
                    } else {
                        false
                    }
                }
                Err(e) => {
                    tracing::warn!("[TANTIVY] Failed to open index: {}, recreating", e);
                    true // Corrupted, need to recreate
                }
            }
        } else {
            false // Index doesn't exist, will create new
        };

        if needs_recreate && path.exists() {
            std::fs::remove_dir_all(path)?;
        }

        let index = if path.exists() {
            Index::open_in_dir(path)
                .context("Failed to open existing Tantivy index")?
        } else {
            std::fs::create_dir_all(path)?;
            Index::create_in_dir(path, schema.clone())
                .context("Failed to create Tantivy index")?
        };

        let writer = index.writer(50_000_000)?;

        // Initialize AtomicU64 counter from existing max id
        // If the index is corrupted, fallback to 0
        let id_counter = {
            let mut max_id: u64 = 0;
            if let Ok(reader) = index.reader() {
                let searcher = reader.searcher();
                if let Ok(all_docs) = searcher.search(&AllQuery, &TopDocs::with_limit(100_000)) {
                    for (_score, doc_address) in all_docs {
                        if let Ok(doc) = searcher.doc(doc_address) {
                            if let Some(id_field) = schema.get_field("id") {
                                if let Some(id_val) = doc.get_first(id_field) {
                                    if let Value::Str(id_str) = id_val {
                                        if let Ok(n) = id_str.parse::<u64>() {
                                            if n > max_id {
                                                max_id = n;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            AtomicU64::new(max_id)
        };

        let get_field = |name: &str| -> Result<Field> {
            schema.get_field(name)
                .ok_or_else(|| anyhow::anyhow!("schema field '{}' not found", name))
        };

        Ok(Self {
            index,
            index_path: path.to_path_buf(),
            writer: std::sync::Mutex::new(writer),
            id_counter,
            id_field: get_field("id")?,
            session_id_field: get_field("session_id")?,
            doc_type_field: get_field("doc_type")?,
            task_desc_field: get_field("task_desc")?,
            created_at_field: get_field("created_at")?,
            timestamp_field: get_field("timestamp")?,
            tool_field: get_field("tool")?,
            target_field: get_field("target")?,
            outcome_field: get_field("outcome")?,
            decision_field: get_field("decision")?,
            assistant_text_field: get_field("assistant_text")?,
            reasoning_field: get_field("reasoning")?,
            tool_result_field: get_field("tool_result")?,
            summary_field: get_field("summary")?,
            detail_field: get_field("detail")?,
            content_field: get_field("content")?,
            keywords_field: get_field("keywords")?,
            // Graph-specific fields
            tier_field: get_field("tier")?,
            weight_field: get_field("weight")?,
            merged_into_field: get_field("merged_into")?,
            hit_count_field: get_field("hit_count")?,
            last_hit_at_field: get_field("last_hit_at")?,
        })
    }

    fn build_schema() -> Schema {
        let mut builder = Schema::builder();
        
        // Core fields
        builder.add_text_field("id", STRING | STORED);
        builder.add_text_field("session_id", STRING | STORED);
        builder.add_text_field("doc_type", STRING | STORED); // "react" or "graph"
        builder.add_text_field("task_desc", TEXT | STORED);
        builder.add_text_field("created_at", TEXT | STORED);
        builder.add_u64_field("timestamp", INDEXED | STORED | FAST);
        builder.add_text_field("tool", TEXT | STORED);
        builder.add_text_field("target", TEXT | STORED);
        builder.add_text_field("outcome", TEXT | STORED);
        builder.add_text_field("decision", TEXT | STORED);
        builder.add_text_field("assistant_text", TEXT | STORED);
        builder.add_text_field("reasoning", TEXT | STORED);
        builder.add_text_field("tool_result", TEXT | STORED);
        builder.add_text_field("summary", TEXT | STORED);
        builder.add_text_field("detail", TEXT | STORED);
        
        // Full-text search field (aggregated content)
        builder.add_text_field("content", TEXT);
        
        // Keywords field for graph connections
        builder.add_text_field("keywords", STRING | STORED);
        
        // Graph-specific fields
        builder.add_i64_field("tier", INDEXED | STORED | FAST);
        builder.add_f64_field("weight", STORED | FAST);
        builder.add_text_field("merged_into", STRING | STORED);
        builder.add_i64_field("hit_count", STORED | FAST);
        builder.add_text_field("last_hit_at", TEXT | STORED);
        
        builder.build()
    }

    fn build_doc(&self, record: &ReactRecord, keywords: &[String]) -> Document {
        let mut doc = Document::new();
        doc.add_text(self.id_field, &record.id);
        doc.add_text(self.session_id_field, &record.session_id);
        doc.add_text(self.doc_type_field, &record.doc_type);
        doc.add_text(self.task_desc_field, &record.task_desc);
        doc.add_text(self.created_at_field, &record.created_at);
        doc.add_u64(self.timestamp_field, record.timestamp);
        doc.add_text(self.tool_field, &record.tool);
        doc.add_text(self.target_field, &record.target);
        doc.add_text(self.outcome_field, &record.outcome);
        doc.add_text(self.decision_field, &record.decision);
        doc.add_text(self.assistant_text_field, &record.assistant_text);
        doc.add_text(self.reasoning_field, &record.reasoning);
        doc.add_text(self.tool_result_field, &record.tool_result);
        doc.add_text(self.summary_field, &record.summary);
        doc.add_text(self.detail_field, &record.detail);
        
        // Build content field for full-text search
        let content = format!(
            "{} {} {} {} {} {} {} {} {} {}",
            record.task_desc, record.tool, record.target, record.outcome,
            record.decision, record.assistant_text, record.reasoning,
            record.summary, record.detail,
            keywords.join(" ")
        );
        doc.add_text(self.content_field, content);
        
        // Keywords
        for kw in keywords {
            doc.add_text(self.keywords_field, kw);
        }
        
        // Graph-specific fields
        doc.add_i64(self.tier_field, record.tier);
        doc.add_f64(self.weight_field, record.weight);
        if let Some(ref merged) = record.merged_into {
            doc.add_text(self.merged_into_field, merged);
        }
        doc.add_i64(self.hit_count_field, record.hit_count);
        doc.add_text(self.last_hit_at_field, &record.last_hit_at);
        
        doc
    }

    fn doc_to_record(&self, doc: &Document) -> ReactRecord {
        ReactRecord {
            id: self.get_text(doc, self.id_field),
            session_id: self.get_text(doc, self.session_id_field),
            doc_type: self.get_text(doc, self.doc_type_field),
            task_desc: self.get_text(doc, self.task_desc_field),
            created_at: self.get_text(doc, self.created_at_field),
            timestamp: self.get_u64(doc, self.timestamp_field),
            tool: self.get_text(doc, self.tool_field),
            target: self.get_text(doc, self.target_field),
            outcome: self.get_text(doc, self.outcome_field),
            decision: self.get_text(doc, self.decision_field),
            assistant_text: self.get_text(doc, self.assistant_text_field),
            reasoning: self.get_text(doc, self.reasoning_field),
            tool_result: self.get_text(doc, self.tool_result_field),
            summary: self.get_text(doc, self.summary_field),
            detail: self.get_text(doc, self.detail_field),
            keywords: self.get_keywords(doc, self.keywords_field),
            tier: self.get_i64(doc, self.tier_field),
            weight: self.get_f64(doc, self.weight_field),
            merged_into: self.get_opt_text(doc, self.merged_into_field),
            hit_count: self.get_i64(doc, self.hit_count_field),
            last_hit_at: self.get_text(doc, self.last_hit_at_field),
        }
    }

    fn get_text(&self, doc: &Document, field: Field) -> String {
        doc.get_first(field)
            .and_then(|v| {
                if let Value::Str(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    fn get_opt_text(&self, doc: &Document, field: Field) -> Option<String> {
        doc.get_first(field)
            .and_then(|v| {
                if let Value::Str(s) = v {
                    if s.is_empty() { None } else { Some(s.clone()) }
                } else {
                    None
                }
            })
    }

    fn get_u64(&self, doc: &Document, field: Field) -> u64 {
        doc.get_first(field)
            .and_then(|v| {
                match v {
                    Value::U64(v) => Some(*v),
                    Value::I64(v) => Some(*v as u64),
                    _ => None,
                }
            })
            .unwrap_or(0)
    }

    fn get_i64(&self, doc: &Document, field: Field) -> i64 {
        doc.get_first(field)
            .and_then(|v| {
                match v {
                    Value::I64(v) => Some(*v),
                    Value::U64(v) => Some(*v as i64),
                    _ => None,
                }
            })
            .unwrap_or(0)
    }

    fn get_f64(&self, doc: &Document, field: Field) -> f64 {
        doc.get_first(field)
            .and_then(|v| {
                if let Value::F64(v) = v {
                    Some(*v)
                } else {
                    None
                }
            })
            .unwrap_or(1.0)
    }

    fn get_keywords(&self, doc: &Document, field: Field) -> Vec<String> {
        doc.get_all(field)
            .filter_map(|v| {
                if let Value::Str(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Add react_log record (complete, uncut) to Tantivy index.
    /// ID is auto-generated using AtomicU64 counter.
    pub fn add_record(
        &self,
        session_id: &str,
        task_desc: &str,
        created_at: &str,
        timestamp: u64,
        tool: &str,
        target: &str,
        outcome: &str,
        decision: &str,
        assistant_text: &str,
        reasoning: &str,
        tool_result: &str,
        keywords: &[String],
    ) -> Result<u64> {
        let new_id = self.id_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let record = ReactRecord {
            id: new_id.to_string(),
            session_id: session_id.to_string(),
            doc_type: DOC_TYPE_REACT.to_string(),
            task_desc: task_desc.to_string(),
            created_at: created_at.to_string(),
            timestamp,
            tool: tool.to_string(),
            target: target.to_string(),
            outcome: outcome.to_string(),
            decision: decision.to_string(),
            assistant_text: assistant_text.to_string(),
            reasoning: reasoning.to_string(),
            tool_result: tool_result.to_string(),
            summary: String::new(),
            detail: String::new(),
            keywords: keywords.to_vec(),
            // Graph-specific fields (defaults for react records)
            tier: 0,
            weight: 1.0,
            merged_into: None,
            hit_count: 0,
            last_hit_at: String::new(),
        };
        let doc = self.build_doc(&record, keywords);
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.add_document(doc)?;
        writer.commit()?;
        // Release the lock before reading
        drop(writer);
        // Small delay to ensure Tantivy flushes on Windows
        std::thread::sleep(std::time::Duration::from_millis(5));
        Ok(new_id)
    }

    /// Add graph summary record (archived memory) to Tantivy index.
    /// ID is auto-generated using AtomicU64 counter.
    /// Note: Does NOT commit immediately. Call commit() after batch operations.
    pub fn add_graph_record(
        &self,
        session_id: &str,
        summary: &str,
        detail: &str,
        timestamp: u64,
        keywords: &[String],
        tier: i64,
        weight: f64,
    ) -> Result<u64> {
        let new_id = self.id_counter.fetch_add(1, Ordering::SeqCst) + 1;
        
        let created_at = chrono::DateTime::from_timestamp_millis(timestamp as i64)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or_else(|| "".to_string());
        
        let record = ReactRecord {
            id: new_id.to_string(),
            session_id: session_id.to_string(),
            doc_type: DOC_TYPE_GRAPH.to_string(),
            task_desc: summary.to_string(),
            created_at,
            timestamp,
            tool: String::new(),
            target: String::new(),
            outcome: String::new(),
            decision: String::new(),
            assistant_text: String::new(),
            reasoning: String::new(),
            tool_result: String::new(),
            summary: summary.to_string(),
            detail: detail.to_string(),
            keywords: keywords.to_vec(),
            tier,
            weight,
            merged_into: None,
            hit_count: 0,
            last_hit_at: String::new(),
        };
        
        let doc = self.build_doc(&record, keywords);
        let writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.add_document(doc)?;
        // Don't commit here - commit after batch operations
        Ok(new_id)
    }

    /// Update keywords for an existing record (delete + re-add).
    pub fn update_record_keywords(&self, row_id: u64, keywords: &[String]) -> Result<()> {
        let old = self.get_record_by_id(row_id)?
            .ok_or_else(|| anyhow::anyhow!("id={row_id} not found in Tantivy"))?;
        
        let term = Term::from_field_text(self.id_field, &row_id.to_string());
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.delete_term(term);
        
        let doc = self.build_doc(&old, keywords);
        writer.add_document(doc)?;
        writer.commit()?;
        drop(writer);
        Ok(())
    }

    /// Update graph metadata (tier, weight, merged_into, hit_count, last_hit_at).
    pub fn update_graph_metadata(
        &self,
        row_id: u64,
        tier: Option<i64>,
        weight: Option<f64>,
        merged_into: Option<String>,
        hit_count: Option<i64>,
        last_hit_at: Option<String>,
    ) -> Result<()> {
        let old = self.get_record_by_id(row_id)?
            .ok_or_else(|| anyhow::anyhow!("id={row_id} not found in Tantivy"))?;
        
        let term = Term::from_field_text(self.id_field, &row_id.to_string());
        let writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.delete_term(term);
        
        let mut updated = old.clone();
        if let Some(t) = tier {
            updated.tier = t;
        }
        if let Some(w) = weight {
            updated.weight = w;
        }
        if let Some(m) = merged_into {
            updated.merged_into = Some(m);
        }
        if let Some(h) = hit_count {
            updated.hit_count = h;
        }
        if let Some(l) = last_hit_at {
            updated.last_hit_at = l;
        }
        
        let doc = self.build_doc(&updated, &updated.keywords);
        writer.add_document(doc)?;
        // Don't commit here - commit after batch operations
        Ok(())
    }

    /// Get a single record by id.
    pub fn get_record_by_id(&self, row_id: u64) -> Result<Option<ReactRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let q = TermQuery::new(
            Term::from_field_text(self.id_field, &row_id.to_string()),
            IndexRecordOption::Basic,
        );
        let top_docs = searcher.search(&q, &TopDocs::with_limit(1))?;
        if let Some((_score, doc_address)) = top_docs.first() {
            let doc = searcher.doc(*doc_address)?;
            return Ok(Some(self.doc_to_record(&doc)));
        }
        Ok(None)
    }

    /// Get all unimpacted react_log records for a session (doc_type='react', tier=0).
    /// These are the active memory entries for context injection.
    pub fn get_active_react_records(&self, session_id: &str, limit: usize) -> Result<Vec<ReactRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        // Query for session_id AND doc_type='react' AND tier=0
        let session_term = Term::from_field_text(self.session_id_field, session_id);
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_REACT);
        
        let session_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            session_term,
            IndexRecordOption::Basic,
        ));
        let doc_type_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            doc_type_term,
            IndexRecordOption::Basic,
        ));
        
        let combined = BooleanQuery::new(vec![
            (Occur::Must, session_query),
            (Occur::Must, doc_type_query),
        ]);

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(limit))?;
        let mut records = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            let record = self.doc_to_record(&doc);
            // Only include tier=0 (unimpacted) records
            if record.tier == 0 {
                records.push(record);
            }
        }
        
        // Sort by timestamp ascending (oldest first)
        records.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(records)
    }

    /// Search for records matching the query text (BM25).
    pub fn search(&self, session_id: &str, query: &str, top_n: usize) -> Result<Vec<SearchResult>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        // Build query parser
        let mut parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        parser.set_conjunction_by_default();

        // Session filter
        let session_filter: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.session_id_field, session_id),
            IndexRecordOption::Basic,
        ));

        // Content query
        let content_query = parser.parse_query(query)?;

        let combined = BooleanQuery::new(vec![
            (Occur::Must, session_filter),
            (Occur::Must, Box::new(content_query)),
        ]);

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(top_n))?;
        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            let record = self.doc_to_record(&doc);
            results.push(SearchResult { record, score });
        }

        Ok(results)
    }

    /// Search by keywords (exact match on keywords field).
    pub fn search_by_keywords(&self, session_id: &str, keywords: &[String], limit: usize) -> Result<Vec<ReactRecord>> {
        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        // Build keyword queries
        let keyword_queries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = keywords.iter()
            .map(|kw| {
                let term = Term::from_field_text(self.keywords_field, kw);
                (Occur::Should, Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn tantivy::query::Query>)
            })
            .collect();

        // Session filter
        let session_filter: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.session_id_field, session_id),
            IndexRecordOption::Basic,
        ));

        let combined = BooleanQuery::new(vec![
            (Occur::Must, session_filter),
            (Occur::Should, Box::new(BooleanQuery::new(keyword_queries))),
        ]);

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(limit))?;
        let mut records = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_record(&doc));
        }

        Ok(records)
    }

    /// Get all records for building the memory graph (both react and graph records)
    pub fn get_all_records_for_graph(&self, session_id: &str, limit: usize) -> Result<Vec<ReactRecord>> {
        let mut last_error = None;
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(200 * attempt as u64));
            }
            match self.try_get_all_records_for_graph(session_id, limit) {
                Ok(records) => return Ok(records),
                Err(e) => {
                    tracing::warn!("[TANTIVY] get_all_records_for_graph attempt {} failed: {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap())
    }

    fn try_get_all_records_for_graph(&self, session_id: &str, limit: usize) -> Result<Vec<ReactRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        let session_filter: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.session_id_field, session_id),
            IndexRecordOption::Basic,
        ));

        let top_docs = searcher.search(&session_filter, &TopDocs::with_limit(limit))?;
        let mut records = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_record(&doc));
        }
        
        records.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(records)
    }

    /// Get all graph records for a session (doc_type='graph')
    pub fn get_graph_records(&self, session_id: &str, limit: usize) -> Result<Vec<ReactRecord>> {
        // Retry up to 5 times with backoff (Tantivy may need time to flush on Windows)
        let mut last_error = None;
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(200 * attempt as u64));
            }
            match self.try_get_graph_records(session_id, limit) {
                Ok(records) => return Ok(records),
                Err(e) => {
                    tracing::warn!("[TANTIVY] get_graph_records attempt {} failed: {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap())
    }

    fn try_get_graph_records(&self, session_id: &str, limit: usize) -> Result<Vec<ReactRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        let session_term = Term::from_field_text(self.session_id_field, session_id);
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_GRAPH);
        
        let session_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            session_term,
            IndexRecordOption::Basic,
        ));
        let doc_type_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            doc_type_term,
            IndexRecordOption::Basic,
        ));
        
        let combined = BooleanQuery::new(vec![
            (Occur::Must, session_query),
            (Occur::Must, doc_type_query),
        ]);

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(limit))?;
        let mut records = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_record(&doc));
        }

        // Sort by weight DESC, timestamp ASC
        records.sort_by(|a, b| {
            b.weight.partial_cmp(&a.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.timestamp.cmp(&b.timestamp))
        });
        Ok(records)
    }

    /// Get all graph records for a session (legacy name)
    pub fn get_all_graphs_for_session(&self, session_id: &str, limit: usize) -> Result<Vec<ReactRecord>> {
        self.get_graph_records(session_id, limit)
    }

    /// Get document count
    pub fn doc_count(&self) -> Result<u64> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        Ok(searcher.num_docs())
    }

    /// Commit pending changes
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.commit()?;
        drop(writer);
        // Give Windows time to flush file system cache
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Get all records for a session in chronological order (legacy name)
    pub fn get_session_records_chronological(&self, session_id: &str) -> Result<Vec<ReactRecord>> {
        self.get_active_react_records(session_id, 10_000)
    }

    /// Get multiple records by their IDs
    pub fn get_records_by_ids(&self, ids: &[u64]) -> Result<Vec<ReactRecord>> {
        let mut records = Vec::new();
        for id in ids {
            if let Some(record) = self.get_record_by_id(*id)? {
                records.push(record);
            }
        }
        // Sort by timestamp
        records.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(records)
    }
}