use anyhow::Result;
/// Pre-turn knowledge retrieval pipeline.
///
/// Implements the "深度优先 + Token预算" (depth-first + token budget) retrieval
/// strategy per design doc §5.2.
///
/// # Pipeline Steps (every user message)
/// 1. **Intent Classify** — detect exploration/understanding/modification/general
/// 2. **Multi-path Search** — A) semantic search (all kinds, expand_depth=2)
///    B) precise file-path match C) recent session context
/// 3. **Result Fusion** — dedup by entity_id, merge scores, sort by priority
/// 4. **Budget-aware Cut** — truncate to token budget, prioritize L0 > code > L3 > L1 > L2
///
/// # Extraction Filtering (per design doc §3.2)
/// Auto-filters: greetings, repeated confirmations, exploratory chatter with no signal.
use std::collections::HashMap;

use super::KnowledgeEngine;
use super::entity::{Entity, EntityKind, EntityMetadata, injection_priority};
use super::memory_cluster;
use crate::context::detect_intent;

/// Result of intent classification for query decomposition.
#[derive(Debug, Clone)]
pub struct QueryIntent {
    pub intent: crate::context::UserIntent,
    /// Extracted file paths from the query (e.g., "auth.rs", "src/main.rs")
    pub file_paths: Vec<String>,
    /// Extracted symbol name hints (e.g., "validate_token", "User")
    pub symbol_hints: Vec<String>,
    /// Core search query (user message, stripped of noise)
    pub search_query: String,
}

/// A fused retrieval result ready for context injection.
#[derive(Debug, Clone)]
pub struct ContextInjection {
    /// Entities sorted by injection priority (L0 first)
    pub entities: Vec<Entity>,
    /// Structured text blocks for system prompt injection
    pub blocks: ContextBlocks,
    /// Estimated token count
    pub token_estimate: usize,
}

/// Structured context blocks for formatted system prompt injection.
#[derive(Debug, Clone, Default)]
pub struct ContextBlocks {
    /// Relevant code symbols found
    pub code_symbols: String,
    /// 1-hop call-graph neighbors of top symbol hits
    pub code_graph: String,
    /// L0–L3 entities linked via memory graph traversal
    pub memory_clusters: String,
    /// Relevant memories (L1-L3)
    pub memories: String,
    /// Recent working memory (L0)
    pub working_memory: String,
}

/// Run the full pre-turn retrieval pipeline.
///
/// This is called once per user message BEFORE the LLM call.
///
/// Strategy: **layered retrieval** — L0 is always injected (conversation continuity),
/// L1-L3 + CodeSymbols fill the remaining budget (semantic search).
pub fn run_retrieval(
    engine: &KnowledgeEngine,
    user_query: &str,
    _session_id: &str,
    max_tokens: usize,
) -> Result<ContextInjection> {
    // ── Step 1: Intent Classify ──
    let intent = classify_intent(user_query);

    // ── Step 2: ALWAYS inject recent L0 WorkingMemory (conversation continuity) ──
    // Cap at 2 turns to reduce overlap with full session history in context_builder.
    let recent_turns = engine.get_recent_turns(2);
    let mut candidates: HashMap<String, (Entity, f32)> = HashMap::new();
    for turn in &recent_turns {
        candidates
            .entry(turn.id.clone())
            .or_insert_with(|| (turn.clone(), 1.0)); // Score 1.0 = always present
    }

    // ── Step 3: Semantic search for L1-L3 + CodeSymbols (fill remaining budget) ──
    // Exclude WorkingMemory — we already have recent turns above
    let hits = engine.hybrid_search_by_kinds(
        &intent.search_query,
        &[
            EntityKind::CodeSymbol,
            EntityKind::CodeFile,
            EntityKind::CodeModule,
            EntityKind::AtomicMemory,
            EntityKind::EpisodicMemory,
            EntityKind::SemanticMemory,
        ],
        15,
        0.2,
    )?;
    for hit in hits {
        candidates
            .entry(hit.entity.id.clone())
            .and_modify(|(_, score)| *score = (*score + hit.score * 1.0).min(2.0))
            .or_insert_with(|| (hit.entity, hit.score * 1.0));
    }

    // Path B: Precise file-path match — search for CodeSymbol in named files
    for file_path in &intent.file_paths {
        if let Ok(symbols) = engine.find_symbols_in_file(file_path) {
            for sym in symbols {
                candidates
                    .entry(sym.id.clone())
                    .and_modify(|(_, score)| *score = (*score + 0.95).min(2.0))
                    .or_insert_with(|| (sym, 0.95));
            }
        }
    }

    // Path C: Named symbol hints — boost + exact fq_name match (hybrid retrieval)
    for hint in &intent.symbol_hints {
        let hint_lower = hint.to_lowercase();
        for (_, (entity, score)) in candidates.iter_mut() {
            if entity.content.to_lowercase().contains(&hint_lower) {
                *score = (*score + 0.5).min(2.0);
            }
            if let EntityMetadata::CodeSymbol { fq_name, .. } = &entity.metadata
                && fq_name.eq_ignore_ascii_case(hint) {
                    *score = (*score + 0.95).min(2.0);
                }
        }
    }

    let graph_block = expand_code_graph_neighbors(engine, &mut candidates, 5, 12);
    let memory_cluster_block = memory_cluster::expand_into_candidates(engine, &mut candidates, 10);

    // ── Step 4: Result Fusion ──
    // L0 WorkingMemory entities ALWAYS come first, then by injection priority + score
    let mut fused: Vec<(Entity, f32)> = candidates.into_values().collect();

    fused.sort_by(|(a, a_score), (b, b_score)| {
        // L0 always top
        let a_l0 = a.kind == EntityKind::WorkingMemory;
        let b_l0 = b.kind == EntityKind::WorkingMemory;
        match (a_l0, b_l0) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => injection_priority(a.kind)
                .cmp(&injection_priority(b.kind))
                .then_with(|| {
                    b_score
                        .partial_cmp(a_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }),
        }
    });

    let fused = deduplicate_near_duplicates(fused);

    // ── Step 5: Budget-aware Cut ──
    let (selected, mut blocks) = cut_by_budget(&fused, max_tokens);
    blocks.code_graph = graph_block;
    blocks.memory_clusters = memory_cluster_block;

    let token_estimate = estimate_tokens(&blocks);

    tracing::info!(
        "[RETRIEVAL] Query '{}' → {} entities ({} L0 recent, {} tokens)",
        user_query,
        selected.len(),
        recent_turns.len(),
        token_estimate
    );

    Ok(ContextInjection {
        entities: selected,
        blocks,
        token_estimate,
    })
}

/// Run retrieval targeted to specific memory layers (for workflow steps).
///
/// `memory_layers` is a list of EntityKind strings like ["WorkingMemory", "AtomicMemory"].
/// Only entities matching these kinds are retrieved. L0 WorkingMemory is always included
/// as conversation continuity regardless of what's in `memory_layers`.
pub fn run_retrieval_for_step(
    engine: &KnowledgeEngine,
    user_query: &str,
    _session_id: &str,
    max_tokens: usize,
    memory_layers: &[String],
) -> Result<ContextInjection> {
    // Parse memory layers to EntityKind
    let mut kinds: Vec<EntityKind> = memory_layers
        .iter()
        .filter_map(|s| EntityKind::from_str(s))
        .collect();

    // Plan/review steps still need code-symbol context even if not listed explicitly
    if !kinds.iter().any(|k| {
        matches!(
            k,
            EntityKind::CodeSymbol | EntityKind::CodeFile | EntityKind::CodeModule
        )
    }) {
        kinds.push(EntityKind::CodeSymbol);
    }

    let intent = classify_intent(user_query);

    // Always inject recent L0 turns (conversation continuity) — max 2 to avoid duplicating session msgs
    let recent_turns = engine.get_recent_turns(2);
    let mut candidates: HashMap<String, (Entity, f32)> = HashMap::new();
    for turn in &recent_turns {
        candidates
            .entry(turn.id.clone())
            .or_insert_with(|| (turn.clone(), 1.0));
    }

    // Semantic search — ONLY for the specified memory layers (not all kinds)
    if !kinds.is_empty() {
        let hits = engine.search_by_kinds(&intent.search_query, &kinds, 15, 0.2)?;
        for hit in hits {
            candidates
                .entry(hit.entity.id.clone())
                .and_modify(|(_, score)| *score = (*score + hit.score).min(2.0))
                .or_insert_with(|| (hit.entity, hit.score));
        }
    }

    // Precise file-path match
    for file_path in &intent.file_paths {
        if let Ok(symbols) = engine.find_symbols_in_file(file_path) {
            for sym in symbols {
                candidates
                    .entry(sym.id.clone())
                    .and_modify(|(_, score)| *score = (*score + 0.95).min(2.0))
                    .or_insert_with(|| (sym, 0.95));
            }
        }
    }

    let graph_block = expand_code_graph_neighbors(engine, &mut candidates, 5, 12);
    let memory_cluster_block = memory_cluster::expand_into_candidates(engine, &mut candidates, 10);

    // Fusion: L0 always top
    let mut fused: Vec<(Entity, f32)> = candidates.into_values().collect();
    fused.sort_by(|(a, a_score), (b, b_score)| {
        let a_l0 = a.kind == EntityKind::WorkingMemory;
        let b_l0 = b.kind == EntityKind::WorkingMemory;
        match (a_l0, b_l0) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => injection_priority(a.kind)
                .cmp(&injection_priority(b.kind))
                .then_with(|| {
                    b_score
                        .partial_cmp(a_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }),
        }
    });
    let fused = deduplicate_near_duplicates(fused);
    let (selected, mut blocks) = cut_by_budget(&fused, max_tokens);
    blocks.code_graph = graph_block;
    blocks.memory_clusters = memory_cluster_block;
    let token_estimate = estimate_tokens(&blocks);

    tracing::info!(
        "[RETRIEVAL-STEP] '{}' → {} entities (layers={:?}, L0={})",
        user_query,
        selected.len(),
        memory_layers,
        recent_turns.len()
    );

    Ok(ContextInjection {
        entities: selected,
        blocks,
        token_estimate,
    })
}

/// Expand top CodeSymbol hits along 1-hop `Calls` edges (in-memory EntityGraph).
fn expand_code_graph_neighbors(
    engine: &KnowledgeEngine,
    candidates: &mut HashMap<String, (Entity, f32)>,
    max_seeds: usize,
    max_neighbors: usize,
) -> String {
    let seed_ids: Vec<String> = candidates
        .iter()
        .filter(|(_, (e, _))| e.kind == EntityKind::CodeSymbol)
        .take(max_seeds)
        .map(|(id, _)| id.clone())
        .collect();
    if seed_ids.is_empty() {
        return String::new();
    }
    let neighbors = engine.graph_call_neighbors(&seed_ids, max_neighbors);
    let mut lines = Vec::new();
    for n in neighbors {
        let is_new = !candidates.contains_key(&n.id);
        if is_new {
            if let EntityMetadata::CodeSymbol {
                fq_name,
                file_path,
                start_line,
                ..
            } = &n.metadata
            {
                lines.push(format!("- → `{fq_name}` @ {file_path}:{start_line}"));
            }
            candidates.entry(n.id.clone()).or_insert_with(|| (n, 0.55));
        }
    }
    lines.join("\n")
}

/// Classify the user's query intent and extract entities.
fn classify_intent(query: &str) -> QueryIntent {
    let intent = detect_intent(query);

    // Extract file paths: common patterns like "src/auth.rs", "auth.rs"
    let file_paths = extract_file_paths(query);

    // Extract symbol name hints: words that look like function/struct names
    let symbol_hints = extract_symbol_hints(query);

    // Build clean search query (remove file paths, keep natural language)
    let mut search_query = query.to_string();
    for fp in &file_paths {
        search_query = search_query.replace(fp, "");
    }
    let mut search_query = search_query.trim().to_string();
    if search_query.is_empty() {
        search_query = query.to_string();
    }

    tracing::debug!(
        "[RETRIEVAL] Intent: {:?} | files: {:?} | hints: {:?}",
        intent,
        file_paths,
        symbol_hints
    );

    QueryIntent {
        intent,
        file_paths,
        symbol_hints,
        search_query,
    }
}

/// Extract file paths from a query string using regex.
pub fn extract_file_paths(query: &str) -> Vec<String> {
    let exts = crate::source_paths::query_path_extensions_regex();
    let re = regex::Regex::new(&format!(r"([\w./\\-]+\.({exts}))\b")).unwrap();

    let mut paths = Vec::new();
    for cap in re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let p = m.as_str().to_string();
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    paths
}

/// Extract likely symbol names from a query.
/// Heuristic: camelCase or PascalCase words, or words after "function"/"struct"/"class".
fn extract_symbol_hints(query: &str) -> Vec<String> {
    let mut hints = Vec::new();

    // CamelCase / PascalCase patterns
    let re =
        regex::Regex::new(r"\b([A-Z][a-z]+(?:[A-Z][a-z]+)+|[a-z]+(?:[A-Z][a-z]+)+)\b").unwrap();
    for cap in re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str().to_string();
            if s.len() >= 3 && !hints.contains(&s) {
                hints.push(s);
            }
        }
    }

    // Words after common code-related keywords
    for keyword in &["function", "struct", "class", "trait", "enum", "fn", "mod"] {
        if let Some(pos) = query.find(keyword) {
            let after = &query[pos + keyword.len()..];
            if let Some(word) = after.split_whitespace().next() {
                let clean: String = word
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if clean.len() >= 2 && !hints.contains(&clean) {
                    hints.push(clean);
                }
            }
        }
    }

    hints
}

/// Remove near-duplicate entities (same file_path + same kind + high content overlap).
fn deduplicate_near_duplicates(mut entities: Vec<(Entity, f32)>) -> Vec<(Entity, f32)> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    entities.retain(|(entity, _)| {
        let key = match &entity.metadata {
            EntityMetadata::CodeSymbol {
                file_path, fq_name, ..
            } => {
                format!("code:{}:{}", file_path, fq_name)
            }
            EntityMetadata::WorkingMemory {
                session_id, action, ..
            } => {
                format!("wm:{}:{}", session_id, action)
            }
            EntityMetadata::AtomicMemory { project_id, .. } => {
                format!("am:{}:{}", project_id.as_deref().unwrap_or(""), entity.id)
            }
            _ => entity.id.clone(),
        };
        if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(key) {
            e.insert(());
            true
        } else {
            false
        }
    });
    entities
}

/// Cut entities by token budget, producing formatted context blocks.
///
/// Allocation priority per design doc §5.2:
/// 1. L0 budget (75% of knowledge budget): inject WorkingMemory
/// 2. L1/L2 budget: AtomicMemory + EpisodicMemory by semantic similarity
/// 3. L3 budget: SemanticMemory only if score ≥ 0.5
fn cut_by_budget(entities: &[(Entity, f32)], max_tokens: usize) -> (Vec<Entity>, ContextBlocks) {
    let mut blocks = ContextBlocks::default();
    let mut selected: Vec<Entity> = Vec::new();
    let mut used_tokens: usize = 0;

    // Track per-kind limits
    let max_per_kind = 4;
    let max_l0 = 2;

    for (entity, score) in entities {
        if selected.len() >= 15 {
            break; // Hard cap at 15 entities total
        }
        if used_tokens >= max_tokens {
            break;
        }

        let kind_items = selected.iter().filter(|e| e.kind == entity.kind).count();
        if kind_items >= max_per_kind {
            continue; // Don't monopolize with one kind
        }
        if entity.kind == EntityKind::WorkingMemory && kind_items >= max_l0 {
            continue;
        }

        let formatted = format_entity_for_context(entity);

        // L3 SemanticMemory: only if highly relevant (score ≥ 0.5 per design doc)
        if entity.kind == EntityKind::SemanticMemory
            && (*score < 0.5 || entity.content.len() < 30) {
                continue;
            }

        // Apply signal filter
        if !entity.has_signal() {
            continue;
        }

        let tokens = formatted.len() / 4; // Rough token estimate (4 chars ≈ 1 token)
        if used_tokens + tokens > max_tokens {
            break;
        }

        used_tokens += tokens;

        // Append to the right block
        match entity.kind {
            EntityKind::WorkingMemory => {
                if !blocks.working_memory.is_empty() {
                    blocks.working_memory.push('\n');
                }
                blocks.working_memory.push_str(&formatted);
            }
            EntityKind::CodeSymbol | EntityKind::CodeFile | EntityKind::CodeModule => {
                if !blocks.code_symbols.is_empty() {
                    blocks.code_symbols.push('\n');
                }
                blocks.code_symbols.push_str(&formatted);
            }
            _ => {
                // Memory layers
                if !blocks.memories.is_empty() {
                    blocks.memories.push('\n');
                }
                blocks.memories.push_str(&formatted);
            }
        }

        selected.push(entity.clone());
    }

    (selected, blocks)
}

/// Format a single entity as a concise one-line context entry.
fn format_entity_for_context(entity: &Entity) -> String {
    match &entity.metadata {
        EntityMetadata::CodeSymbol {
            symbol_type,
            fq_name,
            file_path,
            start_line,
            end_line,
            signature,
            ..
        } => {
            let sig = if signature.len() > 80 {
                // Char-boundary-safe: `&signature[..77]` panics mid-UTF-8 char.
                let mut end = 77;
                while end > 0 && !signature.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &signature[..end])
            } else {
                signature.clone()
            };
            format!(
                "- [{}] `{}` @ {}:{}-{} — {}",
                symbol_type, fq_name, file_path, start_line, end_line, sig
            )
        }
        EntityMetadata::WorkingMemory {
            action,
            has_code_changes,
            ..
        } => {
            let marker = if *has_code_changes { " ✏️" } else { "" };
            format!("- [L0:可能为历史会话] {}{}", action, marker)
        }
        EntityMetadata::AtomicMemory { memory_type, .. } => {
            let preview: String = entity.content.chars().take(120).collect();
            format!("- [L1:背景记忆:{}] {}", memory_type, preview)
        }
        EntityMetadata::EpisodicMemory {
            episode_name,
            task_description,
            ..
        } => {
            let preview: String = task_description.chars().take(100).collect();
            format!(
                "- [L2:历史任务:{}] {} — 已结束，非本轮待办",
                episode_name, preview
            )
        }
        EntityMetadata::SemanticMemory { domain, .. } => {
            let preview: String = entity.content.chars().take(120).collect();
            format!("- [L3:{}] {}", domain, preview)
        }
        EntityMetadata::CodeFile { path, language, .. } => {
            format!("- [file] {} ({})", path, language)
        }
        EntityMetadata::CodeModule { name, path } => {
            format!("- [module] {} @ {}", name, path)
        }
    }
}

/// Estimate token count from formatted context blocks.
fn estimate_tokens(blocks: &ContextBlocks) -> usize {
    let total_chars = blocks.code_symbols.len()
        + blocks.code_graph.len()
        + blocks.memory_clusters.len()
        + blocks.memories.len()
        + blocks.working_memory.len();
    total_chars / 4 // Rough: 4 chars ≈ 1 token
}

/// Format the full context for injection into the system prompt.
///
/// Produces three sections:
/// ```
/// ## Knowledge Context (auto-retrieved)
/// ### Recent Context (L0 — Working Memory)
/// ...
/// ### Relevant Code Symbols
/// ...
/// ### Relevant Memories
/// ...
/// ```
pub fn format_context_for_prompt(
    injection: &ContextInjection,
    current_task: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    if !injection.blocks.working_memory.is_empty() {
        parts.push(format!(
            "### Recent Context (L0 — 可能为历史会话)\n{}",
            injection.blocks.working_memory
        ));
    }

    if !injection.blocks.code_symbols.is_empty() {
        parts.push(format!(
            "### Relevant Code Symbols（代码背景）\n{}",
            injection.blocks.code_symbols
        ));
    }

    if !injection.blocks.code_graph.is_empty() {
        parts.push(format!(
            "### Code Graph（调用关联 — 1-hop）\n{}",
            injection.blocks.code_graph
        ));
    }

    if !injection.blocks.memory_clusters.is_empty() {
        parts.push(format!(
            "### Memory Clusters（记忆关联图 — L0–L3）\n{}",
            injection.blocks.memory_clusters
        ));
    }

    if !injection.blocks.memories.is_empty() {
        parts.push(format!(
            "### Relevant Memories (L1-L3 — 历史/背景)\n{}",
            injection.blocks.memories
        ));
    }

    if parts.is_empty() {
        return String::new();
    }

    let task_anchor = current_task
        .filter(|t| !t.trim().is_empty())
        .map(|t| {
            format!(
                "🎯 **本轮任务锚点（CURRENT）**: {}\n\n",
                t.chars().take(600).collect::<String>()
            )
        })
        .unwrap_or_default();

    format!(
        "[KNOWLEDGE_RETRIEVAL — HISTORICAL/BACKGROUND ONLY]\n\
         {task_anchor}\
         ⚠️ 以下由知识库自动检索，可能来自**过往任务或其它会话（HISTORICAL）**。\n\
         仅作背景参考；**不得**将其中描述当作本轮待办或继续执行。\n\n\
         ## Knowledge Context (auto-retrieved, ~{} tokens)\n\n{}",
        injection.token_estimate,
        parts.join("\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_paths() {
        let paths = extract_file_paths("look at src/auth.rs and src/main.rs");
        assert!(paths.contains(&"src/auth.rs".to_string()));
        assert!(paths.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_extract_file_paths_no_match() {
        let paths = extract_file_paths("how does token validation work?");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_symbol_hints_camelcase() {
        let hints = extract_symbol_hints("explain validateToken and UserAuth");
        assert!(
            hints
                .iter()
                .any(|h| h == "validateToken" || h == "UserAuth")
        );
    }

    #[test]
    fn test_extract_symbol_hints_from_keywords() {
        let hints = extract_symbol_hints("how does struct OrderProcessor work?");
        assert!(hints.contains(&"OrderProcessor".to_string()));
    }

    #[test]
    fn test_format_entity_code_symbol() {
        let entity = Entity::code_symbol(
            "validate_token",
            "auth::validate_token",
            crate::knowledge::entity::SymbolType::Function,
            "rust",
            "src/auth.rs",
            42,
            58,
            "fn validate_token(token: &Token) -> Result<bool>",
            None,
        );
        let formatted = format_entity_for_context(&entity);
        assert!(formatted.contains("validate_token"));
        assert!(formatted.contains("src/auth.rs"));
    }

    #[test]
    fn test_format_entity_working_memory() {
        let entity = Entity::working_memory(
            "sess-1",
            "fixed auth bug",
            None,
            Some("patched file"),
            vec!["edit_file".into()],
            true,
        );
        let formatted = format_entity_for_context(&entity);
        assert!(formatted.contains("可能为历史会话"));
        assert!(formatted.contains("✏️"));
    }

    #[test]
    fn test_deduplicate_near_duplicates() {
        let e1 = (
            Entity::code_symbol(
                "a",
                "a::a",
                crate::knowledge::entity::SymbolType::Function,
                "rust",
                "src/a.rs",
                1,
                2,
                "fn a()",
                None,
            ),
            0.9,
        );
        let e2 = (
            Entity::code_symbol(
                "a",
                "a::a",
                crate::knowledge::entity::SymbolType::Function,
                "rust",
                "src/a.rs",
                1,
                2,
                "fn a()",
                None,
            ),
            0.8,
        );
        let deduped = deduplicate_near_duplicates(vec![e1, e2]);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn test_format_context_for_prompt() {
        let injection = ContextInjection {
            entities: vec![],
            blocks: ContextBlocks {
                code_symbols: "- [function] `auth::validate_token` @ src/auth.rs:42-58".into(),
                code_graph: String::new(),
                memory_clusters: String::new(),
                memories: String::new(),
                working_memory: "- [L0] fixed auth bug ✏️".into(),
            },
            token_estimate: 15,
        };
        let formatted = format_context_for_prompt(&injection, None);
        assert!(formatted.contains("Knowledge Context"));
        assert!(formatted.contains("HISTORICAL"));
        assert!(formatted.contains("可能为历史会话"));
        assert!(formatted.contains("Relevant Code Symbols"));
        assert!(!formatted.contains("Relevant Memories")); // Empty block omitted
    }
}
