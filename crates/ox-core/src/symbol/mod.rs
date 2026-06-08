pub mod types;
pub mod language;
pub mod extractor;
pub mod embedding;
pub mod vector_store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use types::{Symbol, SymbolQueryResult};
use extractor::AstExtractor;
use vector_store::VectorStore;
use crate::config::EmbeddingConfig;

/// Code indexer with both keyword and semantic search.
///
/// Walks the project, extracts AST symbols via tree-sitter,
/// stores them in memory for fast keyword search,
/// and uses vector embeddings for semantic search.
pub struct CodeIndexer {
    /// In-memory symbol list (for keyword search)
    symbols: Arc<RwLock<Vec<Symbol>>>,
    /// Vector store (for semantic search) — lazily initialized
    vector_store: Option<VectorStore>,
    /// Multi-language AST extractor
    extractor: AstExtractor,
    /// Root project path
    project_path: PathBuf,
    /// Embedding configuration (stored for deferred VectorStore init)
    embedding_config: EmbeddingConfig,
}

impl CodeIndexer {
    /// Create a new indexer for the given project path.
    /// Vector store is NOT initialized here — call `init_vector_store()` in background.
    pub fn new(project_path: &Path, embedding_config: EmbeddingConfig) -> Self {
        Self {
            symbols: Arc::new(RwLock::new(Vec::new())),
            vector_store: None,
            extractor: AstExtractor::new(),
            project_path: project_path.to_path_buf(),
            embedding_config,
        }
    }

    /// Initialize vector store (call this in a background task to avoid blocking startup).
    /// Downloads the embedding model on first run.
    pub fn init_vector_store(&mut self) {
        if !self.embedding_config.enabled {
            tracing::info!("[INDEXER] Embedding disabled via config — semantic symbol search OFF");
            return;
        }
        if self.vector_store.is_some() {
            tracing::debug!("[INDEXER] Vector store already initialized");
            return;
        }

        let db_path = dirs::home_dir()
            .map(|home| home.join(".ox").join("symbols.tdb"))
            .expect("Cannot determine home directory");

        tracing::info!(
            "[INDEXER] Initializing vector store at {:?} (model={}, dim={})...",
            db_path, self.embedding_config.model_id, self.embedding_config.dimension
        );

        match VectorStore::open(db_path.to_str().unwrap(), &self.embedding_config) {
            Ok(store) => {
                tracing::info!("[INDEXER] ✅ Vector store initialized for semantic symbol search");
                self.vector_store = Some(store);
            }
            Err(e) => {
                tracing::warn!(
                    "[INDEXER] ❌ Failed to initialize vector store: {}. Semantic symbol search disabled.\n\
                     💡 Hint: Check that the embedding model can be downloaded (modelscope/huggingface).",
                    e
                );
            }
        }
    }

    /// Full-project index: walk all files, extract symbols.
    pub async fn index_project(&mut self) -> anyhow::Result<usize> {
        tracing::info!("[INDEXER] Indexing project: {:?}", self.project_path);

        let mut total = 0usize;

        for entry in walkdir::WalkDir::new(&self.project_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let path_str = path.to_string_lossy();
            // Skip common non-source directories (both / and \ separators for cross-platform)
            if path_str.contains("target/") || path_str.contains("target\\")
                || path_str.contains("node_modules/") || path_str.contains("node_modules\\")
                || path_str.contains(".git/") || path_str.contains(".git\\")
                || path_str.contains("dist/") || path_str.contains("dist\\")
                || path_str.contains("build/") || path_str.contains("build\\")
                || path_str.contains("__pycache__/") || path_str.contains("__pycache__\\")
                || path_str.contains(".venv/") || path_str.contains(".venv\\")
                || path_str.contains(".ox/") || path_str.contains(".ox\\")
            {
                continue;
            }

            match self.index_file(path).await {
                Ok(n) => total += n,
                Err(e) => {
                    tracing::debug!("[INDEXER] Skipped {}: {e}", path.display());
                }
            }
        }

        tracing::info!("[INDEXER] Indexing complete. {} symbols indexed.", total);
        Ok(total)
    }

    /// Index a single file: extract symbols and store in memory + vector store.
    pub async fn index_file(&mut self, path: &Path) -> anyhow::Result<usize> {
        let code = std::fs::read_to_string(path)?;
        let symbols = self.extractor.extract_symbols(path, &code)?;
        let count = symbols.len();

        if count == 0 {
            return Ok(0);
        }

        // Update in-memory store (deduplicate by file_path)
        let mut symbols_lock = self.symbols.write().await;
        let path_str = path.to_string_lossy().to_string();
        symbols_lock.retain(|s| s.file_path != path_str);
        symbols_lock.extend(symbols.clone());
        drop(symbols_lock);

        // Batch-insert into vector store (semantic search)
        if let Some(ref mut vs) = self.vector_store {
            if let Err(e) = vs.insert_symbols(&symbols) {
                tracing::debug!("[VECTOR_STORE] Failed to batch insert for {}: {}", path_str, e);
            }
        }

        Ok(count)
    }

    /// Keyword search against in-memory symbol list.
    pub async fn find_by_name(&self, name: &str) -> SymbolQueryResult {
        let lower = name.to_lowercase();
        let mut results = Vec::new();

        let symbols = self.symbols.read().await;
        for sym in symbols.iter() {
            if sym.name.to_lowercase().contains(&lower) {
                results.push(sym.clone());
            }
        }

        let total = results.len();
        results.sort_by(|a, b| {
            let a_exact = a.name.to_lowercase() == lower;
            let b_exact = b.name.to_lowercase() == lower;
            b_exact.cmp(&a_exact)
                .then_with(|| {
                    let a_pref = a.name.to_lowercase().starts_with(&lower);
                    let b_pref = b.name.to_lowercase().starts_with(&lower);
                    b_pref.cmp(&a_pref)
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        SymbolQueryResult { symbols: results, total_count: total, query: name.to_string() }
    }

    /// Unified search: keyword first, then semantic fallback.
    ///
    /// Strategy:
    /// 1. Try keyword match (fast, exact/prefix matching)
    /// 2. If keyword returns results → return them
    /// 3. If keyword is empty AND vector store is available → try semantic search
    /// 4. Return whatever we found
    pub async fn search(&self, query: &str, top_k: usize) -> anyhow::Result<SymbolQueryResult> {
        // Step 1: Keyword search (always fast)
        let kw_result = self.find_by_name(query).await;
        if !kw_result.symbols.is_empty() {
            return Ok(kw_result);
        }

        // Step 2: Semantic search fallback
        if let Some(ref vs) = self.vector_store {
            match vs.search(query, top_k) {
                Ok(semantic_results) if !semantic_results.is_empty() => {
                    let count = semantic_results.len();
                    tracing::debug!("[SEARCH] Found {} semantic results for '{}'", count, query);
                    return Ok(SymbolQueryResult {
                        symbols: semantic_results,
                        total_count: count,
                        query: query.to_string(),
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("[SEARCH] Semantic search failed: {}", e);
                }
            }
        }

        // Nothing found
        Ok(kw_result)
    }

    /// Get total in-memory symbol count.
    pub async fn symbol_count(&self) -> usize {
        self.symbols.read().await.len()
    }

    /// Start file system watcher for real-time incremental indexing.
    ///
    /// Watches the project directory for file create/modify/remove events.
    /// Events are debounced (500ms) and source files are re-indexed automatically.
    /// Non-source directories (target/, node_modules/, .git/, etc.) are excluded.
    pub async fn start_watcher(indexer: Arc<Mutex<Self>>) -> anyhow::Result<()> {
        let project_path = {
            let idx = indexer.lock().await;
            idx.project_path.clone()
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();

        // Create the OS-level file watcher (runs in its own thread)
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = event_tx.send(event);
            }
        })
        .map_err(|e| anyhow::anyhow!("Failed to create file watcher: {}", e))?;

        use notify::{RecursiveMode, Watcher};
        watcher
            .watch(&project_path, RecursiveMode::Recursive)
            .map_err(|e| anyhow::anyhow!("Failed to watch {:?}: {}", project_path, e))?;

        let self_clone = indexer;

        // Background task: receive events, debounce, re-index
        tokio::spawn(async move {
            // Keep watcher alive for the lifetime of this task
            let _watcher = watcher;
            let mut pending: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

            loop {
                match event_rx.recv().await {
                    Some(event) => {
                        // Skip pure access events (open/close without mutation)
                        if event.kind.is_access() {
                            continue;
                        }
                        let is_remove = event.kind.is_remove();
                        for path in event.paths {
                            if path.is_file() || is_remove {
                                pending.insert(path);
                            }
                        }

                        // Debounce: drain any additional events within 500ms
                        let deadline = tokio::time::sleep(Duration::from_millis(500));
                        tokio::pin!(deadline);
                        loop {
                            tokio::select! {
                                biased;
                                Some(ev) = event_rx.recv() => {
                                    if !ev.kind.is_access() {
                                        let rm = ev.kind.is_remove();
                                        for p in ev.paths {
                                            if p.is_file() || rm {
                                                pending.insert(p);
                                            }
                                        }
                                    }
                                }
                                _ = &mut deadline => break,
                            }
                        }

                        // Drain pending set and process each path
                        let paths: Vec<PathBuf> = pending.drain().collect();
                        for path in paths {
                            if Self::should_skip_path(&path) {
                                continue;
                            }

                            if is_remove || !path.exists() {
                                // File removed: clean from in-memory index
                                let idx = self_clone.lock().await;
                                let path_str = path.to_string_lossy().to_string();
                                idx.symbols.write().await.retain(|s| s.file_path != path_str);
                                tracing::debug!("[WATCHER] Removed symbols for {}", path_str);
                            } else {
                                // File created/modified: re-index
                                let mut idx = self_clone.lock().await;
                                match idx.index_file(&path).await {
                                    Ok(n) if n > 0 => {
                                        tracing::debug!("[WATCHER] Re-indexed {} ({} symbols)", path.display(), n);
                                    }
                                    Err(e) => {
                                        tracing::debug!("[WATCHER] Skip {}: {}", path.display(), e);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    None => break, // Channel closed
                }
            }
        });

        tracing::info!("[INDEXER] File watcher started for {:?}", project_path);
        Ok(())
    }

    /// Check if a path should be excluded from watching (non-source directories).
    fn should_skip_path(path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        path_str.contains("target/") || path_str.contains("target\\")
            || path_str.contains("node_modules/") || path_str.contains("node_modules\\")
            || path_str.contains(".git/") || path_str.contains(".git\\")
            || path_str.contains("dist/") || path_str.contains("dist\\")
            || path_str.contains("build/") || path_str.contains("build\\")
            || path_str.contains("__pycache__/") || path_str.contains("__pycache__\\")
            || path_str.contains(".venv/") || path_str.contains(".venv\\")
            || path_str.contains(".ox/") || path_str.contains(".ox\\")
    }
}
