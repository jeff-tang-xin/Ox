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
pub mod reranker;

pub use bge::{BgeEmbedder, cosine_similarity};
pub use chunker::{ChunkerConfig, SimpleChunker, chunks_to_messages, message_to_chunks};
pub use kadane::{KadaneConfig, compress_with_kadane, filter_messages};
pub use reranker::{LlmReranker, RerankerConfig, MemoryJudgment};

use std::sync::Arc;

use crate::context::sanitize_tool_pairs;
use crate::llm::tokenizer::estimate_tokens;
use crate::message::Message;
use anyhow::Result;

/// ModelScope repository URLs for BGE models.
pub const MODELSCOPE_BGE_SMALL_ZH: &str =
    "https://www.modelscope.cn/AI-ModelScope/bge-small-zh-v1.5.git";
pub const MODELSCOPE_BGE_BASE_ZH: &str =
    "https://www.modelscope.cn/AI-ModelScope/bge-base-zh-v1.5.git";
pub const MODELSCOPE_BGE_LARGE_ZH: &str =
    "https://www.modelscope.cn/AI-ModelScope/bge-large-zh-v1.5.git";

/// Download a BGE model from ModelScope using git clone.
///
/// # Arguments
/// * `model_name` - Model name (e.g., "bge-small-zh-v1.5", "bge-base-zh-v1.5", "bge-large-zh-v1.5")
/// * `target_dir` - Target directory for the model (e.g., ~/.ox/models/bge-small-zh-v1.5)
///
/// # Returns
/// * `Ok(())` if download succeeds
/// * `Err` if git is not available or download fails
pub fn download_model(model_name: &str, target_dir: &std::path::Path) -> Result<()> {
    // Determine the ModelScope URL based on model name
    let repo_url = match model_name {
        "bge-base-zh-v1.5" => MODELSCOPE_BGE_BASE_ZH,
        "bge-large-zh-v1.5" => MODELSCOPE_BGE_LARGE_ZH,
        _ => MODELSCOPE_BGE_SMALL_ZH, // Default to small model
    };

    tracing::info!("Downloading model {} from ModelScope...", model_name);
    tracing::info!("Target directory: {:?}", target_dir);

    // Check if target directory already exists
    if target_dir.exists() {
        return Err(anyhow::anyhow!(
            "Model directory already exists: {:?}. Remove it first if you want to re-download.",
            target_dir
        ));
    }

    // Create parent directory if needed
    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Execute git clone with progress output
    tracing::info!("Starting git clone...");
    let mut child = std::process::Command::new("git")
        .args(&["clone", repo_url])
        .arg(target_dir.to_str().unwrap())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to execute git clone: {}. Is git installed?", e))?;

    // Read and log progress in real-time
    use std::io::{BufRead, BufReader};

    if let Some(stderr) = child.stderr.take() {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                // Log progress lines
                if !line.is_empty() {
                    tracing::info!("[git] {}", line);
                }
            }
        }
    }

    // Wait for git clone to complete
    let status = child
        .wait()
        .map_err(|e| anyhow::anyhow!("Failed to wait for git clone: {}", e))?;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "Git clone failed with exit code: {}",
            status.code().unwrap_or(-1)
        ));
    }

    tracing::info!("Verifying downloaded files...");

    // Verify that essential files exist
    let required_files = ["model.safetensors", "tokenizer.json", "config.json"];
    let mut missing_count = 0;
    for file in &required_files {
        let file_path = target_dir.join(file);
        if !file_path.exists() {
            tracing::warn!("Warning: Expected file not found: {:?}", file_path);
            missing_count += 1;
        } else {
            let file_size = std::fs::metadata(&file_path)?.len();
            tracing::info!("✓ {} ({:.2} MB)", file, file_size as f64 / 1024.0 / 1024.0);
        }
    }

    if missing_count > 0 {
        tracing::warn!("Warning: {} expected files are missing", missing_count);
    }

    tracing::info!("Model downloaded successfully to {:?}", target_dir);
    Ok(())
}

/// Re-rank memory nodes using cross-encoding with BGE embedder.
/// 
/// This performs pairwise similarity scoring between query and each memory,
/// providing more accurate relevance than simple keyword matching.
/// 
/// # Arguments
/// * `embedder` - BGE embedding model
/// * `query` - The search query
/// * `memories` - List of memory nodes to re-rank
/// * `top_k` - Number of top results to return
/// 
/// # Returns
/// Re-ranked list of memory nodes, sorted by relevance score (descending)
pub fn rerank_memories(
    embedder: &BgeEmbedder,
    query: &str,
    memories: Vec<crate::memory::MemoryNode>,
    top_k: usize,
) -> Result<Vec<crate::memory::MemoryNode>> {
    if memories.is_empty() {
        return Ok(vec![]);
    }
    
    tracing::debug!("[RERANK] Re-ranking {} memories for query: {}", memories.len(), query.chars().take(50).collect::<String>());
    
    // Encode query once
    let query_emb = embedder.encode(query)?;
    
    // Compute cross-encoding scores
    let mut scored_memories: Vec<(crate::memory::MemoryNode, f32)> = Vec::with_capacity(memories.len());
    
    for memory in memories {
        // Create pair text for cross-encoding
        // Format: "Query: {query}\nDocument: {content}"
        let pair_text = format!("Query: {}\nDocument: {}", query, memory.content);
        
        // Encode the pair
        let pair_emb = embedder.encode(&pair_text)?;
        
        // Calculate cosine similarity as rerank score
        let score = cosine_similarity(&query_emb, &pair_emb);
        
        tracing::debug!("[RERANK] Memory score: {:.4} for content: {}", score, memory.content.chars().take(40).collect::<String>());
        
        scored_memories.push((memory, score));
    }
    
    // Sort by score descending
    scored_memories.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });
    
    // Take top-k
    scored_memories.truncate(top_k);
    
    tracing::info!("[RERANK] Top {} scores: {:?}", 
        top_k.min(scored_memories.len()),
        scored_memories.iter().map(|(_, s)| format!("{:.3}", s)).collect::<Vec<_>>()
    );
    
    // Extract just the memories
    Ok(scored_memories.into_iter().map(|(m, _)| m).collect())
}

/// Compression level for different scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    /// Light compression: only remove tool call details, keep summaries
    Light,
    /// Medium compression: semantic relevance-based selection (default)
    Medium,
    /// Heavy compression: aggressive selection with memory-based summarization
    Heavy,
}

impl Default for CompressionLevel {
    fn default() -> Self {
        CompressionLevel::Medium
    }
}

/// Compression manager that handles compression triggering logic.
///
/// This struct encapsulates the logic for determining when to compress
/// and performing the compression, reducing coupling between UI and compression.
pub struct CompressionManager {
    embedder: Arc<BgeEmbedder>,
    kadane_config: KadaneConfig,
    history_ratio: f32,
    default_level: CompressionLevel,
}

impl Clone for CompressionManager {
    fn clone(&self) -> Self {
        Self {
            embedder: Arc::clone(&self.embedder),
            kadane_config: self.kadane_config.clone(),
            history_ratio: self.history_ratio,
            default_level: self.default_level,
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
            default_level: CompressionLevel::Medium,
        }
    }

    /// Create a new CompressionManager with a specific compression level.
    pub fn with_level(
        embedder: BgeEmbedder,
        kadane_config: KadaneConfig,
        history_ratio: f32,
        level: CompressionLevel,
    ) -> Self {
        Self {
            embedder: Arc::new(embedder),
            kadane_config,
            history_ratio,
            default_level: level,
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
                    Message::Assistant {
                        content,
                        tool_calls,
                    } => {
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

        // Check token-based trigger
        let token_based = context_tokens >= history_budget;

        // Check structure-based trigger
        let structure_based = has_incomplete_task_context(messages);

        token_based || structure_based
    }

    /// Smart compression trigger that considers both token count and dialogue structure.
    pub fn should_compress_smart(&self, messages: &[Message], context_window: u32) -> bool {
        let context_tokens = self.calculate_context_tokens(messages);
        let history_budget = (context_window as f32 * self.history_ratio) as usize;

        // Token-based trigger (80% of budget to allow some headroom)
        let token_trigger = context_tokens >= (history_budget as f32 * 0.8) as usize;

        // Structure-based triggers
        let structure_trigger = has_incomplete_task_context(messages)
            || has_growing_tool_interactions(messages)
            || has_topic_drift(messages);

        // Only compress if we have enough messages to make it worthwhile
        let has_enough_messages = messages.len() > 10;

        (token_trigger || structure_trigger) && has_enough_messages
    }

    /// Perform compression on the given messages.
    /// Returns None if compression fails or is not needed.
    pub fn compress(&self, messages: &[Message], query: &str) -> Result<Option<Vec<Message>>> {
        self.compress_with_level(messages, query, self.default_level, None)
    }

    /// Perform compression with memory context for better relevance.
    pub fn compress_with_memory(
        &self,
        messages: &[Message],
        query: &str,
        memory_context: Option<&str>,
    ) -> Result<Option<Vec<Message>>> {
        self.compress_with_level(messages, query, self.default_level, memory_context)
    }

    /// Perform compression with a specific level and optional memory context.
    pub fn compress_with_level(
        &self,
        messages: &[Message],
        query: &str,
        level: CompressionLevel,
        memory_context: Option<&str>,
    ) -> Result<Option<Vec<Message>>> {
        match level {
            CompressionLevel::Light => self.light_compress(messages, query),
            CompressionLevel::Medium => compress_context_enhanced(
                &self.embedder,
                query,
                messages,
                &self.kadane_config,
                memory_context,
            ),
            CompressionLevel::Heavy => {
                // For heavy compression, we use stricter Kadane config
                let mut strict_config = self.kadane_config.clone();
                strict_config.threshold += 0.2; // Higher threshold for more aggressive filtering
                strict_config.max_segments = 3; // Fewer segments
                compress_context_enhanced(
                    &self.embedder,
                    query,
                    messages,
                    &strict_config,
                    memory_context,
                )
            }
        }
    }

    /// Light compression: remove tool call details but keep structure.
    fn light_compress(&self, messages: &[Message], _query: &str) -> Result<Option<Vec<Message>>> {
        // For light compression, we just truncate long tool results
        let mut compressed = Vec::with_capacity(messages.len());
        for msg in messages {
            match msg {
                Message::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    // Truncate very long tool results
                    let truncated = if content.chars().count() > 500 {
                        let preview: String = content.chars().take(200).collect();
                        format!("{}...[truncated]", preview)
                    } else {
                        content.clone()
                    };
                    compressed.push(Message::ToolResult {
                        tool_call_id: tool_call_id.clone(),
                        content: truncated,
                    });
                }
                _ => compressed.push(msg.clone()),
            }
        }
        Ok(Some(compressed))
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
    compress_context_enhanced(embedder, query, messages, config, None)
}

/// Enhanced version of compress_context that can incorporate memory context
/// and recent conversation history for better relevance scoring.
pub fn compress_context_enhanced(
    embedder: &BgeEmbedder,
    query: &str,
    messages: &[Message],
    config: &KadaneConfig,
    memory_context: Option<&str>,
) -> Result<Option<Vec<Message>>> {
    if messages.is_empty() {
        return Ok(None);
    }

    // Build enriched query by combining current query with recent context
    let enriched_query = build_enriched_query(query, messages, memory_context);

    // Identify protected message indices that should not be compressed
    let protected_indices = identify_protected_context(messages);

    // Build chunks from messages
    let chunker_config = ChunkerConfig {
        threshold_tokens: config.chunk_threshold_tokens,
        max_chunk_tokens: config.max_chunk_tokens,
        overlap_ratio: 0.15, // 15% overlap for semantic continuity
    };
    let simple_chunker = SimpleChunker::new(embedder.tokenizer().clone());
    let chunks = message_to_chunks(messages, &simple_chunker, &chunker_config);

    // Encode enriched query
    let query_emb = embedder.encode(&enriched_query)?;

    // Build chunk texts for encoding
    let chunk_texts: Vec<String> = chunks
        .iter()
        .map(|c| format!("{}{}", c.role_prefix, c.text))
        .collect();

    // Encode chunks in batch
    let chunk_embeddings =
        embedder.encode_batch(&chunk_texts.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;

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
    let mut compressed = chunks_to_messages(&chunks, &result.indices, messages);

    // Always keep the first message (system context) and recent messages.
    // KadaneDial may drop them if their relevance score is low.
    let keep_recent = config.keep_recent;
    let recent_start = messages.len().saturating_sub(keep_recent);
    // Collect message indices already present in compressed
    let existing_indices: std::collections::HashSet<usize> = chunks
        .iter()
        .enumerate()
        .filter(|(ci, _)| result.indices.contains(ci))
        .map(|(_, c)| c.message_idx)
        .collect();

    // Add first message if missing
    if !messages.is_empty() && !existing_indices.contains(&0) {
        compressed.insert(0, messages[0].clone());
    }

    // Add protected messages that are missing
    for idx in &protected_indices {
        if !existing_indices.contains(idx) {
            // Insert in chronological order based on index
            let insert_pos = compressed
                .iter()
                .position(|m| {
                    // Find this message's original index
                    messages
                        .iter()
                        .enumerate()
                        .find(|(_, orig)| {
                            // Compare by content and role (simple heuristic)
                            match (orig, m) {
                                (Message::User { content: c1 }, Message::User { content: c2 }) => {
                                    c1 == c2
                                }
                                (
                                    Message::Assistant { content: c1, .. },
                                    Message::Assistant { content: c2, .. },
                                ) => c1 == c2,
                                (
                                    Message::ToolResult {
                                        tool_call_id: id1, ..
                                    },
                                    Message::ToolResult {
                                        tool_call_id: id2, ..
                                    },
                                ) => id1 == id2,
                                _ => false,
                            }
                        })
                        .map(|(pos, _)| pos)
                        .unwrap_or(0)
                        > *idx
                })
                .unwrap_or(compressed.len());
            compressed.insert(insert_pos, messages[*idx].clone());
        }
    }

    // Add recent messages if missing
    for idx in recent_start..messages.len() {
        if !existing_indices.contains(&idx) && !protected_indices.contains(&idx) {
            compressed.push(messages[idx].clone());
        }
    }

    // 🚨 SCHEME 2: Add compression notice to inform LLM about missing context
    // Insert a system message at the beginning to alert LLM about compression
    let compressed_count = messages.len().saturating_sub(compressed.len());
    if compressed_count > 0 {
        // Extract semantic summary from removed messages
        let removed_summary = extract_removed_message_summary(messages, &compressed);
        
        let notice = format!(
            "⚠️ Context compressed: {} msgs → {} msgs ({} removed).\n\
             Removed topics: {}\n\
             \n\
             💡 ACTION REQUIRED:\n\
             If your current task relates to any of the removed topics above,\n\
             you MUST use the `memory_search` tool to retrieve complete information.\n\
             Example: memory_search(query=\"{}\", scope=\"project\")",
            messages.len(),
            compressed.len(),
            compressed_count,
            removed_summary,
            removed_summary.split(':').next().unwrap_or("project architecture").trim()
        );
        
        // Insert notice as first message
        compressed.insert(0, Message::system(&notice));
    }

    // 🚨 CRITICAL FIX: Sanitize tool pairs after compression
    // Compression may have broken tool_call/ToolResult pairs, causing
    // "tool result's tool id not found" API errors. This ensures all
    // ToolResults have matching Assistant tool_calls before returning.
    sanitize_tool_pairs(&mut compressed);

    Ok(Some(compressed))
}

/// Identify message indices that should be protected from compression.
/// These include:
/// - Messages with pending tool calls
/// - Multi-turn task contexts (consecutive user-assistant exchanges on same topic)
/// - Messages explicitly referenced by user
/// - Task planning and reasoning chains (NEW)
/// - Key decisions and constraints (NEW)
fn identify_protected_context(messages: &[Message]) -> Vec<usize> {
    let mut protected = Vec::new();

    // Find messages with pending/incomplete tool interactions
    for (i, msg) in messages.iter().enumerate() {
        match msg {
            Message::Assistant { tool_calls, .. } => {
                // Check if any tool call doesn't have a corresponding result
                for tc in tool_calls {
                    let has_result = messages[i+1..].iter().any(|m| {
                        matches!(m, Message::ToolResult { tool_call_id, .. } if tool_call_id == &tc.id)
                    });
                    if !has_result {
                        protected.push(i);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    // 🚨 PROTECT REASONING CHAINS: Detect and protect logical reasoning patterns
    // Look for messages containing planning markers, step-by-step analysis, or decision points
    let reasoning_markers = [
        "step", "plan", "first", "second", "third", "finally",
        "therefore", "because", "however", "conclusion",
        "decision", "choose", "option", "alternative",
        "reasoning", "analysis", "consider", "evaluate",
        "must", "should", "constraint", "requirement",
    ];
    
    for (i, msg) in messages.iter().enumerate() {
        let content = match msg {
            Message::User { content } | Message::Assistant { content, .. } => content,
            _ => continue,
        };
        
        let lower = content.to_lowercase();
        let has_reasoning = reasoning_markers.iter().any(|&marker| {
            lower.contains(marker)
        });
        
        // Also check for structured lists (numbered or bulleted)
        let has_structure = content.contains("1.") || content.contains("2.") || 
                           content.contains("- ") || content.contains("•");
        
        if has_reasoning || has_structure {
            if !protected.contains(&i) {
                protected.push(i);
            }
        }
    }

    // 🚨 PROTECT TASK PLANNING: Detect multi-step task descriptions
    // Look for sequences where assistant provides detailed plans
    let mut i = 0;
    while i < messages.len() {
        if let Message::User {
            content: ref user_content,
        } = messages[i]
        {
            // Check if next few messages form a coherent task sequence
            let mut sequence_len = 1;
            let mut j = i + 1;

            while j < messages.len() && sequence_len < 6 {
                // Max 3 exchanges
                match &messages[j] {
                    Message::Assistant { .. } => {
                        sequence_len += 1;
                        j += 1;
                    }
                    Message::User { content } => {
                        // Check if this user message references previous context
                        if references_previous_context(content, &user_content) {
                            sequence_len += 1;
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    _ => break,
                }
            }

            // If we found a multi-turn sequence, protect all messages in it
            if sequence_len >= 4 {
                // At least 2 full exchanges
                for k in i..j {
                    if !protected.contains(&k) {
                        protected.push(k);
                    }
                }
            }
        }
        i += 1;
    }

    protected.sort();
    protected
}

/// Check if a user message references previous context.
fn references_previous_context(current: &str, _previous: &str) -> bool {
    // Simple heuristic: check for pronouns and reference words
    let refs = [
        "it",
        "this",
        "that",
        "the",
        "previous",
        "above",
        "mentioned",
        "earlier",
    ];
    let current_lower = current.to_lowercase();
    refs.iter().any(|&r| current_lower.contains(r))
}

/// Extract semantic summary from removed messages to guide LLM on what to search for.
/// Instead of just keywords, generates brief topic summaries with context.
fn extract_removed_message_summary(original: &[Message], compressed: &[Message]) -> String {
    // Build a set of content from compressed messages
    let compressed_content: std::collections::HashSet<String> = compressed
        .iter()
        .filter_map(|m| match m {
            Message::User { content } | Message::Assistant { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    
    // Find messages that were removed and group them by topic
    let mut topics = Vec::new();
    let mut current_topic = String::new();
    let mut topic_messages = 0;
    
    for msg in original {
        let (role, content) = match msg {
            Message::User { content } => ("User", content),
            Message::Assistant { content, .. } => ("Assistant", content),
            _ => continue,
        };
        
        // If this content is not in compressed messages, it was removed
        if !compressed_content.contains(content) {
            // Extract key information: file names, technical terms, actions
            let key_info = extract_key_information(content);
            
            if !key_info.is_empty() {
                if topic_messages == 0 {
                    current_topic = format!("{}: {}", role, key_info);
                } else if topic_messages < 3 {
                    // Limit each topic to 3 messages max
                    current_topic.push_str(", ");
                    current_topic.push_str(&key_info);
                }
                topic_messages += 1;
            }
        } else {
            // This message is preserved, so finalize current topic if any
            if !current_topic.is_empty() && topic_messages > 0 {
                topics.push(current_topic.clone());
                current_topic.clear();
                topic_messages = 0;
            }
        }
    }
    
    // Don't forget the last topic
    if !current_topic.is_empty() && topic_messages > 0 {
        topics.push(current_topic);
    }
    
    // Format output: limit to top 3-4 topics, keep concise
    if topics.is_empty() {
        "general discussion".to_string()
    } else {
        topics.truncate(4);
        topics.join("; ")
    }
}

/// Extract key information from a message content.
/// Focuses on: file names, function names, error messages, technical terms.
fn extract_key_information(content: &str) -> String {
    let mut key_parts = Vec::new();
    
    // 1. Extract file paths (e.g., "user_service.rs", "config.toml")
    let file_pattern = regex::Regex::new(r"[\w.-]+\.rs|[\w.-]+\.toml|[\w.-]+\.json|[\w.-]+\.md").unwrap();
    for mat in file_pattern.find_iter(content) {
        key_parts.push(mat.as_str().to_string());
    }
    
    // 2. Extract code identifiers (function names, variables)
    let ident_pattern = regex::Regex::new(r"`([\w_]+)`").unwrap();
    for mat in ident_pattern.find_iter(content) {
        let ident = mat.as_str().trim_matches('`');
        if ident.len() > 3 && !key_parts.contains(&ident.to_string()) {
            key_parts.push(ident.to_string());
        }
    }
    
    // 3. Extract action verbs + object patterns (e.g., "fix bug", "add validation")
    let action_pattern = regex::Regex::new(r"(?i)(?:fix|add|remove|update|create|delete|implement|refactor)\s+([\w\s]{5,30})").unwrap();
    for mat in action_pattern.captures_iter(content) {
        if let Some(action) = mat.get(1) {
            let action_text = action.as_str().trim();
            if action_text.len() > 5 && action_text.len() < 30 {
                key_parts.push(action_text.to_string());
            }
        }
    }
    
    // 4. If no specific patterns found, extract first meaningful phrase
    if key_parts.is_empty() {
        // Take first sentence or clause (up to 40 chars)
        let first_phrase = content.split(|c| c == '.' || c == '?' || c == '!')
            .next()
            .unwrap_or(content)
            .trim();
        
        if first_phrase.len() > 10 {
            let truncated: String = first_phrase.chars().take(40).collect();
            key_parts.push(truncated);
        }
    }
    
    // Join and limit
    key_parts.truncate(3);
    key_parts.join(", ")
}

/// Check if there are incomplete task contexts that need protection.
fn has_incomplete_task_context(messages: &[Message]) -> bool {
    // Look for assistant messages with tool calls that don't have results yet
    for msg in messages.iter().rev() {
        if let Message::Assistant { tool_calls, .. } = msg {
            if !tool_calls.is_empty() {
                // Check if all tool calls have corresponding results
                let all_complete = tool_calls.iter().all(|tc| {
                    messages.iter().any(|m| {
                        matches!(m, Message::ToolResult { tool_call_id, .. } if tool_call_id == &tc.id)
                    })
                });
                if !all_complete {
                    return true; // Found incomplete task
                }
            }
        }
    }
    false
}

/// Check if tool interactions are growing (many back-and-forth exchanges).
fn has_growing_tool_interactions(messages: &[Message]) -> bool {
    let mut tool_call_count = 0;
    let mut tool_result_count = 0;

    for msg in messages {
        match msg {
            Message::Assistant { tool_calls, .. } => {
                tool_call_count += tool_calls.len();
            }
            Message::ToolResult { .. } => {
                tool_result_count += 1;
            }
            _ => {}
        }
    }

    // If we have many tool calls but few results, or vice versa
    tool_call_count > 5 && (tool_result_count as f32 / tool_call_count.max(1) as f32) < 0.5
}

/// Check if there's topic drift (multiple different topics being discussed).
fn has_topic_drift(messages: &[Message]) -> bool {
    // Simple heuristic: count distinct user query patterns
    let mut user_queries = Vec::new();

    for msg in messages.iter().rev().take(20) {
        // Check last 20 messages
        if let Message::User { content } = msg {
            // Extract first sentence or key phrase as topic indicator
            let topic = content.split('.').next().unwrap_or(content).trim();
            if topic.len() > 10 {
                user_queries.push(topic.to_lowercase());
            }
        }
    }

    // If we have many distinct topics in recent history
    let unique_topics = user_queries
        .iter()
        .collect::<std::collections::HashSet<_>>();
    unique_topics.len() >= 4
}

/// Build an enriched query by combining the current user query with
/// recent conversation context and memory information.
fn build_enriched_query(query: &str, messages: &[Message], memory_context: Option<&str>) -> String {
    let mut enriched = String::with_capacity(512);

    // 🧠 MEMORY CROSS-VALIDATION: Add memory context with emphasis
    if let Some(mem_ctx) = memory_context {
        if !mem_ctx.is_empty() {
            enriched.push_str("📚 RELEVANT KNOWLEDGE (IMPORTANT - Use this context):\n");
            enriched.push_str(mem_ctx);
            enriched.push_str("\n\n");
        }
    }

    // Extract key entities from current query for semantic matching
    let query_entities = extract_key_entities(query);
    if !query_entities.is_empty() {
        enriched.push_str("Key topics: ");
        enriched.push_str(&query_entities.join(", "));
        enriched.push_str("\n");
    }

    // Add recent conversation context (last 2-3 exchanges)
    let recent_context = extract_recent_context(messages, 3);
    if !recent_context.is_empty() {
        enriched.push_str("Recent conversation: ");
        enriched.push_str(&recent_context);
        enriched.push_str("\n");
    }

    // Add current query with emphasis
    enriched.push_str("\nCurrent question: ");
    enriched.push_str(query);

    enriched
}

/// Extract key entities from query for better semantic matching.
/// Focuses on: technical terms, file names, function names, concepts.
fn extract_key_entities(query: &str) -> Vec<String> {
    let mut entities = Vec::new();
    
    // 1. Extract file paths and names
    let file_pattern = regex::Regex::new(r"[\w.-]+\.(rs|toml|json|md|py|js|ts|go)").unwrap();
    for mat in file_pattern.find_iter(query) {
        entities.push(mat.as_str().to_string());
    }
    
    // 2. Extract code identifiers (backtick-wrapped)
    let ident_pattern = regex::Regex::new(r"`([\w_]+)`").unwrap();
    for mat in ident_pattern.find_iter(query) {
        let ident = mat.as_str().trim_matches('`');
        if ident.len() > 2 && !entities.contains(&ident.to_string()) {
            entities.push(ident.to_string());
        }
    }
    
    // 3. Extract technical terms (capitalized words or known patterns)
    let tech_terms = [
        "authentication", "authorization", "database", "API", "HTTP",
        "async", "await", "error handling", "testing", "deployment",
        "refactor", "optimize", "performance", "security"
    ];
    let query_lower = query.to_lowercase();
    for term in &tech_terms {
        if query_lower.contains(term) && !entities.iter().any(|e| e.to_lowercase() == *term) {
            entities.push(term.to_string());
        }
    }
    
    // Limit to top 5 most relevant entities
    entities.truncate(5);
    entities
}

/// Extract recent conversation context for query enrichment.
/// Keeps the last `max_exchanges` user-assistant pairs.
fn extract_recent_context(messages: &[Message], max_exchanges: usize) -> String {
    if messages.is_empty() {
        return String::new();
    }

    let mut context_parts = Vec::new();
    let mut exchange_count = 0;

    // Iterate backwards to get most recent exchanges
    for msg in messages.iter().rev() {
        if exchange_count >= max_exchanges * 2 {
            break;
        }

        match msg {
            Message::User { content } => {
                let truncated: String = content.chars().take(100).collect();
                if content.chars().count() > 100 {
                    context_parts.push(format!("User: {}...", truncated));
                } else {
                    context_parts.push(format!("User: {}", content));
                }
                exchange_count += 1;
            }
            Message::Assistant { content, .. } => {
                let truncated: String = content.chars().take(100).collect();
                if content.chars().count() > 100 {
                    context_parts.push(format!("Assistant: {}...", truncated));
                } else {
                    context_parts.push(format!("Assistant: {}", content));
                }
                exchange_count += 1;
            }
            _ => continue,
        }
    }

    // Reverse to maintain chronological order
    context_parts.reverse();
    context_parts.join(" | ")
}
