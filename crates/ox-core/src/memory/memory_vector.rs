//! Vector-backed semantic search for memory nodes.
//!
//! Uses the same embedding model (BERT MiniLM) + TriviumDB as the symbol vector store,
//! enabling true semantic similarity search over stored memories.
//! This complements the existing keyword-based SQLite search (PATH 1–4)
//! with a PATH 5 that catches semantically similar memories that don't share exact keywords.

use std::sync::Arc;
use anyhow::Result;
use serde_json::json;
use triviumdb;

use crate::config::EmbeddingConfig;
use crate::symbol::embedding::EmbeddingModel;
use super::{MemoryNode, MemoryNodeType};

/// Vector store for semantic memory search.
/// Shares the embedding model with the symbol vector store via Arc.
pub struct MemoryVectorStore {
    db: triviumdb::Database<f32>,
    embedding_model: Arc<EmbeddingModel>,
    dimension: usize,
}

/// A search result from the memory vector store.
pub struct MemorySearchHit {
    pub node_id: String,
    pub content: String,
    pub node_type: MemoryNodeType,
    pub project_id: Option<String>,
    pub score: f32,
}

impl MemoryVectorStore {
    /// Open or create the memory vector database.
    ///
    /// # Arguments
    /// * `db_path` - Path to the TriviumDB file (e.g. `~/.ox/db/memories.tdb`)
    /// * `embedding_model` - Shared embedding model (from symbol vector store)
    /// * `config` - Embedding configuration (for dimension)
    pub fn open(
        db_path: &str,
        embedding_model: Arc<EmbeddingModel>,
        config: &EmbeddingConfig,
    ) -> Result<Self> {
        let dim = config.dimension;
        let db = triviumdb::Database::<f32>::open(db_path, dim)
            .map_err(|e| anyhow::anyhow!("Failed to open memory TriviumDB at {db_path}: {e}"))?;

        tracing::info!("[MEMORY_VECTOR] TriviumDB opened at {db_path} (dim={dim})");

        Ok(Self {
            db,
            embedding_model,
            dimension: dim,
        })
    }

    /// Create a new MemoryVectorStore with its own embedding model.
    /// Use this when the symbol vector store is not available.
    pub fn open_standalone(db_path: &str, config: &EmbeddingConfig) -> Result<Self> {
        let model = EmbeddingModel::with_config(config)?;
        Self::open(db_path, Arc::new(model), config)
    }

    /// Index a memory node (embed content + store metadata).
    pub fn index_node(&mut self, node: &MemoryNode) -> Result<()> {
        let text = self.node_to_text(node);
        let embedding = self.embedding_model.embed(&text)?;

        let _id = self.db.insert(
            &embedding,
            json!({
                "node_id": node.id,
                "content": node.content,
                "node_type": node.node_type.as_str(),
                "project_id": node.project_id,
                "depth": node.depth,
                "source": format!("{:?}", node.source),
                "created_at": node.created_at,
            }),
        ).map_err(|e| anyhow::anyhow!("Memory TriviumDB insert error: {e}"))?;

        tracing::debug!(
            "[MEMORY_VECTOR] Indexed node '{}' (type={}, {} chars)",
            node.id, node.node_type.as_str(), node.content.len()
        );

        Ok(())
    }

    /// Batch-index multiple memory nodes.
    pub fn index_batch(&mut self, nodes: &[MemoryNode]) -> Result<usize> {
        if nodes.is_empty() {
            return Ok(0);
        }

        let texts: Vec<String> = nodes.iter().map(|n| self.node_to_text(n)).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let embeddings = self.embedding_model.embed_batch(&text_refs)?;

        let mut count = 0;
        for (node, embedding) in nodes.iter().zip(embeddings.iter()) {
            match self.db.insert(
                embedding,
                json!({
                    "node_id": node.id,
                    "content": node.content,
                    "node_type": node.node_type.as_str(),
                    "project_id": node.project_id,
                    "depth": node.depth,
                    "source": format!("{:?}", node.source),
                    "created_at": node.created_at,
                }),
            ) {
                Ok(_) => count += 1,
                Err(e) => {
                    tracing::debug!("[MEMORY_VECTOR] Failed to index node {}: {e}", node.id);
                }
            }
        }

        tracing::info!("[MEMORY_VECTOR] Batch indexed {}/{} memory nodes", count, nodes.len());
        Ok(count)
    }

    /// Semantic search: embed query → find top-K memories by cosine similarity.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<MemorySearchHit>> {
        let query_embedding = self.embedding_model.embed(query)?;

        let results = self.db.search(
            &query_embedding,
            top_k,
            0,    // no graph expansion
            0.3,  // minimum similarity threshold
        ).map_err(|e| anyhow::anyhow!("Memory TriviumDB search error: {e}"))?;

        let hits: Vec<MemorySearchHit> = results
            .into_iter()
            .filter_map(|r| {
                let payload = &r.payload;
                let node_id = payload["node_id"].as_str()?.to_string();
                let content = payload["content"].as_str().unwrap_or("").to_string();
                let node_type = MemoryNodeType::from_str(
                    payload["node_type"].as_str().unwrap_or("fact")
                ).unwrap_or(MemoryNodeType::Fact);
                let project_id = payload["project_id"].as_str().map(|s| s.to_string());

                Some(MemorySearchHit {
                    node_id,
                    content,
                    node_type,
                    project_id,
                    score: r.score,
                })
            })
            .collect();

        tracing::debug!(
            "[MEMORY_VECTOR] Search '{}' → {} hits (threshold=0.3)",
            query, hits.len()
        );

        Ok(hits)
    }

    /// Convert a MemoryNode to a rich text for embedding.
    /// Includes type context + content for better semantic matching.
    fn node_to_text(&self, node: &MemoryNode) -> String {
        let type_label = match node.node_type {
            MemoryNodeType::Architectural => "architecture design",
            MemoryNodeType::BestPractice => "best practice pattern",
            MemoryNodeType::AntiPattern => "anti-pattern warning",
            MemoryNodeType::Style => "coding style convention",
            MemoryNodeType::Pattern => "code pattern",
            MemoryNodeType::MetaSkill => "skill technique",
            MemoryNodeType::Business => "business logic domain",
            MemoryNodeType::Fact => "fact knowledge",
        };
        // Truncate long content to ~500 chars for embedding efficiency (safe UTF-8 boundary)
        let content = if node.content.len() > 500 {
            let boundary = node.content.char_indices()
                .take_while(|(i, _)| *i < 500)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(node.content.len());
            &node.content[..boundary]
        } else {
            &node.content
        };
        format!("[{type_label}] {content}")
    }

    /// Get the embedding dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}
