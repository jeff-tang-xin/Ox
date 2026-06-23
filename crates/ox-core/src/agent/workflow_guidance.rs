//! User corrections while a workflow is in progress — stay on the current step.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;

pub const WORKFLOW_GUIDANCE_TAG: &str = "[WORKFLOW_GUIDANCE]";
const KEY: &str = "_workflow_guidance";
const MAX_ENTRIES: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuidanceEntry {
    pub step_index: usize,
    pub step_name: String,
    pub text: String,
}

pub fn load(engine: &WorkflowEngine) -> Vec<GuidanceEntry> {
    engine
        .get_variable(KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(engine: &WorkflowEngine, entries: &[GuidanceEntry]) {
    if let Ok(json) = serde_json::to_string(entries) {
        engine.set_variable(KEY, json);
    }
}

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(KEY, "[]".to_string());
}

pub fn append(engine: &WorkflowEngine, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let step_index = engine.get_current_step_index();
    let step_name = engine
        .current_step()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| format!("Step{}", step_index + 1));

    let mut entries = load(engine);
    // Dedupe: same text at same step
    if entries
        .last()
        .is_some_and(|e| e.step_index == step_index && e.text == trimmed)
    {
        return;
    }
    entries.push(GuidanceEntry {
        step_index,
        step_name: step_name.clone(),
        text: trimmed.to_string(),
    });
    while entries.len() > MAX_ENTRIES {
        entries.remove(0);
    }
    save(engine, &entries);
    tracing::info!(
        "[WORKFLOW_GUIDANCE] appended at step {} ({}): {}",
        step_index + 1,
        step_name,
        trimmed.chars().take(80).collect::<String>()
    );
}

pub fn format_block(engine: &WorkflowEngine) -> String {
    let entries = load(engine);
    if entries.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        WORKFLOW_GUIDANCE_TAG.to_string(),
        "📣 **用户补充说明** — 采纳后继续当前任务，勿重头探索。".to_string(),
    ];
    for e in &entries {
        let snippet: String = e.text.chars().take(800).collect();
        lines.push(format!(
            "- [{} · 步骤{}] {snippet}",
            e.step_name,
            e.step_index + 1
        ));
    }
    lines.join("\n")
}

/// Wrap a live interjection for the message list.
pub fn format_interjection_message(_engine: &WorkflowEngine, text: &str) -> String {
    format!(
        "[WORKFLOW_INTERJECTION — 用户补充，请采纳并继续当前任务]\n{}",
        text.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn append_and_format() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.set_variable("_step0_output", r#"{"intent":"coding"}"#.into());
        let _ = engine.advance_to_step(Some(1));

        append(&engine, "focus on foo.rs only");
        let block = format_block(&engine);
        assert!(block.contains("foo.rs"));
        assert!(block.contains("WORKFLOW_GUIDANCE"));
    }
}
