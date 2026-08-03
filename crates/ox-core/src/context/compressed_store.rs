//! Compressed context store backed by Tantivy.
//! Stores compressed conversation context for sessions.
//! JSONL keeps the full chat log; this holds the compressed snapshot
//! so that context building uses compressed + new messages.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter, Document};

use crate::message::Message;

const MAX_OPEN_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u64 = 200;

/// A stored compressed context record
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CompressedRecord {
    session_id: String,
    messages_json: String,
    source_msg_count: u64,
    created_at: String,
}

pub struct CompressedContextStore {
    index: Index,
    writer: Mutex<IndexWriter>,
    session_id_field: Field,
    messages_json_field: Field,
    source_msg_count_field: Field,
    created_at_field: Field,
}

impl CompressedContextStore {
    /// Open or create store at the given path.
    /// The path is treated as a directory path for the Tantivy index.
    /// If the path looks like a file (has an extension), it is converted to a directory path.
    ///
    /// Tantivy's lock file (`index.lock`) always belongs to the previous process.
    /// On startup we delete it unconditionally, then retry a few times for Windows
    /// file-system cache / antivirus lag.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let index_dir = Self::prepare_index_dir(path)?;

        // Tantivy lock file belongs to a previous (dead) process — always safe to remove.
        let lock_file = index_dir.join("index.lock");
        if lock_file.exists() {
            tracing::info!(
                "[CompressedContextStore] clearing stale index.lock: {}",
                lock_file.display()
            );
            let _ = std::fs::remove_file(&lock_file);
        }

        // Retry a few times for transient Windows file-locking issues.
        let mut last_err = None;
        for attempt in 0..MAX_OPEN_RETRIES {
            match Self::try_open(&index_dir) {
                Ok(store) => return Ok(store),
                Err(e) => {
                    let msg = e.to_string();
                    if attempt < MAX_OPEN_RETRIES - 1 {
                        tracing::warn!(
                            "[CompressedContextStore] open attempt {}/{} failed: {}",
                            attempt + 1,
                            MAX_OPEN_RETRIES,
                            msg
                        );
                        // Lock may have been recreated by another writer; clear it again.
                        let _ = std::fs::remove_file(&lock_file);
                        std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
                        last_err = Some(e);
                        continue;
                    }
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Failed to open index")))
    }

    /// Convert the user-supplied path to an index directory and create it if missing.
    fn prepare_index_dir(path: &Path) -> anyhow::Result<std::path::PathBuf> {
        let index_dir = if path.extension().is_some() {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("compressed_context");
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            parent.join(format!("{}_index", stem))
        } else {
            path.to_path_buf()
        };

        std::fs::create_dir_all(&index_dir)?;
        Ok(index_dir)
    }

    fn try_open(index_dir: &Path) -> anyhow::Result<Self> {
        let index = if index_dir.join("meta.json").exists() {
            Index::open_in_dir(index_dir)?
        } else {
            Index::create_in_dir(index_dir, Self::build_schema())?
        };

        let writer = index.writer(50_000_000)?;
        let schema = index.schema();

        Ok(Self {
            index,
            writer: Mutex::new(writer),
            session_id_field: schema
                .get_field("session_id")
                .ok_or_else(|| anyhow::anyhow!("field not found: session_id"))?,
            messages_json_field: schema
                .get_field("messages_json")
                .ok_or_else(|| anyhow::anyhow!("field not found: messages_json"))?,
            source_msg_count_field: schema
                .get_field("source_msg_count")
                .ok_or_else(|| anyhow::anyhow!("field not found: source_msg_count"))?,
            created_at_field: schema
                .get_field("created_at")
                .ok_or_else(|| anyhow::anyhow!("field not found: created_at"))?,
        })
    }

    /// Create an in-memory store for fallback when disk is unavailable
    /// or the on-disk index cannot be opened (e.g. persistent lock conflict).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let index = Index::create_in_ram(Self::build_schema());
        let writer = index.writer(50_000_000)?;
        let schema = index.schema();

        Ok(Self {
            index,
            writer: Mutex::new(writer),
            session_id_field: schema
                .get_field("session_id")
                .ok_or_else(|| anyhow::anyhow!("field not found: session_id"))?,
            messages_json_field: schema
                .get_field("messages_json")
                .ok_or_else(|| anyhow::anyhow!("field not found: messages_json"))?,
            source_msg_count_field: schema
                .get_field("source_msg_count")
                .ok_or_else(|| anyhow::anyhow!("field not found: source_msg_count"))?,
            created_at_field: schema
                .get_field("created_at")
                .ok_or_else(|| anyhow::anyhow!("field not found: created_at"))?,
        })
    }

    fn build_schema() -> Schema {
        let mut builder = Schema::builder();
        builder.add_text_field("session_id", STRING | STORED);
        builder.add_text_field("messages_json", STRING | STORED);
        builder.add_u64_field("source_msg_count", STORED);
        builder.add_text_field("created_at", STRING | STORED);
        builder.build()
    }

    /// Load compressed context for a session.
    /// Returns (compressed_messages, source_msg_count) or None if not found.
    pub fn load(&self, session_id: &str) -> anyhow::Result<Option<(Vec<Message>, usize)>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        let term = Term::from_field_text(self.session_id_field, session_id);
        let query = TermQuery::new(term, IndexRecordOption::Basic);

        let top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;
        if let Some((_score, doc_address)) = top_docs.first() {
            let doc = searcher.doc(*doc_address)?;
            let record = Self::doc_to_record(
                &doc,
                self.session_id_field,
                self.messages_json_field,
                self.source_msg_count_field,
                self.created_at_field,
            );
            let messages: Vec<Message> = serde_json::from_str(&record.messages_json)?;
            return Ok(Some((messages, record.source_msg_count as usize)));
        }

        Ok(None)
    }

    /// Save compressed context for a session (upserts).
    pub fn save(
        &self,
        session_id: &str,
        messages: &[Message],
        source_msg_count: usize,
    ) -> anyhow::Result<()> {
        let json = serde_json::to_string(messages)?;
        let now = chrono::Utc::now().to_rfc3339();

        let term = Term::from_field_text(self.session_id_field, session_id);
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.delete_term(term);

        let doc = doc!(
            self.session_id_field => session_id,
            self.messages_json_field => json.as_str(),
            self.source_msg_count_field => source_msg_count as u64,
            self.created_at_field => now.as_str(),
        );

        writer.add_document(doc)?;
        writer.commit()?;
        drop(writer);

        // Give Windows time to flush file system cache
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// Delete compressed context for a session (e.g. on /new).
    pub fn delete(&self, session_id: &str) -> anyhow::Result<()> {
        let term = Term::from_field_text(self.session_id_field, session_id);
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
        writer.delete_term(term);
        writer.commit()?;
        drop(writer);

        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    fn doc_to_record(
        doc: &Document,
        session_id_field: Field,
        messages_json_field: Field,
        source_msg_count_field: Field,
        created_at_field: Field,
    ) -> CompressedRecord {
        CompressedRecord {
            session_id: Self::get_text(doc, session_id_field),
            messages_json: Self::get_text(doc, messages_json_field),
            source_msg_count: Self::get_u64(doc, source_msg_count_field),
            created_at: Self::get_text(doc, created_at_field),
        }
    }

    fn get_text(doc: &Document, field: Field) -> String {
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

    fn get_u64(doc: &Document, field: Field) -> u64 {
        use tantivy::schema::Value;
        doc.get_first(field)
            .and_then(|v| {
                if let Value::U64(val) = v {
                    Some(*val)
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }
}
