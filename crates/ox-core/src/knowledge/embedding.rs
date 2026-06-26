/// Knowledge embedding module — re-exports and enhances the EmbeddingModel
/// from symbol/embedding.rs with Arc-sharing semantics.
///
/// The embedding model (all-MiniLM-L6-v2, 384-dim BERT) is heavyweight (~90MB).
/// To avoid loading multiple instances, all consumers should share a single
/// `Arc<EmbeddingModel>` via `KnowledgeEngine`.
use std::sync::Arc;

// Re-export the core model type from symbol/embedding.rs
use crate::config::EmbeddingConfig;
pub use crate::symbol::embedding::EmbeddingModel;

/// Convenience: load the model and return it wrapped in Arc for sharing.
pub fn load_shared(config: &EmbeddingConfig) -> anyhow::Result<Arc<EmbeddingModel>> {
    let model = EmbeddingModel::with_config(config)?;
    Ok(Arc::new(model))
}
