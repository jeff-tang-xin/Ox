use std::sync::Arc;

use crate::config::{CouncilConfig, ModelsConfig};
use crate::llm::{self, LlmProvider, LlmStreamEvent};
use crate::message::Message;

use super::{Arbitration, CouncilSession, CouncilTokenUsage, DebatePhase, Participant, ParticipantRole, Proposal, Rebuttal, Review};

async fn call_model_simple(models_config: &ModelsConfig, model: &str, system: &str, user: &str) -> anyhow::Result<(String, u32, u32)> {
    let (provider, _) = llm::create_provider_with_info(model, models_config)?;
    let messages = vec![
        Message::system(system),
        Message::user(user),
    ];

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LlmStreamEvent>();
    let provider: Arc<dyn LlmProvider> = Arc::from(provider);

    let provider_clone = Arc::clone(&provider);
    let msgs = messages.clone();
    let handle = tokio::spawn(async move {
        let _ = provider_clone.stream_chat(&msgs, &[], tx).await;
    });

    let mut response = String::new();
    let mut prompt_tokens = 0u32;
    let mut completion_tokens = 0u32;

    while let Some(event) = rx.recv().await {
        match event {
            LlmStreamEvent::TextDelta(delta) => response.push_str(&delta),
            LlmStreamEvent::Done { usage } => {
                prompt_tokens = usage.prompt_tokens;
                completion_tokens = usage.completion_tokens;
            }
            LlmStreamEvent::Error(e) => {
                handle.abort();
                anyhow::bail!("LLM error in council: {}", e);
            }
            _ => {}
        }
    }

    let _ = handle.await;
    Ok((response, prompt_tokens, completion_tokens))
}

pub struct CouncilOrchestrator {
    models_config: ModelsConfig,
    council_config: CouncilConfig,
}

impl CouncilOrchestrator {
    pub fn new(models_config: ModelsConfig, council_config: CouncilConfig) -> Self {
        Self {
            models_config,
            council_config,
        }
    }

    pub async fn convene(
        &self,
        question: &str,
        context_messages: &[Message],
        rounds: Option<u8>,
        verbose: bool,
    ) -> anyhow::Result<CouncilSession> {
        let rounds = rounds
            .unwrap_or(self.council_config.default_rounds as u8)
            .min(self.council_config.max_rounds as u8);

        let participant_models = self.resolve_participants()?;
        if participant_models.len() < 2 {
            anyhow::bail!("Council requires at least 2 different models. Configure council.participants in ~/.ox/config.toml");
        }

        let mut session = CouncilSession {
            id: uuid::Uuid::new_v4().to_string(),
            question: question.to_string(),
            participants: participant_models,
            rounds,
            phases: Vec::new(),
            arbitration: None,
            token_usage: CouncilTokenUsage::default(),
            created_at: chrono::Utc::now().timestamp(),
        };

        // Phase 1: Proposals
        let (proposals, tokens) = self.run_proposals(&session, question, context_messages).await?;
        session.token_usage.add(tokens.0, tokens.1, 0.0);
        session.phases.push(DebatePhase::Proposal(proposals));

        // Phase 2: Cross Reviews
        let (reviews, tokens) = self.run_reviews(&session).await?;
        session.token_usage.add(tokens.0, tokens.1, 0.0);
        session.phases.push(DebatePhase::CrossReview(reviews));

        // Early convergence check
        let empty_reviews = Vec::new();
        let last_reviews: &Vec<Review> = match session.phases.last() {
            Some(DebatePhase::CrossReview(r)) => r,
            _ => &empty_reviews,
        };
        let all_high = !last_reviews.is_empty()
            && last_reviews.iter().all(|r| r.score >= self.council_config.early_convergence_threshold as f32);

        // Phase 3: Rebuttals (skip if early convergence and rounds > 1)
        if rounds > 1 && !all_high {
            for _ in 1..rounds {
                let (rebuttals, tokens) = self.run_rebuttals(&session).await?;
                session.token_usage.add(tokens.0, tokens.1, 0.0);
                session.phases.push(DebatePhase::Rebuttal(rebuttals));

                let (reviews, tokens) = self.run_reviews(&session).await?;
                session.token_usage.add(tokens.0, tokens.1, 0.0);
                session.phases.push(DebatePhase::CrossReview(reviews));
            }
        } else if all_high {
            tracing::info!("Council early convergence: all review scores >= {:.2}, skipping rebuttals",
                self.council_config.early_convergence_threshold);
        }

        // Phase 4: Arbitration
        let (arbitration, tokens) = self.run_arbitration(&session, context_messages).await?;
        session.token_usage.add(tokens.0, tokens.1, 0.0);
        session.arbitration = Some(arbitration);

        let _ = verbose;
        Ok(session)
    }

    fn resolve_participants(&self) -> anyhow::Result<Vec<Participant>> {
        let mut participants = Vec::new();
        for model_name in &self.council_config.participants {
            let provider_name = llm::resolve_provider_name_with_config(model_name, &self.models_config);
            participants.push(Participant {
                role: ParticipantRole::Proposer,
                provider: provider_name.to_string(),
                model: model_name.clone(),
            });
        }
        participants.truncate(self.council_config.max_participants as usize);
        if participants.len() < 2 {
            anyhow::bail!("Need at least 2 participants for council, got {}", participants.len());
        }
        Ok(participants)
    }

    async fn run_proposals(
        &self,
        session: &CouncilSession,
        question: &str,
        context_messages: &[Message],
    ) -> anyhow::Result<(Vec<Proposal>, (u32, u32))> {
        let context_summary = summarize_context(context_messages, 2000);
        let system = proposal_system_prompt();
        let user = format!("Question: {}\n\nContext:\n{}", question, context_summary);

        let futures_vec: Vec<_> = session.participants.iter()
            .map(|participant| {
                let mc = self.models_config.clone();
                let model = participant.model.clone();
                let system = system.clone();
                let user = user.clone();
                tokio::spawn(async move {
                    call_model_simple(&mc, &model, &system, &user).await
                })
            })
            .collect();

        let results = futures::future::join_all(futures_vec).await;

        let mut proposals = Vec::new();
        let mut total_prompt = 0u32;
        let mut total_completion = 0u32;
        for (idx, result) in results.into_iter().enumerate() {
            let participant = &session.participants[idx];
            match result {
                Ok(Ok((response, pt, ct))) => {
                    total_prompt += pt;
                    total_completion += ct;
                    let (content, reasoning) = parse_proposal_response(&response);
                    proposals.push(Proposal {
                        participant_idx: idx,
                        content,
                        reasoning,
                    });
                }
                _ => {
                    tracing::warn!("Council proposal failed for {}", participant.model);
                    proposals.push(Proposal {
                        participant_idx: idx,
                        content: format!("(Model {} unavailable)", participant.model),
                        reasoning: String::new(),
                    });
                }
            }
        }

        Ok((proposals, (total_prompt, total_completion)))
    }

    async fn run_reviews(&self, session: &CouncilSession) -> anyhow::Result<(Vec<Review>, (u32, u32))> {
        let proposals = match session.phases.iter().find_map(|p| match p {
            DebatePhase::Proposal(ps) => Some(ps),
            _ => None,
        }) {
            Some(p) => p,
            None => anyhow::bail!("No proposals found for review"),
        };

        let system = review_system_prompt();

        let mut tasks: Vec<(usize, usize, tokio::task::JoinHandle<anyhow::Result<(String, u32, u32)>>)> = Vec::new();

        for (reviewer_idx, reviewer) in session.participants.iter().enumerate() {
            for (target_idx, target_proposal) in proposals.iter().enumerate() {
                if reviewer_idx == target_idx { continue; }

                let target_label = session.participants.get(target_idx)
                    .map(|p| p.label())
                    .unwrap_or_else(|| "unknown".into());

                let user = format!(
                    "You are reviewing the proposal from {}.\n\nTheir proposal: {}\nTheir reasoning: {}\n\nQuestion: {}\n\nProvide your critique and a score (0.0-1.0) for this proposal.",
                    target_label, target_proposal.content, target_proposal.reasoning, session.question
                );

                let mc = self.models_config.clone();
                let model = reviewer.model.clone();
                let sys = system.clone();
                let handle = tokio::spawn(async move {
                    call_model_simple(&mc, &model, &sys, &user).await
                });
                tasks.push((reviewer_idx, target_idx, handle));
            }
        }

        let mut reviews = Vec::new();
        let mut total_prompt = 0u32;
        let mut total_completion = 0u32;
        for (reviewer_idx, target_idx, handle) in tasks {
            let target_label = session.participants.get(target_idx)
                .map(|p| p.label())
                .unwrap_or_else(|| "unknown".into());
            let reviewer_label = session.participants.get(reviewer_idx)
                .map(|p| p.label())
                .unwrap_or_else(|| "unknown".into());
            match handle.await {
                Ok(Ok((response, pt, ct))) => {
                    total_prompt += pt;
                    total_completion += ct;
                    let (critique, score) = parse_review_response(&response);
                    reviews.push(Review {
                        reviewer_idx,
                        target_idx,
                        critique,
                        score,
                    });
                }
                _ => {
                    tracing::warn!("Council review failed for {} → {}", reviewer_label, target_label);
                }
            }
        }

        Ok((reviews, (total_prompt, total_completion)))
    }

    async fn run_rebuttals(&self, session: &CouncilSession) -> anyhow::Result<(Vec<Rebuttal>, (u32, u32))> {
        let empty_proposals = Vec::new();
        let proposals: &Vec<Proposal> = session.phases.iter().find_map(|p| match p {
            DebatePhase::Proposal(ps) => Some(ps),
            _ => None,
        }).unwrap_or(&empty_proposals);

        let reviews: Vec<&Review> = session.phases.iter()
            .filter_map(|p| match p {
                DebatePhase::CrossReview(rs) => Some(rs.iter().collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();

        let system = rebuttal_system_prompt();
        let mut rebuttals = Vec::new();
        let mut total_prompt = 0u32;
        let mut total_completion = 0u32;

        for (idx, participant) in session.participants.iter().enumerate() {
            let my_proposal = proposals.iter().find(|p| p.participant_idx == idx);
            let my_reviews: Vec<&&Review> = reviews.iter().filter(|r| r.target_idx == idx).collect();

            if my_reviews.is_empty() { continue; }

            let review_summary: String = my_reviews.iter().map(|r| {
                let reviewer_label = session.participants.get(r.reviewer_idx)
                    .map(|p| p.label())
                    .unwrap_or_else(|| "unknown".into());
                format!("[{} score={:.2}] {}", reviewer_label, r.score, r.critique)
            }).collect::<Vec<_>>().join("\n");

            let user = format!(
                "Your proposal: {}\n\nCritiques from others:\n{}\n\nQuestion: {}\n\nRespond to the critiques. You may revise your proposal if persuaded.",
                my_proposal.map(|p| p.content.as_str()).unwrap_or("(no proposal)"),
                review_summary,
                session.question
            );

            match call_model_simple(&self.models_config, &participant.model, &system, &user).await {
                Ok((response, pt, ct)) => {
                    total_prompt += pt;
                    total_completion += ct;
                    let (response_text, revised) = parse_rebuttal_response(&response);
                    rebuttals.push(Rebuttal {
                        participant_idx: idx,
                        original_proposal: my_proposal.map(|p| p.content.clone()).unwrap_or_default(),
                        response_to_critiques: response_text,
                        revised_proposal: revised,
                    });
                }
                Err(e) => {
                    tracing::warn!("Council rebuttal failed for {}: {}", participant.label(), e);
                }
            }
        }

        Ok((rebuttals, (total_prompt, total_completion)))
    }

    async fn run_arbitration(
        &self,
        session: &CouncilSession,
        context_messages: &[Message],
    ) -> anyhow::Result<(Arbitration, (u32, u32))> {
        let arbiter_model = if self.council_config.arbiter_model == "default" {
            &self.models_config.default
        } else {
            &self.council_config.arbiter_model
        };

        let system = arbitration_system_prompt();
        let discussion_summary = format_discussion_summary(session);
        let context_summary = summarize_context(context_messages, 1500);

        let user = format!(
            "Question: {}\n\nContext:\n{}\n\nDiscussion:\n{}\n\nProvide your arbitration: final recommendation, key disagreements, confidence (0.0-1.0), and which proposal (by index) is the primary source.",
            session.question, context_summary, discussion_summary
        );

        let (response, pt, ct) = call_model_simple(&self.models_config, arbiter_model, &system, &user).await?;

        let arbitration = parse_arbitration_response(&response, session.participants.len());
        Ok((arbitration, (pt, ct)))
    }
}

// ── Prompt templates ──

fn proposal_system_prompt() -> String {
    "You are an expert AI participating in a council debate. \
     Provide your independent proposal for the given question. \
     Format your response as:\n\
     PROPOSAL: <your proposal>\n\
     REASONING: <your reasoning>\n\n\
     Be specific and practical. State your position clearly.".to_string()
}

fn review_system_prompt() -> String {
    "You are reviewing a proposal from another AI in a council debate. \
     Critically evaluate the proposal's strengths and weaknesses. \
     Format your response as:\n\
     CRITIQUE: <your critique>\n\
     SCORE: <number 0.0 to 1.0>\n\n\
     Be fair but thorough. Consider practical implications.".to_string()
}

fn rebuttal_system_prompt() -> String {
    "You are responding to critiques of your proposal in a council debate. \
     Address each critique honestly. If valid, revise your proposal. \
     Format your response as:\n\
     RESPONSE: <your response to critiques>\n\
     REVISED: <your revised proposal, or \"none\" if unchanged>\n\n\
     Be open to valid criticism but defend well-reasoned positions.".to_string()
}

fn arbitration_system_prompt() -> String {
    "You are the arbiter of a council debate. Synthesize all proposals, \
     reviews, and rebuttals into a final recommendation. \
     Format your response as:\n\
     RECOMMENDATION: <final recommendation>\n\
     PRIMARY_SOURCE: <index of the most influential proposal, 0-based>\n\
     REASONING: <your arbitration reasoning>\n\
     DISAGREEMENTS: <comma-separated key disagreements>\n\
     CONFIDENCE: <number 0.0 to 1.0>\n\
     COMPARISON: <brief comparison of proposals>\n\n\
     Be thorough and balanced. Acknowledge trade-offs.".to_string()
}

// ── Response parsers ──

fn parse_proposal_response(response: &str) -> (String, String) {
    let mut proposal = String::new();
    let mut reasoning = String::new();
    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("PROPOSAL:") {
            proposal = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("REASONING:") {
            reasoning = rest.trim().to_string();
        }
    }
    if proposal.is_empty() {
        proposal = response.chars().take(500).collect();
    }
    (proposal, reasoning)
}

fn parse_review_response(response: &str) -> (String, f32) {
    let mut critique = String::new();
    let mut score = 0.5_f32;
    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("CRITIQUE:") {
            critique = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("SCORE:") {
            if let Ok(s) = rest.trim().parse::<f32>() {
                score = s.clamp(0.0, 1.0);
            }
        }
    }
    if critique.is_empty() {
        critique = response.chars().take(500).collect();
    }
    (critique, score)
}

fn parse_rebuttal_response(response: &str) -> (String, Option<String>) {
    let mut resp_text = String::new();
    let mut revised: Option<String> = None;
    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("RESPONSE:") {
            resp_text = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("REVISED:") {
            let r = rest.trim();
            if r != "none" && !r.is_empty() {
                revised = Some(r.to_string());
            }
        }
    }
    if resp_text.is_empty() {
        resp_text = response.chars().take(500).collect();
    }
    (resp_text, revised)
}

fn parse_arbitration_response(response: &str, _num_participants: usize) -> Arbitration {
    let mut recommendation = String::new();
    let mut primary_source = 0_usize;
    let mut reasoning = String::new();
    let mut disagreements = Vec::new();
    let mut confidence = 0.7_f32;
    let mut comparison = String::new();

    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("RECOMMENDATION:") {
            recommendation = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("PRIMARY_SOURCE:") {
            if let Ok(idx) = rest.trim().parse::<usize>() {
                primary_source = idx;
            }
        } else if let Some(rest) = trimmed.strip_prefix("REASONING:") {
            reasoning = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("DISAGREEMENTS:") {
            disagreements = rest.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        } else if let Some(rest) = trimmed.strip_prefix("CONFIDENCE:") {
            if let Ok(c) = rest.trim().parse::<f32>() {
                confidence = c.clamp(0.0, 1.0);
            }
        } else if let Some(rest) = trimmed.strip_prefix("COMPARISON:") {
            comparison = rest.trim().to_string();
        }
    }

    if recommendation.is_empty() {
        recommendation = response.chars().take(500).collect();
    }

    Arbitration {
        arbiter_idx: 0,
        final_recommendation: recommendation,
        primary_source_idx: primary_source,
        reasoning,
        key_disagreements: disagreements,
        comparison_table: comparison,
        confidence,
    }
}

// ── Helpers ──

fn summarize_context(messages: &[Message], max_chars: usize) -> String {
    let mut summary = String::new();
    for msg in messages {
        let text = match msg {
            Message::System { content } => format!("[System] {}", content),
            Message::User { content } => format!("[User] {}", content),
            Message::Assistant { content, .. } => format!("[Assistant] {}", content),
            Message::ToolResult { content, .. } => format!("[Tool] {}", content),
        };
        if summary.len() + text.len() + 1 > max_chars { break; }
        summary.push_str(&text);
        summary.push('\n');
    }
    summary
}

fn format_discussion_summary(session: &CouncilSession) -> String {
    let mut out = String::new();

    for phase in &session.phases {
        match phase {
            DebatePhase::Proposal(proposals) => {
                out.push_str("=== Proposals ===\n");
                for p in proposals {
                    let label = session.participants.get(p.participant_idx)
                        .map(|p| p.label())
                        .unwrap_or_else(|| "?".into());
                    out.push_str(&format!("[{}] {} (reasoning: {})\n", label, p.content, p.reasoning));
                }
            }
            DebatePhase::CrossReview(reviews) => {
                out.push_str("=== Reviews ===\n");
                for r in reviews {
                    let reviewer = session.participants.get(r.reviewer_idx).map(|p| p.label()).unwrap_or_else(|| "?".into());
                    let target = session.participants.get(r.target_idx).map(|p| p.label()).unwrap_or_else(|| "?".into());
                    out.push_str(&format!("[{}→{} score={:.2}] {}\n", reviewer, target, r.score, r.critique));
                }
            }
            DebatePhase::Rebuttal(rebuttals) => {
                out.push_str("=== Rebuttals ===\n");
                for rb in rebuttals {
                    let label = session.participants.get(rb.participant_idx).map(|p| p.label()).unwrap_or_else(|| "?".into());
                    out.push_str(&format!("[{}] {}\n", label, rb.response_to_critiques));
                    if let Some(ref revised) = rb.revised_proposal {
                        out.push_str(&format!("  Revised: {}\n", revised));
                    }
                }
            }
        }
    }

    out
}
