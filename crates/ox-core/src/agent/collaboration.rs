//! Multi-model collaboration — route LLM calls by task intent / workspace mode.

use std::sync::Arc;

use crate::config::{AgentConfig, ModelsConfig};
use crate::llm::{self, LlmProvider};

use super::engine::WorkflowEngine;
use super::task_intent::TaskIntent;
use super::workspace::{WorkflowWorkspace, WorkspaceMode};

/// Optional per-role LLM providers (falls back to default when unset).
#[derive(Clone, Default)]
pub struct RoleProviders {
    pub enabled: bool,
    pub review: Option<Arc<dyn LlmProvider>>,
    pub implement: Option<Arc<dyn LlmProvider>>,
    pub qa: Option<Arc<dyn LlmProvider>>,
}

impl RoleProviders {
    /// Pick provider for the current workflow state.
    pub fn pick(
        &self,
        default: &Arc<dyn LlmProvider>,
        engine: &WorkflowEngine,
    ) -> Arc<dyn LlmProvider> {
        if !self.enabled {
            return Arc::clone(default);
        }
        let slot = match engine.get_task_intent() {
            TaskIntent::Review => &self.review,
            TaskIntent::Fix => &self.implement,
            TaskIntent::Qa => &self.qa,
            TaskIntent::General => {
                if let Some(ws) = WorkflowWorkspace::build(engine) {
                    match ws.mode {
                        WorkspaceMode::ExecuteImpl => &self.implement,
                        WorkspaceMode::ExecuteReview => &self.review,
                        _ => &None,
                    }
                } else {
                    &None
                }
            }
        };
        slot.as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(default))
    }

    pub fn role_label(&self, engine: &WorkflowEngine) -> &'static str {
        if !self.enabled {
            return "default";
        }
        match engine.get_task_intent() {
            TaskIntent::Review => "review",
            TaskIntent::Fix => "implement",
            TaskIntent::Qa => "qa",
            TaskIntent::General => {
                if WorkflowWorkspace::build(engine)
                    .is_some_and(|w| w.mode == WorkspaceMode::ExecuteImpl)
                {
                    "implement"
                } else {
                    "review"
                }
            }
        }
    }
}

/// Build role providers from config (`[agent.collaboration]`).
pub fn build_role_providers(models: &ModelsConfig, agent: &AgentConfig) -> RoleProviders {
    let c = &agent.collaboration;
    if !c.enabled {
        return RoleProviders::default();
    }
    RoleProviders {
        enabled: true,
        review: try_provider(&c.review_model, models),
        implement: try_provider(&c.implement_model, models),
        qa: try_provider(&c.qa_model, models),
    }
}

fn try_provider(model: &str, models: &ModelsConfig) -> Option<Arc<dyn LlmProvider>> {
    let name = model.trim();
    if name.is_empty() {
        return None;
    }
    match llm::create_provider_with_info(name, models) {
        Ok((p, _)) => Some(Arc::from(p)),
        Err(e) => {
            tracing::warn!("[COLLAB] Failed to create provider for `{name}`: {e}");
            None
        }
    }
}
