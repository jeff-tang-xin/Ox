/// `knowledge` — Unified Knowledge Context Engine.
///
/// # Architecture
///
/// This module consolidates what was previously scattered across `memory/`,
/// `symbol/`, and `context/` into a single knowledge engine with:
///
/// - **Unified Entity model** (`entity.rs`): Four memory layers (L0-L3) +
///   code symbols/files/modules all stored as `Entity`.
/// - **Single TriviumDB** (`vector_store.rs`): Replaces two separate instances,
///   shared `Arc<EmbeddingModel>`. `expand_depth=2` for graph-aware search.
/// - **AST extraction** (`extractor.rs`): tree-sitter multi-language parsing
///   producing `Entity::CodeSymbol`.
/// - **Language detection** (`language.rs`): File extension → tree-sitter grammar.
/// - **Embedding** (`embedding.rs`): Re-exports `symbol::embedding::EmbeddingModel`
///   with `Arc`-sharing helper.
///
/// # Four-Layer Memory Model
/// - **L0 Working Memory**: Per-session temporary context and self-state.
/// - **L1 Atomic Memory**: Indivisible facts, preferences, tool results.
/// - **L2 Episodic Memory**: Timeline-organized events, task checkpoints.
/// - **L3 Semantic Memory**: Abstracted patterns, architectural principles.
///
/// # Retrieval Strategy (per design doc §5.2)
/// "深度优先 + Token 预算": inject L0 first (always), then L1/L2 by semantic
/// similarity, then L3 only if highly relevant, until token budget exhausted.

pub mod entity;
pub mod embedding;
pub mod extractor;
pub mod graph;
pub mod language;
pub mod layering;
pub mod live_update;
pub mod retrieval;
pub mod vector_store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::Result;

use entity::{Entity, EntityKind, injection_priority};
use vector_store::{UnifiedVectorStore, SearchHit};
use std::sync::Mutex as StdMutex;
use extractor::AstExtractor;
use crate::config::EmbeddingConfig;

/// Top-level coordinator for the unified knowledge engine.
///
/// Owns the TriviumDB vector store, AST extractor, and language registry.
/// Provides high-level APIs for indexing code, storing memories, and
/// executing the pre-turn retrieval pipeline.
pub struct KnowledgeEngine {
    /// Unified vector store (single `knowledge.tdb`)
    store: UnifiedVectorStore,
    /// AST symbol extractor (tree-sitter, 7 languages)
    extractor: StdMutex<AstExtractor>,
    /// Project root path
    project_path: PathBuf,
    /// Embedding dimension
    dimension: usize,
    /// Recent L0 WorkingMemory ring buffer (last N turns for conversation continuity)
    recent_turns: std::collections::VecDeque<Entity>,
    /// Max recent turns to keep
    max_recent_turns: usize,
}

impl KnowledgeEngine {
    /// Create a new KnowledgeEngine.
    ///
    /// # Arguments
    /// * `db_path` - Path to `knowledge.tdb`
    /// * `embedding_model` - Shared embedding model (Arc<EmbeddingModel>)
    /// * `embedding_config` - Embedding configuration
    /// * `project_path` - Root path of the project being indexed
    pub fn new(
        db_path: &str,
        embedding_model: Arc<crate::symbol::embedding::EmbeddingModel>,
        embedding_config: &EmbeddingConfig,
        project_path: &Path,
    ) -> Result<Self> {
        let store = UnifiedVectorStore::open(db_path, embedding_model, embedding_config)?;
        let extractor = AstExtractor::new();
        let dimension = embedding_config.dimension;

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Initialized with db={db_path}, dim={dimension}, project={}",
            project_path.display()
        );

        Ok(Self {
            store,
            extractor: StdMutex::new(extractor),
            project_path: project_path.to_path_buf(),
            dimension,
            recent_turns: std::collections::VecDeque::new(),
            max_recent_turns: 20,
        })
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Working Memory (L0) ring buffer
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Get the most recent N WorkingMemory entities (L0 conversation continuity).
    /// Always returns in chronological order (oldest first).
    /// This is O(1) — no TriviumDB search needed.
    pub fn get_recent_turns(&self, count: usize) -> Vec<Entity> {
        let skip = self.recent_turns.len().saturating_sub(count);
        self.recent_turns.iter().skip(skip).cloned().collect()
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Indexing
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Extract AST symbols from a single file WITHOUT embedding/storing.
    /// Returns entities ready for batch embedding. Fast (no BERT inference).
    pub fn extract_file_symbols(&self, file_path: &Path) -> Result<Vec<Entity>> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self.extractor.lock().unwrap().extract_entities(file_path, &code)?;
        Ok(entities)
    }

    /// Phase 1: Walk project, extract ALL symbols (AST only, no embedding).
    /// Fast — no BERT inference. **Uses mtime-based file cache to skip unchanged files.**
    /// Only walks within `self.project_path` (the user's working directory).
    /// Reports progress via optional channel.
    /// Returns (all_entities, total_files) for Phase 2 batch embedding.
    pub fn collect_all_symbols(
        &self,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<(usize, usize, usize)>>,
    ) -> Result<(Vec<Entity>, usize)> {
        use std::collections::HashMap;
        use std::io::Read;

        // Load file mtime cache (~/.ox/cache/ast_{hash}.json)
        let cache_path = self.cache_path();
        let file_cache: HashMap<String, i64> = if let Ok(mut f) = std::fs::File::open(&cache_path) {
            let mut data = String::new();
            f.read_to_string(&mut data).ok();
            serde_json::from_str::<HashMap<String, i64>>(&data).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let walker = ignore::WalkBuilder::new(&self.project_path)
            .standard_filters(true)
            .hidden(false)
            .build();

        let all_files: Vec<PathBuf> = walker
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |ft| ft.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let total_files = all_files.len();
        let mut all_entities = Vec::new();
        let mut total_symbols = 0;
        let mut new_cache: HashMap<String, i64> = HashMap::new();
        let mut skipped_from_cache = 0;

        for (i, path) in all_files.iter().enumerate() {
            let path_str = path.to_string_lossy().to_string();

            // Check mtime — if unchanged, skip AST parse entirely
            if let Ok(meta) = std::fs::metadata(path) {
                let mtime = meta.modified().ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                new_cache.insert(path_str.clone(), mtime);

                if let Some(&cached_mtime) = file_cache.get(&path_str) {
                    if cached_mtime == mtime {
                        skipped_from_cache += 1;
                        continue; // Skip — file unchanged, entities already in TriviumDB
                    }
                }
            }

            // Parse and extract
            match self.extract_file_symbols(path) {
                Ok(entities) => {
                    total_symbols += entities.len();
                    all_entities.extend(entities);
                }
                Err(e) => {
                    tracing::debug!("[KNOWLEDGE_ENGINE] Skipping {}: {e}", path.display());
                }
            }

            if let Some(ref tx) = progress_tx {
                let _ = tx.send((i + 1, total_files, total_symbols));
            }
        }

        // Save updated cache
        if let Ok(data) = serde_json::to_string(&new_cache) {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&cache_path, data);
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Phase 1: {} symbols from {} files ({} skipped from cache, {} parsed)",
            total_symbols, total_files, skipped_from_cache, total_files - skipped_from_cache
        );

        Ok((all_entities, total_files))
    }

    fn cache_path(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.project_path.to_string_lossy().hash(&mut hasher);
            hasher.finish()
        };
        home.join(".ox").join("cache").join(format!("ast_{:016x}.json", hash))
    }

    /// Phase 2: Batch-embed and store pre-extracted entities.
    /// Slow — runs BERT inference on all entities in chunks of 100.
    /// Call this AFTER collect_all_symbols(), ideally in a separate spawn.
    pub fn embed_and_store(&mut self, entities: &[Entity]) -> Result<usize> {
        if entities.is_empty() {
            return Ok(0);
        }
        let total = self._embed_chunk(entities, 0, entities.len())?;
        tracing::info!("[KNOWLEDGE_ENGINE] Embedding complete: {} entities", total);
        Ok(total)
    }

    /// Phase 2 chunked: embed & store entities in chunks of `chunk_size`.
    /// Returns (entity_count_done, total_entities). Caller should loop until done.
    /// Each call does 1 chunk, then returns so caller can yield.
    pub fn embed_and_store_chunk(
        &mut self,
        entities: &[Entity],
        offset: usize,
        chunk_size: usize,
    ) -> Result<usize> {
        // On first chunk, remove old vectors
        if offset == 0 {
            use std::collections::HashSet;
            let mut seen = HashSet::new();
            for e in entities {
                if let Some(fp) = e.file_path() {
                    if seen.insert(fp.to_string()) {
                        self.store.remove_by_file(fp);
                    }
                }
            }
        }
        self._embed_chunk(entities, offset, chunk_size)
    }

    fn _embed_chunk(&mut self, entities: &[Entity], offset: usize, count: usize) -> Result<usize> {
        let end = (offset + count).min(entities.len());
        let chunk = &entities[offset..end];
        if chunk.is_empty() {
            return Ok(0);
        }
        self.store.insert_entities_batch(chunk)
    }

    /// Collect all eligible source file paths in the project directory.
    /// Respects .gitignore via the `ignore` crate (same rules as ripgrep).
    /// Skips hidden dirs, target/, node_modules/, etc.
    pub fn collect_source_files(&self) -> Vec<PathBuf> {
        let walker = ignore::WalkBuilder::new(&self.project_path)
            .standard_filters(true)   // .gitignore, .ignore, hidden files
            .hidden(false)
            .build();

        walker
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |ft| ft.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    /// Index a single source file: extract AST symbols → store as CodeSymbol entities.
    /// Use this for incremental (live) indexing. For startup, use collect_all_symbols() + embed_and_store().
    pub fn index_file(&mut self, file_path: &Path) -> Result<usize> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self.extractor.lock().unwrap().extract_entities(file_path, &code)?;

        if entities.is_empty() {
            return Ok(0);
        }

        let file_path_str = file_path.to_string_lossy().to_string();
        self.store.remove_by_file(&file_path_str);

        let count = self.store.insert_entities_batch(&entities)?;

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Indexed {} → {} symbols",
            file_path.display(), count
        );

        Ok(count)
    }

    /// Full project index: walk the project directory and index all source files.
    /// For startup, prefer collect_all_symbols() + embed_and_store() for better responsiveness.
    pub fn index_project(&mut self) -> Result<usize> {
        self.index_project_with_progress(None)
    }

    /// Full project index with progress reporting (single-pass, includes embedding).
    /// Prefer collect_all_symbols() + embed_and_store() for startup scenarios.
    /// Respects .gitignore via `ignore` crate; only walks within `self.project_path`.
    pub fn index_project_with_progress(
        &mut self,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<(usize, usize, usize)>>,
    ) -> Result<usize> {
        // Pre-count eligible files (single walk, .gitignore-aware)
        let walker = ignore::WalkBuilder::new(&self.project_path)
            .standard_filters(true)
            .hidden(false)
            .build();

        // Collect files first so we can report total
        let all_files: Vec<PathBuf> = walker
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |ft| ft.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let total_files = all_files.len();
        let mut total_symbols = 0;

        for (i, path) in all_files.iter().enumerate() {
            match self.index_file(path) {
                Ok(count) => {
                    total_symbols += count;
                    if let Some(ref tx) = progress_tx {
                        let _ = tx.send((i + 1, total_files, total_symbols));
                    }
                }
                Err(e) => {
                    tracing::debug!("[KNOWLEDGE_ENGINE] Skipping {}: {e}", path.display());
                }
            }
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Project index complete: {} symbols across {} files",
            total_symbols, total_files
        );

        Ok(total_symbols)
    }

    /// Re-index a file after it was modified (live update).
    /// Returns the new entities so callers can update the EntityGraph.
    pub fn reindex_file(&mut self, file_path: &Path) -> Result<Vec<Entity>> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self.extractor.lock().unwrap().extract_entities(file_path, &code)?;

        let file_path_str = file_path.to_string_lossy().to_string();
        self.store.remove_by_file(&file_path_str);

        if !entities.is_empty() {
            self.store.insert_entities_batch(&entities)?;
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Re-indexed {} → {} symbols",
            file_path.display(), entities.len()
        );

        Ok(entities)
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Memory Storage
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Store an entity (memory or code) into the knowledge base.
    pub fn store_entity(&mut self, entity: &Entity) -> Result<u64> {
        self.store.insert_entity(entity)
    }

    /// Store a batch of entities.
    pub fn store_entities(&mut self, entities: &[Entity]) -> Result<usize> {
        self.store.insert_entities_batch(entities)
    }

    /// Create and store an L0 WorkingMemory entity for the current turn.
    pub fn record_turn(
        &mut self,
        session_id: &str,
        action: &str,
        intent: Option<&str>,
        result: Option<&str>,
        tools_used: Vec<String>,
        has_code_changes: bool,
    ) -> Result<Entity> {
        let entity = Entity::working_memory(
            session_id, action, intent, result, tools_used, has_code_changes,
        );
        self.store.insert_entity(&entity)?;
        // Push into ring buffer for fast conversation continuity retrieval
        self.recent_turns.push_back(entity.clone());
        if self.recent_turns.len() > self.max_recent_turns {
            self.recent_turns.pop_front();
        }
        Ok(entity)
    }

    /// Lightweight push: only into ring buffer (no TriviumDB write).
    /// For cross-step state when we can't afford TriviumDB overhead.
    pub fn push_turn_buffer(&mut self, entity: Entity) {
        self.recent_turns.push_back(entity);
        if self.recent_turns.len() > self.max_recent_turns {
            self.recent_turns.pop_front();
        }
    }

    /// Create and store an L1 AtomicMemory.
    pub fn record_atomic_fact(
        &mut self,
        content: &str,
        memory_type: &str,
        project_id: Option<&str>,
        language: &str,
        source: &str,
    ) -> Result<Entity> {
        let entity = Entity::atomic_memory(content, memory_type, project_id, language, source);
        self.store.insert_entity(&entity)?;
        Ok(entity)
    }

    /// Create and store an L2 EpisodicMemory (checkpoint / wrap-up).
    pub fn record_episode(
        &mut self,
        episode_name: &str,
        session_id: &str,
        project_id: Option<&str>,
        task_description: &str,
    ) -> Result<Entity> {
        let entity = Entity::episodic_memory(episode_name, session_id, project_id, task_description);
        self.store.insert_entity(&entity)?;
        Ok(entity)
    }

    /// Create and store an L3 SemanticMemory (abstraction).
    pub fn record_semantic(
        &mut self,
        project_id: &str,
        content: &str,
        domain: &str,
        source_episodes: Vec<String>,
    ) -> Result<Entity> {
        let entity = Entity::semantic_memory(project_id, content, domain, source_episodes);
        self.store.insert_entity(&entity)?;
        Ok(entity)
    }

    /// Start a file-system watcher for the project directory.
    /// When source files change, automatically re-indexes them via `reindex_file()`.
    /// Runs in a background tokio task — call once at startup.
    pub fn start_file_watcher(engine: Arc<tokio::sync::RwLock<Self>>) {
        let project_path = {
            let eng = engine.try_read().unwrap_or_else(|_| panic!("KnowledgeEngine lock held during watcher init"));
            eng.project_path.clone()
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();

        let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = event_tx.send(event);
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("[KNOWLEDGE_ENGINE] Failed to create file watcher: {e}");
                return;
            }
        };

        use notify::{RecursiveMode, Watcher};
        if let Err(e) = watcher.watch(&project_path, RecursiveMode::Recursive) {
            tracing::warn!("[KNOWLEDGE_ENGINE] Failed to watch {:?}: {e}", project_path);
            return;
        }

        let engine_clone = engine;

        tokio::spawn(async move {
            let _watcher = watcher; // keep alive
            let mut pending: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

            loop {
                match event_rx.recv().await {
                    Some(event) => {
                        if event.kind.is_access() { continue; }
                        let is_remove = event.kind.is_remove();
                        for path in event.paths {
                            if path.is_file() || is_remove {
                                pending.insert(path);
                            }
                        }

                        // Debounce 500ms
                        let deadline = tokio::time::sleep(std::time::Duration::from_millis(500));
                        tokio::pin!(deadline);
                        loop {
                            tokio::select! {
                                biased;
                                Some(ev) = event_rx.recv() => {
                                    if !ev.kind.is_access() {
                                        let rm = ev.kind.is_remove();
                                        for p in ev.paths {
                                            if p.is_file() || rm { pending.insert(p); }
                                        }
                                    }
                                }
                                _ = &mut deadline => break,
                            }
                        }

                        let paths: Vec<PathBuf> = pending.drain().collect();
                        for path in paths {
                            let path_str = path.to_string_lossy().to_string();
                            // Skip non-source files and build dirs
                            if path_str.contains("/.git/") || path_str.contains("\\/.git\\")
                                || path_str.contains("/target/") || path_str.contains("\\target\\")
                                || path_str.contains("/node_modules/") || path_str.contains("\\node_modules\\")
                                || path_str.contains("/.ox/") || path_str.contains("\\.ox\\")
                            { continue; }

                            if is_remove || !path.exists() {
                                let mut eng = engine_clone.write().await;
                                eng.store.remove_by_file(&path_str);
                                tracing::debug!("[WATCHER] Removed symbols for {}", path_str);
                            } else if path.is_file() {
                                let mut eng = engine_clone.write().await;
                                let _ = eng.index_file(&path);
                                tracing::debug!("[WATCHER] Re-indexed {}", path_str);
                            }
                        }
                    }
                    None => break,
                }
            }
        });

        tracing::info!("[KNOWLEDGE_ENGINE] File watcher started for {:?}", project_path);
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Pre-turn Retrieval (per design doc §5.2: 深度优先 + Token预算)
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Execute the full pre-turn retrieval pipeline.
    ///
    /// The pipeline (per design doc):
    /// 1. L0 budget (highest priority): inject current WorkingMemory and recent actions
    /// 2. L1/L2 budget: inject AtomicMemory + EpisodicMemory by semantic similarity
    /// 3. L3 budget: inject SemanticMemory only if highly relevant (score ≥ 0.5)
    ///
    /// Returns entities sorted by injection priority.
    pub fn retrieve_for_context(
        &self,
        query: &str,
        session_id: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>> {
        // Search across all memory layers + code symbols
        let mut hits = self.store.search_unified(
            query,
            max_results * 2, // Over-fetch for filtering
            None,            // All kinds
            0.2,             // Min score
        )?;

        // Sort by injection priority (lower = more important for context)
        hits.sort_by(|a, b| {
            let pa = injection_priority(a.entity.kind);
            let pb = injection_priority(b.entity.kind);
            pa.cmp(&pb)
                .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
        });

        // Ensure L3 is only included if highly relevant
        hits.retain(|h| {
            if h.entity.kind == EntityKind::SemanticMemory {
                h.score >= 0.5
            } else {
                true
            }
        });

        // Truncate to max_results
        hits.truncate(max_results);

        tracing::debug!(
            "[KNOWLEDGE_ENGINE] Retrieval for '{}': {} hits (session={})",
            query, hits.len(), session_id
        );

        Ok(hits)
    }

    /// Retrieve only long-term memories (L1-L3), excluding WorkingMemory and code.
    pub fn retrieve_memories(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>> {
        self.store.search_long_term_memory(query, max_results, 0.2)
    }

    /// Retrieve code symbols matching the query.
    pub fn retrieve_code(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchHit>> {
        self.store.search_code(query, max_results)
    }

    /// Search entities by specific kinds (for layered retrieval — e.g., exclude L0).
    pub fn search_by_kinds(
        &self,
        query: &str,
        kinds: &[EntityKind],
        max_results: usize,
        min_score: f32,
    ) -> Result<Vec<SearchHit>> {
        self.store.search_unified(query, max_results, Some(kinds), min_score)
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Lookup
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Find code symbols in a specific file.
    pub fn find_symbols_in_file(&self, file_path: &str) -> Result<Vec<Entity>> {
        self.store.find_symbols_in_file(file_path, "")
    }

    /// Check source code for syntax errors via tree-sitter.
    pub fn check_syntax(&mut self, path: &Path, code: &str) -> Option<Vec<language::SyntaxError>> {
        let lang_name = self.extractor.lock().unwrap().detect_language(path)?.to_string();
        self.extractor.lock().unwrap().check_syntax(code, &lang_name).ok()
    }

    /// Detect language from file extension.
    pub fn detect_language(&self, path: &Path) -> Option<String> {
        self.extractor.lock().unwrap().detect_language(path).map(|s| s.to_string())
    }

    /// Get the embedding dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}
