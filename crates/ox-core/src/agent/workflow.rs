/// Workflow step definition
#[derive(Debug, Clone)]
pub struct WorkflowStep {
    pub id: String,
    pub name: String,
    pub description: String,
    pub requires_user_confirmation: bool,
    pub allow_tool_execution: bool,
    pub allow_code_modification: bool,
    pub step_prompt: String,
    pub validator_name: Option<String>,
    pub allowed_tools: Vec<String>,
    pub memory_layers: Vec<String>,
    pub display_status: String,
}

impl WorkflowStep {
    pub fn new(id: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            requires_user_confirmation: false,
            allow_tool_execution: true,
            allow_code_modification: true,
            step_prompt: String::new(),
            validator_name: None,
            allowed_tools: Vec::new(),
            memory_layers: Vec::new(),
            display_status: String::new(),
        }
    }

    pub fn with_display_status(mut self, status: &str) -> Self {
        self.display_status = status.to_string();
        self
    }

    pub fn require_confirmation(mut self) -> Self {
        self.requires_user_confirmation = true;
        self
    }

    pub fn disallow_tools(mut self) -> Self {
        self.allow_tool_execution = false;
        self.allow_code_modification = false;
        self
    }

    pub fn allow_tools_disallow_code(mut self) -> Self {
        self.allow_tool_execution = true;
        self.allow_code_modification = false;
        self
    }

    pub fn with_prompt(mut self, prompt: &str) -> Self {
        self.step_prompt = prompt.to_string();
        self
    }

    pub fn with_validator(mut self, validator_name: &str) -> Self {
        self.validator_name = Some(validator_name.to_string());
        self
    }

    pub fn with_allowed_tools(mut self, tools: &[&str]) -> Self {
        self.allowed_tools = tools.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_memory_layers(mut self, layers: &[&str]) -> Self {
        self.memory_layers = layers.iter().map(|s| s.to_string()).collect();
        self
    }
}

#[derive(Debug, Clone)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

impl Workflow {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            steps: Vec::new(),
        }
    }

    pub fn add_step(&mut self, step: WorkflowStep) {
        self.steps.push(step);
    }

    pub fn get_step(&self, index: usize) -> Option<&WorkflowStep> {
        self.steps.get(index)
    }

    pub fn total_steps(&self) -> usize {
        self.steps.len()
    }
}

pub const DEFAULT_WORKFLOW_ID: &str = "single_step";

/// Minimal step directive during task — `[TURN_CONTEXT]` is the authority.
pub const IMPLEMENT_TURN_STEP_HINT: &str = "\
【执行阶段】\n\
按 [TURN_CONTEXT]「下一步」逐项 file_read → edit_file → 验证。";

const TASK_PROMPT: &str = "\
【任务】\n\
用户请求: {USER_REQUEST}{USER_GUIDANCE}\n\
遵循 [TURN_CONTEXT]「下一步」。finish 含 finding_json → 门禁确认；finish 不含 → 结束等用户。不确定就探索。";

/// Single-step agent workflow — no Intent/Plan/Review pipeline.
pub fn create_default_workflow() -> Workflow {
    let mut workflow = Workflow::new(DEFAULT_WORKFLOW_ID, "Agent");
    workflow.add_step(
        WorkflowStep::new("task", "Task", "Complete the user's request")
            .with_prompt(TASK_PROMPT)
            .with_display_status("⚡ Agent"),
    );
    workflow
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::phase::{self, SingleFlowPhase};
    use crate::agent::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn implement_turn_omits_full_task_prompt() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine.set_variable(
            phase::PHASE_STATE_KEY,
            SingleFlowPhase::Implement.as_str().to_string(),
        );
        let prompt = engine.get_step_system_prompt().unwrap();
        assert_eq!(prompt, IMPLEMENT_TURN_STEP_HINT);
        assert!(!prompt.contains("```json"));
    }
}
