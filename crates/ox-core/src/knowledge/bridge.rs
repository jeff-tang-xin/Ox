//! Bridge between `KnowledgeEngine` (Entity model) and `MemoryNode` display types.

use super::KnowledgeEngine;
use super::entity::{Entity, EntityKind, EntityMetadata};
use super::memory_node::{MemoryNode, MemoryNodeType, MemorySource};

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
