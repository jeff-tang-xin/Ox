/// In-memory Entity Graph for relation traversal and mutation.
///
/// Maintains the directed graph of Relations between Entities. This graph is
/// the runtime representation — the authoritative storage is in TriviumDB
/// (each entity carries its own relations in its payload). The in-memory graph
/// enables fast queries like "find all symbols that call X" or "find all
/// sessions that modified Y" without scanning TriviumDB.

use std::collections::{HashMap, HashSet};

use super::entity::{Entity, EntityKind, RelationType};

/// A directed graph of entity relations stored in adjacency lists.
#[derive(Debug, Default)]
pub struct EntityGraph {
    /// entity_id → (outgoing relations)
    out_edges: HashMap<String, Vec<Edge>>,
    /// entity_id → (incoming relations)
    in_edges: HashMap<String, Vec<Edge>>,
    /// Full entity cache keyed by id
    entities: HashMap<String, Entity>,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: RelationType,
    pub weight: f32,
}

/// Result of a graph traversal query.
#[derive(Debug, Clone)]
pub struct GraphTraversalResult {
    pub entity: Entity,
    /// Distance from the query origin (0 = origin, 1 = 1-hop neighbor, 2 = 2-hop)
    pub distance: u32,
    /// The relation that connects this entity to the traversal
    pub via: Option<RelationType>,
    /// Accumulated edge weight along the path
    pub path_weight: f32,
}

impl EntityGraph {
    pub fn new() -> Self {
        Self {
            out_edges: HashMap::new(),
            in_edges: HashMap::new(),
            entities: HashMap::new(),
        }
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Mutation
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Add or update an entity in the graph.
    /// Previous edges from this entity are cleared and re-added from `entity.relations`.
    pub fn upsert(&mut self, entity: Entity) {
        // Clear old outgoing edges
        if let Some(old_edges) = self.out_edges.remove(&entity.id) {
            for edge in &old_edges {
                if let Some(incoming) = self.in_edges.get_mut(&edge.to_id) {
                    incoming.retain(|e| e.from_id != entity.id);
                }
            }
        }

        // Insert new outgoing edges
        let mut new_edges = Vec::new();
        for rel in &entity.relations {
            let edge = Edge {
                from_id: entity.id.clone(),
                to_id: rel.target_id.clone(),
                relation_type: rel.relation_type,
                weight: rel.weight,
            };
            self.in_edges
                .entry(rel.target_id.clone())
                .or_default()
                .push(edge.clone());
            new_edges.push(edge);
        }
        self.out_edges.insert(entity.id.clone(), new_edges);
        self.entities.insert(entity.id.clone(), entity);
    }

    /// Remove an entity and all its edges from the graph.
    pub fn remove(&mut self, entity_id: &str) {
        // Remove outgoing edges
        if let Some(out_edges) = self.out_edges.remove(entity_id) {
            for edge in &out_edges {
                if let Some(incoming) = self.in_edges.get_mut(&edge.to_id) {
                    incoming.retain(|e| e.from_id != entity_id);
                }
            }
        }
        // Remove from incoming edges (reverse clean)
        for (_, incoming) in self.in_edges.iter_mut() {
            incoming.retain(|e| e.to_id != entity_id);
        }
        self.entities.remove(entity_id);
    }

    /// Return entity ids associated with a file path.
    pub fn entity_ids_for_file(&self, file_path: &str) -> Vec<String> {
        self.entities
            .iter()
            .filter(|(_, e)| e.file_path() == Some(file_path))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Remove all entities associated with a file path (for re-indexing).
    pub fn remove_by_file(&mut self, file_path: &str) {
        let ids: Vec<String> = self.entities
            .iter()
            .filter(|(_, e)| e.file_path() == Some(file_path))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove(&id);
        }
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Traversal
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Breadth-first traversal from a set of origin entity IDs.
    ///
    /// Returns all entities reachable within `max_hops` steps, annotated with
    /// distance and the relation type that connects them.
    ///
    /// Optionally filters by `relation_types`: if Some, only follow edges of
    /// the specified types.
    pub fn traverse(
        &self,
        origin_ids: &[String],
        max_hops: u32,
        relation_types: Option<&[RelationType]>,
    ) -> Vec<GraphTraversalResult> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut results = Vec::new();
        let mut frontier: Vec<(String, u32, f32)> = origin_ids
            .iter()
            .map(|id| (id.clone(), 0, 1.0))
            .collect();

        for (id, dist, _) in &frontier {
            visited.insert(id.clone());
            if let Some(entity) = self.entities.get(id) {
                results.push(GraphTraversalResult {
                    entity: entity.clone(),
                    distance: *dist,
                    via: None,
                    path_weight: 1.0,
                });
            }
        }

        let mut step = 1;
        while step <= max_hops && !frontier.is_empty() {
            let mut next_frontier = Vec::new();
            for (current_id, _current_dist, path_weight) in &frontier {
                if let Some(edges) = self.out_edges.get(current_id) {
                    for edge in edges {
                        if visited.contains(&edge.to_id) {
                            continue;
                        }
                        if let Some(allowed) = relation_types {
                            if !allowed.contains(&edge.relation_type) {
                                continue;
                            }
                        }
                        visited.insert(edge.to_id.clone());
                        let new_weight = path_weight * edge.weight;
                        if let Some(entity) = self.entities.get(&edge.to_id) {
                            results.push(GraphTraversalResult {
                                entity: entity.clone(),
                                distance: step,
                                via: Some(edge.relation_type),
                                path_weight: new_weight,
                            });
                        }
                        next_frontier.push((edge.to_id.clone(), step, new_weight));
                    }
                }
            }
            frontier = next_frontier;
            step += 1;
        }

        results
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Specialized queries
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Find all entities that have a specific relation TO the given entity.
    pub fn find_incoming(
        &self,
        entity_id: &str,
        relation_type: Option<RelationType>,
    ) -> Vec<&Entity> {
        self.in_edges
            .get(entity_id)
            .map(|edges| {
                edges.iter()
                    .filter(|e| {
                        relation_type.map_or(true, |rt| e.relation_type == rt)
                    })
                    .filter_map(|e| self.entities.get(&e.from_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Find all entities that have a specific relation FROM the given entity.
    pub fn find_outgoing(
        &self,
        entity_id: &str,
        relation_type: Option<RelationType>,
    ) -> Vec<&Entity> {
        self.out_edges
            .get(entity_id)
            .map(|edges| {
                edges.iter()
                    .filter(|e| {
                        relation_type.map_or(true, |rt| e.relation_type == rt)
                    })
                    .filter_map(|e| self.entities.get(&e.to_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Find all CodeSymbol entities that call the given symbol.
    pub fn find_callers(&self, symbol_id: &str) -> Vec<&Entity> {
        self.find_incoming(symbol_id, Some(RelationType::Calls))
    }

    /// Find all WorkingMemory turns that modified a specific symbol.
    pub fn find_sessions_modifying(&self, symbol_id: &str) -> Vec<&Entity> {
        self.find_incoming(symbol_id, Some(RelationType::ModifiesSymbol))
            .into_iter()
            .filter(|e| e.kind == EntityKind::WorkingMemory)
            .collect()
    }

    /// Find all WorkingMemory turns that mentioned a specific symbol.
    pub fn find_sessions_mentioning(&self, symbol_id: &str) -> Vec<&Entity> {
        self.find_incoming(symbol_id, Some(RelationType::MentionsSymbol))
            .into_iter()
            .filter(|e| e.kind == EntityKind::WorkingMemory)
            .collect()
    }

    /// Find all AtomicMemory entities related to a specific symbol.
    pub fn find_memories_for_symbol(&self, symbol_id: &str) -> Vec<&Entity> {
        self.find_incoming(symbol_id, Some(RelationType::RelatesToSymbol))
            .into_iter()
            .filter(|e| e.kind == EntityKind::AtomicMemory)
            .collect()
    }

    /// Group WorkingMemory entities by the symbols they modified.
    /// Returns map: symbol_id → list of WorkingMemory entity IDs.
    /// Used for L0→L1 layering candidate detection.
    pub fn group_modifications_by_symbol(&self) -> HashMap<String, Vec<String>> {
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for (from_id, edges) in &self.out_edges {
            // Only consider WorkingMemory entities
            if let Some(entity) = self.entities.get(from_id) {
                if entity.kind != EntityKind::WorkingMemory {
                    continue;
                }
                for edge in edges {
                    if edge.relation_type == RelationType::ModifiesSymbol {
                        groups
                            .entry(edge.to_id.clone())
                            .or_default()
                            .push(from_id.clone());
                    }
                }
            }
        }
        groups
    }

    /// Find entities that share similar content (for dedup and clustering).
    /// This is a simplified approach using entity ID overlap in relations.
    pub fn find_related_entities(
        &self,
        entity_id: &str,
        max_results: usize,
    ) -> Vec<&Entity> {
        let mut scored: Vec<(&Entity, usize)> = Vec::new();

        // Entities that share the same outgoing targets
        if let Some(my_edges) = self.out_edges.get(entity_id) {
            let my_targets: HashSet<&str> = my_edges.iter().map(|e| e.to_id.as_str()).collect();
            for (other_id, other_edges) in &self.out_edges {
                if other_id == entity_id {
                    continue;
                }
                let overlap = other_edges
                    .iter()
                    .filter(|e| my_targets.contains(e.to_id.as_str()))
                    .count();
                if overlap > 0 {
                    if let Some(other) = self.entities.get(other_id) {
                        scored.push((other, overlap));
                    }
                }
            }
        }

        scored.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        scored.truncate(max_results);
        scored.into_iter().map(|(e, _)| e).collect()
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Stats & Iteration
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    pub fn edge_count(&self) -> usize {
        self.out_edges.values().map(|v| v.len()).sum()
    }

    pub fn get(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)
    }

    /// Iterate all entities in the graph.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    /// Get all entities of a specific kind.
    pub fn entities_of_kind(&self, kind: EntityKind) -> Vec<&Entity> {
        self.entities
            .values()
            .filter(|e| e.kind == kind)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::entity::{SymbolType, EntityKind, EntityMetadata, Relation};

    fn make_entity(id: &str, kind: EntityKind, content: &str) -> Entity {
        let _now = chrono::Utc::now().timestamp();
        Entity {
            id: id.to_string(),
            kind,
            content: content.to_string(),
            coordinate: crate::knowledge::entity::MemoryCoordinate::new(0, id, 384),
            metadata: match kind {
                EntityKind::CodeSymbol => EntityMetadata::CodeSymbol {
                    symbol_type: SymbolType::Function,
                    language: "rust".into(),
                    start_line: 1,
                    end_line: 2,
                    file_path: "src/test.rs".into(),
                    signature: "fn test()".into(),
                    parent: None,
                    fq_name: id.to_string(),
                    calls: vec![],
                },
                EntityKind::WorkingMemory => EntityMetadata::WorkingMemory {
                    session_id: id.to_string(),
                    action: "test action".into(),
                    intent: None,
                    result: None,
                    tools_used: vec![],
                    has_code_changes: false,
                    modified_entities: vec![],
                    self_state: None,
                },
                _ => EntityMetadata::AtomicMemory {
                    memory_type: "Fact".into(),
                    project_id: None,
                    language: String::new(),
                    source: "test".into(),
                    related_files: vec![],
                    quality_score: 0.0,
                    judge_eval_count: 0,
                },
            },
            relations: vec![],
            is_critical: false,
        }
    }

    #[test]
    fn test_upsert_and_find_outgoing() {
        let mut graph = EntityGraph::new();
        let mut e1 = make_entity("e1", EntityKind::CodeSymbol, "main");
        e1.relations.push(Relation {
            target_id: "e2".into(),
            relation_type: RelationType::Calls,
            weight: 0.9,
        });
        graph.upsert(e1);

        let outgoing = graph.find_outgoing("e1", None);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].id, "e2");
    }

    #[test]
    fn test_remove_cleans_edges() {
        let mut graph = EntityGraph::new();
        let mut e1 = make_entity("e1", EntityKind::CodeSymbol, "main");
        e1.relations.push(Relation {
            target_id: "e2".into(),
            relation_type: RelationType::Calls,
            weight: 0.9,
        });
        let e2 = make_entity("e2", EntityKind::CodeSymbol, "helper");
        graph.upsert(e1);
        graph.upsert(e2);

        graph.remove("e1");
        assert!(graph.get("e1").is_none());
        let incoming_to_e2 = graph.find_incoming("e2", None);
        assert!(incoming_to_e2.is_empty());
    }

    #[test]
    fn test_traverse_multi_hop() {
        let mut graph = EntityGraph::new();
        // e1 → e2 → e3
        let mut e1 = make_entity("e1", EntityKind::CodeSymbol, "a");
        e1.relations.push(Relation { target_id: "e2".into(), relation_type: RelationType::Calls, weight: 0.9 });
        let mut e2 = make_entity("e2", EntityKind::CodeSymbol, "b");
        e2.relations.push(Relation { target_id: "e3".into(), relation_type: RelationType::Calls, weight: 0.8 });
        let e3 = make_entity("e3", EntityKind::CodeSymbol, "c");
        graph.upsert(e1);
        graph.upsert(e2);
        graph.upsert(e3);

        let results = graph.traverse(&["e1".into()], 2, None);
        // Should find e1 (dist 0), e2 (dist 1), e3 (dist 2)
        assert_eq!(results.len(), 3);

        let d0: Vec<_> = results.iter().filter(|r| r.distance == 0).collect();
        assert_eq!(d0.len(), 1);
        assert_eq!(d0[0].entity.id, "e1");

        let d1: Vec<_> = results.iter().filter(|r| r.distance == 1).collect();
        assert_eq!(d1.len(), 1);
        assert_eq!(d1[0].entity.id, "e2");
        assert_eq!(d1[0].via, Some(RelationType::Calls));

        let d2: Vec<_> = results.iter().filter(|r| r.distance == 2).collect();
        assert_eq!(d2.len(), 1);
        assert_eq!(d2[0].entity.id, "e3");
    }

    #[test]
    fn test_group_modifications_by_symbol() {
        let mut graph = EntityGraph::new();
        let sym = make_entity("sym1", EntityKind::CodeSymbol, "mod::f");
        graph.upsert(sym);

        let mut wm1 = make_entity("wm1", EntityKind::WorkingMemory, "edit f");
        wm1.relations.push(Relation {
            target_id: "sym1".into(),
            relation_type: RelationType::ModifiesSymbol,
            weight: 0.9,
        });
        graph.upsert(wm1);

        let groups = graph.group_modifications_by_symbol();
        assert!(groups.contains_key("sym1"));
        assert_eq!(groups["sym1"], vec!["wm1"]);
    }
}
