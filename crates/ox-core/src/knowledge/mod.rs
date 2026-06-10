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
    extractor: AstExtractor,
    /// Project root path
    project_path: PathBuf,
    /// Embedding dimension
    dimension: usize,
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
            extractor,
            project_path: project_path.to_path_buf(),
            dimension,
        })
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Indexing
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Extract AST symbols from a single file WITHOUT embedding/storing.
    /// Returns entities ready for batch embedding. Fast (no BERT inference).
    pub fn extract_file_symbols(&mut self, file_path: &Path) -> Result<Vec<Entity>> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self.extractor.extract_entities(file_path, &code)?;
        Ok(entities)
    }

    /// Phase 1: Walk project, extract ALL symbols (AST only, no embedding).
    /// Fast — no BERT inference. Respects .gitignore via `ignore` crate.
    /// Only walks within `self.project_path` (the user's working directory).
    /// Reports progress via optional channel.
    /// Returns (all_entities, total_files) for Phase 2 batch embedding.
    pub fn collect_all_symbols(
        &mut self,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<(usize, usize, usize)>>,
    ) -> Result<(Vec<Entity>, usize)> {
        // Use ignore::Walk for .gitignore-aware traversal (same as ripgrep)
        let walker = ignore::WalkBuilder::new(&self.project_path)
            .standard_filters(true)   // respect .gitignore, .ignore, etc.
            .hidden(false)            // skip hidden files/dirs by default
            .build();

        let all_files: Vec<PathBuf> = walker
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().map_or(false, |ft| ft.is_file())
            })
            .map(|e| e.path().to_path_buf())
            .collect();

        let total_files = all_files.len();
        let mut all_entities = Vec::new();
        let mut total_symbols = 0;

        for (i, path) in all_files.iter().enumerate() {
            match self.extract_file_symbols(path) {
                Ok(entities) => {
                    for entity in entities {
                        total_symbols += 1;
                        all_entities.push(entity);
                    }
                }
                Err(e) => {
                    tracing::debug!("[KNOWLEDGE_ENGINE] Skipping {}: {e}", path.display());
                }
            }

            if let Some(ref tx) = progress_tx {
                let _ = tx.send((i + 1, total_files, total_symbols));
            }
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Phase 1 complete: {} symbols from {} files (AST only, no embedding yet)",
            total_symbols, total_files
        );

        Ok((all_entities, total_files))
    }

    /// Phase 2: Batch-embed and store pre-extracted entities.
    /// Slow — runs BERT inference on all entities in chunks of 100.
    /// Call this AFTER collect_all_symbols(), ideally in a separate spawn.
    pub fn embed_and_store(&mut self, entities: &[Entity]) -> Result<usize> {
        if entities.is_empty() {
            return Ok(0);
        }

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Phase 2: Embedding {} entities...",
            entities.len()
        );

        // First remove old vectors for all affected files
        use std::collections::HashSet;
        let mut seen_files = HashSet::new();
        for e in entities {
            if let Some(fp) = e.file_path() {
                if seen_files.insert(fp.to_string()) {
                    self.store.remove_by_file(fp);
                }
            }
        }

        let count = self.store.insert_entities_batch(entities)?;

        tracing::info!(
            "[KNOWLEDGE_ENGINE] Phase 2 complete: {} entities embedded and stored",
            count
        );

        Ok(count)
    }

    /// Index a single source file: extract AST symbols → store as CodeSymbol entities.
    /// Use this for incremental (live) indexing. For startup, use collect_all_symbols() + embed_and_store().
    pub fn index_file(&mut self, file_path: &Path) -> Result<usize> {
        let code = std::fs::read_to_string(file_path)?;
        let entities = self.extractor.extract_entities(file_path, &code)?;

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
        let entities = self.extractor.extract_entities(file_path, &code)?;

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
        Ok(entity)
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

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Lookup
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Find code symbols in a specific file.
    pub fn find_symbols_in_file(&self, file_path: &str) -> Result<Vec<Entity>> {
        self.store.find_symbols_in_file(file_path, "")
    }

    /// Check source code for syntax errors via tree-sitter.
    pub fn check_syntax(&mut self, path: &Path, code: &str) -> Option<Vec<language::SyntaxError>> {
        let lang_name = self.extractor.detect_language(path)?.to_string();
        self.extractor.check_syntax(code, &lang_name).ok()
    }

    /// Detect language from file extension.
    pub fn detect_language(&self, path: &Path) -> Option<&str> {
        self.extractor.detect_language(path)
    }

    /// Get the embedding dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}
