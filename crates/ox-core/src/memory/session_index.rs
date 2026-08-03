//! Session-level memory index (Tantivy).
//! Stores session summaries, key facts, and file tracking — replaces SQLite tables.
//!
//! Document types:
//! - "session":会话摘要（id, task_desc, content_summary, learnings, created_at）
//! - "fact": 关键事实（session_id, fact_text, related_files）
//! - "file_read": 读取文件（session_id, file_path, purpose）
//! - "file_modified": 修改文件（session_id, file_path, change_summary）

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, TermQuery};
use tantivy::schema::*;
use tantivy::{Document, Index, IndexWriter, Term};

/// Session summary record (replaces SQLite sessions table)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub task_desc: String,
    pub content_summary: String,
    pub learnings: String,
    pub created_at: String,
    pub doc_type: String, // "session"
}

/// Key fact record (replaces SQLite key_facts table)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactRecord {
    pub id: String,
    pub session_id: String,
    pub fact_text: String,
    pub related_files: String,
    pub doc_type: String, // "fact"
}

/// File read record (replaces SQLite files_read table)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadRecord {
    pub id: String,
    pub session_id: String,
    pub file_path: String,
    pub purpose: String,
    pub doc_type: String, // "file_read"
}

/// File modified record (replaces SQLite files_modified table)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileModifiedRecord {
    pub id: String,
    pub session_id: String,
    pub file_path: String,
    pub change_summary: String,
    pub doc_type: String, // "file_modified"
}

pub const DOC_TYPE_SESSION: &str = "session";
pub const DOC_TYPE_FACT: &str = "fact";
pub const DOC_TYPE_FILE_READ: &str = "file_read";
pub const DOC_TYPE_FILE_MODIFIED: &str = "file_modified";

pub struct SessionIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    // Schema fields
    id_field: tantivy::schema::Field,
    session_id_field: tantivy::schema::Field,
    doc_type_field: tantivy::schema::Field,
    task_desc_field: tantivy::schema::Field,
    content_summary_field: tantivy::schema::Field,
    learnings_field: tantivy::schema::Field,
    created_at_field: tantivy::schema::Field,
    fact_text_field: tantivy::schema::Field,
    related_files_field: tantivy::schema::Field,
    file_path_field: tantivy::schema::Field,
    purpose_field: tantivy::schema::Field,
    change_summary_field: tantivy::schema::Field,
}

impl SessionIndex {
    /// Open or create session index at the given directory path.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(path)?;
        let index_path = path.join("session_index");
        
        let index = if index_path.exists() {
            Index::open_in_dir(&index_path)?
        } else {
            std::fs::create_dir_all(&index_path)?;
            Index::create_in_dir(&index_path, Self::build_schema())?
        };
        
        let writer = index.writer(50_000_000)?;
        
        let schema = index.schema();
        Ok(Self {
            index,
            writer: Mutex::new(writer),
            id_field: schema.get_field("id").ok_or_else(|| anyhow!("Field 'id' not found"))?,
            session_id_field: schema.get_field("session_id").ok_or_else(|| anyhow!("Field 'session_id' not found"))?,
            doc_type_field: schema.get_field("doc_type").ok_or_else(|| anyhow!("Field 'doc_type' not found"))?,
            task_desc_field: schema.get_field("task_desc").ok_or_else(|| anyhow!("Field 'task_desc' not found"))?,
            content_summary_field: schema.get_field("content_summary").ok_or_else(|| anyhow!("Field 'content_summary' not found"))?,
            learnings_field: schema.get_field("learnings").ok_or_else(|| anyhow!("Field 'learnings' not found"))?,
            created_at_field: schema.get_field("created_at").ok_or_else(|| anyhow!("Field 'created_at' not found"))?,
            fact_text_field: schema.get_field("fact_text").ok_or_else(|| anyhow!("Field 'fact_text' not found"))?,
            related_files_field: schema.get_field("related_files").ok_or_else(|| anyhow!("Field 'related_files' not found"))?,
            file_path_field: schema.get_field("file_path").ok_or_else(|| anyhow!("Field 'file_path' not found"))?,
            purpose_field: schema.get_field("purpose").ok_or_else(|| anyhow!("Field 'purpose' not found"))?,
            change_summary_field: schema.get_field("change_summary").ok_or_else(|| anyhow!("Field 'change_summary' not found"))?,
        })
    }

    fn build_schema() -> Schema {
        let mut builder = Schema::builder();
        
        // Core fields
        builder.add_text_field("id", STRING | STORED);
        builder.add_text_field("session_id", STRING | STORED);
        builder.add_text_field("doc_type", STRING | STORED); // "session", "fact", "file_read", "file_modified"
        builder.add_text_field("task_desc", TEXT | STORED);
        builder.add_text_field("content_summary", TEXT | STORED);
        builder.add_text_field("learnings", TEXT | STORED);
        builder.add_text_field("created_at", TEXT | STORED);
        
        // Fact fields
        builder.add_text_field("fact_text", TEXT | STORED);
        builder.add_text_field("related_files", TEXT | STORED);
        
        // File fields
        builder.add_text_field("file_path", TEXT | STORED);
        builder.add_text_field("purpose", TEXT | STORED);
        builder.add_text_field("change_summary", TEXT | STORED);
        
        // Full-text search field
        builder.add_text_field("content", TEXT);
        
        builder.build()
    }

    /// Insert a session record
    pub fn insert_session(&self, record: &SessionRecord) -> Result<()> {
        let mut doc = Document::new();
        doc.add_text(self.id_field, &record.id);
        doc.add_text(self.session_id_field, &record.id); // session_id = id for sessions
        doc.add_text(self.doc_type_field, DOC_TYPE_SESSION);
        doc.add_text(self.task_desc_field, &record.task_desc);
        doc.add_text(self.content_summary_field, &record.content_summary);
        doc.add_text(self.learnings_field, &record.learnings);
        doc.add_text(self.created_at_field, &record.created_at);
        
        // Build content for full-text search
        let content = format!(
            "{} {} {} {}",
            record.task_desc, record.content_summary, record.learnings, record.created_at
        );
        doc.add_text(self.get_content_field(), content);
        
        let writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        // Delete existing session with same id first
        let term = Term::from_field_text(self.id_field, &record.id);
        writer.delete_term(term);
        writer.add_document(doc)?;
        Ok(())
    }

    /// Insert a fact record
    pub fn insert_fact(&self, record: &FactRecord) -> Result<()> {
        let mut doc = Document::new();
        doc.add_text(self.id_field, &record.id);
        doc.add_text(self.session_id_field, &record.session_id);
        doc.add_text(self.doc_type_field, DOC_TYPE_FACT);
        doc.add_text(self.fact_text_field, &record.fact_text);
        doc.add_text(self.related_files_field, &record.related_files);
        
        let content = format!(
            "{} {}",
            record.fact_text, record.related_files
        );
        doc.add_text(self.get_content_field(), content);
        
        let writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.add_document(doc)?;
        Ok(())
    }

    /// Insert a file read record
    pub fn insert_file_read(&self, record: &FileReadRecord) -> Result<()> {
        let mut doc = Document::new();
        doc.add_text(self.id_field, &record.id);
        doc.add_text(self.session_id_field, &record.session_id);
        doc.add_text(self.doc_type_field, DOC_TYPE_FILE_READ);
        doc.add_text(self.file_path_field, &record.file_path);
        doc.add_text(self.purpose_field, &record.purpose);
        
        let content = format!(
            "{} {}",
            record.file_path, record.purpose
        );
        doc.add_text(self.get_content_field(), content);
        
        let writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.add_document(doc)?;
        Ok(())
    }

    /// Insert a file modified record
    pub fn insert_file_modified(&self, record: &FileModifiedRecord) -> Result<()> {
        let mut doc = Document::new();
        doc.add_text(self.id_field, &record.id);
        doc.add_text(self.session_id_field, &record.session_id);
        doc.add_text(self.doc_type_field, DOC_TYPE_FILE_MODIFIED);
        doc.add_text(self.file_path_field, &record.file_path);
        doc.add_text(self.change_summary_field, &record.change_summary);
        
        let content = format!(
            "{} {}",
            record.file_path, record.change_summary
        );
        doc.add_text(self.get_content_field(), content);
        
        let writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.add_document(doc)?;
        Ok(())
    }

    /// Delete all records for a session (by session_id).
    /// Deletes session, facts, file_read, and file_modified records.
    pub fn delete_by_session(&self, session_id: &str) -> Result<()> {
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        let session_term = Term::from_field_text(self.session_id_field, session_id);
        writer.delete_term(session_term);
        writer.commit()?;
        drop(writer);
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Get recent sessions, sorted by created_at DESC
    pub fn get_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_SESSION);
        let query = TermQuery::new(doc_type_term, tantivy::schema::IndexRecordOption::Basic);
        
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;
        let mut records = Vec::new();
        
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_session(&doc));
        }
        
        Ok(records)
    }

    /// Get sessions by file path (for file history queries)
    pub fn get_sessions_by_file(&self, file_path: &str, limit: usize) -> Result<Vec<(SessionRecord, String)>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        
        let file_norm = file_path.replace('\\', "/").to_lowercase();
        let file_base = std::path::Path::new(&file_norm)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&file_norm)
            .to_string();
        
        // Search file_modified records matching the file path
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_FILE_MODIFIED);
        
        let query = TermQuery::new(doc_type_term, tantivy::schema::IndexRecordOption::Basic);
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit * 5))?;
        
        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            let modified = self.doc_to_file_modified(&doc);
            let norm_path = modified.file_path.replace('\\', "/").to_lowercase();
            let base = std::path::Path::new(&norm_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&norm_path);
            
            if base == file_base || norm_path.contains(&file_base) {
                // Get session record
                if let Some(session) = self.get_session(&modified.session_id)? {
                    results.push((session, modified.change_summary));
                }
                if results.len() >= limit {
                    break;
                }
            }
        }
        
        Ok(results)
    }

    /// Get a session by id
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        
        let id_term = Term::from_field_text(self.id_field, session_id);
        let query = TermQuery::new(id_term, tantivy::schema::IndexRecordOption::Basic);
        
        let top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;
        if let Some((_score, doc_address)) = top_docs.first() {
            let doc = searcher.doc(*doc_address)?;
            return Ok(Some(self.doc_to_session(&doc)));
        }
        
        Ok(None)
    }

    /// Get all file_modified records
    pub fn get_all_file_modified(&self, limit: usize) -> Result<Vec<FileModifiedRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_FILE_MODIFIED);
        let query = TermQuery::new(doc_type_term, tantivy::schema::IndexRecordOption::Basic);
        
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;
        let mut records = Vec::new();
        
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_file_modified(&doc));
        }
        
        Ok(records)
    }

    /// Get all facts for a session
    pub fn get_facts_for_session(&self, session_id: &str) -> Result<Vec<FactRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_FACT);
        let session_term = Term::from_field_text(self.session_id_field, session_id);
        
        let doc_type_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            doc_type_term,
            tantivy::schema::IndexRecordOption::Basic,
        ));
        let session_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            session_term,
            tantivy::schema::IndexRecordOption::Basic,
        ));
        
        let combined = BooleanQuery::new(vec![
            (Occur::Must, doc_type_query),
            (Occur::Must, session_query),
        ]);
        
        let top_docs = searcher.search(&combined, &TopDocs::with_limit(100))?;
        let mut records = Vec::new();
        
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_fact(&doc));
        }
        
        Ok(records)
    }

    /// Get all file_read records for a session
    pub fn get_file_reads_for_session(&self, session_id: &str) -> Result<Vec<FileReadRecord>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        
        let doc_type_term = Term::from_field_text(self.doc_type_field, DOC_TYPE_FILE_READ);
        let session_term = Term::from_field_text(self.session_id_field, session_id);
        
        let doc_type_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            doc_type_term,
            tantivy::schema::IndexRecordOption::Basic,
        ));
        let session_query: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
            session_term,
            tantivy::schema::IndexRecordOption::Basic,
        ));
        
        let combined = BooleanQuery::new(vec![
            (Occur::Must, doc_type_query),
            (Occur::Must, session_query),
        ]);
        
        let top_docs = searcher.search(&combined, &TopDocs::with_limit(100))?;
        let mut records = Vec::new();
        
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            records.push(self.doc_to_file_read(&doc));
        }
        
        Ok(records)
    }

    fn get_content_field(&self) -> tantivy::schema::Field {
        self.index.schema().get_field("content").unwrap()
    }

    fn doc_to_session(&self, doc: &Document) -> SessionRecord {
        SessionRecord {
            id: self.get_text(doc, self.id_field),
            task_desc: self.get_text(doc, self.task_desc_field),
            content_summary: self.get_text(doc, self.content_summary_field),
            learnings: self.get_text(doc, self.learnings_field),
            created_at: self.get_text(doc, self.created_at_field),
            doc_type: DOC_TYPE_SESSION.to_string(),
        }
    }

    fn doc_to_fact(&self, doc: &Document) -> FactRecord {
        FactRecord {
            id: self.get_text(doc, self.id_field),
            session_id: self.get_text(doc, self.session_id_field),
            fact_text: self.get_text(doc, self.fact_text_field),
            related_files: self.get_text(doc, self.related_files_field),
            doc_type: DOC_TYPE_FACT.to_string(),
        }
    }

    fn doc_to_file_read(&self, doc: &Document) -> FileReadRecord {
        FileReadRecord {
            id: self.get_text(doc, self.id_field),
            session_id: self.get_text(doc, self.session_id_field),
            file_path: self.get_text(doc, self.file_path_field),
            purpose: self.get_text(doc, self.purpose_field),
            doc_type: DOC_TYPE_FILE_READ.to_string(),
        }
    }

    fn doc_to_file_modified(&self, doc: &Document) -> FileModifiedRecord {
        FileModifiedRecord {
            id: self.get_text(doc, self.id_field),
            session_id: self.get_text(doc, self.session_id_field),
            file_path: self.get_text(doc, self.file_path_field),
            change_summary: self.get_text(doc, self.change_summary_field),
            doc_type: DOC_TYPE_FILE_MODIFIED.to_string(),
        }
    }

    fn get_text(&self, doc: &Document, field: Field) -> String {
        use tantivy::schema::Value;
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

    /// Commit pending changes
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.commit()?;
        drop(writer);
        // Give Windows time to flush file system cache
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Get document count
    pub fn doc_count(&self) -> Result<u64> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        Ok(searcher.num_docs())
    }
}
