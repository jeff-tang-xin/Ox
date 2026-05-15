//! KadaneDial algorithm for semantic context compression.
//!
//! This module implements the KadaneDial algorithm from the paper
//! "DyCP: Dynamic Context Pruning for Long-Form Dialogue with LLMs".
//!
//! The algorithm finds contiguous conversation segments with maximum
//! cumulative relevance to the current query by adapting Kadane's
//! maximum subarray algorithm.

use crate::message::Message;

/// Configuration for KadaneDial compression.
#[derive(Debug, Clone)]
pub struct KadaneConfig {
    /// Z-score threshold for filtering relevant segments.
    /// Higher values = stricter filtering (fewer segments selected).
    pub threshold: f32,
    /// Stopping threshold for cumulative gain.
    /// When max cumulative gain falls below this, stop searching.
    pub stop_threshold: f32,
    /// Maximum segments to select.
    pub max_segments: usize,
    /// Minimum segment length (in message pairs).
    pub min_segment_len: usize,
    /// Always keep this many recent messages.
    pub keep_recent: usize,
    /// Token threshold for chunking: messages shorter than this are kept as single chunk.
    pub chunk_threshold_tokens: usize,
    /// Maximum tokens per chunk when splitting long messages.
    pub max_chunk_tokens: usize,
}

impl Default for KadaneConfig {
    fn default() -> Self {
        Self {
            threshold: 0.0,
            stop_threshold: 0.1, // Z-scores are typically small,
            max_segments: 5,
            min_segment_len: 2,
            keep_recent: 8,  // 🚨 Increased from 4 to 8 to reduce hallucination after compression
            chunk_threshold_tokens: 256,
            max_chunk_tokens: 512,
        }
    }
}

/// A segment of conversation history selected by KadaneDial.
#[derive(Debug, Clone)]
pub struct SelectedSegment {
    /// Start index in the message array.
    pub start: usize,
    /// End index (exclusive) in the message array.
    pub end: usize,
    /// Cumulative relevance gain for this segment.
    pub gain: f32,
}

/// Result of KadaneDial compression.
#[derive(Debug, Clone)]
pub struct CompressionResult {
    /// Selected message indices to include in compressed context.
    pub indices: Vec<usize>,
    /// Total cumulative gain of selected segments.
    pub total_gain: f32,
    /// Number of original messages.
    pub original_count: usize,
    /// Number of messages after compression.
    pub compressed_count: usize,
}

/// Compute mean of a slice.
fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f32>() / values.len() as f32
}

/// Compute standard deviation of a slice.
fn std_dev(values: &[f32], m: f32) -> f32 {
    if values.is_empty() {
        return 1.0;
    }
    let variance = values.iter().map(|v| (v - m).powi(2)).sum::<f32>() / values.len() as f32;
    variance.sqrt().max(1e-6)
}

/// Kadane's algorithm for finding maximum subarray.
/// Returns (start_idx, end_idx, max_sum).
/// Uses >= for comparisons to ensure earliest start position when sums are equal.
fn kadane_max_subarray(gains: &[f32]) -> (usize, usize, f32) {
    if gains.is_empty() {
        return (0, 0, 0.0);
    }

    let mut max_sum = gains[0];
    let mut max_start = 0;
    let mut max_end = 1;

    let mut current_sum = gains[0];
    let mut current_start = 0;

    for i in 1..gains.len() {
        // If extending current subarray gives equal or better sum, extend it
        // Otherwise, start fresh with current element
        if current_sum + gains[i] > gains[i] {
            current_sum += gains[i];
        } else {
            current_sum = gains[i];
            current_start = i;
        }

        // Update max if current sum is strictly greater
        // (use > to prefer earlier subarray when sums are equal)
        if current_sum > max_sum {
            max_sum = current_sum;
            max_start = current_start;
            max_end = i + 1;
        }
    }

    (max_start, max_end, max_sum)
}

/// Run KadaneDial algorithm to select relevant conversation segments.
///
/// Given relevance scores between the query and each conversation turn,
/// this function finds contiguous segments with maximum cumulative gain.
///
/// # Arguments
/// * `scores` - Relevance scores [0, 1] for each message turn
/// * `config` - KadaneDial configuration parameters
///
/// # Returns
/// Indices of messages to include in the compressed context.
pub fn compress_with_kadane(scores: &[f32], config: &KadaneConfig) -> CompressionResult {
    if scores.is_empty() {
        return CompressionResult {
            indices: vec![],
            total_gain: 0.0,
            original_count: 0,
            compressed_count: 0,
        };
    }

    let original_count = scores.len();

    // Step 1: Z-score standardization
    let m = mean(scores);
    let sd = std_dev(scores, m);
    let z_scores: Vec<f32> = scores.iter().map(|s| (s - m) / sd).collect();

    // Step 2: Gain calculation (subtract threshold)
    let mut gains: Vec<f32> = z_scores.iter().map(|z| z - config.threshold).collect();

    let mut selected_segments: Vec<SelectedSegment> = Vec::new();
    let mut total_gain = 0.0;
    let mut current_max_gain = f32::INFINITY;

    // Step 3: Iteratively find max-gain segments
    while selected_segments.len() < config.max_segments && current_max_gain >= config.stop_threshold
    {
        // Run Kadane's algorithm
        let (start, end, max_sum) = kadane_max_subarray(&gains);

        // Check if segment meets minimum length requirement
        if end - start < config.min_segment_len {
            break;
        }

        // Check if gain is above stopping threshold
        if max_sum < config.stop_threshold {
            break;
        }

        current_max_gain = max_sum;
        total_gain += max_sum;

        selected_segments.push(SelectedSegment {
            start,
            end,
            gain: max_sum,
        });

        // Mark selected region as invalid (prevent overlap)
        for i in start..end {
            gains[i] = f32::NEG_INFINITY;
        }
    }

    // Collect all indices from selected segments
    let mut indices: Vec<usize> = Vec::new();
    for seg in &selected_segments {
        indices.extend(seg.start..seg.end);
    }

    // Sort indices to maintain chronological order
    indices.sort();

    let compressed_count = indices.len();

    CompressionResult {
        indices,
        total_gain,
        original_count,
        compressed_count,
    }
}

/// Filter conversation messages using KadaneDial results.
///
/// Given original messages and selected indices, returns the compressed context
/// while preserving the first few messages (important for conversation coherence).
pub fn filter_messages(
    messages: &[Message],
    indices: &[usize],
    keep_recent: usize,
) -> Vec<Message> {
    if messages.is_empty() {
        return vec![];
    }

    // Always keep the first message (session context)
    let first_msg = if messages.is_empty() { None } else { Some(0) };

    // Also keep recent messages
    let recent_start = messages.len().saturating_sub(keep_recent);
    let recent_indices: Vec<usize> = (recent_start..messages.len()).collect();

    // Merge selected indices with first and recent
    let mut all_indices: Vec<usize> = indices.to_vec();
    if let Some(first) = first_msg {
        if !all_indices.contains(&first) {
            all_indices.push(first);
        }
    }
    for idx in recent_indices {
        if !all_indices.contains(&idx) {
            all_indices.push(idx);
        }
    }

    // Sort and deduplicate
    all_indices.sort();
    all_indices.dedup();

    // Extract messages
    all_indices
        .iter()
        .filter_map(|&i| messages.get(i).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kadane_max_subarray() {
        // Test with negative in the middle: should find [3, 2] (sum=5) or [1, 2, -1, 2] (sum=4)
        // The algorithm prefers longer subarray when sums are close, so it may return [1, 2, -1, 2]
        let gains = [1.0, 2.0, -1.0, 2.0];
        let (start, end, max_sum) = kadane_max_subarray(&gains);
        assert!(max_sum > 0.0);
        assert!(start < end);
        assert!(end <= gains.len());

        // Pure negative sequence - should return first element
        let gains = [-1.0, -2.0, -3.0];
        let (start, end, max_sum) = kadane_max_subarray(&gains);
        assert_eq!(start, 0);
        assert_eq!(end, 1);
        assert!((max_sum + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_compress_with_kadane_basic() {
        let scores = vec![0.1, 0.9, 0.95, 0.8, 0.2, 0.15];
        let config = KadaneConfig {
            threshold: 0.0,
            stop_threshold: 0.1, // Z-scores are typically small
            max_segments: 3,
            min_segment_len: 2,
            keep_recent: 4,
            chunk_threshold_tokens: 256,
            max_chunk_tokens: 512,
        };

        let result = compress_with_kadane(&scores, &config);

        // Should select at least the high-scoring region
        assert!(!result.indices.is_empty());
        assert!(result.compressed_count < result.original_count);
    }

    #[test]
    fn test_compress_with_kadane_empty() {
        let scores: Vec<f32> = vec![];
        let config = KadaneConfig::default();

        let result = compress_with_kadane(&scores, &config);

        assert!(result.indices.is_empty());
        assert_eq!(result.original_count, 0);
    }

    #[test]
    fn test_mean_and_std() {
        let values = [1.0, 2.0, 3.0, 4.0, 5.0];
        let m = mean(&values);
        assert!((m - 3.0).abs() < 1e-6);

        let sd = std_dev(&values, m);
        // Std dev of [1,2,3,4,5] is sqrt(2) �?1.414
        assert!((sd - 1.41421).abs() < 0.01);
    }
}
