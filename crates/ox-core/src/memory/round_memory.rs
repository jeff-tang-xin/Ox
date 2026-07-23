//! Per-round checkpoint memory — persisted on finish, injected on session start.

use serde::{Deserialize, Serialize};

use crate::agent::engine::WorkflowEngine;

const ROUND_MEMORY_KEY: &str = "_round_memory_log";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoundRecord {
    pub round_id: u32,
    pub user_intent: String,
    pub actions_summary: Vec<String>,
    pub deliverables_summary: String,
    pub gate_outcomes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoundMemoryLog {
    pub rounds: Vec<RoundRecord>,
}

pub fn load(engine: &WorkflowEngine) -> RoundMemoryLog {
    engine
        .get_variable(ROUND_MEMORY_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(engine: &WorkflowEngine, log: &RoundMemoryLog) {
    if let Ok(json) = serde_json::to_string(log) {
        engine.set_variable(ROUND_MEMORY_KEY, json);
    }
}

pub fn append_round(engine: &WorkflowEngine, record: RoundRecord) {
    let mut log = load(engine);
    log.rounds.push(record);
    const MAX_ROUNDS: usize = 40;
    if log.rounds.len() > MAX_ROUNDS {
        let drop = log.rounds.len() - MAX_ROUNDS;
        log.rounds.drain(0..drop);
    }
    save(engine, &log);
}

/// Short injection block for session cold start.
pub fn format_injection(log: &RoundMemoryLog) -> String {
    if log.rounds.is_empty() {
        return String::new();
    }
    let mut lines = vec!["[ROUND_MEMORY] 近期轮次摘要（tool 链为主记忆，此为索引）".to_string()];
    for r in log.rounds.iter().rev().take(8) {
        lines.push(format!(
            "- round {}: {} | actions: {} | {}",
            r.round_id,
            r.user_intent.chars().take(120).collect::<String>(),
            r.actions_summary.join(", "),
            r.deliverables_summary.chars().take(160).collect::<String>()
        ));
    }
    lines.join("\n")
}