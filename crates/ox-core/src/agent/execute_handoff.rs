//! Execute confirmation handoff — unused in single-step model (kept for API compat).

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;

const HANDOFF_KEY: &str = "_execute_handoff";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecuteHandoff {
    pub confirmation_markdown: String,
    pub user_request: String,
    pub intent_output: Option<String>,
    pub plan_output: Option<String>,
    pub exploration_snapshot: String,
    pub from_step: usize,
    pub pipeline: String,
    pub preflight_done: bool,
}

impl ExecuteHandoff {
    pub fn freeze(
        _: &WorkflowEngine,
        confirmation_markdown: &str,
        from_step: usize,
        preflight_done: bool,
    ) -> Self {
        Self {
            confirmation_markdown: confirmation_markdown.to_string(),
            from_step,
            preflight_done,
            ..Default::default()
        }
    }

    pub fn save(&self, engine: &WorkflowEngine) {
        if let Ok(json) = serde_json::to_string(self) {
            engine.set_variable(HANDOFF_KEY, json);
        }
    }

    pub fn load(_: &WorkflowEngine) -> Option<Self> {
        None
    }

    pub fn clear(engine: &WorkflowEngine) {
        engine.set_variable(HANDOFF_KEY, String::new());
    }

    pub fn format_for_execute(&self) -> String {
        if self.confirmation_markdown.is_empty() {
            String::new()
        } else {
            self.confirmation_markdown.clone()
        }
    }
}
