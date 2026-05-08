pub mod orchestrator;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilSession {
    pub id: String,
    pub question: String,
    pub participants: Vec<Participant>,
    pub rounds: u8,
    pub phases: Vec<DebatePhase>,
    pub arbitration: Option<Arbitration>,
    pub token_usage: CouncilTokenUsage,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub role: ParticipantRole,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantRole {
    Proposer,
    Reviewer,
    Arbiter,
}

impl Participant {
    pub fn label(&self) -> String {
        format!("{}:{}", self.provider, self.model)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DebatePhase {
    Proposal(Vec<Proposal>),
    CrossReview(Vec<Review>),
    Rebuttal(Vec<Rebuttal>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DebatePhaseType {
    Proposal,
    CrossReview,
    Rebuttal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub participant_idx: usize,
    pub content: String,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub reviewer_idx: usize,
    pub target_idx: usize,
    pub critique: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rebuttal {
    pub participant_idx: usize,
    pub original_proposal: String,
    pub response_to_critiques: String,
    pub revised_proposal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arbitration {
    pub arbiter_idx: usize,
    pub final_recommendation: String,
    pub primary_source_idx: usize,
    pub reasoning: String,
    pub key_disagreements: Vec<String>,
    pub comparison_table: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CouncilTokenUsage {
    pub total_prompt: u32,
    pub total_completion: u32,
    pub estimated_cost: f64,
}

impl CouncilTokenUsage {
    pub fn add(&mut self, prompt: u32, completion: u32, cost: f64) {
        self.total_prompt += prompt;
        self.total_completion += completion;
        self.estimated_cost += cost;
    }
}

// ── Output formatting ──

impl CouncilSession {
    pub fn format_summary(&self) -> String {
        let arb = match &self.arbitration {
            Some(a) => a,
            None => return "Council session incomplete — no arbitration yet.".to_string(),
        };

        let participants: Vec<String> = self.participants.iter().map(|p| p.label()).collect();
        let disagreements = if arb.key_disagreements.is_empty() {
            "  (none)".to_string()
        } else {
            arb.key_disagreements
                .iter()
                .map(|d| format!("  - {}", d))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "╔══════════════════════════════════════════════════╗\n\
             ║  Council Conclusion                              ║\n\
             ╠══════════════════════════════════════════════════╣\n\
             ║                                                  ║\n\
             Recommendation: {}\n\
             Confidence: {:.2}\n\
             ║                                                  ║\n\
             Key disagreements:\n\
             {}\n\
             ║                                                  ║\n\
             Participants: {}\n\
             Rounds: {} | Tokens: {} | Cost: ${:.4}\n\
             ╚══════════════════════════════════════════════════╝",
            arb.final_recommendation,
            arb.confidence,
            disagreements,
            participants.join(" / "),
            self.rounds,
            self.token_usage.total_prompt + self.token_usage.total_completion,
            self.token_usage.estimated_cost,
        )
    }

    pub fn format_verbose(&self) -> String {
        let mut out = String::new();

        for phase in &self.phases {
            match phase {
                DebatePhase::Proposal(proposals) => {
                    out.push_str("──── Phase: Independent Proposals ────\n");
                    for p in proposals {
                        let label = self
                            .participants
                            .get(p.participant_idx)
                            .map(|p| p.label())
                            .unwrap_or_else(|| "unknown".into());
                        out.push_str(&format!(
                            "[{}] Proposal:\n  {}\n  Reasoning: {}\n\n",
                            label, p.content, p.reasoning
                        ));
                    }
                }
                DebatePhase::CrossReview(reviews) => {
                    out.push_str("──── Phase: Cross Review ────\n");
                    for r in reviews {
                        let reviewer = self
                            .participants
                            .get(r.reviewer_idx)
                            .map(|p| p.label())
                            .unwrap_or_else(|| "unknown".into());
                        let target = self
                            .participants
                            .get(r.target_idx)
                            .map(|p| p.label())
                            .unwrap_or_else(|| "unknown".into());
                        out.push_str(&format!(
                            "[{} → {}] Score: {:.2}\n  {}\n\n",
                            reviewer, target, r.score, r.critique
                        ));
                    }
                }
                DebatePhase::Rebuttal(rebuttals) => {
                    out.push_str("──── Phase: Rebuttal ────\n");
                    for rb in rebuttals {
                        let label = self
                            .participants
                            .get(rb.participant_idx)
                            .map(|p| p.label())
                            .unwrap_or_else(|| "unknown".into());
                        out.push_str(&format!(
                            "[{}] Response: {}\n",
                            label, rb.response_to_critiques
                        ));
                        if let Some(ref revised) = rb.revised_proposal {
                            out.push_str(&format!("  Revised proposal: {}\n", revised));
                        }
                        out.push('\n');
                    }
                }
            }
        }

        if let Some(_) = self.arbitration {
            out.push_str("──── Arbitration ────\n");
            out.push_str(&self.format_summary());
        }

        out
    }
}

// ── Topic category for model capability learning ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TopicCategory {
    Architecture,
    Algorithm,
    Debugging,
    CodeReview,
    DevOps,
    Frontend,
    Database,
    Security,
    General,
}

impl TopicCategory {
    pub fn classify(question: &str) -> Self {
        let q = question.to_lowercase();
        let keywords: &[(&[&str], TopicCategory)] = &[
            (
                &[
                    "architect",
                    "design",
                    "system",
                    "microservice",
                    "monolith",
                    "modular",
                ],
                TopicCategory::Architecture,
            ),
            (
                &[
                    "algorithm",
                    "data structure",
                    "sort",
                    "search",
                    "graph",
                    "tree",
                    "complexity",
                ],
                TopicCategory::Algorithm,
            ),
            (
                &["debug", "error", "bug", "fix", "crash", "trace", "stack"],
                TopicCategory::Debugging,
            ),
            (
                &["review", "quality", "lint", "refactor", "clean", "style"],
                TopicCategory::CodeReview,
            ),
            (
                &["deploy", "ci", "cd", "docker", "kubernetes", "infra"],
                TopicCategory::DevOps,
            ),
            (
                &["ui", "ux", "frontend", "react", "vue", "css", "component"],
                TopicCategory::Frontend,
            ),
            (
                &["database", "sql", "query", "index", "migration", "orm"],
                TopicCategory::Database,
            ),
            (
                &[
                    "security",
                    "auth",
                    "encrypt",
                    "token",
                    "vulnerability",
                    "sanitize",
                ],
                TopicCategory::Security,
            ),
        ];
        for (kws, cat) in keywords {
            if kws.iter().any(|k| q.contains(k)) {
                return *cat;
            }
        }
        TopicCategory::General
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopicScore {
    pub proposal_adopted_rate: f32,
    pub review_quality: f32,
    pub session_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilityScore {
    pub provider: String,
    pub model: String,
    pub topic_scores: std::collections::HashMap<TopicCategory, TopicScore>,
}

impl ModelCapabilityScore {
    pub fn new(provider: String, model: String) -> Self {
        Self {
            provider,
            model,
            topic_scores: std::collections::HashMap::new(),
        }
    }

    pub fn update(
        &mut self,
        topic: TopicCategory,
        proposal_adopted: bool,
        review_cited_ratio: f32,
    ) {
        let ts = self.topic_scores.entry(topic).or_default();
        let alpha = 0.3_f32;
        ts.proposal_adopted_rate = ema(
            ts.proposal_adopted_rate,
            if proposal_adopted { 1.0 } else { 0.0 },
            alpha,
        );
        ts.review_quality = ema(ts.review_quality, review_cited_ratio, alpha);
        ts.session_count += 1;
    }

    pub fn from_store(
        provider: String,
        model: String,
        store: &crate::memory::store::MemoryStore,
    ) -> anyhow::Result<Self> {
        let mut scores = Self::new(provider.clone(), model.clone());
        for cat in &[
            TopicCategory::Architecture,
            TopicCategory::Algorithm,
            TopicCategory::Debugging,
            TopicCategory::CodeReview,
            TopicCategory::DevOps,
            TopicCategory::Frontend,
            TopicCategory::Database,
            TopicCategory::Security,
            TopicCategory::General,
        ] {
            if let Some((adopted, quality, count)) =
                store.load_model_capability(&provider, &model, cat.as_str())?
            {
                let mut ts = TopicScore::default();
                ts.proposal_adopted_rate = adopted;
                ts.review_quality = quality;
                ts.session_count = count;
                scores.topic_scores.insert(*cat, ts);
            }
        }
        Ok(scores)
    }

    pub fn persist_to_store(
        &self,
        store: &crate::memory::store::MemoryStore,
    ) -> anyhow::Result<()> {
        for (topic, score) in &self.topic_scores {
            store.save_model_capability(
                &self.provider,
                &self.model,
                topic.as_str(),
                score.proposal_adopted_rate,
                score.review_quality,
                score.session_count,
            )?;
        }
        Ok(())
    }
}

fn ema(prev: f32, new: f32, alpha: f32) -> f32 {
    prev + alpha * (new - prev)
}

// ── Participant selection helper ──

impl TopicCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Architecture => "architecture",
            Self::Algorithm => "algorithm",
            Self::Debugging => "debugging",
            Self::CodeReview => "code_review",
            Self::DevOps => "devops",
            Self::Frontend => "frontend",
            Self::Database => "database",
            Self::Security => "security",
            Self::General => "general",
        }
    }
}

pub fn select_best_participants(
    available_models: &[String],
    topic: TopicCategory,
    store: &crate::memory::store::MemoryStore,
    max_participants: usize,
) -> Vec<String> {
    use std::collections::HashMap;

    // Load capability scores for all models on this topic
    let mut model_scores: HashMap<String, f32> = HashMap::new();

    for model_name in available_models {
        let provider = crate::llm::resolve_provider_name(model_name);
        if let Ok(Some((adopted, _quality, count))) =
            store.load_model_capability(provider, model_name, topic.as_str())
        {
            // Score based on adoption rate and experience (session count)
            let experience_bonus = (count as f32).min(10.0) / 10.0 * 0.2; // Up to 0.2 bonus for experience
            let score = adopted * 0.8 + experience_bonus;
            model_scores.insert(model_name.clone(), score);
        } else {
            // Unknown models get default score
            model_scores.insert(model_name.clone(), 0.5);
        }
    }

    // Sort by score descending
    let mut sorted: Vec<_> = model_scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Take top N participants
    sorted
        .into_iter()
        .take(max_participants)
        .map(|(model, _)| model)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_category_classify() {
        assert_eq!(
            TopicCategory::classify("Should we use microservices architecture?"),
            TopicCategory::Architecture
        );
        assert_eq!(
            TopicCategory::classify("Fix the null pointer bug in handler"),
            TopicCategory::Debugging
        );
        assert_eq!(
            TopicCategory::classify("Deploy to kubernetes with CI/CD"),
            TopicCategory::DevOps
        );
        assert_eq!(
            TopicCategory::classify("How to optimize SQL query?"),
            TopicCategory::Database
        );
        assert_eq!(
            TopicCategory::classify("What color should the button be?"),
            TopicCategory::General
        );
    }

    #[test]
    fn model_capability_ema_update() {
        let mut score = ModelCapabilityScore::new("openai".into(), "gpt-4o".into());
        score.update(TopicCategory::Architecture, true, 0.8);
        assert_eq!(
            score.topic_scores[&TopicCategory::Architecture].session_count,
            1
        );
        assert!(score.topic_scores[&TopicCategory::Architecture].proposal_adopted_rate > 0.0);

        score.update(TopicCategory::Architecture, false, 0.2);
        assert_eq!(
            score.topic_scores[&TopicCategory::Architecture].session_count,
            2
        );
    }

    #[test]
    fn council_session_format_summary() {
        let session = CouncilSession {
            id: "test".into(),
            question: "gRPC vs REST?".into(),
            participants: vec![
                Participant {
                    role: ParticipantRole::Proposer,
                    provider: "openai".into(),
                    model: "gpt-4o".into(),
                },
                Participant {
                    role: ParticipantRole::Proposer,
                    provider: "anthropic".into(),
                    model: "claude".into(),
                },
            ],
            rounds: 2,
            phases: vec![],
            arbitration: Some(Arbitration {
                arbiter_idx: 0,
                final_recommendation: "Use REST".into(),
                primary_source_idx: 1,
                reasoning: "Simpler".into(),
                key_disagreements: vec!["Performance vs simplicity".into()],
                comparison_table: "REST simpler, gRPC faster".into(),
                confidence: 0.85,
            }),
            token_usage: CouncilTokenUsage {
                total_prompt: 5000,
                total_completion: 3000,
                estimated_cost: 0.12,
            },
            created_at: 0,
        };
        let summary = session.format_summary();
        assert!(summary.contains("Use REST"));
        assert!(summary.contains("0.85"));
        assert!(summary.contains("Performance vs simplicity"));
    }

    #[test]
    fn council_token_usage_add() {
        let mut usage = CouncilTokenUsage::default();
        usage.add(100, 50, 0.01);
        assert_eq!(usage.total_prompt, 100);
        assert_eq!(usage.total_completion, 50);
        usage.add(200, 100, 0.02);
        assert_eq!(usage.total_prompt, 300);
        assert_eq!(usage.total_completion, 150);
    }

    #[test]
    fn participant_label() {
        let p = Participant {
            role: ParticipantRole::Proposer,
            provider: "openai".into(),
            model: "gpt-4o".into(),
        };
        assert_eq!(p.label(), "openai:gpt-4o");
    }
}
