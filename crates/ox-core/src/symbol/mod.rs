pub mod embedding;
pub mod extractor;
pub mod language;
pub mod types;
pub mod vector_store;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use crate::config::EmbeddingConfig;
use extractor::AstExtractor;
use language::SyntaxError;
use types::{CallGraphResult, Symbol, SymbolQueryResult};
use vector_store::VectorStore;

/// Cached file metadata for incremental indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileCacheEntry {
    /// File modification time in seconds since UNIX epoch
    modified_secs: i64,
    /// Symbols extracted from this file
    symbols: Vec<Symbol>,
}

/// Persistent symbol cache for fast startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SymbolCache {
    /// Project path this cache was built for
    project_path: String,
    /// Per-file cache entries
    files: HashMap<String, FileCacheEntry>,
}

/// Code indexer with both keyword and semantic search.
///
/// Walks the project, extracts AST symbols via tree-sitter,
/// stores them in memory for fast keyword search,
/// and uses vector embeddings for semantic search.
pub struct CodeIndexer {
    /// In-memory symbol list (for keyword search)
    symbols: Arc<RwLock<Vec<Symbol>>>,
    /// Vector store (for semantic search) — independently locked, lazily initialized
    vector_store: Arc<Mutex<Option<VectorStore>>>,
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
            vector_store: Arc::new(Mutex::new(None)),
            extractor: AstExtractor::new(),
            project_path: project_path.to_path_buf(),
            embedding_config,
        }
    }

    /// Initialize vector store (async, independently locked from CodeIndexer).
    pub async fn init_vector_store(&self) {
        if !self.embedding_config.enabled {
            tracing::info!("[INDEXER] Embedding disabled via config — semantic symbol search OFF");
            return;
        }
        {
            let guard = self.vector_store.lock().await;
            if guard.is_some() {
                tracing::debug!("[INDEXER] Vector store already initialized");
                return;
            }
        }

        let db_path = dirs::home_dir()
            .map(|home| home.join(".ox").join("symbols.tdb"))
            .unwrap_or_else(|| {
                tracing::warn!("Cannot determine home directory, using temp dir");
                std::env::temp_dir().join(".ox").join("symbols.tdb")
            });

        tracing::info!(
            "[INDEXER] Initializing vector store at {:?} (model={}, dim={})...",
            db_path,
            self.embedding_config.model_id,
            self.embedding_config.dimension
        );

        match VectorStore::open(
            db_path.to_str().unwrap_or("symbols.tdb"),
            &self.embedding_config,
        ) {
            Ok(store) => {
                let mut guard = self.vector_store.lock().await;
                *guard = Some(store);
                tracing::info!("[INDEXER] ✅ Vector store initialized for semantic symbol search");
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

    /// Get a clone of the vector_store Arc (for background tasks that need independent access).
    pub fn get_vector_store(&self) -> Arc<Mutex<Option<VectorStore>>> {
        Arc::clone(&self.vector_store)
    }

    /// Get a clone of the symbols Arc (for background tasks that need independent access).
    pub fn get_symbols(&self) -> Arc<RwLock<Vec<Symbol>>> {
        Arc::clone(&self.symbols)
    }

    /// Get the cache file path for this project.
    fn cache_path(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        // Hash the full project path to avoid conflicts between projects
        let path_hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.project_path.to_string_lossy().hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };
        home.join(".ox")
            .join("cache")
            .join(format!("symbols_{}.json", path_hash))
    }

    /// Load symbol cache from disk.
    fn load_cache(&self) -> Option<SymbolCache> {
        let path = self.cache_path();
        if !path.exists() {
            return None;
        }
        match std::fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<SymbolCache>(&data) {
                Ok(cache) if cache.project_path == self.project_path.to_string_lossy() => {
                    tracing::info!(
                        "[INDEXER] Loaded symbol cache: {} files from {:?}",
                        cache.files.len(),
                        path
                    );
                    Some(cache)
                }
                Ok(_) => {
                    tracing::debug!("[INDEXER] Cache project path mismatch, will re-index");
                    None
                }
                Err(e) => {
                    tracing::debug!("[INDEXER] Failed to parse symbol cache: {}", e);
                    None
                }
            },
            Err(e) => {
                tracing::debug!("[INDEXER] Failed to read symbol cache: {}", e);
                None
            }
        }
    }

    /// Save symbol cache to disk.
    fn save_cache(&self, files: &HashMap<String, FileCacheEntry>) {
        let cache = SymbolCache {
            project_path: self.project_path.to_string_lossy().to_string(),
            files: files.clone(),
        };
        let path = self.cache_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(&cache) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&path, data) {
                    tracing::warn!("[INDEXER] Failed to write symbol cache: {}", e);
                } else {
                    tracing::info!(
                        "[INDEXER] Saved symbol cache: {} files to {:?}",
                        files.len(),
                        path
                    );
                }
            }
            Err(e) => {
                tracing::warn!("[INDEXER] Failed to serialize symbol cache: {}", e);
            }
        }
    }

    /// Get file modification time in seconds since UNIX epoch.
    fn file_modified_secs(path: &Path) -> Option<i64> {
        std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            })
    }

    /// Full-project index with incremental caching.
    /// Only re-indexes files that have been modified since last run.
    /// Accepts an optional progress callback: (files_done, total_files, symbols_indexed).
    pub async fn index_project(
        &mut self,
        progress: Option<tokio::sync::mpsc::UnboundedSender<(usize, usize, usize)>>,
    ) -> anyhow::Result<usize> {
        tracing::info!("[INDEXER] Indexing project: {:?}", self.project_path);

        // Load cache for incremental indexing
        let cache = self.load_cache();
        let mut file_cache: HashMap<String, FileCacheEntry> =
            cache.map(|c| c.files).unwrap_or_default();

        // Pre-scan source files
        let source_files: Vec<PathBuf> = walkdir::WalkDir::new(&self.project_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                let p = e.path().to_string_lossy();
                !(p.contains("target/")
                    || p.contains("target\\")
                    || p.contains("node_modules/")
                    || p.contains("node_modules\\")
                    || p.contains(".git/")
                    || p.contains(".git\\")
                    || p.contains("dist/")
                    || p.contains("dist\\")
                    || p.contains("build/")
                    || p.contains("build\\")
                    || p.contains("__pycache__/")
                    || p.contains("__pycache__\\")
                    || p.contains(".venv/")
                    || p.contains(".venv\\")
                    || p.contains(".ox/")
                    || p.contains(".ox\\"))
            })
            .map(|e| e.path().to_path_buf())
            .collect();

        let total_files = source_files.len();

        // Determine which files need re-indexing
        let mut files_to_index: Vec<PathBuf> = Vec::new();
        let mut cached_symbols: Vec<Symbol> = Vec::new();
        let mut stale_keys: Vec<String> = Vec::new();

        // Collect current file paths for stale detection
        let current_paths: std::collections::HashSet<String> = source_files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        // Remove deleted files from cache
        for key in file_cache.keys() {
            if !current_paths.contains(key) {
                stale_keys.push(key.clone());
            }
        }
        for key in &stale_keys {
            file_cache.remove(key);
        }

        for path in &source_files {
            let path_str = path.to_string_lossy().to_string();
            let current_mtime = Self::file_modified_secs(path).unwrap_or(0);

            if let Some(entry) = file_cache.get(&path_str)
                && entry.modified_secs == current_mtime && !entry.symbols.is_empty() {
                    // File unchanged — reuse cached symbols
                    cached_symbols.extend(entry.symbols.clone());
                    continue;
                }
            // File is new or modified — needs re-indexing
            files_to_index.push(path.clone());
        }

        let changed_count = files_to_index.len();
        let unchanged_count = total_files - changed_count;
        tracing::info!(
            "[INDEXER] {} files total: {} unchanged (from cache), {} to re-index",
            total_files,
            unchanged_count,
            changed_count
        );

        // Send initial count
        if let Some(ref tx) = progress {
            let _ = tx.send((0, total_files, cached_symbols.len()));
        }

        // Load cached symbols into memory immediately (fast!)
        if !cached_symbols.is_empty() {
            let mut symbols_lock = self.symbols.write().await;
            symbols_lock.extend(cached_symbols.clone());
        }

        // Index files: AST sync + memory update, embedding deferred
        const BATCH_FILES: usize = 50; // Process 50 files at once
        let mut all_new_symbols: Vec<Symbol> = Vec::new();
        let mut file_cache_updates: HashMap<String, (i64, Vec<Symbol>)> = HashMap::new();

        for batch_start in (0..files_to_index.len()).step_by(BATCH_FILES) {
            let batch_end = (batch_start + BATCH_FILES).min(files_to_index.len());
            let batch = &files_to_index[batch_start..batch_end];

            // Step 1: Extract AST (FAST)
            let mut batch_symbols: Vec<Symbol> = Vec::new();
            for path in batch {
                let path_str = path.to_string_lossy().to_string();
                let current_mtime = Self::file_modified_secs(path).unwrap_or(0);

                match self.index_file_ast_only(path).await {
                    Ok(symbols) => {
                        batch_symbols.extend(symbols.clone());
                        file_cache_updates.insert(path_str, (current_mtime, symbols));
                    }
                    Err(e) => {
                        tracing::debug!("[INDEXER] Skipped {}: {e}", path.display());
                    }
                }
            }

            // Step 2: IMMEDIATELY update memory (agent can search NOW!)
            if !batch_symbols.is_empty() {
                let mut symbols_lock = self.symbols.write().await;
                symbols_lock.extend(batch_symbols.clone());
                drop(symbols_lock);
                all_new_symbols.extend(batch_symbols);
            }

            // Step 3: Report progress
            if let Some(ref tx) = progress {
                let files_done = unchanged_count + batch_end;
                let total_sym = cached_symbols.len() + all_new_symbols.len();
                let _ = tx.send((files_done, total_files, total_sym));
            }
        }

        // Apply cache updates
        for (path_str, (mtime, symbols)) in file_cache_updates {
            file_cache.insert(
                path_str,
                FileCacheEntry {
                    modified_secs: mtime,
                    symbols,
                },
            );
        }

        // Save updated cache
        self.save_cache(&file_cache);

        let total = cached_symbols.len() + all_new_symbols.len();
        let new_symbols = all_new_symbols.len();
        if changed_count == 0 {
            tracing::info!(
                "[INDEXER] ✅ All {} files cached. {} symbols loaded instantly.",
                total_files,
                total
            );
        } else {
            tracing::info!(
                "[INDEXER] Indexing complete. {} symbols indexed ({} from cache, {} re-indexed).",
                total,
                cached_symbols.len(),
                new_symbols
            );
        }

        // Embedding is now deferred to background task
        // Return immediately so agent can start searching!
        if !all_new_symbols.is_empty() {
            tracing::info!(
                "[INDEXER] {} symbols ready for keyword search. Embedding will complete in background.",
                new_symbols
            );
        }

        Ok(total)
    }

    /// Index a single file: extract symbols and store in memory + vector store.
    /// Uses two-phase approach: Phase1 = insert all symbols, Phase2 = link calls via FQ names.
    pub async fn index_file(&mut self, path: &Path) -> anyhow::Result<usize> {
        let code = std::fs::read_to_string(path)?;
        let symbols = self.extractor.extract_symbols(path, &code)?;
        let count = symbols.len();

        if count == 0 {
            return Ok(0);
        }

        // === PHASE 1: Insert all symbols first (to build lookup table) ===
        {
            let mut symbols_lock = self.symbols.write().await;
            let path_str = path.to_string_lossy().to_string();
            symbols_lock.retain(|s| s.file_path != path_str);
            symbols_lock.extend(symbols.clone());
        }

        // === PHASE 2: Resolve calls to FQ names ===
        // Resolve each call: if it matches another symbol's fq_name or name, resolve to fq_name
        {
            let mut symbols_lock = self.symbols.write().await;
            // First pass: build a lookup map from name/fq_name -> fq_name
            let lookup: std::collections::HashMap<String, String> = symbols_lock
                .iter()
                .flat_map(|s| {
                    let mut m = std::collections::HashMap::new();
                    m.insert(s.name.clone(), s.fq_name.clone());
                    if !s.fq_name.is_empty() {
                        m.insert(s.fq_name.clone(), s.fq_name.clone());
                    }
                    m
                })
                .collect();
            // Second pass: resolve calls
            for sym in symbols_lock.iter_mut() {
                if !sym.calls.is_empty() {
                    let resolved: Vec<String> = sym
                        .calls
                        .iter()
                        .filter_map(|call| lookup.get(call).cloned())
                        .collect();
                    sym.calls = resolved;
                }
            }
        }

        // Batch-insert into vector store (semantic search)
        {
            let mut vs_guard = self.vector_store.lock().await;
            if let Some(ref mut vs) = *vs_guard
                && let Err(e) = vs.insert_symbols(&symbols) {
                    tracing::debug!(
                        "[VECTOR_STORE] Failed to batch insert for {}: {}",
                        path.display(),
                        e
                    );
                }
        }

        Ok(count)
    }

    /// Embed all in-memory symbols to vector store (for background/async embedding).
    /// Only locks vector_store independently — does NOT block CodeIndexer.
    pub async fn embed_all_symbols(&self) {
        let all_symbols: Vec<Symbol> = {
            let lock = self.symbols.read().await;
            lock.clone()
        };

        if all_symbols.is_empty() {
            return;
        }

        tracing::info!(
            "[VECTOR_STORE] Background embedding {} symbols...",
            all_symbols.len()
        );
        let mut vs_guard = self.vector_store.lock().await;
        if let Some(ref mut vs) = *vs_guard {
            if let Err(e) = vs.insert_symbols_batch(&all_symbols) {
                tracing::warn!("[VECTOR_STORE] Background batch embed failed: {}", e);
            } else {
                tracing::info!("[VECTOR_STORE] Background embedding complete.");
            }
        }
    }

    /// Extract AST symbols only (no embedding). Used for batch embedding optimization.
    pub async fn index_file_ast_only(&mut self, path: &Path) -> anyhow::Result<Vec<Symbol>> {
        let code = std::fs::read_to_string(path)?;
        let symbols = self.extractor.extract_symbols(path, &code)?;
        Ok(symbols)
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
            b_exact
                .cmp(&a_exact)
                .then_with(|| {
                    let a_pref = a.name.to_lowercase().starts_with(&lower);
                    let b_pref = b.name.to_lowercase().starts_with(&lower);
                    b_pref.cmp(&a_pref)
                })
                .then_with(|| a.name.cmp(&b.name))
        });

        SymbolQueryResult {
            symbols: results,
            total_count: total,
            query: name.to_string(),
        }
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
        let vs_guard = self.vector_store.lock().await;
        if let Some(ref vs) = *vs_guard {
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

    /// Find all symbols that call the given function name (callers).
    /// Uses FQ names for accurate matching. Expands 1 level: finds functions that call the target,
    /// and also what those callers call (2nd level).
    pub async fn find_callers(&self, name: &str) -> Vec<CallGraphResult> {
        let symbols = self.symbols.read().await;

        // Build lookup: fq_name -> Symbol
        let fq_lookup: std::collections::HashMap<String, &Symbol> =
            symbols.iter().map(|s| (s.fq_name.clone(), s)).collect();

        // Resolve input name to FQ name
        let target_fq = {
            let mut found: Option<String> = None;
            // Try exact FQ match
            for fq in fq_lookup.keys() {
                if fq.to_lowercase() == name.to_lowercase() {
                    found = Some(fq.clone());
                    break;
                }
            }
            // Try simple name match
            if found.is_none() {
                for fq in fq_lookup.keys() {
                    if fq.ends_with(&format!("::{}", name)) || fq == name {
                        found = Some(fq.clone());
                        break;
                    }
                }
            }
            found.unwrap_or_else(|| name.to_string())
        };

        let target_lower = target_fq.to_lowercase();

        // First pass: find all functions that directly call `target_fq`
        let mut results = Vec::new();

        for sym in symbols.iter() {
            if !sym.calls.is_empty() {
                for call in &sym.calls {
                    if call.to_lowercase() == target_lower {
                        // This symbol calls the target
                        results.push(CallGraphResult {
                            name: sym.name.clone(),
                            fq_name: sym.fq_name.clone(),
                            file_path: sym.file_path.clone(),
                            start_line: sym.start_line,
                            end_line: sym.end_line,
                            kind: sym.kind.clone(),
                            relation: "calls".to_string(),
                        });
                        break;
                    }
                }
            }
        }

        // Second pass: expand 1 level - find what these callers call
        let caller_fqs: std::collections::HashSet<String> =
            results.iter().map(|r| r.fq_name.to_lowercase()).collect();

        for sym in symbols.iter() {
            if !sym.calls.is_empty() {
                for call in &sym.calls {
                    if caller_fqs.contains(&call.to_lowercase()) {
                        results.push(CallGraphResult {
                            name: sym.name.clone(),
                            fq_name: sym.fq_name.clone(),
                            file_path: sym.file_path.clone(),
                            start_line: sym.start_line,
                            end_line: sym.end_line,
                            kind: sym.kind.clone(),
                            relation: "calls_calls".to_string(),
                        });
                        break;
                    }
                }
            }
        }

        results
    }

    /// Check a file for syntax errors without full indexing.
    /// Only checks source code files (.rs, .py, .js, .ts, .go, .java, .cpp, etc.)
    /// Skips non-source files (.md, .txt, .toml, .json, .yaml, .html, .css, etc.)
    /// Returns None if the file is valid or unsupported, Some(errors) if syntax issues found.
    pub fn check_syntax(&mut self, path: &Path, code: &str) -> Option<Vec<SyntaxError>> {
        // Only check files with recognized source language extensions
        let lang_name = self.extractor.detect_language(path)?.to_string();
        match self.extractor.check_syntax(code, &lang_name) {
            Ok(errors) if errors.is_empty() => None,
            Ok(errors) => Some(errors),
            Err(e) => {
                tracing::debug!("[SYNTAX_CHECK] Parse failed for {}: {}", path.display(), e);
                None
            }
        }
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
        let mut watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
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
                                idx.symbols
                                    .write()
                                    .await
                                    .retain(|s| s.file_path != path_str);
                                tracing::debug!("[WATCHER] Removed symbols for {}", path_str);
                            } else {
                                // File created/modified: re-index
                                let mut idx = self_clone.lock().await;
                                match idx.index_file(&path).await {
                                    Ok(n) if n > 0 => {
                                        tracing::debug!(
                                            "[WATCHER] Re-indexed {} ({} symbols)",
                                            path.display(),
                                            n
                                        );
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
        path_str.contains("target/")
            || path_str.contains("target\\")
            || path_str.contains("node_modules/")
            || path_str.contains("node_modules\\")
            || path_str.contains(".git/")
            || path_str.contains(".git\\")
            || path_str.contains("dist/")
            || path_str.contains("dist\\")
            || path_str.contains("build/")
            || path_str.contains("build\\")
            || path_str.contains("__pycache__/")
            || path_str.contains("__pycache__\\")
            || path_str.contains(".venv/")
            || path_str.contains(".venv\\")
            || path_str.contains(".ox/")
            || path_str.contains(".ox\\")
    }
}
