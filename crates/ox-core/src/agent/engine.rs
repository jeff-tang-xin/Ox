use crate::agent::intervention::InterventionRequest;
use crate::agent::session::SessionState;
use crate::agent::workflow::{Workflow, WorkflowStep};
use std::sync::Arc;

/// Workflow Engine - enforces step-by-step execution with validation
pub struct WorkflowEngine {
    /// Registered workflows
    workflows: std::collections::HashMap<String, Workflow>,
    /// Current active workflow
    current_workflow: Option<Workflow>,
    /// Session state tracker
    session_state: Arc<tokio::sync::Mutex<SessionState>>,

}

impl WorkflowEngine {
    pub fn new(session_state: Arc<tokio::sync::Mutex<SessionState>>) -> Self {
        let mut engine = Self {
            workflows: std::collections::HashMap::new(),
            current_workflow: None,
            session_state,
        };

        // Register default workflows
        engine.register_workflow(crate::agent::workflow::create_free_workflow());

        engine
    }

    /// Register a workflow
    pub fn register_workflow(&mut self, workflow: Workflow) {
        let id = workflow.id.clone();
        self.workflows.insert(id, workflow);
    }

    /// Activate a workflow by ID
    pub fn activate_workflow(&mut self, workflow_id: &str) -> Result<(), String> {
        if let Some(workflow) = self.workflows.get(workflow_id).cloned() {
            tracing::info!("Activating workflow: {}", workflow.name);
            self.current_workflow = Some(workflow);

            // Update session state (use try_lock to avoid blocking in async context)
            if let Ok(mut session) = self.session_state.try_lock() {
                session.current_workflow = workflow_id.to_string();
                session.current_step_index = 0;
                session.awaiting_user_confirmation = false;
            } else {
                tracing::warn!("Failed to acquire session lock for workflow activation");
            }

            Ok(())
        } else {
            Err(format!("Workflow '{}' not found", workflow_id))
        }
    }

    /// Get current workflow
    pub fn current_workflow(&self) -> Option<&Workflow> {
        self.current_workflow.as_ref()
    }

    /// Get current step
    pub fn current_step(&self) -> Option<&WorkflowStep> {
        self.current_workflow.as_ref().and_then(|wf| {
            if let Ok(session) = self.session_state.try_lock() {
                wf.get_step(session.current_step_index)
            } else {
                None
            }
        })
    }

    /// Get current step number and total steps
    pub fn get_progress(&self) -> Option<(usize, usize)> {
        if let Some(workflow) = &self.current_workflow {
            if let Ok(session) = self.session_state.try_lock() {
                Some((session.current_step_index + 1, workflow.total_steps()))
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Check if current step requires user confirmation
    pub fn requires_user_confirmation(&self) -> bool {
        if let Some(step) = self.current_step() {
            step.requires_user_confirmation
        } else {
            false
        }
    }

    /// Check if tool execution is allowed in current step
    pub fn allows_tool_execution(&self) -> bool {
        if let Some(step) = self.current_step() {
            step.allow_tool_execution
        } else {
            false
        }
    }

    /// Check if code file modification is allowed in current step
    pub fn allows_code_modification(&self) -> bool {
        if let Some(step) = self.current_step() {
            step.allow_code_modification
        } else {
            false
        }
    }

    /// Get allowed tools for current step (empty list means all tools allowed)
    pub fn get_allowed_tools(&self) -> Vec<String> {
        if let Some(step) = self.current_step() {
            step.allowed_tools.clone()
        } else {
            Vec::new()
        }
    }

    /// Validate current step (check if prerequisites are met)
    pub fn validate_current_step(&self) -> bool {
        if let Some(step) = self.current_step() {
            if let Some(ref validator_name) = step.validator_name {
                if let Ok(session) = self.session_state.try_lock() {
                    // TODO: Integrate with StateRegistry for validators
                    // For now, just check if the variable exists
                    session.has_variable(validator_name)
                } else {
                    false
                }
            } else {
                true // No validator means step is always valid
            }
        } else {
            false
        }
    }

    /// Request user confirmation for current step
    pub fn request_confirmation(&self, message: &str) -> Option<InterventionRequest> {
        if let Some(step) = self.current_step() {
            if let Ok(session) = self.session_state.try_lock() {
                let request =
                    InterventionRequest::confirmation(message, &step.name, &session.current_mode);
                Some(request)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Advance to next step
    pub fn advance_step(&mut self) -> Result<bool, String> {
        let workflow = self.current_workflow.as_mut().ok_or("No active workflow")?;

        if let Ok(mut session) = self.session_state.try_lock() {
            if session.advance_step(workflow.total_steps()) {
                tracing::info!(
                    "Advanced to step {}/{}: {}",
                    session.current_step_index + 1,
                    workflow.total_steps(),
                    workflow
                        .get_step(session.current_step_index)
                        .map(|s| s.name.as_str())
                        .unwrap_or("Unknown")
                );
                Ok(true)
            } else {
                tracing::info!("Workflow completed");
                Ok(false)
            }
        } else {
            Err("Failed to acquire session lock".to_string())
        }
    }

    /// Check if workflow is complete
    pub fn is_workflow_complete(&self) -> bool {
        if let Some(workflow) = &self.current_workflow {
            if let Ok(session) = self.session_state.try_lock() {
                session.current_step_index >= workflow.total_steps() - 1
            } else {
                true // If we can't get the lock, assume complete to avoid blocking
            }
        } else {
            true // No workflow means we're done
        }
    }

    /// Reset workflow to first step
    pub fn reset_workflow(&mut self) {
        if let Some(workflow) = &mut self.current_workflow {
            workflow.steps.iter_mut().for_each(|_| {});
            if let Ok(mut session) = self.session_state.try_lock() {
                session.current_step_index = 0;
                session.awaiting_user_confirmation = false;
            } else {
                tracing::warn!("Failed to acquire session lock for workflow reset");
            }
        }
    }

    /// Go back to a specific step (for user feedback/revision)
    pub fn go_to_step(&mut self, step_index: usize) -> Result<(), String> {
        let workflow = self.current_workflow.as_ref().ok_or("No active workflow")?;

        if step_index >= workflow.total_steps() {
            return Err(format!(
                "Step index {} out of range (total steps: {})",
                step_index,
                workflow.total_steps()
            ));
        }

        if let Ok(mut session) = self.session_state.try_lock() {
            let old_step = session.current_step_index;
            session.current_step_index = step_index;
            session.awaiting_user_confirmation = false;

            tracing::info!(
                "Workflow stepped back from step {} to step {}: {}",
                old_step + 1,
                step_index + 1,
                workflow
                    .get_step(step_index)
                    .map(|s| s.name.as_str())
                    .unwrap_or("Unknown")
            );
            Ok(())
        } else {
            Err("Failed to acquire session lock".to_string())
        }
    }

    /// Deactivate current workflow (return to free mode)
    pub fn deactivate_workflow(&mut self) {
        self.current_workflow = None;
        if let Ok(mut session) = self.session_state.try_lock() {
            session.current_mode = "free".to_string();
            session.current_step_index = 0;
            session.awaiting_user_confirmation = false;
        } else {
            tracing::warn!("Failed to acquire session lock for workflow deactivation");
        }
    }

    /// Get system prompt for current step (to inject into LLM context)
    pub fn get_step_system_prompt(&self) -> Option<String> {
        if let Some(step) = self.current_step() {
            if !step.step_prompt.is_empty() {
                Some(step.step_prompt.clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Check if a tool call is allowed based on current workflow step
    pub fn validate_tool_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), String> {
        let step = self.current_step().ok_or("No active workflow step")?;

        // Check if tools are allowed at all
        if !step.allow_tool_execution {
            return Err(format!(
                "Tool execution not allowed in current step: {}",
                step.name
            ));
        }

        // Check tool whitelist (if specified)
        if !step.allowed_tools.is_empty() {
            if !step.allowed_tools.contains(&tool_name.to_string()) {
                return Err(format!(
                    "Tool '{}' is not allowed in current step '{}'. Allowed tools: {}",
                    tool_name,
                    step.name,
                    step.allowed_tools.join(", ")
                ));
            }
        }

        // Check if code modification is allowed
        if !step.allow_code_modification {
            // Check if this is a code-modifying tool
            let is_code_tool = matches!(tool_name, "file_write" | "edit_file" | "delete_range");

            if is_code_tool {
                // Extract file path from arguments
                let file_path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

                // Check if it's a source code file
                if Self::is_source_code_file(file_path) {
                    return Err(format!(
                        "Code modification not allowed in current step. You can only create/modify documentation files (.md, .txt, etc.), not source code files (.rs, .py, .js, etc.). Attempted to modify: {}",
                        file_path
                    ));
                }
            }
        }

        Ok(())
    }

    /// Check if a file path is a source code file
    fn is_source_code_file(file_path: &str) -> bool {
        let extensions = [
            ".rs", ".py", ".js", ".ts", ".jsx", ".tsx", ".java", ".cpp", ".c", ".h", ".hpp", ".go",
            ".rb", ".php", ".swift", ".kt", ".scala", ".cs", ".fs", ".r", ".m",
        ];

        let file_lower = file_path.to_lowercase();
        extensions.iter().any(|ext| file_lower.ends_with(ext))
    }

    /// Check if workflow is currently active
    pub fn is_workflow_active(&self) -> bool {
        self.current_workflow.is_some()
    }

    /// Check if current step is waiting for user confirmation
    pub fn is_current_step_waiting_confirmation(&self) -> bool {
        if let Ok(session) = self.session_state.try_lock() {
            session.awaiting_user_confirmation
        } else {
            false
        }
    }

    /// Set confirmation flag (block next LLM call)
    pub fn set_confirmation_flag(&self) {
        if let Ok(mut session) = self.session_state.try_lock() {
            session.wait_for_confirmation();
        }
    }

    /// Clear confirmation flag (after user confirms)
    pub fn clear_confirmation_flag(&self) {
        if let Ok(mut session) = self.session_state.try_lock() {
            session.clear_confirmation();
        }
    }

    /// Get current step information for display
    pub fn get_current_step_info(&self) -> Option<StepDisplayInfo> {
        if let Some(workflow) = &self.current_workflow {
            if let Ok(session) = self.session_state.try_lock() {
                if let Some(step) = workflow.get_step(session.current_step_index) {
                    return Some(StepDisplayInfo {
                        name: step.name.clone(),
                        current_step: session.current_step_index + 1,
                        total_steps: workflow.total_steps(),
                    });
                }
            }
        }
        None
    }

    /// Get current step index
    pub fn get_current_step_index(&self) -> usize {
        if let Ok(session) = self.session_state.try_lock() {
            session.current_step_index
        } else {
            0
        }
    }
    
    /// 🚨 Set a variable in session state
    pub fn set_variable(&self, key: &str, value: String) {
        if let Ok(mut session) = self.session_state.try_lock() {
            session.set_variable(key, &value);
        }
    }
    
    /// 🚨 Get a variable from session state
    pub fn get_variable(&self, key: &str) -> Option<String> {
        if let Ok(session) = self.session_state.try_lock() {
            session.get_variable(key).cloned()
        } else {
            None
        }
    }
}

/// Display information for the current workflow step
#[derive(Debug, Clone)]
pub struct StepDisplayInfo {
    pub name: String,
    pub current_step: usize,
    pub total_steps: usize,
}
