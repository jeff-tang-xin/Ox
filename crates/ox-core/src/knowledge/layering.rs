/// Auto-Layering Engine — progressive memory promotion with rule prefilter + LLM confirmation.
///
/// # Four-Layer Promotion Model
/// - **L0→L1** (WorkingMemory → AtomicMemory): ≥3 turns modifying same symbol → candidate fact
/// - **L1→L2** (AtomicMemory → EpisodicMemory): ≥5 facts sharing topic/project → candidate episode
/// - **L2→L3** (EpisodicMemory → SemanticMemory): ≥3 episodes sharing architectural pattern → candidate abstraction
///
/// # Confirmation Pacing
/// Candidates are batched. LLM confirmation fires only when:
/// - Candidates have accumulated (candidate_count > 0)
/// - Enough time/iterations have passed since last confirmation (≥ N agent iterations)
///
/// This prevents token-burning on every turn while still automating the layering process.

use super::entity::{Entity, EntityKind, EntityMetadata, Relation, RelationType};
use super::graph::EntityGraph;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Configuration
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Thresholds that trigger each promotion level.
#[derive(Debug, Clone)]
pub struct LayeringConfig {
    /// L0→L1: min WorkingMemory turns modifying the same symbol
    pub l0_to_l1_min_turns: u32,
    /// L1→L2: min AtomicMemory facts sharing the same topic
    pub l1_to_l2_min_facts: u32,
    /// L2→L3: min EpisodicMemory sharing the same architectural pattern
    pub l2_to_l3_min_episodes: u32,
    /// Minimum agent iterations between LLM confirmation calls
    pub confirmation_interval: u32,
    /// Max candidates per batch (to limit prompt size)
    pub max_candidates_per_batch: usize,
}

impl Default for LayeringConfig {
    fn default() -> Self {
        Self {
            l0_to_l1_min_turns: 3,
            l1_to_l2_min_facts: 5,
            l2_to_l3_min_episodes: 3,
            confirmation_interval: 5,
            max_candidates_per_batch: 3,
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Candidates
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A layering candidate detected by the rule prefilter.
#[derive(Debug, Clone)]
pub enum LayeringCandidate {
    /// L0→L1: WorkingMemory turns → new AtomicMemory
    L0ToL1 {
        /// Source WorkingMemory entity IDs
        source_turns: Vec<String>,
        /// Common symbol IDs modified across these turns
        common_symbols: Vec<String>,
        /// Suggested fact summary (for LLM prompt)
        summary_hint: String,
    },
    /// L1→L2: AtomicMemory facts → new EpisodicMemory checkpoint
    L1ToL2 {
        /// Source AtomicMemory entity IDs
        source_facts: Vec<String>,
        /// Shared topic / domain
        topic: String,
        /// Suggested episode name
        episode_hint: String,
    },
    /// L2→L3: EpisodicMemory checkpoints → new SemanticMemory abstraction
    L2ToL3 {
        /// Source EpisodicMemory entity IDs
        source_episodes: Vec<String>,
        /// Shared architectural pattern / domain
        pattern_hint: String,
        /// Suggested abstraction domain
        domain: String,
    },
}

/// The result of applying LLM-confirmed promotions.
#[derive(Debug, Clone)]
pub struct LayeringResult {
    /// Newly created entities (at promoted layers)
    pub new_entities: Vec<Entity>,
    /// IDs of source entities that should be marked as consumed/abstracted
    pub updated_source_ids: Vec<String>,
    /// How many candidates were confirmed vs rejected
    pub confirmed_count: usize,
    pub rejected_count: usize,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// AutoLayering Engine
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

pub struct AutoLayering {
    config: LayeringConfig,
    /// Tracks how many agent iterations have passed since last LLM confirmation
    iterations_since_confirm: u32,
    /// Accumulated candidates waiting for confirmation
    pending_candidates: Vec<LayeringCandidate>,
}

impl AutoLayering {
    pub fn new(config: LayeringConfig) -> Self {
        Self {
            config,
            iterations_since_confirm: 0,
            pending_candidates: Vec::new(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(LayeringConfig::default())
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Prefilter — rule-based candidate detection (no LLM involved)
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Run the rule prefilter over the current EntityGraph.
    /// Returns new candidates detected. Call this after each tool execution.
    pub fn prefilter(&mut self, graph: &EntityGraph) -> &[LayeringCandidate] {
        let start_len = self.pending_candidates.len();

        // ── L0→L1: WorkingMemory turns modifying same symbols ──
        self.detect_l0_to_l1(graph);

        // ── L1→L2: AtomicMemory facts sharing topics ──
        self.detect_l1_to_l2(graph);

        // ── L2→L3: EpisodicMemory sharing patterns ──
        self.detect_l2_to_l3(graph);

        if self.pending_candidates.len() > start_len {
            tracing::info!(
                "[AUTO_LAYERING] Detected {} new candidates (total={})",
                self.pending_candidates.len() - start_len,
                self.pending_candidates.len()
            );
        }

        &self.pending_candidates
    }

    /// L0→L1: Group WorkingMemory entities by the CodeSymbol they modified.
    /// If ≥3 turns modified the same symbol, they're candidates for fact extraction.
    fn detect_l0_to_l1(&mut self, graph: &EntityGraph) {
        let groups = graph.group_modifications_by_symbol();

        for (symbol_id, turn_ids) in &groups {
            if turn_ids.len() as u32 >= self.config.l0_to_l1_min_turns {
                // Avoid duplicates: check if we already have a candidate for these turns
                let already_pending = self.pending_candidates.iter().any(|c| {
                    if let LayeringCandidate::L0ToL1 { source_turns, .. } = c {
                        source_turns.iter().any(|t| turn_ids.contains(t))
                    } else {
                        false
                    }
                });
                if already_pending {
                    continue;
                }

                // Build a summary hint from the turn actions
                let actions: Vec<String> = turn_ids.iter()
                    .filter_map(|tid| {
                        graph.get(tid).and_then(|e| {
                            if let EntityMetadata::WorkingMemory { ref action, .. } = e.metadata {
                                Some(action.clone())
                            } else {
                                None
                            }
                        })
                    })
                    .collect();

                let hint = actions.join("; ");

                if self.pending_candidates.len() < self.config.max_candidates_per_batch * 3 {
                    self.pending_candidates.push(LayeringCandidate::L0ToL1 {
                        source_turns: turn_ids.clone(),
                        common_symbols: vec![symbol_id.clone()],
                        summary_hint: hint,
                    });
                }
            }
        }
    }

    /// L1→L2: Group AtomicMemory entities by shared topic or related_files.
    /// If ≥5 facts share a topic, cluster into an EpisodicMemory.
    fn detect_l1_to_l2(&mut self, graph: &EntityGraph) {
        // Group AtomicMemory entities by shared topic or related_files
        let facts: Vec<&Entity> = graph.entities_of_kind(EntityKind::AtomicMemory);
        if facts.len() < self.config.l1_to_l2_min_facts as usize {
            return;
        }

        use std::collections::HashMap;

        // Group by project_id (or empty for global)
        let mut by_project: HashMap<String, Vec<String>> = HashMap::new();
        for f in &facts {
            if let EntityMetadata::AtomicMemory { ref project_id, .. } = f.metadata {
                let key = project_id.as_deref().unwrap_or("global").to_string();
                by_project.entry(key).or_default().push(f.id.clone());
            }
        }

        for (project, fact_ids) in &by_project {
            if fact_ids.len() as u32 >= self.config.l1_to_l2_min_facts {
                let already_pending = self.pending_candidates.iter().any(|c| {
                    if let LayeringCandidate::L1ToL2 { source_facts, .. } = c {
                        source_facts.iter().any(|f| fact_ids.contains(f))
                    } else {
                        false
                    }
                });
                if already_pending {
                    continue;
                }

                // Build a topic hint from the first few facts
                let topic_items: Vec<String> = fact_ids.iter().take(3)
                    .filter_map(|id| graph.get(id))
                    .map(|e| e.content.chars().take(80).collect())
                    .collect();
                let topic = format!("{} ({} facts)", project, fact_ids.len());
                let episode_hint = topic_items.join(" | ");

                if self.pending_candidates.len() < self.config.max_candidates_per_batch * 3 {
                    self.pending_candidates.push(LayeringCandidate::L1ToL2 {
                        source_facts: fact_ids.clone(),
                        topic,
                        episode_hint,
                    });
                }
            }
        }
    }

    /// L2→L3: Group EpisodicMemory by shared domain keywords.
    /// If ≥3 episodes share an architectural pattern, abstract to SemanticMemory.
    fn detect_l2_to_l3(&mut self, graph: &EntityGraph) {
        // Group EpisodicMemory entities by shared domain/pattern keywords
        let episodes: Vec<&Entity> = graph.entities_of_kind(EntityKind::EpisodicMemory);
        if episodes.len() < self.config.l2_to_l3_min_episodes as usize {
            return;
        }

        // Group by common architectural keywords found in content
        use std::collections::HashMap;
        let mut by_domain: HashMap<String, Vec<String>> = HashMap::new();

        for ep in &episodes {
            let domain = classify_domain(&ep.content);
            by_domain.entry(domain).or_default().push(ep.id.clone());
        }

        for (domain, episode_ids) in &by_domain {
            if episode_ids.len() as u32 >= self.config.l2_to_l3_min_episodes {
                let already_pending = self.pending_candidates.iter().any(|c| {
                    if let LayeringCandidate::L2ToL3 { source_episodes, .. } = c {
                        source_episodes.iter().any(|e| episode_ids.contains(e))
                    } else {
                        false
                    }
                });
                if already_pending {
                    continue;
                }

                let pattern_hint = format!(
                    "{} episodes in domain '{}'",
                    episode_ids.len(),
                    domain
                );

                if self.pending_candidates.len() < self.config.max_candidates_per_batch * 3 {
                    self.pending_candidates.push(LayeringCandidate::L2ToL3 {
                        source_episodes: episode_ids.clone(),
                        pattern_hint,
                        domain: domain.clone(),
                    });
                }
            }
        }
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Confirmation pacing
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Should we trigger an LLM confirmation call right now?
    ///
    /// Returns true when:
    /// - Candidates are pending
    /// - Enough iterations have passed since last confirmation
    pub fn should_confirm(&self) -> bool {
        !self.pending_candidates.is_empty()
            && self.iterations_since_confirm >= self.config.confirmation_interval
    }

    /// Mark that one agent iteration has passed.
    pub fn tick(&mut self) {
        self.iterations_since_confirm += 1;
    }

    /// Get the current candidates for LLM confirmation, then clear the queue.
    pub fn drain_candidates(&mut self) -> Vec<LayeringCandidate> {
        self.iterations_since_confirm = 0;
        std::mem::take(&mut self.pending_candidates)
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // LLM Prompt generation
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Build the confirmation prompt for the LLM judge.
    ///
    /// This is a compact, structured prompt that presents each candidate
    /// and asks the LLM to either confirm (with upgraded content) or reject.
    pub fn build_confirmation_prompt(
        candidates: &[LayeringCandidate],
        graph: &EntityGraph,
    ) -> String {
        let mut prompt = String::from(
            "## Auto-Layering: Knowledge Promotion Candidates\n\n\
             The system has auto-detected the following knowledge upgrade candidates. \
             For each candidate, decide: **APPROVE** (generate the upgraded content) or **REJECT** \
             (the pattern is noise).\n\n\
             Respond ONLY with a JSON array:\n\
             [{\"id\":\"...\", \"decision\":\"approve\"|\"reject\", \"upgraded_content\":\"...\"}]\n\n",
        );

        for (idx, candidate) in candidates.iter().enumerate() {
            prompt.push_str(&format!("### Candidate {}\n", idx + 1));

            match candidate {
                LayeringCandidate::L0ToL1 { source_turns, common_symbols, summary_hint } => {
                    prompt.push_str("**Type**: L0 WorkingMemory → L1 AtomicMemory\n");
                    prompt.push_str(&format!("**Source turns**: {} turns\n", source_turns.len()));
                    prompt.push_str(&format!(
                        "**Common symbols**: {}\n",
                        common_symbols.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    ));
                    prompt.push_str(&format!("**Context**: {}\n", summary_hint));

                    // Include turn details from graph
                    for tid in source_turns.iter().take(3) {
                        if let Some(e) = graph.get(tid) {
                            prompt.push_str(&format!("  - {}\n", e.content.chars().take(150).collect::<String>()));
                        }
                    }
                    prompt.push_str("**If approved, produce**: A single L1 AtomicMemory fact capturing what was learned.\n\n");
                }
                LayeringCandidate::L1ToL2 { source_facts, topic, episode_hint } => {
                    prompt.push_str("**Type**: L1 AtomicMemory → L2 EpisodicMemory\n");
                    prompt.push_str(&format!("**Source facts**: {} facts\n", source_facts.len()));
                    prompt.push_str(&format!("**Topic**: {}\n", topic));
                    prompt.push_str(&format!("**Hint**: {}\n", episode_hint));
                    prompt.push_str("**If approved, produce**: An L2 EpisodicMemory checkpoint summarizing the task/topic.\n\n");
                }
                LayeringCandidate::L2ToL3 { source_episodes, pattern_hint, domain } => {
                    prompt.push_str("**Type**: L2 EpisodicMemory → L3 SemanticMemory\n");
                    prompt.push_str(&format!("**Source episodes**: {} episodes\n", source_episodes.len()));
                    prompt.push_str(&format!("**Pattern**: {}\n", pattern_hint));
                    prompt.push_str(&format!("**Domain**: {}\n", domain));
                    prompt.push_str("**If approved, produce**: An L3 SemanticMemory abstraction of the architectural pattern.\n\n");
                }
            }
        }

        prompt
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // Apply LLM confirmation
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    /// Parse the LLM judge's JSON response and create promoted entities.
    ///
    /// # Arguments
    /// * `candidates` — The original candidates sent to the LLM
    /// * `llm_response` — The JSON array response from the LLM
    /// * `session_id` — Current session ID for entity creation
    pub fn apply_confirmation(
        candidates: &[LayeringCandidate],
        llm_response: &str,
        session_id: &str,
    ) -> LayeringResult {
        let decisions = parse_llm_decisions(llm_response);
        let mut new_entities = Vec::new();
        let mut updated_source_ids = Vec::new();
        let mut confirmed_count = 0;
        let mut rejected_count = 0;

        for (idx, candidate) in candidates.iter().enumerate() {
            let decision = decisions.iter().find(|d| d.id == format!("c{}", idx + 1));
            let is_approved = decision
                .map(|d| d.decision == "approve")
                .unwrap_or(false);
            let content = decision
                .and_then(|d| d.upgraded_content.clone())
                .unwrap_or_default();

            if is_approved && !content.is_empty() {
                confirmed_count += 1;
                match candidate {
                    LayeringCandidate::L0ToL1 { source_turns, .. } => {
                        let mut entity = Entity::atomic_memory(
                            &content, "Fact", None, "", "LlmExtraction",
                        );
                        // Link to source turns
                        for tid in source_turns {
                            entity.relations.push(Relation {
                                target_id: tid.clone(),
                                relation_type: RelationType::Precedes,
                                weight: 0.8,
                            });
                        }
                        new_entities.push(entity);
                        updated_source_ids.extend(source_turns.iter().cloned());
                    }
                    LayeringCandidate::L1ToL2 { source_facts, topic, .. } => {
                        let mut entity = Entity::episodic_memory(
                            &topic, session_id, None, &content,
                        );
                        for fid in source_facts {
                            entity.relations.push(Relation {
                                target_id: fid.clone(),
                                relation_type: RelationType::BelongsTo,
                                weight: 0.8,
                            });
                        }
                        new_entities.push(entity);
                        updated_source_ids.extend(source_facts.iter().cloned());
                    }
                    LayeringCandidate::L2ToL3 { source_episodes, .. } => {
                        let mut entity = Entity::semantic_memory(
                            "", &content, "architecture", source_episodes.clone(),
                        );
                        for eid in source_episodes {
                            entity.relations.push(Relation {
                                target_id: eid.clone(),
                                relation_type: RelationType::Abstracts,
                                weight: 0.9,
                            });
                        }
                        new_entities.push(entity);
                        updated_source_ids.extend(source_episodes.iter().cloned());
                    }
                }
            } else {
                rejected_count += 1;
            }
        }

        tracing::info!(
            "[AUTO_LAYERING] Applied confirmations: {} approved, {} rejected, {} new entities",
            confirmed_count, rejected_count, new_entities.len()
        );

        LayeringResult {
            new_entities,
            updated_source_ids,
            confirmed_count,
            rejected_count,
        }
    }

    /// Promote memory layers using rule prefilter only (no LLM judge).
    /// Uses each candidate's hint text as the stored content.
    pub fn apply_rule_based_promotions(
        &mut self,
        graph: &EntityGraph,
        session_id: &str,
        project_id: Option<&str>,
    ) -> LayeringResult {
        self.prefilter(graph);
        let candidates = self.drain_candidates();
        let mut new_entities = Vec::new();
        let mut updated_source_ids = Vec::new();
        let mut confirmed_count = 0;

        for candidate in &candidates {
            match candidate {
                LayeringCandidate::L0ToL1 {
                    source_turns,
                    summary_hint,
                    ..
                } if !summary_hint.is_empty() => {
                    let mut entity = Entity::atomic_memory(
                        summary_hint,
                        "Fact",
                        project_id,
                        "",
                        "AutoLayering",
                    );
                    for tid in source_turns {
                        entity.relations.push(Relation {
                            target_id: tid.clone(),
                            relation_type: RelationType::Precedes,
                            weight: 0.8,
                        });
                    }
                    new_entities.push(entity);
                    updated_source_ids.extend(source_turns.iter().cloned());
                    confirmed_count += 1;
                }
                LayeringCandidate::L1ToL2 {
                    source_facts,
                    topic,
                    episode_hint,
                } => {
                    let content = if episode_hint.is_empty() {
                        topic.clone()
                    } else {
                        format!("{topic}\n{episode_hint}")
                    };
                    let mut entity =
                        Entity::episodic_memory(topic, session_id, project_id, &content);
                    for fid in source_facts {
                        entity.relations.push(Relation {
                            target_id: fid.clone(),
                            relation_type: RelationType::BelongsTo,
                            weight: 0.8,
                        });
                    }
                    new_entities.push(entity);
                    updated_source_ids.extend(source_facts.iter().cloned());
                    confirmed_count += 1;
                }
                LayeringCandidate::L2ToL3 {
                    source_episodes,
                    pattern_hint,
                    domain,
                } => {
                    let mut entity = Entity::semantic_memory(
                        project_id.unwrap_or("global"),
                        pattern_hint,
                        domain,
                        source_episodes.clone(),
                    );
                    for eid in source_episodes {
                        entity.relations.push(Relation {
                            target_id: eid.clone(),
                            relation_type: RelationType::Abstracts,
                            weight: 0.9,
                        });
                    }
                    new_entities.push(entity);
                    updated_source_ids.extend(source_episodes.iter().cloned());
                    confirmed_count += 1;
                }
                _ => {}
            }
        }

        let rejected_count = candidates.len().saturating_sub(confirmed_count);
        LayeringResult {
            new_entities,
            updated_source_ids,
            confirmed_count,
            rejected_count,
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// LLM Response Parsing
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Clone)]
struct LlmDecision {
    id: String,
    decision: String,
    upgraded_content: Option<String>,
}

/// Parse the LLM's JSON array response. Tolerant of markdown fences and trailing text.
fn parse_llm_decisions(raw: &str) -> Vec<LlmDecision> {
    // Strip markdown code fences if present
    let json_str = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => {
            // Try to find the first JSON array in the response
            if let Some(start) = json_str.find('[') {
                if let Some(end) = json_str.rfind(']') {
                    let slice = &json_str[start..=end];
                    serde_json::from_str(slice).unwrap_or_default()
                } else {
                    return Vec::new();
                }
            } else {
                return Vec::new();
            }
        }
    };

    parsed
        .into_iter()
        .filter_map(|v| {
            Some(LlmDecision {
                id: v.get("id")?.as_str()?.to_string(),
                decision: v.get("decision")?.as_str()?.to_string(),
                upgraded_content: v.get("upgraded_content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string()),
            })
        })
        .collect()
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helpers
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Classify content into a domain for L2→L3 grouping.
fn classify_domain(content: &str) -> String {
    let lower = content.to_lowercase();
    if lower.contains("architecture") || lower.contains("module") || lower.contains("trait") || lower.contains("interface") {
        "architecture".into()
    } else if lower.contains("test") || lower.contains("bug") || lower.contains("error") || lower.contains("fix") {
        "debugging".into()
    } else if lower.contains("build") || lower.contains("deploy") || lower.contains("ci") || lower.contains("release") {
        "deployment".into()
    } else if lower.contains("style") || lower.contains("format") || lower.contains("lint") {
        "coding_style".into()
    } else if lower.contains("api") || lower.contains("endpoint") || lower.contains("auth") {
        "api_design".into()
    } else if lower.contains("db") || lower.contains("sql") || lower.contains("query") || lower.contains("cache") {
        "data_layer".into()
    } else {
        "general".into()
    }
}
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Export an EpisodicMemory entity to a human-readable Markdown file.
pub fn export_episode_to_markdown(entity: &Entity, related_atoms: &[&Entity]) -> String {
    let em = match &entity.metadata {
        EntityMetadata::EpisodicMemory {
            episode_name, start_time, end_time,
            task_description, conclusions, unresolved,
            continuation_hint, ..
        } => {
            let start_dt = chrono::DateTime::from_timestamp(*start_time, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            let end_dt = end_time
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "ongoing".to_string());

            format!(
                "# Episode: {name}\n\n\
                 **ID**: {id}\n\
                 **Started**: {start_dt}\n\
                 **Ended**: {end_dt}\n\n\
                 ---\n\n\
                 ## Task\n\n{task}\n\n\
                 ## Conclusions\n\n{conclusions}\n\n\
                 ## Unresolved\n\n{unresolved}\n\n\
                 ## Continuation\n\n{hint}\n\n\
                 ## Related Facts\n\n{atoms}\n",
                name = episode_name,
                id = entity.id,
                task = task_description,
                conclusions = conclusions.iter().map(|c| format!("- {}", c)).collect::<Vec<_>>().join("\n"),
                unresolved = if unresolved.is_empty() { "(none)".into() } else { unresolved.join("\n") },
                hint = continuation_hint.as_deref().unwrap_or("(none)"),
                atoms = related_atoms.iter()
                    .map(|a| format!("- [L1] {}", a.content.chars().take(200).collect::<String>()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        _ => return String::new(),
    };
    em
}

/// Export a SemanticMemory entity to a human-readable Markdown file.
pub fn export_semantic_to_markdown(entity: &Entity, related_episodes: &[&Entity]) -> String {
    let sm = match &entity.metadata {
        EntityMetadata::SemanticMemory { domain, version, confidence, .. } => {
            format!(
                "# Semantic Memory: {domain}\n\n\
                 **ID**: {id}\n\
                 **Version**: v{version}\n\
                 **Confidence**: {confidence_pct}\n\n\
                 ---\n\n\
                 ## Abstraction\n\n{content}\n\n\
                 ## Source Episodes\n\n{episodes}\n",
                domain = domain,
                id = entity.id,
                version = version,
                confidence_pct = (confidence * 100.0) as u32,
                content = entity.content,
                episodes = related_episodes.iter()
                    .map(|e| format!("- {}", e.content.chars().take(200).collect::<String>()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        _ => return String::new(),
    };
    sm
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::entity::{SymbolType, EntityKind};

    fn make_entity(id: &str, kind: EntityKind, content: &str) -> Entity {
        let _now = chrono::Utc::now().timestamp();
        Entity {
            id: id.to_string(),
            kind,
            content: content.to_string(),
            coordinate: crate::knowledge::entity::MemoryCoordinate::new(
                kind.depth().unwrap_or(0), id, 384,
            ),
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
                    session_id: "sess-1".into(),
                    action: content.to_string(),
                    intent: None,
                    result: None,
                    tools_used: vec!["edit_file".into()],
                    has_code_changes: true,
                    modified_entities: vec![],
                    self_state: None,
                },
                EntityKind::AtomicMemory => EntityMetadata::AtomicMemory {
                    memory_type: "Fact".into(),
                    project_id: None,
                    language: String::new(),
                    source: "LlmExtraction".into(),
                    related_files: vec![],
                    quality_score: 0.7,
                    judge_eval_count: 1,
                },
                EntityKind::EpisodicMemory => EntityMetadata::EpisodicMemory {
                    episode_name: content.to_string(),
                    project_id: None,
                    session_id: "sess-1".into(),
                    start_time: chrono::Utc::now().timestamp(),
                    end_time: None,
                    task_description: content.to_string(),
                    conclusions: vec![],
                    unresolved: vec![],
                    continuation_hint: None,
                    usage_count: 0,
                    related_atoms: vec![],
                },
                EntityKind::SemanticMemory => EntityMetadata::SemanticMemory {
                    project_id: String::new(),
                    version: 1,
                    domain: "architecture".into(),
                    source_episodes: vec![],
                    confidence: 0.8,
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
    fn test_should_confirm_requires_candidates_and_interval() {
        let mut layering = AutoLayering::with_defaults();
        assert!(!layering.should_confirm()); // No candidates, no iterations

        // Simulate candidates present but not enough iterations
        layering.pending_candidates.push(LayeringCandidate::L0ToL1 {
            source_turns: vec!["t1".into(), "t2".into(), "t3".into()],
            common_symbols: vec!["sym1".into()],
            summary_hint: "test".into(),
        });
        assert!(!layering.should_confirm()); // iterations_since_confirm = 0

        // Tick past the interval
        for _ in 0..5 {
            layering.tick();
        }
        assert!(layering.should_confirm());
    }

    #[test]
    fn test_drain_candidates_resets_state() {
        let mut layering = AutoLayering::with_defaults();
        layering.iterations_since_confirm = 5;
        layering.pending_candidates.push(LayeringCandidate::L0ToL1 {
            source_turns: vec!["t1".into()],
            common_symbols: vec![],
            summary_hint: "test".into(),
        });

        let drained = layering.drain_candidates();
        assert_eq!(drained.len(), 1);
        assert!(layering.pending_candidates.is_empty());
        assert_eq!(layering.iterations_since_confirm, 0);
    }

    #[test]
    fn test_build_confirmation_prompt() {
        let candidates = vec![
            LayeringCandidate::L0ToL1 {
                source_turns: vec!["t1".into(), "t2".into(), "t3".into()],
                common_symbols: vec!["auth::validate_token".into()],
                summary_hint: "Fixed token expiry handling".into(),
            },
        ];
        let graph = EntityGraph::new();
        let prompt = AutoLayering::build_confirmation_prompt(&candidates, &graph);
        assert!(prompt.contains("L0 WorkingMemory"));
        assert!(prompt.contains("L1 AtomicMemory"));
        assert!(prompt.contains("validate_token"));
    }

    #[test]
    fn test_parse_llm_decisions_basic() {
        let response = r#"[
            {"id": "c1", "decision": "approve", "upgraded_content": "Token validation needs expiry check before refresh"},
            {"id": "c2", "decision": "reject", "upgraded_content": null}
        ]"#;

        let decisions = parse_llm_decisions(response);
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].decision, "approve");
        assert_eq!(decisions[1].decision, "reject");
    }

    #[test]
    fn test_parse_llm_decisions_with_fences() {
        let response = "```json\n[{\"id\": \"c1\", \"decision\": \"approve\", \"upgraded_content\": \"Test fact\"}]\n```";
        let decisions = parse_llm_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].upgraded_content.as_deref(), Some("Test fact"));
    }

    #[test]
    fn test_apply_confirmation_approve_l0_to_l1() {
        let candidates = vec![
            LayeringCandidate::L0ToL1 {
                source_turns: vec!["t1".into(), "t2".into(), "t3".into()],
                common_symbols: vec!["sym1".into()],
                summary_hint: "test".into(),
            },
        ];
        let response = r#"[{"id": "c1", "decision": "approve", "upgraded_content": "Token must be validated before use"}]"#;
        let result = AutoLayering::apply_confirmation(&candidates, response, "sess-1");

        assert_eq!(result.confirmed_count, 1);
        assert_eq!(result.rejected_count, 0);
        assert_eq!(result.new_entities.len(), 1);
        assert_eq!(result.new_entities[0].kind, EntityKind::AtomicMemory);
        assert!(result.new_entities[0].content.contains("Token"));
    }

    #[test]
    fn test_apply_confirmation_reject() {
        let candidates = vec![
            LayeringCandidate::L0ToL1 {
                source_turns: vec!["t1".into()],
                common_symbols: vec![],
                summary_hint: "noise".into(),
            },
        ];
        let response = r#"[{"id": "c1", "decision": "reject", "upgraded_content": null}]"#;
        let result = AutoLayering::apply_confirmation(&candidates, response, "sess-1");

        assert_eq!(result.confirmed_count, 0);
        assert_eq!(result.rejected_count, 1);
        assert!(result.new_entities.is_empty());
    }

    #[test]
    fn test_export_episode_to_markdown() {
        let entity = make_entity("ep1", EntityKind::EpisodicMemory, "Fixed auth bug");
        let markdown = export_episode_to_markdown(&entity, &[]);
        assert!(markdown.contains("Episode: Fixed auth bug"));
        assert!(markdown.contains("ep1"));
    }

    #[test]
    fn test_export_semantic_to_markdown() {
        let entity = make_entity("sm1", EntityKind::SemanticMemory, "Hexagonal architecture");
        let markdown = export_semantic_to_markdown(&entity, &[]);
        assert!(markdown.contains("Semantic Memory: architecture"));
        assert!(markdown.contains("Hexagonal architecture"));
    }
}
