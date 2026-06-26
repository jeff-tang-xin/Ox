use anyhow::Result;
use serde_json::json;
/// Unified TriviumDB-backed vector store for ALL knowledge entities.
///
/// Replaces the two separate instances (`symbols.tdb` + `memories.tdb`) with a
/// single `knowledge.tdb` database. Entity kind is stored in the payload as
/// `entity_kind` for namespace filtering at the application layer.
///
/// # Key features
/// - Single TriviumDB instance, shared embedding model via `Arc<EmbeddingModel>`
/// - `kind_filter` on searches: simultaneously search code symbols + memory layers
/// - `expand_depth=2`: graph expansion along entity relations
/// - File-level deduplication: re-indexing a file removes old entities first
/// - Batch insert with chunked embedding (100 entities/chunk)
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use super::entity::{Entity, EntityKind, EntityMetadata, MemoryCoordinate, Relation, SymbolType};
use crate::config::EmbeddingConfig;
use crate::symbol::embedding::EmbeddingModel;

/// A unified search result carrying the entity and its similarity score.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub entity: Entity,
    pub score: f32,
}

pub struct UnifiedVectorStore {
    db: triviumdb::Database<f32>,
    embedding_model: Arc<EmbeddingModel>,
    dimension: usize,
    embed_batch_size: usize,
    embed_max_chars: usize,
    /// Track TriviumDB vector IDs by file_path for deduplication on re-index.
    file_ids: HashMap<String, Vec<u64>>,
    /// entity_id → TriviumDB vector ID for targeted deletion.
    entity_ids: HashMap<String, u64>,
}

impl UnifiedVectorStore {
    /// Open or create the unified knowledge database.
    ///
    /// # Arguments
    /// * `path` - e.g. `~/.ox/knowledge.tdb`
    /// * `embedding_model` - Shared embedding model (Arc for cross-component sharing)
    /// * `config` - Embedding configuration (for dimension)
    pub fn open(
        path: &str,
        embedding_model: Arc<EmbeddingModel>,
        config: &EmbeddingConfig,
    ) -> Result<Self> {
        let dim = config.dimension;
        let db = triviumdb::Database::<f32>::open(path, dim)
            .map_err(|e| anyhow::anyhow!("Failed to open Unified TriviumDB at {path}: {e}"))?;

        tracing::info!(
            "[UNIFIED_VECTOR] Opened knowledge.tdb at {path} (dim={dim}, expand_depth=2)"
        );

        Ok(Self {
            db,
            embedding_model,
            dimension: dim,
            embed_batch_size: config.index_embed_chunk_size.max(1),
            embed_max_chars: config.index_embed_max_chars.max(256),
            file_ids: HashMap::new(),
            entity_ids: HashMap::new(),
        })
    }

    /// True when this file path already has stored vectors (resume / incremental skip).
    pub fn has_file_vectors(&self, file_path: &str) -> bool {
        self.file_ids
            .get(file_path)
            .is_some_and(|ids| !ids.is_empty())
    }

    fn embed_text<'a>(&self, entity: &'a Entity) -> String {
        entity.text_for_embedding(self.embed_max_chars)
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Insertion
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Insert a single entity into the vector store.
    /// Returns the TriviumDB internal vector ID.
    pub fn insert_entity(&mut self, entity: &Entity) -> Result<u64> {
        let embed_text = self.embed_text(entity);
        let embedding = self
            .embedding_model
            .embed_passage(&embed_text, entity.kind.is_code_entity())?;
        let payload = entity_to_payload(entity);
        let id = self
            .db
            .insert(&embedding, payload)
            .map_err(|e| anyhow::anyhow!("TriviumDB insert error for entity {}: {e}", entity.id))?;

        self.entity_ids.insert(entity.id.clone(), id);

        // Track file association for CodeSymbol/CodeFile entities
        if let Some(fp) = entity.file_path() {
            self.file_ids.entry(fp.to_string()).or_default().push(id);
        }

        tracing::debug!(
            "[UNIFIED_VECTOR] Inserted entity {} (kind={}, {} chars)",
            entity.id,
            entity.kind.as_str(),
            entity.content.len()
        );

        Ok(id)
    }

    /// Batch-insert multiple entities, chunked by 100 for memory efficiency.
    /// Automatically deletes old vectors for affected files before inserting.
    pub fn insert_entities_batch(&mut self, entities: &[Entity]) -> Result<usize> {
        if entities.is_empty() {
            return Ok(0);
        }

        // Delete old vectors for all affected files (dedup)
        self.remove_by_files_from_entities(entities);

        let chunk_size = self.embed_batch_size;
        let mut total_count = 0;
        let mut new_file_ids: HashMap<String, Vec<u64>> = HashMap::new();
        let total = entities.len();
        let mut embed_texts: Vec<String> = Vec::new();

        for (chunk_idx, chunk) in entities.chunks(chunk_size).enumerate() {
            embed_texts.clear();
            embed_texts.extend(chunk.iter().map(|e| self.embed_text(e)));
            let items: Vec<(&str, bool)> = embed_texts
                .iter()
                .zip(chunk.iter())
                .map(|(text, e)| (text.as_str(), e.kind.is_code_entity()))
                .collect();
            let embeddings = self.embedding_model.embed_passages_batch(&items)?;

            for (entity, embedding) in chunk.iter().zip(embeddings.iter()) {
                let payload = entity_to_payload(entity);
                match self.db.insert(embedding, payload) {
                    Ok(id) => {
                        self.entity_ids.insert(entity.id.clone(), id);
                        if let Some(fp) = entity.file_path() {
                            new_file_ids.entry(fp.to_string()).or_default().push(id);
                        }
                        total_count += 1;
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[UNIFIED_VECTOR] Failed to insert entity {}: {e}",
                            entity.id
                        );
                    }
                }
            }

            let processed = ((chunk_idx + 1) * chunk_size).min(total);
            tracing::debug!(
                "[UNIFIED_VECTOR] Embedding progress: {}/{} entities",
                processed,
                total
            );
        }

        // Update file tracking
        self.file_ids.extend(new_file_ids);

        tracing::info!(
            "[UNIFIED_VECTOR] Batch-inserted {}/{} entities",
            total_count,
            total
        );
        Ok(total_count)
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Deletion / Dedup
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Remove all vector entries for a given file path (used before re-indexing).
    pub fn remove_by_file(&mut self, file_path: &str) {
        if let Some(old_ids) = self.file_ids.remove(file_path) {
            let id_set: std::collections::HashSet<u64> = old_ids.iter().copied().collect();
            self.entity_ids.retain(|_, vid| !id_set.contains(vid));
            let n = old_ids.len();
            for id in old_ids {
                let _ = self.db.delete(id);
            }
            tracing::debug!(
                "[UNIFIED_VECTOR] Removed {} old vectors for {}",
                n,
                file_path
            );
        }
    }

    /// Remove old vectors for all files referenced in a batch of entities.
    fn remove_by_files_from_entities(&mut self, entities: &[Entity]) {
        let mut seen: HashMap<String, ()> = HashMap::new();
        for e in entities {
            if let Some(fp) = e.file_path() {
                if seen.insert(fp.to_string(), ()).is_none() {
                    self.remove_by_file(fp);
                }
            }
        }
    }

    /// Delete a single entity by its entity_id (payload key).
    pub fn delete_entity_by_id(&mut self, entity_id: &str) -> Result<bool> {
        if let Some(id) = self.entity_ids.remove(entity_id) {
            self.db.delete(id).map_err(|e| {
                anyhow::anyhow!("TriviumDB delete error for entity {entity_id}: {e}")
            })?;
            for ids in self.file_ids.values_mut() {
                ids.retain(|&vid| vid != id);
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Delete a single entity by its TriviumDB internal ID.
    pub fn delete_by_id(&mut self, id: u64) -> Result<()> {
        self.db
            .delete(id)
            .map_err(|e| anyhow::anyhow!("TriviumDB delete error for id {id}: {e}"))
    }

    /// Load file→vector-id map from disk (survives restarts).
    pub fn load_file_id_map(&mut self, path: &Path) {
        if !path.exists() {
            return;
        }
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, Vec<u64>>>(&data) {
                tracing::info!(
                    "[UNIFIED_VECTOR] Loaded file_ids map ({} files) from {}",
                    map.len(),
                    path.display()
                );
                self.file_ids = map;
            }
        }
    }

    /// Persist file→vector-id map to disk.
    pub fn save_file_id_map(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.file_ids)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Search
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Unified semantic search across all entity kinds.
    ///
    /// Embeds the query, searches TriviumDB with `expand_depth=2` for graph
    /// expansion along Relation edges, then optionally filters by entity kind.
    ///
    /// # Arguments
    /// * `query` - Natural language search query
    /// * `top_k` - Maximum results to return
    /// * `kind_filter` - If Some, only return entities of these kinds
    /// * `min_score` - Minimum similarity threshold (0.0-1.0)
    pub fn search_unified(
        &self,
        query: &str,
        top_k: usize,
        kind_filter: Option<&[EntityKind]>,
        min_score: f32,
    ) -> Result<Vec<SearchHit>> {
        let query_embedding = self.embedding_model.embed_query(query)?;

        // expand_depth=2: 2-hop graph traversal along entity relations
        let raw_results = self
            .db
            .search(
                &query_embedding,
                top_k * 3, // Fetch more to account for kind filtering
                2,         // expand_depth=2
                min_score,
            )
            .map_err(|e| anyhow::anyhow!("TriviumDB search error: {e}"))?;

        let hits: Vec<SearchHit> = raw_results
            .into_iter()
            .filter_map(|r| {
                let entity = payload_to_entity(&r.payload)?;

                // Apply kind filter
                if let Some(filter) = kind_filter {
                    if !filter.contains(&entity.kind) {
                        return None;
                    }
                }

                Some(SearchHit {
                    entity,
                    score: r.score,
                })
            })
            .take(top_k)
            .collect();

        tracing::debug!(
            "[UNIFIED_VECTOR] Search '{}' → {} hits (min_score={}, filtered from {})",
            query,
            hits.len(),
            min_score,
            if kind_filter.is_some() {
                "with filter"
            } else {
                "no filter"
            }
        );

        Ok(hits)
    }

    /// Search only code symbols (CodeSymbol, CodeFile, CodeModule).
    pub fn search_code(&self, query: &str, top_k: usize) -> Result<Vec<SearchHit>> {
        self.search_unified(
            query,
            top_k,
            Some(&[
                EntityKind::CodeSymbol,
                EntityKind::CodeFile,
                EntityKind::CodeModule,
            ]),
            0.0,
        )
    }

    /// Search only memory layers (L0-L3).
    pub fn search_memory(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<SearchHit>> {
        self.search_unified(
            query,
            top_k,
            Some(&[
                EntityKind::WorkingMemory,
                EntityKind::AtomicMemory,
                EntityKind::EpisodicMemory,
                EntityKind::SemanticMemory,
            ]),
            min_score,
        )
    }

    /// Search only long-term memory layers (L1-L3, exclude WorkingMemory).
    pub fn search_long_term_memory(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<SearchHit>> {
        self.search_unified(
            query,
            top_k,
            Some(&[
                EntityKind::AtomicMemory,
                EntityKind::EpisodicMemory,
                EntityKind::SemanticMemory,
            ]),
            min_score,
        )
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Lookup
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Find CodeSymbol entities in a specific file.
    pub fn find_symbols_in_file(&self, file_path: &str, query_hint: &str) -> Result<Vec<Entity>> {
        let normalized_target = normalize_path_key(file_path);
        let search_query = if query_hint.is_empty() {
            file_path
        } else {
            query_hint
        };

        let hits = self.search_unified(search_query, 100, Some(&[EntityKind::CodeSymbol]), 0.0)?;

        let symbols: Vec<Entity> = hits
            .into_iter()
            .filter(|h| {
                if let EntityMetadata::CodeSymbol { file_path: fp, .. } = &h.entity.metadata {
                    paths_match(fp, file_path) || normalize_path_key(fp) == normalized_target
                } else {
                    false
                }
            })
            .map(|h| h.entity)
            .collect();

        Ok(symbols)
    }

    /// Get the embedding dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Payload serialization (Entity ↔ serde_json::Value for TriviumDB)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Serialize an Entity into a TriviumDB payload (JSON object).
/// Stashes the full metadata and relations as JSON strings while keeping key
/// fields accessible at the top level for query-time inspection.
fn entity_to_payload(entity: &Entity) -> serde_json::Value {
    let metadata_json = serde_json::to_string(&entity.metadata).unwrap_or_default();
    let relations_json = serde_json::to_string(&entity.relations).unwrap_or_default();
    let coordinate_json = serde_json::to_string(&entity.coordinate).unwrap_or_default();

    let mut payload = json!({
        "entity_kind": entity.kind.as_str(),
        "entity_id": entity.id,
        "content": entity.content,
        "metadata_json": metadata_json,
        "relations_json": relations_json,
        "coordinate_json": coordinate_json,
        "is_critical": entity.is_critical,
    });

    // Flatten commonly-searched metadata fields for query-time access
    match &entity.metadata {
        EntityMetadata::CodeSymbol {
            symbol_type,
            language,
            start_line,
            end_line,
            file_path,
            signature,
            parent,
            fq_name,
            calls,
        } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("file_path".into(), json!(file_path));
            map.insert(
                "symbol_name".into(),
                json!(fq_name.rsplit("::").next().unwrap_or(fq_name)),
            );
            map.insert("symbol_type".into(), json!(symbol_type.to_string()));
            map.insert("language".into(), json!(language));
            map.insert("start_line".into(), json!(start_line));
            map.insert("end_line".into(), json!(end_line));
            map.insert("signature".into(), json!(signature));
            map.insert("parent".into(), json!(parent));
            map.insert("fq_name".into(), json!(fq_name));
            map.insert("calls".into(), json!(calls));
        }
        EntityMetadata::CodeFile { path, language, .. } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("file_path".into(), json!(path));
            map.insert("language".into(), json!(language));
        }
        EntityMetadata::CodeModule { name, path } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("module_name".into(), json!(name));
            map.insert("file_path".into(), json!(path));
        }
        EntityMetadata::WorkingMemory {
            session_id,
            action,
            has_code_changes,
            ..
        } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("session_id".into(), json!(session_id));
            map.insert("action".into(), json!(action));
            map.insert("has_code_changes".into(), json!(has_code_changes));
        }
        EntityMetadata::AtomicMemory {
            memory_type,
            project_id,
            ..
        } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("memory_type".into(), json!(memory_type));
            map.insert("project_id".into(), json!(project_id));
        }
        EntityMetadata::EpisodicMemory {
            episode_name,
            session_id,
            task_description,
            ..
        } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("episode_name".into(), json!(episode_name));
            map.insert("session_id".into(), json!(session_id));
            map.insert("task_description".into(), json!(task_description));
        }
        EntityMetadata::SemanticMemory {
            domain, project_id, ..
        } => {
            let map = payload.as_object_mut().unwrap();
            map.insert("domain".into(), json!(domain));
            map.insert("project_id".into(), json!(project_id));
        }
    }

    payload
}

/// Deserialize a TriviumDB payload back into an Entity.
fn payload_to_entity(payload: &serde_json::Value) -> Option<Entity> {
    let entity_kind = EntityKind::from_str(payload.get("entity_kind")?.as_str()?)?;
    let entity_id = payload.get("entity_id")?.as_str()?.to_string();
    let content = payload.get("content")?.as_str().unwrap_or("").to_string();

    // Try full metadata deserialization first
    let metadata = payload
        .get("metadata_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<EntityMetadata>(s).ok());

    let relations = payload
        .get("relations_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Vec<Relation>>(s).ok())
        .unwrap_or_default();

    let coordinate = payload
        .get("coordinate_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<MemoryCoordinate>(s).ok())
        .unwrap_or_else(|| {
            MemoryCoordinate::new(entity_kind.depth().unwrap_or(0), entity_id.as_str(), 384)
        });

    let is_critical = payload
        .get("is_critical")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // If metadata deserialization failed, reconstruct from flattened fields
    let metadata = metadata.unwrap_or_else(|| reconstruct_metadata(entity_kind, payload));

    Some(Entity {
        id: entity_id,
        kind: entity_kind,
        content,
        coordinate,
        metadata,
        relations,
        is_critical,
    })
}

/// Fallback: reconstruct EntityMetadata from flattened payload fields.
fn reconstruct_metadata(kind: EntityKind, payload: &serde_json::Value) -> EntityMetadata {
    let s = |key: &str| {
        payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let s_opt = |key: &str| {
        payload
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let i = |key: &str| payload.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let f = |key: &str| payload.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let b = |key: &str| payload.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    let strings = |key: &str| {
        payload
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };

    match kind {
        EntityKind::WorkingMemory => EntityMetadata::WorkingMemory {
            session_id: s("session_id"),
            action: s("action"),
            intent: s_opt("intent"),
            result: s_opt("result"),
            tools_used: strings("tools_used"),
            has_code_changes: b("has_code_changes"),
            modified_entities: strings("modified_entities"),
            self_state: s_opt("self_state"),
        },
        EntityKind::AtomicMemory => EntityMetadata::AtomicMemory {
            memory_type: s("memory_type"),
            project_id: s_opt("project_id"),
            language: s("language"),
            source: s("source"),
            related_files: strings("related_files"),
            quality_score: f("quality_score"),
            judge_eval_count: payload
                .get("judge_eval_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        },
        EntityKind::EpisodicMemory => EntityMetadata::EpisodicMemory {
            episode_name: s("episode_name"),
            project_id: s_opt("project_id"),
            session_id: s("session_id"),
            start_time: payload
                .get("start_time")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            end_time: payload.get("end_time").and_then(|v| v.as_i64()),
            task_description: s("task_description"),
            conclusions: strings("conclusions"),
            unresolved: strings("unresolved"),
            continuation_hint: s_opt("continuation_hint"),
            usage_count: payload
                .get("usage_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            related_atoms: strings("related_atoms"),
        },
        EntityKind::SemanticMemory => EntityMetadata::SemanticMemory {
            project_id: s("project_id"),
            version: payload.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
            domain: s("domain"),
            source_episodes: strings("source_episodes"),
            confidence: f("confidence"),
        },
        EntityKind::CodeSymbol => EntityMetadata::CodeSymbol {
            symbol_type: SymbolType::from_str(&s("symbol_type")).unwrap_or(SymbolType::Function),
            language: s("language"),
            start_line: i("start_line"),
            end_line: i("end_line"),
            file_path: s("file_path"),
            signature: s("signature"),
            parent: s_opt("parent"),
            fq_name: s("fq_name"),
            calls: strings("calls"),
        },
        EntityKind::CodeFile => EntityMetadata::CodeFile {
            path: s("file_path"),
            language: s("language"),
            symbol_count: i("symbol_count"),
        },
        EntityKind::CodeModule => EntityMetadata::CodeModule {
            name: s("module_name"),
            path: s("file_path"),
        },
    }
}

fn normalize_path_key(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .to_lowercase()
}

fn paths_match(stored: &str, query: &str) -> bool {
    if stored == query {
        return true;
    }
    let stored_norm = normalize_path_key(stored);
    let query_norm = normalize_path_key(query);
    if stored_norm == query_norm {
        return true;
    }
    stored_norm.ends_with(&query_norm)
        || query_norm.ends_with(&stored_norm)
        || stored.ends_with(query)
        || query.ends_with(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_payload_roundtrip_code_symbol() {
        let entity = Entity::code_symbol(
            "validate_token",
            "auth::validate_token",
            SymbolType::Function,
            "rust",
            "src/auth.rs",
            42,
            58,
            "fn validate_token(token: &Token) -> Result<bool>",
            Some("AuthService"),
        );

        let payload = entity_to_payload(&entity);
        let reconstructed = payload_to_entity(&payload).expect("roundtrip should succeed");

        assert_eq!(reconstructed.kind, EntityKind::CodeSymbol);
        assert_eq!(reconstructed.id, entity.id);
        assert_eq!(reconstructed.content, entity.content);
    }

    #[test]
    fn test_entity_payload_roundtrip_atomic_memory() {
        let entity = Entity::atomic_memory(
            "User prefers tabs over spaces",
            "Style",
            Some("my-project"),
            "rust",
            "UserExplicit",
        );

        let payload = entity_to_payload(&entity);
        let reconstructed = payload_to_entity(&payload).expect("roundtrip should succeed");

        assert_eq!(reconstructed.kind, EntityKind::AtomicMemory);
        assert_eq!(reconstructed.id, entity.id);
    }

    #[test]
    fn test_entity_payload_roundtrip_working_memory() {
        let entity = Entity::working_memory(
            "sess-1",
            "fixed auth bug",
            Some("user reported crash"),
            Some("patched validate_token"),
            vec!["edit_file".into()],
            true,
        );

        let payload = entity_to_payload(&entity);
        let reconstructed = payload_to_entity(&payload).expect("roundtrip should succeed");

        assert_eq!(reconstructed.kind, EntityKind::WorkingMemory);
        assert_eq!(reconstructed.id, entity.id);
    }

    #[test]
    fn test_entity_payload_roundtrip_episodic_memory() {
        let entity = Entity::episodic_memory(
            "Fixed auth token bug",
            "sess-1",
            Some("my-project"),
            "Fixed token expiration handling",
        );

        let payload = entity_to_payload(&entity);
        let reconstructed = payload_to_entity(&payload).expect("roundtrip should succeed");

        assert_eq!(reconstructed.kind, EntityKind::EpisodicMemory);
    }

    #[test]
    fn test_entity_payload_roundtrip_semantic_memory() {
        let entity = Entity::semantic_memory(
            "my-project",
            "This project uses hexagonal architecture",
            "architecture",
            vec!["ep-1".into()],
        );

        let payload = entity_to_payload(&entity);
        let reconstructed = payload_to_entity(&payload).expect("roundtrip should succeed");

        assert_eq!(reconstructed.kind, EntityKind::SemanticMemory);
    }
}
