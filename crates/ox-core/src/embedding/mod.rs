//! Embedding-based session compression using KadaneDial algorithm.
//!
//! This module provides semantic context compression for long conversations
//! by combining BGE embedding models with the KadaneDial algorithm.
//!
//! ## KadaneDial Algorithm
//!
//! KadaneDial adapts the classic Kadane's maximum subarray algorithm for
//! selecting relevant conversation segments. Given a sequence of relevance
//! scores between the current query and conversation history, it finds
//! contiguous segments with maximum cumulative gain.
//!
//! ### Algorithm Flow
//!
//! 1. **Embedding**: Encode current query and each conversation turn into vectors
//! 2. **Scoring**: Compute cosine similarity between query and history vectors
//! 3. **Standardization**: Apply z-score normalization to scores
//! 4. **Gain Calculation**: Subtract threshold τ to get gain values
//! 5. **Kadane Search**: Find max-gain contiguous segments iteratively
//! 6. **Output**: Return selected segments ordered by time

pub mod bge;
pub mod chunker;
pub mod kadane;

pub use bge::{cosine_similarity, BgeEmbedder};
pub use chunker::{message_to_chunks, chunks_to_messages, ChunkerConfig, SimpleChunker};
pub use kadane::{compress_with_kadane, filter_messages, KadaneConfig};

use std::sync::Arc;

use crate::llm::tokenizer::estimate_tokens;
use crate::message::Message;
use anyhow::Result;

/// Compression manager that handles compression triggering logic.
///
/// This struct encapsulates the logic for determining when to compress
/// and performing the compression, reducing coupling between UI and compression.
pub struct CompressionManager {
    embedder: Arc<BgeEmbedder>,
    kadane_config: KadaneConfig,
    history_ratio: f32,
}

impl Clone for CompressionManager {
    fn clone(&self) -> Self {
        Self {
            embedder: Arc::clone(&self.embedder),
            kadane_config: self.kadane_config.clone(),
            history_ratio: self.history_ratio,
        }
    }
}

impl CompressionManager {
    /// Create a new CompressionManager with the given embedder and config.
    pub fn new(embedder: BgeEmbedder, kadane_config: KadaneConfig, history_ratio: f32) -> Self {
        Self {
            embedder: Arc::new(embedder),
            kadane_config,
            history_ratio,
        }
    }

    /// Calculate total tokens in the message history (for compression trigger).
    /// Includes both content and tool_calls for accurate token estimation.
    pub fn calculate_context_tokens(&self, messages: &[Message]) -> usize {
        messages
            .iter()
            .map(|m| {
                let tokens = match m {
                    Message::System { content } => estimate_tokens(content),
                    Message::User { content } => estimate_tokens(content),
                    Message::ToolResult { content, .. } => estimate_tokens(content),
                    Message::Assistant { content, tool_calls } => {
                        let mut t = estimate_tokens(content);
                        // Include tool_calls tokens (name + arguments + overhead)
                        for tc in tool_calls {
                            t += estimate_tokens(&tc.name);
                            t += estimate_tokens(&tc.arguments);
                            t += 10; // tool call structure overhead
                        }
                        t
                    }
                };
                tokens as usize
            })
            .sum()
    }

    /// Check if compression should be triggered based on context window and history ratio.
    pub fn should_compress(&self, messages: &[Message], context_window: u32) -> bool {
        let context_tokens = self.calculate_context_tokens(messages);
        let history_budget = (context_window as f32 * self.history_ratio) as usize;
        context_tokens >= history_budget
    }

    /// Perform compression on the given messages.
    /// Returns None if compression fails or is not needed.
    pub fn compress(&self, messages: &[Message], query: &str) -> Result<Option<Vec<Message>>> {
        compress_context(&self.embedder, query, messages, &self.kadane_config)
    }

    /// Get the embedder reference for batch operations.
    pub fn embedder(&self) -> &BgeEmbedder {
        &self.embedder
    }

    /// Get the KadaneConfig reference.
    pub fn kadane_config(&self) -> &KadaneConfig {
        &self.kadane_config
    }
}

impl std::fmt::Debug for CompressionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressionManager")
            .field("history_ratio", &self.history_ratio)
            .field("kadane_config", &self.kadane_config)
            .finish()
    }
}

/// Compress conversation context using BGE embeddings and KadaneDial algorithm.
///
/// Given the current user query, all session messages, and the embedder,
/// this function:
/// 1. Chunks messages (short messages = single chunk, long messages = multiple chunks)
/// 2. Encodes the query and each chunk into vectors
/// 3. Computes cosine similarity between query and each chunk
/// 4. Runs KadaneDial to select relevant chunks
/// 5. Reconstructs and returns compressed message list
pub fn compress_context(
    embedder: &BgeEmbedder,
    query: &str,
    messages: &[Message],
    config: &KadaneConfig,
) -> Result<Option<Vec<Message>>> {
    if messages.is_empty() {
        return Ok(None);
    }

    // Build chunks from messages
    let chunker_config = ChunkerConfig {
        threshold_tokens: config.chunk_threshold_tokens,
        max_chunk_tokens: config.max_chunk_tokens,
    };
    let simple_chunker = SimpleChunker::new(embedder.tokenizer().clone());
    let chunks = message_to_chunks(messages, &simple_chunker, &chunker_config);

    // Encode query
    let query_emb = embedder.encode(query)?;

    // Build chunk texts for encoding
    let chunk_texts: Vec<String> = chunks
        .iter()
        .map(|c| format!("{}{}", c.role_prefix, c.text))
        .collect();

    // Encode chunks in batch
    let chunk_embeddings = embedder.encode_batch(
        &chunk_texts.iter().map(|s| s.as_str()).collect::<Vec<_>>()
    )?;

    // Compute similarity scores for chunks
    let scores: Vec<f32> = chunk_embeddings
        .iter()
        .map(|emb| cosine_similarity(&query_emb, emb))
        .collect();

    // Run KadaneDial on chunk scores
    let result = compress_with_kadane(&scores, config);

    if result.indices.is_empty() {
        return Ok(None);
    }

    // Reconstruct messages from selected chunk indices
    let compressed = chunks_to_messages(&chunks, &result.indices, messages);

    Ok(Some(compressed))
}
