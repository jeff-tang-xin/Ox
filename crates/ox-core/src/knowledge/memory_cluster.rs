//! Memory-graph cluster expansion for pre-turn retrieval (L0–L3).
//!
//! After vector/BM25 seeds are chosen, walk the in-memory EntityGraph along
//! memory↔code relation edges and merge connected entities into the candidate set.

use std::collections::{HashMap, HashSet};

use super::KnowledgeEngine;
use super::entity::{Entity, EntityKind, EntityMetadata, RelationType};
use super::graph::EntityGraph;

const MEMORY_RELATIONS: &[RelationType] = &[
    RelationType::Precedes,
    RelationType::BelongsTo,
    RelationType::Abstracts,
    RelationType::RelatesToSymbol,
    RelationType::ModifiesSymbol,
    RelationType::MentionsSymbol,
    RelationType::SimilarTo,
];

/// Expand memory clusters from retrieval seeds; returns a compact summary block.
pub fn expand_into_candidates(
    engine: &KnowledgeEngine,
    candidates: &mut HashMap<String, (Entity, f32)>,
    max_entities: usize,
) -> String {
    let graph = engine.lock_entity_graph();
    expand_with_graph(&graph, candidates, max_entities)
}

fn expand_with_graph(
    graph: &EntityGraph,
    candidates: &mut HashMap<String, (Entity, f32)>,
    max_entities: usize,
) -> String {
    let seeds = collect_memory_seeds(candidates);
    if seeds.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut added = 0usize;
    let mut seen: HashSet<String> = candidates.keys().cloned().collect();

    // Symbol-anchored clusters: L0/L1 linked to code symbols in the seed set.
    let symbol_seeds: Vec<String> = seeds
        .iter()
        .filter(|id| {
            candidates
                .get(*id)
                .is_some_and(|(e, _)| e.kind == EntityKind::CodeSymbol)
        })
        .take(4)
        .cloned()
        .collect();

    for sym_id in symbol_seeds {
        let fq_name = match graph.get(&sym_id) {
            Some(sym) => match &sym.metadata {
                EntityMetadata::CodeSymbol { fq_name, .. } => fq_name.clone(),
                _ => continue,
            },
            None => continue,
        };
        let mut cluster: Vec<String> = Vec::new();
        for e in graph.find_sessions_modifying(&sym_id) {
            if ingest_neighbor(candidates, &mut seen, e, 0.52, &mut added, max_entities) {
                cluster.push(format_cluster_line(e));
            }
        }
        for e in graph.find_sessions_mentioning(&sym_id) {
            if ingest_neighbor(candidates, &mut seen, e, 0.5, &mut added, max_entities) {
                cluster.push(format_cluster_line(e));
            }
        }
        for e in graph.find_memories_for_symbol(&sym_id) {
            if ingest_neighbor(candidates, &mut seen, e, 0.55, &mut added, max_entities) {
                cluster.push(format_cluster_line(e));
            }
        }
        if !cluster.is_empty() {
            lines.push(format!("**symbol `{fq_name}`**"));
            lines.extend(cluster.into_iter().take(4));
            lines.push(String::new());
        }
        if added >= max_entities {
            break;
        }
    }

    // Memory-memory traversal (2-hop along L0–L3 edges).
    if added < max_entities {
        let mem_seed_ids: Vec<String> = seeds
            .iter()
            .filter(|id| {
                candidates.get(*id).is_some_and(|(e, _)| {
                    matches!(
                        e.kind,
                        EntityKind::WorkingMemory
                            | EntityKind::AtomicMemory
                            | EntityKind::EpisodicMemory
                            | EntityKind::SemanticMemory
                    )
                })
            })
            .take(6)
            .cloned()
            .collect();

        if !mem_seed_ids.is_empty() {
            let traversed = graph.traverse(&mem_seed_ids, 2, Some(MEMORY_RELATIONS));
            let mut hop_lines: Vec<String> = Vec::new();
            for hit in traversed {
                if hit.distance == 0 {
                    continue;
                }
                if !is_long_term_memory(hit.entity.kind) {
                    continue;
                }
                if ingest_neighbor(
                    candidates,
                    &mut seen,
                    &hit.entity,
                    0.45 * hit.path_weight,
                    &mut added,
                    max_entities,
                ) {
                    let via = hit.via.map(|r| r.as_str()).unwrap_or("link");
                    hop_lines.push(format!(
                        "- [{}·{}] {}",
                        via,
                        hit.entity.kind.as_str(),
                        preview(&hit.entity.content, 72)
                    ));
                }
                if added >= max_entities {
                    break;
                }
            }
            if !hop_lines.is_empty() {
                lines.push("**memory graph (2-hop)**".to_string());
                lines.extend(hop_lines.into_iter().take(6));
            }
        }
    }

    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn collect_memory_seeds(candidates: &HashMap<String, (Entity, f32)>) -> Vec<String> {
    let mut seeds: Vec<(String, f32)> = candidates
        .iter()
        .filter_map(|(id, (e, score))| {
            let priority = match e.kind {
                EntityKind::CodeSymbol => *score + 0.1,
                EntityKind::AtomicMemory | EntityKind::EpisodicMemory => *score,
                EntityKind::SemanticMemory => *score - 0.05,
                EntityKind::WorkingMemory => *score - 0.1,
                _ => return None,
            };
            Some((id.clone(), priority))
        })
        .collect();
    seeds.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    seeds.into_iter().map(|(id, _)| id).take(10).collect()
}

fn ingest_neighbor(
    candidates: &mut HashMap<String, (Entity, f32)>,
    seen: &mut HashSet<String>,
    entity: &Entity,
    score: f32,
    added: &mut usize,
    max: usize,
) -> bool {
    if *added >= max || seen.contains(&entity.id) {
        return false;
    }
    seen.insert(entity.id.clone());
    candidates
        .entry(entity.id.clone())
        .and_modify(|(_, s)| *s = (*s + score).min(1.5))
        .or_insert_with(|| (entity.clone(), score));
    *added += 1;
    true
}

fn is_long_term_memory(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::WorkingMemory
            | EntityKind::AtomicMemory
            | EntityKind::EpisodicMemory
            | EntityKind::SemanticMemory
    )
}

fn format_cluster_line(entity: &Entity) -> String {
    format!(
        "- [{}] {}",
        entity.kind.as_str(),
        preview(&entity.content, 72)
    )
}

fn preview(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::entity::{Relation, SymbolType};

    fn sym(id: &str, fq: &str) -> Entity {
        Entity::code_symbol(
            "f",
            fq,
            SymbolType::Function,
            "rs",
            "a.rs",
            1,
            2,
            "fn f()",
            None,
        )
    }

    #[test]
    fn symbol_cluster_pulls_linked_working_memory() {
        let mut graph = EntityGraph::new();
        let mut wm = Entity::working_memory("s1", "edit a.rs", None, None, vec![], true);
        let mut symbol = sym("sym1", "foo::bar");
        symbol.id = "sym1".into();
        wm.id = "wm1".into();
        wm.relations.push(Relation {
            target_id: "sym1".into(),
            relation_type: RelationType::ModifiesSymbol,
            weight: 0.9,
        });
        graph.upsert(symbol.clone());
        graph.upsert(wm.clone());

        let mut candidates = HashMap::from([("sym1".into(), (symbol, 0.9))]);
        let block = expand_with_graph(&graph, &mut candidates, 8);
        assert!(block.contains("foo::bar"));
        assert!(candidates.contains_key("wm1"));
    }
}
