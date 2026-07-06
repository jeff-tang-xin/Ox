//! Bridge between `KnowledgeEngine` (Entity model) and `MemoryNode` display types.
//!
//! When GitNexus is available, `gitnexus_query_fallback` prefers its `query` op
//! (BM25 + semantic + graph traversal) and silently falls back to the local
//! `retrieve_for_context` (BM25 + vector) when GitNexus is unavailable.

use std::sync::Arc;

use super::KnowledgeEngine;
use super::entity::{Entity, EntityKind, EntityMetadata};
use super::memory_node::{MemoryNode, MemoryNodeType, MemorySource};
use crate::mcp::gitnexus::{GitNexusService, QueryParams};
use serde_json;

/// Convert a knowledge `Entity` into a `MemoryNode` for display / interjection.
pub fn entity_to_memory_node(entity: &Entity, project_id: Option<String>) -> MemoryNode {
    let node_type = match entity.kind {
        EntityKind::AtomicMemory => {
            if let EntityMetadata::AtomicMemory {
                ref memory_type, ..
            } = entity.metadata
            {
                match memory_type.as_str() {
                    "Style" => MemoryNodeType::Style,
                    "BestPractice" => MemoryNodeType::BestPractice,
                    "AntiPattern" => MemoryNodeType::AntiPattern,
                    "Architectural" => MemoryNodeType::Architectural,
                    _ => MemoryNodeType::Fact,
                }
            } else {
                MemoryNodeType::Fact
            }
        }
        EntityKind::EpisodicMemory => MemoryNodeType::Pattern,
        EntityKind::SemanticMemory => MemoryNodeType::Architectural,
        EntityKind::WorkingMemory => MemoryNodeType::Fact,
        EntityKind::CodeSymbol | EntityKind::CodeFile | EntityKind::CodeModule => {
            MemoryNodeType::Fact
        }
    };

    let depth = entity.kind.depth().unwrap_or(0);
    let pid = project_id.or_else(|| entity.project_id().map(|s| s.to_string()));

    MemoryNode {
        id: entity.id.clone(),
        content: entity.content.clone(),
        node_type,
        depth,
        project_id: pid,
        language: entity
            .coordinate
            .tags
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".into()),
        source: MemorySource::LlmExtraction,
        created_at: entity.coordinate.created_at,
        last_accessed: entity.coordinate.last_accessed,
        is_project_critical: false,
        traces: [0.0; 5],
        language_weight: 1.0,
        avg_llm_score: 0.0,
        judge_eval_count: 0,
        recent_scores: [0.0; 5],
        related_files: entity
            .file_path()
            .map(|p| vec![p.to_string()])
            .unwrap_or_default(),
    }
}

impl KnowledgeEngine {
    /// Memory retrieval — hybrid vector + BM25 search over L0-L3 and code.
    pub fn retrieve_memory_nodes(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: usize,
    ) -> Vec<MemoryNode> {
        let q = if query.is_empty() {
            "recent project knowledge"
        } else {
            query
        };

        let hits = self
            .retrieve_for_context(q, "current", limit.saturating_mul(2))
            .unwrap_or_default();

        hits.into_iter()
            .take(limit)
            .map(|h| entity_to_memory_node(&h.entity, project_id.map(|s| s.to_string())))
            .collect()
    }

    /// Store user explicit memory.
    pub fn remember_explicit(
        &mut self,
        content: &str,
        project_id: &str,
        language: &str,
    ) -> anyhow::Result<Entity> {
        let entity =
            self.record_atomic_fact(content, "Style", Some(project_id), language, "UserExplicit")?;
        self.track_entity(&entity);
        Ok(entity)
    }

    /// GitNexus-first retrieval — prefers `query` (BM25 + semantic + graph)
    /// when GitNexus is ready, silently falls back to local `retrieve_for_context`
    /// (BM25 + vector) when unavailable.
    pub async fn gitnexus_query_fallback(
        &self,
        gitnexus: Option<&Arc<GitNexusService>>,
        query: &str,
        project_id: Option<&str>,
        limit: usize,
    ) -> Vec<MemoryNode> {
        let q = if query.is_empty() {
            "recent project knowledge"
        } else {
            query
        };

        // Try GitNexus first
        if let Some(svc) = gitnexus
            && svc.is_ready().await {
                let mut params = QueryParams::new(q);
                params.limit = Some(limit as u32);
                params.include_content = Some(true);
                match svc.query(&params).await {
                    Ok(result) if !result.is_error && !result.text.trim().is_empty() => {
                        return parse_gitnexus_results(&result.text, project_id, limit);
                    }
                    _ => {
                        // GitNexus returned error or empty — fall through to local
                    }
                }
            }

        // Silent fallback to local BM25 + vector search
        self.retrieve_memory_nodes(q, project_id, limit)
    }

    /// Count entities by memory layer (for /memory stats).
    pub fn memory_layer_counts(&self) -> (usize, usize, usize, usize) {
        let l0 = self.recent_turns.len();
        let graph = self.entity_graph.lock().unwrap();
        let l1 = graph.entities_of_kind(EntityKind::AtomicMemory).len();
        let l2 = graph.entities_of_kind(EntityKind::EpisodicMemory).len();
        let l3 = graph.entities_of_kind(EntityKind::SemanticMemory).len();
        (l0, l1, l2, l3)
    }
}

/// Parse GitNexus query results text into `MemoryNode` items.
///
/// GitNexus returns structured text (JSON or Markdown). This best-effort parser
/// extracts entries and wraps them as `MemoryNode` for downstream consumption.
fn parse_gitnexus_results(
    text: &str,
    project_id: Option<&str>,
    limit: usize,
) -> Vec<MemoryNode> {
    // Attempt JSON parse — GitNexus query typically returns a JSON array of objects
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(arr) = val.as_array() {
            return arr
                .iter()
                .take(limit)
                .filter_map(|item| {
                    let content = item
                        .get("content")
                        .or_else(|| item.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if content.is_empty() {
                        return None;
                    }
                    let id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("gitnexus")
                        .to_string();
                    Some(MemoryNode {
                        id,
                        content: content.to_string(),
                        node_type: MemoryNodeType::Fact,
                        depth: 1,
                        project_id: project_id.map(|s| s.to_string()),
                        language: item
                            .get("language")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        source: MemorySource::LlmExtraction,
                        created_at: item
                            .get("created_at")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                        last_accessed: item
                            .get("last_accessed")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                        is_project_critical: false,
                        traces: [0.0; 5],
                        language_weight: 1.0,
                        avg_llm_score: 0.0,
                        judge_eval_count: 0,
                        recent_scores: [0.0; 5],
                        related_files: item
                            .get("file_path")
                            .or_else(|| item.get("file"))
                            .and_then(|v| v.as_str())
                            .map(|p| vec![p.to_string()])
                            .unwrap_or_default(),
                    })
                })
                .collect();
        }

    // Fallback: treat the whole text as a single MemoryNode
    vec![MemoryNode {
        id: "gitnexus-composite".to_string(),
        content: text.to_string(),
        node_type: MemoryNodeType::Fact,
        depth: 1,
        project_id: project_id.map(|s| s.to_string()),
        language: "unknown".to_string(),
        source: MemorySource::LlmExtraction,
        created_at: 0,
        last_accessed: 0,
        is_project_critical: false,
        traces: [0.0; 5],
        language_weight: 1.0,
        avg_llm_score: 0.0,
        judge_eval_count: 0,
        recent_scores: [0.0; 5],
        related_files: vec![],
    }]
}

