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

        // Register default workflow (5-step pipeline)
        engine.register_workflow(crate::agent::workflow::create_default_workflow());

        // Auto-activate the default workflow — no "free mode", all requests go through 5-step pipeline
        let _ = engine.activate_workflow("five_step_pipeline");

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

    /// Advance to next step (or jump to `target_step` if provided).
    /// Returns Ok(true) if more steps remain, Ok(false) if workflow complete.
    pub fn advance_step(&mut self) -> Result<bool, String> {
        self.advance_to_step(None)
    }

    /// Advance to a specific step index (used for route-based skipping).
    pub fn advance_to_step(&mut self, target: Option<usize>) -> Result<bool, String> {
        let workflow = self.current_workflow.as_mut().ok_or("No active workflow")?;

        if let Ok(mut session) = self.session_state.try_lock() {
            let new_idx = if let Some(t) = target {
                t.min(workflow.total_steps() - 1)
            } else {
                session.current_step_index + 1
            };
            
            let jumped = new_idx > session.current_step_index + 1;
            session.current_step_index = new_idx;
            session.awaiting_user_confirmation = false;
            
            if jumped {
                tracing::info!(
                    "[WORKFLOW] Jumped to step {}/{}: {} (skipped intermediate steps)",
                    session.current_step_index + 1,
                    workflow.total_steps(),
                    workflow.get_step(session.current_step_index).map(|s| s.name.as_str()).unwrap_or("Unknown")
                );
            } else {
                tracing::info!(
                    "Advanced to step {}/{}: {}",
                    session.current_step_index + 1,
                    workflow.total_steps(),
                    workflow.get_step(session.current_step_index).map(|s| s.name.as_str()).unwrap_or("Unknown")
                );
            }
            
            // Only mark complete when PAST the last step (index >= total_steps)
            if session.current_step_index >= workflow.total_steps() {
                Ok(false)
            } else {
                Ok(true)
            }
        } else {
            Err("Failed to acquire session lock".to_string())
        }
    }

    /// Check if workflow is complete
    pub fn is_workflow_complete(&self) -> bool {
        if let Some(workflow) = &self.current_workflow {
            if let Ok(session) = self.session_state.try_lock() {
                session.current_step_index >= workflow.total_steps()
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

    /// Get system prompt for current step (with {PREVIOUS_OUTPUT} template substitution)
    pub fn get_step_system_prompt(&self) -> Option<String> {
        if let Some(step) = self.current_step() {
            if !step.step_prompt.is_empty() {
                let mut prompt = step.step_prompt.clone();
                // Substitute {PREVIOUS_OUTPUT} template
                if prompt.contains("{PREVIOUS_OUTPUT}") {
                    if let Some(prev) = self.get_previous_step_output() {
                        let truncated: String = prev.chars().take(2000).collect();
                        prompt = prompt.replace("{PREVIOUS_OUTPUT}", &truncated);
                    } else {
                        prompt = prompt.replace("{PREVIOUS_OUTPUT}", "（无上一步输出）");
                    }
                }
                // Substitute {ALL_PREVIOUS_OUTPUTS} — full aggregate for Step 5
                if prompt.contains("{ALL_PREVIOUS_OUTPUTS}") {
                    let all = self.get_all_step_outputs_summary();
                    prompt = prompt.replace("{ALL_PREVIOUS_OUTPUTS}", &all);
                }
                Some(prompt)
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
        let step = match self.current_step() {
            Some(s) => s,
            None => return Ok(()), // No active workflow — allow all tools
        };

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

    /// Store the LLM output from the previous step (used as {PREVIOUS_OUTPUT} in next step's prompt)
    pub fn set_previous_output(&self, output: &str) {
        self.set_variable("_prev_output", output.to_string());
        // Also store per-step for Step 5's aggregated context
        let step_idx = self.get_current_step_index();
        self.set_variable(&format!("_step{}_output", step_idx), output.to_string());
    }

    /// Retrieve the LLM output from the previous step
    pub fn get_previous_step_output(&self) -> Option<String> {
        self.get_variable("_prev_output")
    }

    /// Build an aggregated summary of all previous steps for the Execute step
    pub fn get_all_step_outputs_summary(&self) -> String {
        let mut summaries = Vec::new();
        let labels = ["意图分类", "任务规划", "审阅计划"];
        for i in 0..3 {
            if let Some(output) = self.get_variable(&format!("_step{}_output", i)) {
                let label = labels.get(i).copied().unwrap_or(&"未知");
                // Extract the JSON portion only (strip surrounding text)
                let json_or_summary = if let (Some(s), Some(e)) = (output.find('{'), output.rfind('}')) {
                    &output[s..=e]
                } else {
                    &output[..output.len().min(500)]
                };
                summaries.push(format!("Step {}: {}\n{}", i + 1, label, json_or_summary));
            }
        }
        if summaries.is_empty() {
            "（无上一步输出）".to_string()
        } else {
            summaries.join("\n\n")
        }
    }

    /// Check if the LLM's response should auto-advance to the next step.
    /// Validates structured output per step. Returns (next_step_idx, None) on success,
    /// or (None, Some(error_message)) if output is invalid.
    ///
    /// Validation per step:
    /// - Step 1: JSON with "intent" field
    /// - Step 2: JSON with "plan" array
    /// - Step 3: JSON with "action" + "description" fields
    /// - Step 4: JSON with "safe" boolean field
    /// - Step 5: text contains "## Done" or "【Done】"
    pub fn advance_on_output(
        &mut self,
        assistant_text: &str,
        had_tool_calls: bool,
    ) -> (Option<usize>, Option<String>) {
        let step_idx = self.get_current_step_index();
        let workflow = match self.current_workflow.as_ref() {
            Some(w) => w, None => return (None, None),
        };
        let _total = workflow.total_steps();

        // In Step 5, check ## Done BEFORE the had_tool_calls check.
        // LLM often outputs ## Done in the same message as the last file_write.
        if step_idx == 4 && (assistant_text.contains("## Done") || assistant_text.contains("【Done】")) {
            return if step_idx + 1 >= _total { (None, None) } else { (Some(step_idx + 1), None) };
        }

        // If tools were called, stay and execute them first.
        if had_tool_calls { return (None, None); }

        match step_idx {
            0 => {
                // Step 0: Intent → advance to Plan
                match validate_json_field(assistant_text, &["intent"]) {
                    Ok(_) => (Some(1), None),
                    Err(msg) => (None, Some(msg)),
                }
            }
            1 => {
                // Step 1: Plan → advance to Review
                match validate_json_field(assistant_text, &["plan", "skills"]) {
                    Ok(_) => (Some(2), None),
                    Err(msg) => (None, Some(msg)),
                }
            }
            2 => {
                // Step 2: Review → advance to Execute
                match validate_json_field(assistant_text, &["safe", "complete"]) {
                    Ok(_) => (Some(3), None),
                    Err(msg) => (None, Some(msg)),
                }
            }
            3 => {
                // Step 3: Execute until ## Done
                if assistant_text.contains("## Done") || assistant_text.contains("【Done】") {
                    if step_idx + 1 >= _total { (None, None) } else { (Some(step_idx + 1), None) }
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        }
    }
}

/// Find and parse JSON from LLM output, validate required fields exist.
fn validate_json_field(text: &str, required_fields: &[&str]) -> Result<(), String> {
    let json_str = extract_json_block(text)
        .ok_or_else(|| format!(
            "❌ 你的回复不包含有效的 JSON。请输出 JSON 对象，包含以下字段：{}。\n请重新输出。",
            required_fields.join("、")
        ))?;

    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("❌ JSON 解析失败：{}。请检查格式后重新输出。", e))?;

    let obj = parsed.as_object()
        .ok_or_else(|| "❌ 你的输出不是 JSON 对象。请输出 {{...}} 格式的 JSON。".to_string())?;

    let mut missing = Vec::new();
    for field in required_fields {
        if !obj.contains_key(*field) || obj[*field].is_null() {
            missing.push(*field);
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "❌ JSON 缺少必填字段：{}。请补全后重新输出。",
            missing.join("、")
        ))
    }
}

/// Extract JSON object from LLM text (handles code fences and inline JSON).
fn extract_json_block(text: &str) -> Option<String> {
    // Try code-fenced JSON first
    if let (Some(start), Some(end)) = (text.find("```json"), text.rfind("```")) {
        let inner = &text[start + 7..end].trim();
        if inner.starts_with('{') { return Some(inner.to_string()); }
    }
    // Try raw JSON object
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            return Some(text[start..=end].to_string());
        }
    }
    None
}

/// Display information for the current workflow step
#[derive(Debug, Clone)]
pub struct StepDisplayInfo {
    pub name: String,
    pub current_step: usize,
    pub total_steps: usize,
}
