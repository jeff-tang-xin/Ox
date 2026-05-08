//! LLM-based memory re-ranking with position bias mitigation.
//!
//! This module implements a "LLM Judge" approach for re-ranking memories:
//! 1. Shuffle candidates to eliminate position bias
//! 2. Use LLM to score each memory's relevance
//! 3. Enforce strict scoring constraints (at least 30% must be low scores)
//! 4. Return top-ranked memories

use crate::memory::MemoryNode;
use anyhow::Result;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

/// Judgment result from LLM for a single memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryJudgment {
    /// Original index of the memory in the input list
    pub id: usize,
    /// Relevance score (0-10)
    pub score: u8,
    /// Brief reason for the score
    pub reason: String,
}

/// Configuration for LLM-based re-ranking.
#[derive(Debug, Clone)]
pub struct RerankerConfig {
    /// Number of top candidates to retrieve before re-ranking
    pub initial_top_k: usize,
    /// Number of final results after re-ranking
    pub final_top_k: usize,
    /// Minimum percentage of candidates that must receive low scores (< 5)
    pub min_low_score_ratio: f32,
    /// Score threshold for considering a memory relevant
    pub relevance_threshold: u8,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            initial_top_k: 15,
            final_top_k: 5,
            min_low_score_ratio: 0.3, // At least 30% must be low scores
            relevance_threshold: 7,   // Only keep memories with score >= 7
        }
    }
}

/// LLM-based re-ranker for memory nodes.
pub struct LlmReranker {
    config: RerankerConfig,
}

impl LlmReranker {
    /// Create a new LLM re-ranker.
    pub fn new(config: RerankerConfig) -> Self {
        Self { config }
    }

    /// Re-rank memories using LLM judgment with position bias mitigation.
    /// 
    /// This enhanced version also updates memory feedback scores to create
    /// a self-improving loop.
    ///
    /// # Arguments
    /// * `query` - The search query
    /// * `memories` - Candidate memories to re-rank (should be pre-filtered, e.g., top 15)
    /// * `llm_call_fn` - Function to call LLM with JSON mode support
    /// * `memory_manager` - Optional memory manager for feedback updates
    /// * `project_id` - Optional project ID for scoped updates
    ///
    /// # Returns
    /// Re-ranked memories sorted by LLM score (descending)
    pub async fn rerank_with_feedback<F>(
        &self,
        query: &str,
        memories: Vec<MemoryNode>,
        llm_call_fn: F,
        memory_manager: Option<&crate::memory::MemoryManager>,
        project_id: Option<&str>,
    ) -> Result<Vec<MemoryNode>>
    where
        F: FnOnce(String) -> std::pin::Pin<Box<dyn futures::Future<Output = Result<String>> + Send>>,
    {
        if memories.is_empty() {
            return Ok(vec![]);
        }

        let candidate_count = memories.len();
        tracing::info!(
            "[LLM_RERANK] Re-ranking {} memories for query: {}",
            candidate_count,
            query.chars().take(50).collect::<String>()
        );

        // Step 1: Create indexed list and shuffle to eliminate position bias
        let mut indexed_memories: Vec<(usize, MemoryNode)> =
            memories.into_iter().enumerate().collect();

        // Shuffle to prevent LLM position bias
        let mut shuffled = indexed_memories.clone();
        shuffled.shuffle(&mut rand::thread_rng());

        tracing::debug!("[LLM_RERANK] Shuffled {} candidates", shuffled.len());

        // Step 2: Build judge prompt with strict constraints
        let memories_context: Vec<String> = shuffled
            .iter()
            .map(|(id, mem)| {
                let preview: String = mem.content.chars().take(150).collect();
                format!("【ID:{}】{}", id, preview)
            })
            .collect();

        let judge_prompt = format!(
            r#"You are a rigorous memory relevance judge.

Scoring Criteria (0-10):
- 0-3: Completely irrelevant to the query
- 4-6: Somewhat related but not directly useful
- 7-10: Contains core facts or highly relevant information

**CRITICAL CONSTRAINT**: To ensure strict filtering, **you MUST assign scores below 5 to at least {min_low_pct}% of the candidates**.
Do NOT give high scores to all memories. Be discriminating.

Current Query: {query}

Candidate Memories:
{memories}

Return ONLY a valid JSON array. Do NOT include any Markdown formatting or explanations.
Format: [{{"id": number, "score": number, "reason": "brief reason"}}]"#,
            min_low_pct = (self.config.min_low_score_ratio * 100.0) as u32,
            query = query,
            memories = memories_context.join("\n")
        );

        // Step 3: Call LLM with JSON mode
        let llm_response = llm_call_fn(judge_prompt).await?;

        // Step 4: Parse JSON response
        let judgments: Vec<MemoryJudgment> = match serde_json::from_str::<Vec<MemoryJudgment>>(&llm_response) {
            Ok(judgments) => {
                tracing::info!("[LLM_RERANK] Successfully parsed {} judgments", judgments.len());
                judgments
            }
            Err(e) => {
                tracing::warn!(
                    "[LLM_RERANK] Failed to parse JSON: {}, falling back to original order",
                    e
                );
                // Fallback: return original memories truncated to final_top_k
                let mut result: Vec<MemoryNode> =
                    indexed_memories.into_iter().map(|(_, m)| m).collect();
                result.truncate(self.config.final_top_k);
                return Ok(result);
            }
        };

        // Step 5: Validate constraint: at least X% must have low scores
        let low_score_count = judgments.iter().filter(|j| j.score < 5).count();
        let actual_low_ratio = low_score_count as f32 / candidate_count as f32;

        if actual_low_ratio < self.config.min_low_score_ratio {
            tracing::warn!(
                "[LLM_RERANK] LLM did not enforce low score constraint: got {:.1}%, expected >= {:.1}%",
                actual_low_ratio * 100.0,
                self.config.min_low_score_ratio * 100.0
            );
            // Optionally: force-adjust scores here if needed
        }

        // Step 6: Filter and sort by score
        let mut scored_memories: Vec<(usize, u8, MemoryNode)> = judgments
            .iter()  // Use iter() instead of into_iter() to keep judgments
            .filter_map(|judgment| {
                // Find original memory by ID
                indexed_memories
                    .iter()
                    .find(|(id, _)| *id == judgment.id)
                    .map(|(_, mem)| (judgment.id, judgment.score, mem.clone()))
            })
            .collect();

        // Sort by score descending
        scored_memories.sort_by(|a, b| b.1.cmp(&a.1));

        // Step 7: Keep only memories above threshold
        let filtered: Vec<MemoryNode> = scored_memories
            .into_iter()
            .filter(|(_, score, _)| *score >= self.config.relevance_threshold)
            .map(|(_, _, mem)| mem)
            .take(self.config.final_top_k)
            .collect();

        tracing::info!(
            "[LLM_RERANK] Final result: {} memories (threshold >= {})",
            filtered.len(),
            self.config.relevance_threshold
        );
        
        // 🆕 Step 8: Update memory feedback scores (batch update for efficiency)
        if let Some(mm) = memory_manager {
            let feedbacks: Vec<(String, f32)> = judgments
                .iter()
                .filter_map(|judgment| {
                    // Find the original memory to get its ID
                    indexed_memories
                        .iter()
                        .find(|(id, _)| *id == judgment.id)
                        .map(|(_, original_mem)| (original_mem.id.clone(), judgment.score as f32))
                })
                .collect();
            
            // Use batch update for better performance
            mm.update_with_llm_feedback_batch(feedbacks, project_id);
            
            tracing::info!(
                "[LLM_RERANK] Updated feedback scores for {} memories (batch)",
                judgments.len()
            );
        }

        Ok(filtered)
    }
}
