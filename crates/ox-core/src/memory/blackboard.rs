//! Constraint blackboard — cross-turn, always-on-top user constraints & facts.
//!
//! Solves "turn drift": by turn 50 the model forgets a rule set at turn 1, because
//! the original text was compacted out. Constraints live here as structured state
//! (engine variables, survive compaction) and are injected at the TOP of every
//! turn's context, in EVERY phase — unlike `[DURABLE_MEMORY]`, which goes silent
//! during the Implement phase exactly when constraints matter most.
//!
//! Also stores **session facts** — key discoveries the LLM made about the codebase
//! that should be remembered across turns within the same session.
//!
//! Sources:
//! - User mid-task input that reads as a constraint ("不要动 X", "必须保持兼容")
//! - LLM discoveries automatically extracted from finish summaries
//! - Explicit facts from [TURN_CONTEXT].decisions

use crate::agent::engine::WorkflowEngine;

const CONSTRAINTS_KEY: &str = "_blackboard_constraints";
const FACTS_KEY: &str = "_session_facts";
const MAX_CONSTRAINTS: usize = 12;
const MAX_FACTS: usize = 20;
const MAX_LEN: usize = 200;

/// Pin a constraint. De-duplicates and caps the list (oldest dropped).
pub fn add_constraint(engine: &WorkflowEngine, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let text: String = text.chars().take(MAX_LEN).collect();
    let mut list = constraints(engine);
    if list.iter().any(|c| c == &text) {
        return;
    }
    if list.len() >= MAX_CONSTRAINTS {
        list.remove(0);
    }
    list.push(text);
    if let Ok(json) = serde_json::to_string(&list) {
        engine.set_variable(CONSTRAINTS_KEY, json);
    }
}

/// All pinned constraints, oldest first.
pub fn constraints(engine: &WorkflowEngine) -> Vec<String> {
    engine
        .get_variable(CONSTRAINTS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(CONSTRAINTS_KEY, String::new());
    engine.set_variable(FACTS_KEY, String::new());
}

/// Block for injection at the top of the turn context. Empty when no constraints.
pub fn block(engine: &WorkflowEngine) -> String {
    let mut out = String::new();
    let list = constraints(engine);
    if !list.is_empty() {
        out.push_str("📌 用户约束（跨轮恒定，必须始终遵守）:");
        for c in &list {
            out.push_str("\n  • ");
            out.push_str(c);
        }
    }
    let facts = session_facts(engine);
    if !facts.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("📚 本轮已发现的业务知识:");
        for f in &facts {
            out.push_str("\n  • ");
            out.push_str(f);
        }
    }
    out
}

/// Heuristic: does this user message read as a durable constraint worth pinning?
///
/// Conservative on purpose — false positives clutter the blackboard. Targets
/// imperative prohibitions/requirements, not questions or discussion.
pub fn looks_like_constraint(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > MAX_LEN {
        return false;
    }
    // Questions are not constraints.
    if t.ends_with('?') || t.ends_with('？') {
        return false;
    }
    const SIGNALS: &[&str] = &[
        "不要",
        "不准",
        "别改",
        "别动",
        "禁止",
        "必须",
        "一定要",
        "务必",
        "记住",
        "始终",
        "保持",
        "不能",
        "勿",
        "don't",
        "do not",
        "must",
        "never",
        "always",
        "keep",
        "ensure",
    ];
    let lower = t.to_lowercase();
    SIGNALS.iter().any(|s| lower.contains(s))
}

/// Record a discovered fact about the codebase (cross-turn memory).
/// Facts are deduplicated and capped to prevent bloat.
pub fn add_fact(engine: &WorkflowEngine, fact: &str) {
    let fact = fact.trim();
    if fact.is_empty() || fact.chars().count() > MAX_LEN {
        return;
    }
    let mut list: Vec<String> = engine
        .get_variable(FACTS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if list.iter().any(|f| f == fact) {
        return;
    }
    if list.len() >= MAX_FACTS {
        list.remove(0);
    }
    list.push(fact.to_string());
    if let Ok(json) = serde_json::to_string(&list) {
        engine.set_variable(FACTS_KEY, json);
    }
}

/// All recorded session facts.
pub fn session_facts(engine: &WorkflowEngine) -> Vec<String> {
    engine
        .get_variable(FACTS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionState;
    use std::sync::Arc;

    fn engine() -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        WorkflowEngine::new(session)
    }

    #[test]
    fn detects_constraints_not_questions() {
        assert!(looks_like_constraint("不要动 auth 模块"));
        assert!(looks_like_constraint("必须保持向后兼容"));
        assert!(looks_like_constraint("don't touch the tests"));
        assert!(!looks_like_constraint("这个函数是干嘛的？"));
        assert!(!looks_like_constraint("看看怎么改"));
    }

    #[test]
    fn add_dedupes_and_caps() {
        let e = engine();
        add_constraint(&e, "必须保持兼容");
        add_constraint(&e, "必须保持兼容");
        assert_eq!(constraints(&e).len(), 1);
        for i in 0..20 {
            add_constraint(&e, &format!("约束 {i}"));
        }
        assert_eq!(constraints(&e).len(), MAX_CONSTRAINTS);
    }

    #[test]
    fn block_lists_constraints() {
        let e = engine();
        add_constraint(&e, "不要动 auth");
        let b = block(&e);
        assert!(b.contains("用户约束"));
        assert!(b.contains("不要动 auth"));
    }

    #[test]
    fn empty_when_none() {
        let e = engine();
        assert!(block(&e).is_empty());
    }
}