use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use crate::symbol::types::{Symbol, SymbolType};
use crate::symbol::embedding::EmbeddingModel;
use crate::config::EmbeddingConfig;

/// TriviumDB-backed vector store for semantic symbol search.
pub struct VectorStore {
    db: triviumdb::Database<f32>,
    embedding_model: EmbeddingModel,
    /// Track vector IDs by file_path for deduplication on re-index.
    file_ids: HashMap<String, Vec<u64>>,
}

impl VectorStore {
    /// Open or create the database at `path` (e.g. "~/.ox/symbols.tdb").
    ///
    /// # Arguments
    /// * `path` - TriviumDB storage path
    /// * `config` - Embedding configuration (model source, endpoint, etc.)
    pub fn open(path: &str, config: &EmbeddingConfig) -> Result<Self> {
        let dim = config.dimension;

        let db = triviumdb::Database::<f32>::open(path, dim)
            .map_err(|e| anyhow::anyhow!("Failed to open TriviumDB: {e}"))?;

        let embedding_model = EmbeddingModel::with_config(config)?;

        tracing::info!("[VECTOR_STORE] TriviumDB opened at {path} (dim={dim})");

        Ok(Self {
            db,
            embedding_model,
            file_ids: HashMap::new(),
        })
    }

    /// Insert symbols from a single file, replacing any previously indexed symbols
    /// from the same file_path (deduplication).
    pub fn insert_symbols(&mut self, symbols: &[Symbol]) -> Result<usize> {
        if symbols.is_empty() {
            return Ok(0);
        }

        let file_path = &symbols[0].file_path;

        // Delete old vectors for this file (deduplication on re-index)
        if let Some(old_ids) = self.file_ids.remove(file_path) {
            let n = old_ids.len();
            for id in old_ids {
                let _ = self.db.delete(id);
            }
            tracing::debug!("[VECTOR_STORE] Removed {} old vectors for {}", n, file_path);
        }

        // Batch-embed all signatures
        let signatures: Vec<&str> = symbols.iter()
            .map(|s| s.signature.as_str())
            .collect();
        let embeddings = self.embedding_model.embed_batch(&signatures)?;

        let mut new_ids = Vec::with_capacity(symbols.len());

        for (symbol, embedding) in symbols.iter().zip(embeddings.iter()) {
            let id = self.db.insert(
                embedding,
                json!({
                    "file_path": symbol.file_path,
                    "symbol_name": symbol.name,
                    "symbol_type": symbol.kind.to_string(),
                    "language": symbol.language,
                    "start_line": symbol.start_line,
                    "end_line": symbol.end_line,
                    "signature": symbol.signature,
                    "parent": symbol.parent,
                }),
            ).map_err(|e| anyhow::anyhow!("TriviumDB insert error: {e}"))?;

            new_ids.push(id);
        }

        let count = new_ids.len();
        self.file_ids.insert(file_path.clone(), new_ids);

        Ok(count)
    }

    /// Batch-insert symbols from multiple files with chunked embedding.
    /// Processes in batches of 100 symbols to avoid memory explosion and provide progress.
    /// Much faster than calling insert_symbols() per file (5-10x speedup).
    pub fn insert_symbols_batch(&mut self, all_symbols: &[Symbol]) -> Result<usize> {
        if all_symbols.is_empty() {
            return Ok(0);
        }

        // Group symbols by file_path for deduplication
        let mut by_file: HashMap<String, Vec<&Symbol>> = HashMap::new();
        for sym in all_symbols {
            by_file.entry(sym.file_path.clone()).or_default().push(sym);
        }

        // Delete old vectors for all affected files
        for file_path in by_file.keys() {
            if let Some(old_ids) = self.file_ids.remove(file_path) {
                let old_count = old_ids.len();
                for id in old_ids {
                    let _ = self.db.delete(id);
                }
                tracing::debug!("[VECTOR_STORE] Removed {} old vectors for {}", old_count, file_path);
            }
        }

        // Process in chunks of 100 to avoid memory explosion
        const CHUNK_SIZE: usize = 100;
        let mut total_count = 0;
        let mut new_ids: HashMap<String, Vec<u64>> = HashMap::new();
        let total_symbols = all_symbols.len();

        for (chunk_idx, chunk) in all_symbols.chunks(CHUNK_SIZE).enumerate() {
            // Collect signatures for this chunk
            let signatures: Vec<&str> = chunk.iter()
                .map(|s| s.signature.as_str())
                .collect();
            
            // Embed this chunk
            let embeddings = self.embedding_model.embed_batch(&signatures)?;

            // Insert all symbols in this chunk
            for (symbol, embedding) in chunk.iter().zip(embeddings.iter()) {
                let id = self.db.insert(
                    embedding,
                    json!({
                        "file_path": symbol.file_path,
                        "symbol_name": symbol.name,
                        "symbol_type": symbol.kind.to_string(),
                        "language": symbol.language,
                        "start_line": symbol.start_line,
                        "end_line": symbol.end_line,
                        "signature": symbol.signature,
                        "parent": symbol.parent,
                    }),
                ).map_err(|e| anyhow::anyhow!("TriviumDB insert error: {e}"))?;

                new_ids.entry(symbol.file_path.clone()).or_default().push(id);
                total_count += 1;
            }

            // Log progress every chunk
            let processed = ((chunk_idx + 1) * CHUNK_SIZE).min(total_symbols);
            tracing::debug!("[VECTOR_STORE] Embedding progress: {}/{} symbols", processed, total_symbols);
        }

        // Update file_ids tracking
        self.file_ids.extend(new_ids);

        Ok(total_count)
    }

    /// Semantic search: embed query → find top-K symbols by cosine similarity.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<Symbol>> {
        let query_embedding = self.embedding_model.embed(query)?;

        // triviumdb search(query_vector, top_k, expand_depth, min_score)
        let results = self.db.search(
            &query_embedding,
            top_k,
            0,    // no graph expansion
            0.0,  // no minimum score threshold
        ).map_err(|e| anyhow::anyhow!("TriviumDB search error: {e}"))?;

        let symbols = results.into_iter().map(|r| {
            let payload = &r.payload;
            Symbol {
                name: payload["symbol_name"].as_str().unwrap_or("").to_string(),
                kind: SymbolType::from_str(
                    payload["symbol_type"].as_str().unwrap_or("function")
                ).unwrap_or(SymbolType::Function),
                start_line: payload["start_line"].as_u64().unwrap_or(0) as usize,
                end_line: payload["end_line"].as_u64().unwrap_or(0) as usize,
                file_path: payload["file_path"].as_str().unwrap_or("").to_string(),
                language: payload["language"].as_str().unwrap_or("").to_string(),
                signature: payload["signature"].as_str().unwrap_or("").to_string(),
                parent: payload["parent"].as_str().map(|s| s.to_string()),
                fq_name: payload["fq_name"].as_str().unwrap_or("").to_string(),
                calls: payload["calls"].as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default(),
            }
        }).collect();

        Ok(symbols)
    }
}
