//! Hybrid chunking strategy for embedding-based compression.
//!
//! This module provides a hybrid approach for splitting messages into chunks:
//! - Short messages (< threshold): Keep as single chunk
//! - Long messages (>= threshold): Split by token count with overlap
//!
//! ## Strategy
//!
//! 1. Tokenize each message
//! 2. If tokens < threshold: treat as single chunk
//! 3. If tokens >= threshold: split into sub-chunks of max_chunk_tokens each
//! 4. Track mapping from chunk index back to original message
//!
//! This ensures:
//! - No chunk exceeds model's max_position_embeddings
//! - Short messages don't get over-split
//! - KadaneDial operates on semantic meaningful units

use crate::message::Message;
use tokenizers::Tokenizer;

/// Maximum tokens before chunking (soft limit).
/// Messages shorter than this are kept as single chunks.
const DEFAULT_CHUNK_THRESHOLD: usize = 256;

/// Maximum tokens per chunk when splitting long messages.
const DEFAULT_MAX_CHUNK_TOKENS: usize = 512;

/// A chunk of text derived from a message.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The text content of this chunk.
    pub text: String,
    /// Role prefix for reconstruction (e.g., "User: ", "Assistant: ").
    pub role_prefix: String,
    /// Index of the original message this chunk came from.
    pub message_idx: usize,
    /// Chunk index within the message (0 for single-chunk messages).
    pub chunk_idx: usize,
    /// Total chunks in the original message.
    pub total_chunks: usize,
}

/// Configuration for chunking behavior.
#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Token count threshold: messages below this are single chunks.
    pub threshold_tokens: usize,
    /// Maximum tokens per chunk when splitting.
    pub max_chunk_tokens: usize,
    /// Overlap ratio between adjacent chunks (0.0 - 0.2 recommended).
    /// This ensures semantic continuity across chunks.
    pub overlap_ratio: f32,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            threshold_tokens: DEFAULT_CHUNK_THRESHOLD,
            max_chunk_tokens: DEFAULT_MAX_CHUNK_TOKENS,
            overlap_ratio: 0.15, // 15% overlap by default
        }
    }
}

/// Simple tokenizer wrapper for chunking decisions.
pub struct SimpleChunker {
    tokenizer: Tokenizer,
}

impl SimpleChunker {
    /// Create a new chunker with a tokenizer.
    pub fn new(tokenizer: Tokenizer) -> Self {
        Self { tokenizer }
    }

    /// Count tokens in text.
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.len())
            .unwrap_or(text.len() / 4) // Fallback: roughly 4 chars per token
    }

    /// Split text into chunks by token count with sliding window overlap.
    /// 
    /// This implements the "semantic chunking + sliding window" strategy:
    /// - Chunks are limited to max_tokens
    /// - Adjacent chunks overlap by overlap_ratio (e.g., 15%)
    /// - Ensures semantic continuity across chunk boundaries
    pub fn split_text_with_overlap(&self, text: &str, max_tokens: usize, overlap_ratio: f32) -> Vec<String> {
        if text.is_empty() {
            return vec![];
        }

        let encoding = match self.tokenizer.encode(text, false) {
            Ok(e) => e,
            Err(_) => return vec![text.to_string()],
        };

        let ids: Vec<u32> = encoding.get_ids().to_vec();
        if ids.len() <= max_tokens {
            return vec![text.to_string()];
        }

        // Calculate overlap size
        let overlap_tokens = (max_tokens as f32 * overlap_ratio) as usize;
        let stride = max_tokens.saturating_sub(overlap_tokens);
        
        let mut chunks = Vec::new();
        let mut start = 0;

        while start < ids.len() {
            let end = (start + max_tokens).min(ids.len());
            let chunk_ids = &ids[start..end];

            // Convert token IDs back to text
            let chunk_text = self.tokenizer.decode(chunk_ids, true).unwrap_or_else(|_| {
                // Fallback: extract substring by characters
                let char_count = text.len() * chunk_ids.len() / ids.len();
                let start_char = text.len() * start / ids.len();
                text[start_char..(start_char + char_count).min(text.len())].to_string()
            });

            if !chunk_text.trim().is_empty() {
                chunks.push(chunk_text.trim().to_string());
            }

            // Move forward by stride (not full max_tokens) to create overlap
            start += stride;
            
            // If we're at the last chunk and it's very small, merge with previous
            if start >= ids.len() && chunks.len() > 1 {
                let last_chunk = chunks.pop().unwrap();
                if let Some(prev_chunk) = chunks.last_mut() {
                    prev_chunk.push_str(" ");
                    prev_chunk.push_str(&last_chunk);
                }
            }
        }

        if chunks.is_empty() {
            chunks.push(text.to_string());
        }

        chunks
    }

    /// Split text into chunks by token count (legacy method without overlap).
    pub fn split_text(&self, text: &str, max_tokens: usize) -> Vec<String> {
        self.split_text_with_overlap(text, max_tokens, 0.0)
    }
}

/// Convert messages to chunks using hybrid strategy with sliding window overlap.
///
/// - Short messages: one chunk per message
/// - Long messages: split into multiple chunks with overlap (config.overlap_ratio)
pub fn message_to_chunks(
    messages: &[Message],
    chunker: &SimpleChunker,
    config: &ChunkerConfig,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();

    for (msg_idx, msg) in messages.iter().enumerate() {
        let (role_prefix, content) = match msg {
            Message::User { content } => ("User: ".to_string(), content.as_str()),
            Message::Assistant { content, .. } => ("Assistant: ".to_string(), content.as_str()),
            Message::System { content } => ("System: ".to_string(), content.as_str()),
            Message::ToolResult { content, .. } => ("Tool: ".to_string(), content.as_str()),
        };

        let token_count = chunker.count_tokens(content);

        if token_count < config.threshold_tokens {
            // Short message: single chunk
            chunks.push(Chunk {
                text: content.to_string(),
                role_prefix,
                message_idx: msg_idx,
                chunk_idx: 0,
                total_chunks: 1,
            });
        } else {
            // Long message: split into multiple chunks with overlap
            let text_chunks = chunker.split_text_with_overlap(
                content,
                config.max_chunk_tokens,
                config.overlap_ratio,
            );
            let total = text_chunks.len();

            for (idx, text) in text_chunks.into_iter().enumerate() {
                chunks.push(Chunk {
                    text,
                    role_prefix: role_prefix.clone(),
                    message_idx: msg_idx,
                    chunk_idx: idx,
                    total_chunks: total,
                });
            }
        }
    }

    chunks
}

/// Alias for creating SimpleChunker from embedder's tokenizer.
/// This is a convenience function that wraps the chunking logic.
pub fn chunker(tokenizer: Tokenizer, config: &ChunkerConfig, messages: &[Message]) -> Vec<Chunk> {
    let simple_chunker = SimpleChunker::new(tokenizer);
    message_to_chunks(messages, &simple_chunker, config)
}
///
/// When KadaneDial selects chunk indices, we need to reconstruct
/// the original messages, merging chunks that belong to the same message.
pub fn chunks_to_messages(
    chunks: &[Chunk],
    selected_chunk_indices: &[usize],
    original_messages: &[Message],
) -> Vec<Message> {
    if selected_chunk_indices.is_empty() || original_messages.is_empty() {
        return vec![];
    }

    // Find which messages are referenced by selected chunks
    let mut selected_message_indices: Vec<usize> = selected_chunk_indices
        .iter()
        .filter_map(|i| chunks.get(*i).map(|c| c.message_idx))
        .collect();
    selected_message_indices.sort();
    selected_message_indices.dedup();

    // Collect selected message IDs first
    let selected_ids: std::collections::HashSet<usize> =
        selected_message_indices.iter().cloned().collect();

    // Reconstruct messages in original form.
    // Tool pair sanitization (orphaned ToolResult/tool_calls) is handled
    // downstream by context_builder after budget truncation.
    let mut result: Vec<Message> = Vec::new();

    for &msg_idx in &selected_message_indices {
        if let Some(msg) = original_messages.get(msg_idx).cloned() {
            result.push(msg);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunker_config_default() {
        let config = ChunkerConfig::default();
        assert_eq!(config.threshold_tokens, 256);
        assert_eq!(config.max_chunk_tokens, 512);
    }
}
