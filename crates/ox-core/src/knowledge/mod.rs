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
pub mod index_progress;
pub use index_progress::IndexProgress;
pub mod bm25;
pub mod bridge;
pub mod consolidation;
pub mod embedding;
pub mod entity;
pub mod extractor;
pub mod graph;
pub mod keywords;
pub mod language;
pub mod layering;
pub mod live_update;
pub mod memory_cluster;
pub mod memory_node;
pub mod retrieval;
pub mod vector_store;

pub use keywords::KeywordExtraction;
pub use memory_node::{MemoryNode, MemoryNodeType, format_memory_context};

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::EmbeddingConfig;
use bm25::Bm25Index;
use entity::{Entity, EntityKind, RelationType, injection_priority};
use extractor::AstExtractor;
use graph::EntityGraph;
use layering::AutoLayering;
use std::sync::Mutex as StdMutex;
use vector_store::{SearchHit, UnifiedVectorStore};

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
    /// In-memory relation graph for layering + consolidation
    entity_graph: StdMutex<EntityGraph>,
    /// L0→L3 promotion engine
    auto_layering: StdMutex<AutoLayering>,
    /// BM25 inverted index for hybrid retrieval
    bm25_index: StdMutex<Bm25Index>,
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
        let mut store = UnifiedVectorStore::open(db_path, embedding_model, embedding_config)?;
        let file_ids_path = Self::file_ids_cache_path(project_path);
        store.load_file_id_map(&file_ids_path);
        let bm25_path = Self::bm25_cache_path(project_path);
        let bm25_index = Bm25Index::load(&bm25_path);
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
            entity_graph: StdMutex::new(EntityGraph::new()),
            auto_layering: StdMutex::new(AutoLayering::with_defaults()),
            bm25_index: StdMutex::new(bm25_index),
        })
    }

    fn bm25_cache_path(project_path: &Path) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            project_path.to_string_lossy().hash(&mut hasher);
            hasher.finish()
        };
        home.join(".ox")
            .join("cache")
            .join(format!("bm25_{:016x}.json", hash))
    }

    fn persist_bm25(&self) {
        let path = Self::bm25_cache_path(&self.project_path);
        if let Ok(index) = self.bm25_index.try_lock()
            && let Err(e) = index.save(&path) {
                tracing::warn!("[KNOWLEDGE_ENGINE] Failed to persist BM25 index: {e}");
            }
    }

    fn index_bm25(&self, entity: &Entity) {
        if let Ok(mut index) = self.bm25_index.try_lock() {
            index.index_document(&entity.id, &entity.content);
        }
    }

    fn remove_bm25(&self, entity_id: &str) {
        if let Ok(mut index) = self.bm25_index.try_lock() {
            index.remove_document(entity_id);
        }
    }

    /// BM25 keyword search — returns entity ids with normalized scores.
    pub fn bm25_search(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        self.bm25_index.lock().unwrap().search(query, top_k)
    }

    /// Hybrid retrieval: fuse vector hits with BM25 scores.
    pub fn hybrid_search_by_kinds(
        &self,
        query: &str,
        kinds: &[EntityKind],
        top_k: usize,
        min_vector_score: f32,
    ) -> Result<Vec<SearchHit>> {
        const VECTOR_WEIGHT: f32 = 0.65;
        const BM25_WEIGHT: f32 = 0.35;

        let vector_hits = self.search_by_kinds(query, kinds, top_k * 2, min_vector_score)?;
        let bm25_hits = self.bm25_search(query, top_k * 2);

        let mut fused: HashMap<String, (Option<Entity>, f32)> = HashMap::new();

        for hit in vector_hits {
            let score = hit.score * VECTOR_WEIGHT;
            fused
                .entry(hit.entity.id.clone())
                .and_modify(|(_, s)| *s += score)
                .or_insert_with(|| (Some(hit.entity), score));
        }

        for (entity_id, bm25_score) in bm25_hits {
            let boost = bm25_score * BM25_WEIGHT;
            if let Some((_entity, score)) = fused.get_mut(&entity_id) {
                *score += boost;
            } else if let Some(e) = self.entity_graph.lock().unwrap().get(&entity_id) {
                fused.insert(entity_id, (Some(e.clone()), boost));
            } else {
                fused.insert(entity_id, (None, boost));
            }
        }

        let mut results: Vec<SearchHit> = fused
            .into_iter()
            .filter_map(|(id, (entity, score))| {
                entity.map(|e| SearchHit { entity: e, score }).or_else(|| {
                    tracing::debug!("[HYBRID] BM25 hit without entity: {id}");
                    None
                })
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        Ok(results)
    }

    /// Delete memories matching keyword (for /forget).
    pub fn forget_matching(&mut self, keyword: &str) -> usize {
        let hits = self.bm25_search(keyword, 50);
        let mut deleted = 0;
        for (entity_id, _) in hits {
            if self.store.delete_entity_by_id(&entity_id).unwrap_or(false) {
                self.remove_bm25(&entity_id);
                if let Ok(mut graph) = self.entity_graph.try_lock() {
                    graph.remove(&entity_id);
                }
                deleted += 1;
            }
        }
        if deleted > 0 {
            self.persist_bm25();
        }
        deleted
    }

    fn purge_file_indexes(&mut self, file_path: &str) {
        let ids = self
            .entity_graph
            .lock()
            .unwrap()
            .entity_ids_for_file(file_path);
        for id in ids {
            self.remove_bm25(&id);
            self.entity_graph.lock().unwrap().remove(&id);
        }
    }

    fn after_entity_stored(&self, entity: &Entity) {
        self.index_bm25(entity);
    }

    fn after_entities_batch_stored(&self, entities: &[Entity]) {
        for entity in entities {
            self.index_bm25(entity);
        }
        self.persist_bm25();
    }

    fn file_ids_cache_path(project_path: &Path) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            project_path.to_string_lossy().hash(&mut hasher);
            hasher.finish()
        };
        home.join(".ox")
            .join("cache")
            .join(format!("file_ids_{:016x}.json", hash))
    }

    fn persist_file_ids(&self) {
        let path = Self::file_ids_cache_path(&self.project_path);
        if let Err(e) = self.store.save_file_id_map(&path) {
            tracing::warn!("[KNOWLEDGE_ENGINE] Failed to persist file_ids: {e}");
        }
    }

    /// Upsert entity into the in-memory graph (for layering).
    pub fn track_entity(&self, entity: &Entity) {
        if let Ok(mut g) = self.entity_graph.try_lock() {
            g.upsert(entity.clone());
        }
    }

    pub(crate) fn lock_entity_graph(&self) -> std::sync::MutexGuard<'_, EntityGraph> {
        self.entity_graph.lock().unwrap()
    }

    /// Persist entities produced by [`live_update::on_tool_executed`].
    pub fn apply_live_update(&mut self, result: live_update::LiveUpdateResult) -> Result<()> {
        let all: Vec<&Entity> = result
            .new_working_memories
            .iter()
            .chain(result.updated_symbols.iter())
            .chain(result.extracted_facts.iter())
            .collect();
        for entity in all {
            self.store.insert_entity(entity)?;
            self.track_entity(entity);
            self.after_entity_stored(entity);
        }
        if !result.new_working_memories.is_empty() || !result.extracted_facts.is_empty() {
            tracing::debug!(
                "[LIVE_UPDATE] stored {} WM, {} facts",
                result.new_working_memories.len(),
                result.extracted_facts.len()
            );
        }
        Ok(())
    }

    /// Run live-update hook and persist new L0 / fact entities.
    pub fn process_tool_execution(
        &mut self,
        ctx: &live_update::ToolExecutionContext,
    ) -> Result<()> {
        let session_id = ctx.session_id.clone();
        let update = {
            let graph = self.entity_graph.lock().unwrap();
            live_update::on_tool_executed(ctx, &graph)
        };
        let layer_check = update.trigger_layering_check;
        self.apply_live_update(update)?;
        if layer_check {
            consolidation::on_tool_layering_check(self, &session_id);
        }
        Ok(())
    }

    /// Rule-based L0→L3 promotion (no LLM). Returns number of new entities stored.
    pub fn run_consolidation(
        &mut self,
        session_id: &str,
        project_id: Option<&str>,
    ) -> Result<usize> {
        let result = {
            let graph = self.entity_graph.lock().unwrap();
            let mut layering = self.auto_layering.lock().unwrap();
            layering.apply_rule_based_promotions(&graph, session_id, project_id)
        };
        let mut stored = 0;
        for entity in result.new_entities {
            self.store.insert_entity(&entity)?;
            self.track_entity(&entity);
            stored += 1;
        }
        if stored > 0 {
            self.persist_file_ids();
        }
        tracing::info!("[KNOWLEDGE_ENGINE] Consolidation stored {stored} entities");
        Ok(stored)
    }

    /// Record an L2 episodic checkpoint after workflow completion.
    pub fn record_workflow_episode(
        &mut self,
        session_id: &str,
        project_id: Option<&str>,
        task_description: &str,
        execution_summary: &str,
    ) -> Result<Entity> {
        let name: String = task_description.chars().take(80).collect();
        let desc = format!("{task_description}\n\nSummary: {execution_summary}");
        let entity = self.record_episode(&name, session_id, project_id, &desc)?;
        self.track_entity(&entity);
        Ok(entity)
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
        if self
            .extractor
            .lock()
            .unwrap()
            .detect_language(file_path)
            .is_none()
        {
            return Ok(Vec::new());
        }
        let code = std::fs::read_to_string(file_path)?;
        let entities = self
            .extractor
            .lock()
            .unwrap()
            .extract_entities(file_path, &code)?;
        Ok(entities)
    }

    /// Phase 1: Walk project, extract ALL symbols (AST only, no embedding).
    /// Fast — no BERT inference. **Uses mtime-based file cache to skip unchanged files.**
    /// Only walks within `self.project_path` (the user's working directory).
    /// Reports progress via optional channel.
    /// Returns (all_entities, total_files) for Phase 2 batch embedding.
    pub fn collect_all_symbols(
        &self,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<IndexProgress>>,
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
            .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let total_files = all_files.len();
        let mut all_entities = Vec::new();
        let mut total_symbols = 0;
        let mut new_cache: HashMap<String, i64> = HashMap::new();
        let mut skipped_from_cache = 0;
        let mut skipped_non_source = 0;

        for (i, path) in all_files.iter().enumerate() {
            let path_str = path.to_string_lossy().to_string();

            // Check mtime — if unchanged, skip AST parse entirely
            if let Ok(meta) = std::fs::metadata(path) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                new_cache.insert(path_str.clone(), mtime);

                if let Some(&cached_mtime) = file_cache.get(&path_str)
                    && cached_mtime == mtime && self.store.has_file_vectors(&path_str) {
                        skipped_from_cache += 1;
                        continue; // Unchanged and already embedded
                    }
            }

            if self.detect_language(path).is_none() {
                skipped_non_source += 1;
                continue;
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

            if let Some(ref tx) = progress_tx
                && ((i + 1) % 20 == 0 || i + 1 == total_files) {
                    let _ = tx.send(IndexProgress::parsing(i + 1, total_files, total_symbols));
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
            "[KNOWLEDGE_ENGINE] Phase 1 done: {} symbols, {} source files indexed ({} non-source skipped, {} cache-hit, {} walk total)",
            total_symbols,
            total_files - skipped_from_cache - skipped_non_source,
            skipped_non_source,
            skipped_from_cache,
            total_files
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
        home.join(".ox")
            .join("cache")
            .join(format!("ast_{:016x}.json", hash))
    }

    /// Returns true if the vector store already has indexed code symbols.
    pub fn has_code_index(&self) -> bool {
        self.store
            .search_code("function struct impl", 1)
            .map(|hits| !hits.is_empty())
            .unwrap_or(false)
    }

    /// Phase 2: Batch-embed and store pre-extracted entities.
    /// Slow — runs BERT inference on all entities in chunks of 100.
    /// Call this AFTER collect_all_symbols(), ideally in a separate spawn.
    pub fn embed_and_store(&mut self, entities: &[Entity]) -> Result<usize> {
        if entities.is_empty() {
            return Ok(0);
        }
        let total = self._embed_chunk(entities, 0, entities.len())?;
        self.persist_file_ids();
        self.persist_bm25();
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
        let count = self._embed_chunk(entities, offset, chunk_size)?;
        self.persist_file_ids();
        if offset + chunk_size >= entities.len() {
            self.persist_bm25();
        }
        Ok(count)
    }

    /// Sort entities so core source paths (`src/`, `crates/`) embed first.
    pub fn sort_entities_for_startup_index(entities: &mut [Entity]) {
        entities.sort_by(|a, b| {
            fn path_priority(path: &str) -> u8 {
                let p = path.replace('\\', "/");
                if p.contains("/src/") || p.starts_with("src/") {
                    0
                } else if p.contains("/crates/") {
                    1
                } else if p.contains("/lib/") {
                    2
                } else if p.contains("/tests/") || p.contains("/test/") {
                    4
                } else if p.contains("/docs/") || p.contains("/doc/") {
                    5
                } else {
                    3
                }
            }
            let pa = a.file_path().map(path_priority).unwrap_or(6);
            let pb = b.file_path().map(path_priority).unwrap_or(6);
            pa.cmp(&pb)
                .then_with(|| a.file_path().unwrap_or("").cmp(b.file_path().unwrap_or("")))
        });
    }

    fn _embed_chunk(&mut self, entities: &[Entity], offset: usize, count: usize) -> Result<usize> {
        let end = (offset + count).min(entities.len());
        let chunk = &entities[offset..end];
        if chunk.is_empty() {
            return Ok(0);
        }
        let stored = self.store.insert_entities_batch(chunk)?;
        for entity in chunk {
            self.track_entity(entity);
        }
        self.after_entities_batch_stored(chunk);
        Ok(stored)
    }

    /// Collect all eligible source file paths in the project directory.
    /// Respects .gitignore via the `ignore` crate (same rules as ripgrep).
    /// Skips hidden dirs, target/, node_modules/, etc.
    pub fn collect_source_files(&self) -> Vec<PathBuf> {
        let walker = ignore::WalkBuilder::new(&self.project_path)
            .standard_filters(true) // .gitignore, .ignore, hidden files
            .hidden(false)
            .build();

        walker
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    /// Index a single source file: extract AST symbols → store as CodeSymbol entities.
    /// Use this for incremental (live) indexing. For startup, use collect_all_symbols() + embed_and_store().
    pub fn index_file(&mut self, file_path: &Path) -> Result<usize> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self
            .extractor
            .lock()
            .unwrap()
            .extract_entities(file_path, &code)?;

        if entities.is_empty() {
            return Ok(0);
        }

        let file_path_str = file_path.to_string_lossy().to_string();
        self.purge_file_indexes(&file_path_str);
        self.store.remove_by_file(&file_path_str);

        let count = self.store.insert_entities_batch(&entities)?;
        for entity in &entities {
            self.track_entity(entity);
        }
        self.after_entities_batch_stored(&entities);
        self.persist_file_ids();

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Indexed {} → {} symbols",
            file_path.display(),
            count
        );

        Ok(count)
    }

    /// Embed a single file if vectors are missing or the file changed since last index.
    pub fn ensure_file_indexed(&mut self, file_path: &Path) -> Result<usize> {
        if !file_path.is_file() {
            return Ok(0);
        }
        let path_str = file_path.to_string_lossy().to_string();
        if self.store.has_file_vectors(&path_str)
            && let Ok(meta) = std::fs::metadata(file_path) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let cache_path = self.cache_path();
                if let Ok(data) = std::fs::read_to_string(&cache_path)
                    && let Ok(cache) =
                        serde_json::from_str::<std::collections::HashMap<String, i64>>(&data)
                        && cache.get(&path_str) == Some(&mtime) {
                            return Ok(0);
                        }
            }
        self.index_file(file_path)
    }

    /// Embed up to `max_files` paths that are not yet indexed (session-relevant lazy load).
    pub fn ensure_paths_indexed(
        &mut self,
        paths: &[std::path::PathBuf],
        max_files: usize,
    ) -> Result<usize> {
        let mut indexed = 0usize;
        let mut done = 0usize;
        for path in paths {
            if done >= max_files {
                break;
            }
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                self.project_path.join(path)
            };
            if !resolved.is_file() {
                continue;
            }
            done += 1;
            indexed += self.ensure_file_indexed(&resolved)?;
        }
        Ok(indexed)
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
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<IndexProgress>>,
    ) -> Result<usize> {
        // Pre-count eligible files (single walk, .gitignore-aware)
        let walker = ignore::WalkBuilder::new(&self.project_path)
            .standard_filters(true)
            .hidden(false)
            .build();

        // Collect files first so we can report total
        let all_files: Vec<PathBuf> = walker
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let total_files = all_files.len();
        let mut total_symbols = 0;

        for (i, path) in all_files.iter().enumerate() {
            match self.index_file(path) {
                Ok(count) => {
                    total_symbols += count;
                    if let Some(ref tx) = progress_tx {
                        let _ = tx.send(IndexProgress::parsing(i + 1, total_files, total_symbols));
                    }
                }
                Err(e) => {
                    tracing::debug!("[KNOWLEDGE_ENGINE] Skipping {}: {e}", path.display());
                }
            }
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Project index complete: {} symbols across {} files",
            total_symbols,
            total_files
        );

        Ok(total_symbols)
    }

    /// Re-index a file after it was modified (live update).
    /// Returns the new entities so callers can update the EntityGraph.
    pub fn reindex_file(&mut self, file_path: &Path) -> Result<Vec<Entity>> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self
            .extractor
            .lock()
            .unwrap()
            .extract_entities(file_path, &code)?;

        let file_path_str = file_path.to_string_lossy().to_string();
        self.purge_file_indexes(&file_path_str);
        self.store.remove_by_file(&file_path_str);

        if !entities.is_empty() {
            self.store.insert_entities_batch(&entities)?;
            for entity in &entities {
                self.track_entity(entity);
            }
            self.after_entities_batch_stored(&entities);
        } else {
            self.persist_bm25();
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Re-indexed {} → {} symbols",
            file_path.display(),
            entities.len()
        );

        Ok(entities)
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Memory Storage
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Store an entity (memory or code) into the knowledge base.
    pub fn store_entity(&mut self, entity: &Entity) -> Result<u64> {
        let id = self.store.insert_entity(entity)?;
        self.after_entity_stored(entity);
        self.track_entity(entity);
        self.persist_bm25();
        Ok(id)
    }

    /// Store a batch of entities.
    pub fn store_entities(&mut self, entities: &[Entity]) -> Result<usize> {
        let count = self.store.insert_entities_batch(entities)?;
        for entity in entities {
            self.track_entity(entity);
        }
        self.after_entities_batch_stored(entities);
        Ok(count)
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
            session_id,
            action,
            intent,
            result,
            tools_used,
            has_code_changes,
        );
        self.store.insert_entity(&entity)?;
        self.after_entity_stored(&entity);
        // Push into ring buffer for fast conversation continuity retrieval
        self.recent_turns.push_back(entity.clone());
        if self.recent_turns.len() > self.max_recent_turns {
            self.recent_turns.pop_front();
        }
        self.track_entity(&entity);
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
        self.after_entity_stored(&entity);
        self.track_entity(&entity);
        self.persist_bm25();
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
        let entity =
            Entity::episodic_memory(episode_name, session_id, project_id, task_description);
        self.store.insert_entity(&entity)?;
        self.after_entity_stored(&entity);
        self.track_entity(&entity);
        self.persist_bm25();
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
        self.after_entity_stored(&entity);
        self.track_entity(&entity);
        self.persist_bm25();
        Ok(entity)
    }

    /// Start a file-system watcher for the project directory.
    /// When source files change, automatically re-indexes them via `reindex_file()`.
    /// Runs in a background tokio task — call once at startup.
    pub fn start_file_watcher(engine: Arc<tokio::sync::RwLock<Self>>) {
        let project_path = {
            let eng = engine
                .try_read()
                .unwrap_or_else(|_| panic!("KnowledgeEngine lock held during watcher init"));
            eng.project_path.clone()
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();

        let mut watcher =
            match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
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
                        if event.kind.is_access() {
                            continue;
                        }
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
                            if path_str.contains("/.git/")
                                || path_str.contains("\\/.git\\")
                                || path_str.contains("/target/")
                                || path_str.contains("\\target\\")
                                || path_str.contains("/node_modules/")
                                || path_str.contains("\\node_modules\\")
                                || path_str.contains("/.ox/")
                                || path_str.contains("\\.ox\\")
                            {
                                continue;
                            }

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

        tracing::info!(
            "[KNOWLEDGE_ENGINE] File watcher started for {:?}",
            project_path
        );
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
        // Hybrid retrieval: dense vectors + BM25 keyword fusion
        let all_kinds = [
            EntityKind::WorkingMemory,
            EntityKind::AtomicMemory,
            EntityKind::EpisodicMemory,
            EntityKind::SemanticMemory,
            EntityKind::CodeSymbol,
            EntityKind::CodeFile,
            EntityKind::CodeModule,
        ];
        let mut hits = self.hybrid_search_by_kinds(query, &all_kinds, max_results * 2, 0.2)?;

        // Sort by injection priority (lower = more important for context)
        hits.sort_by(|a, b| {
            let pa = injection_priority(a.entity.kind);
            let pb = injection_priority(b.entity.kind);
            pa.cmp(&pb).then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
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
            query,
            hits.len(),
            session_id
        );

        Ok(hits)
    }

    /// Retrieve only long-term memories (L1-L3), excluding WorkingMemory and code.
    pub fn retrieve_memories(&self, query: &str, max_results: usize) -> Result<Vec<SearchHit>> {
        self.store.search_long_term_memory(query, max_results, 0.2)
    }

    /// Retrieve code symbols matching the query.
    pub fn retrieve_code(&self, query: &str, max_results: usize) -> Result<Vec<SearchHit>> {
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
        self.store
            .search_unified(query, max_results, Some(kinds), min_score)
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
        let lang_name = self
            .extractor
            .lock()
            .unwrap()
            .detect_language(path)?
            .to_string();
        self.extractor
            .lock()
            .unwrap()
            .check_syntax(code, &lang_name)
            .ok()
    }

    /// Detect language from file extension.
    pub fn detect_language(&self, path: &Path) -> Option<String> {
        self.extractor
            .lock()
            .unwrap()
            .detect_language(path)
            .map(|s| s.to_string())
    }

    /// 1-hop `Calls` neighbors of seed symbols (in-memory EntityGraph).
    pub fn graph_call_neighbors(&self, seed_ids: &[String], max_neighbors: usize) -> Vec<Entity> {
        if seed_ids.is_empty() || max_neighbors == 0 {
            return Vec::new();
        }
        let graph = self.entity_graph.lock().unwrap();
        graph
            .traverse(seed_ids, 1, Some(&[RelationType::Calls]))
            .into_iter()
            .filter(|r| r.distance > 0)
            .take(max_neighbors)
            .map(|r| r.entity)
            .collect()
    }

    /// Symbols that call `symbol_id` (incoming `Calls` edges).
    pub fn graph_callers(&self, symbol_id: &str, max: usize) -> Vec<Entity> {
        let graph = self.entity_graph.lock().unwrap();
        graph
            .find_callers(symbol_id)
            .into_iter()
            .take(max)
            .cloned()
            .collect()
    }

    /// Symbols called by `symbol_id` (outgoing `Calls` edges).
    pub fn graph_callees(&self, symbol_id: &str, max: usize) -> Vec<Entity> {
        let graph = self.entity_graph.lock().unwrap();
        graph
            .find_outgoing(symbol_id, Some(RelationType::Calls))
            .into_iter()
            .take(max)
            .cloned()
            .collect()
    }

    /// Get the embedding dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Poll until a read lock is available (background embed releases write between chunks).
pub async fn acquire_read_with_backoff(
    engine: &std::sync::Arc<tokio::sync::RwLock<KnowledgeEngine>>,
    max_wait: std::time::Duration,
) -> Option<tokio::sync::RwLockReadGuard<'_, KnowledgeEngine>> {
    let deadline = std::time::Instant::now() + max_wait;
    loop {
        match engine.try_read() {
            Ok(guard) => return Some(guard),
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(_) => return None,
        }
    }
}
