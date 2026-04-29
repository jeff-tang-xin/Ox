use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Message;

/// Metadata stored alongside a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub project_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

/// A persistent conversation session.
/// Messages are stored as JSONL (one JSON object per line) for append-only durability.
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    file_path: PathBuf,
    file_handle: Option<BufWriter<File>>,
}

impl Session {
    /// Create a new session in the given directory (e.g. `.ox/`).
    pub fn new(session_dir: &Path, project_id: &str) -> anyhow::Result<Self> {
        fs::create_dir_all(session_dir)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let file_path = session_dir.join("session.jsonl");

        let meta = SessionMeta {
            id,
            project_id: project_id.to_string(),
            created_at: now.clone(),
            updated_at: now,
            message_count: 0,
        };

        // Write meta as first line and keep file handle open.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;
        let mut writer = BufWriter::new(file);
        let meta_line = serde_json::to_string(&serde_json::json!({"_meta": &meta}))?;
        writeln!(writer, "{meta_line}")?;
        writer.flush()?;

        Ok(Self {
            meta,
            messages: Vec::new(),
            file_path,
            file_handle: Some(writer),
        })
    }

    /// Load an existing session from a JSONL file.
    /// Skips malformed last line for crash safety.
    pub fn load(session_dir: &Path) -> anyhow::Result<Option<Self>> {
        let file_path = session_dir.join("session.jsonl");
        if !file_path.exists() {
            return Ok(None);
        }

        let file = File::open(&file_path)?;
        let reader = BufReader::new(file);

        // First line is meta.
        let mut meta: Option<SessionMeta> = None;
        let mut messages = Vec::new();
        let mut is_last = false;

        for (i, line_result) in reader.lines().enumerate() {
            let line = line_result?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Try to parse as meta.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(m) = val.get("_meta")
                    && let Ok(m) = serde_json::from_value::<SessionMeta>(m.clone()) {
                        meta = Some(m);
                        continue;
                    }

            // Try to parse as message. Skip malformed last line (crash safety).
            match serde_json::from_str::<Message>(line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    // We can't know if this is the last line without reading ahead,
                    // so treat all malformed lines as warnings.
                    tracing::warn!("Skipping malformed line {i} in session: {e}");
                    is_last = true;
                }
            }
        }

        if meta.is_none() && messages.is_empty() && !is_last {
            return Ok(None);
        }

        let meta = meta.unwrap_or_else(|| SessionMeta {
            id: Uuid::new_v4().to_string(),
            project_id: String::new(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            message_count: messages.len(),
        });

        Ok(Some(Self {
            meta,
            messages,
            file_path,
            file_handle: None, // Will be opened on first append.
        }))
    }

    /// Append a message to the session (in memory + on disk).
    pub fn append_message(&mut self, msg: Message) -> anyhow::Result<()> {
        let json = serde_json::to_string(&msg)?;

        // Open file handle lazily on first append, then reuse.
        if self.file_handle.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file_path)?;
            self.file_handle = Some(BufWriter::new(file));
        }

        if let Some(ref mut writer) = self.file_handle {
            writeln!(writer, "{json}")?;
            writer.flush()?;
        }

        self.messages.push(msg);
        self.meta.message_count = self.messages.len();
        self.meta.updated_at = Utc::now().to_rfc3339();

        Ok(())
    }

    /// Archive the current session (move to sessions/ with timestamp name).
    pub fn archive(&self, session_dir: &Path) -> anyhow::Result<()> {
        let archive_dir = session_dir.join("sessions");
        fs::create_dir_all(&archive_dir)?;

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let archive_name = format!("session_{timestamp}_{}.jsonl", &self.meta.id[..8]);
        let archive_path = archive_dir.join(archive_name);

        fs::rename(&self.file_path, &archive_path)?;

        Ok(())
    }

    /// Get the number of non-system messages.
    pub fn user_message_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count()
    }

    /// Get the session directory path.
    pub fn dir(&self) -> &Path {
        self.file_path.parent().unwrap_or(Path::new("."))
    }

    /// Clean/reset the session by clearing all messages.
    /// Writes a new meta line, effectively starting fresh.
    pub fn clean(&mut self) -> anyhow::Result<()> {
        // Update meta
        self.meta.message_count = 0;
        self.meta.updated_at = Utc::now().to_rfc3339();

        // Clear in-memory messages
        self.messages.clear();

        // Close existing file handle and rewrite file with fresh meta
        if let Some(ref mut writer) = self.file_handle {
            writer.flush()?;
        }
        self.file_handle = None;

        // Rewrite file with just meta line
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.file_path)?;
        let mut writer = BufWriter::new(file);
        let meta_line = serde_json::to_string(&serde_json::json!({"_meta": &self.meta}))?;
        writeln!(writer, "{meta_line}")?;
        writer.flush()?;
        self.file_handle = Some(writer);

        Ok(())
    }

    /// List archived sessions in the sessions/ directory.
    /// Returns (filename, display_info) pairs sorted by most recent first.
    pub fn list_archived(session_dir: &Path) -> Vec<(String, String)> {
        let archive_dir = session_dir.join("sessions");
        if !archive_dir.exists() {
            return Vec::new();
        }

        let mut entries: Vec<_> = fs::read_dir(&archive_dir)
            .ok()
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .is_some_and(|ext| ext == "jsonl")
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Sort by modification time, most recent first.
        entries.sort_by(|a, b| {
            b.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .cmp(
                    &a.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                )
        });

        entries
            .iter()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                // Try to read first line for meta info.
                let file = File::open(e.path()).ok()?;
                let reader = BufReader::new(file);
                let first_line = reader.lines().next()?.ok()?;
                let val: serde_json::Value = serde_json::from_str(&first_line).ok()?;
                let meta = val.get("_meta")?;
                let id = meta.get("id")?.as_str().unwrap_or("?");
                let count = meta.get("message_count")?.as_u64().unwrap_or(0);
                let created = meta.get("created_at")?.as_str().unwrap_or("?");
                // Shorten the timestamp for display.
                let short_time = if created.len() >= 16 {
                    &created[..16]
                } else {
                    created
                };
                Some((name, format!("{short_time}  [{count} msgs]  id:{:.8}", id)))
            })
            .collect()
    }

    /// Load an archived session by filename.
    pub fn load_archived(session_dir: &Path, filename: &str) -> anyhow::Result<Option<Self>> {
        let archive_path = session_dir.join("sessions").join(filename);
        if !archive_path.exists() {
            return Ok(None);
        }

        let file = File::open(&archive_path)?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        if lines.is_empty() {
            return Ok(None);
        }

        let mut meta: Option<SessionMeta> = None;
        let mut messages = Vec::new();
        let total_lines = lines.len();

        for (i, line) in lines.iter().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(m) = val.get("_meta")
                    && let Ok(m) = serde_json::from_value::<SessionMeta>(m.clone()) {
                        meta = Some(m);
                        continue;
                    }

            match serde_json::from_str::<Message>(line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    if i == total_lines - 1 {
                        tracing::warn!("Skipping malformed last line: {e}");
                    }
                }
            }
        }

        let meta = meta.unwrap_or_else(|| SessionMeta {
            id: Uuid::new_v4().to_string(),
            project_id: String::new(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            message_count: messages.len(),
        });

        Ok(Some(Self {
            meta,
            messages,
            file_path: archive_path,
            file_handle: None,
        }))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(ref mut writer) = self.file_handle {
            let _ = writer.flush();
        }
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
    fn new_session_creates_file() {
        let dir = temp_dir();
        let session = Session::new(dir.path(), "test-project").unwrap();
        assert!(dir.path().join("session.jsonl").exists());
        assert_eq!(session.messages.len(), 0);
    }

    #[test]
    fn append_and_reload() {
        let dir = temp_dir();
        {
            let mut session = Session::new(dir.path(), "proj-1").unwrap();
            session
                .append_message(Message::user("Hello"))
                .unwrap();
            session
                .append_message(Message::assistant("Hi there!"))
                .unwrap();
            assert_eq!(session.messages.len(), 2);
        }

        // Reload from disk.
        let loaded = Session::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.meta.project_id, "proj-1");
    }

    #[test]
    fn crash_safety_skips_malformed_last_line() {
        let dir = temp_dir();
        {
            let mut session = Session::new(dir.path(), "proj-1").unwrap();
            session.append_message(Message::user("Hello")).unwrap();
        }

        // Simulate a crash by appending a truncated line.
        let file_path = dir.path().join("session.jsonl");
        let mut file = OpenOptions::new().append(true).open(&file_path).unwrap();
        writeln!(file, "{{\"role\":\"assistant\",\"conten").unwrap();

        let loaded = Session::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 1); // Only the valid message.
    }
}
