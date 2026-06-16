use crate::agent::intervention::InterventionRequest;
use crate::agent::session::SessionState;
use crate::agent::workflow::{Workflow, WorkflowStep};
use std::sync::Arc;

/// Canonical workflow routing derived from intent + complexity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRouting {
    pub intent: String,
    pub complexity: String,
    pub pipeline: String,
    pub skip_plan: bool,
    pub skip_review: bool,
    /// Entering Execute always requires human confirmation first.
    pub requires_human_confirm: bool,
    pub steps_summary: String,
}

impl IntentRouting {
    pub fn compute(intent: &str, complexity: &str) -> Self {
        let complexity = if complexity.is_empty() {
            "complex"
        } else {
            complexity
        };
        match intent {
            "chat" => Self {
                intent: intent.to_string(),
                complexity: complexity.to_string(),
                pipeline: "chat".to_string(),
                skip_plan: true,
                skip_review: true,
                requires_human_confirm: false,
                steps_summary: "闲聊 → 直接回复".to_string(),
            },
            "exploring" => Self {
                intent: intent.to_string(),
                complexity: complexity.to_string(),
                pipeline: "fast".to_string(),
                skip_plan: true,
                skip_review: true,
                requires_human_confirm: true,
                steps_summary: "意图 → 人工确认 → 只读执行（跳过规划/审阅）".to_string(),
            },
            "coding" if complexity == "simple" => Self {
                intent: intent.to_string(),
                complexity: complexity.to_string(),
                pipeline: "fast".to_string(),
                skip_plan: true,
                skip_review: true,
                requires_human_confirm: true,
                steps_summary: "意图 → 人工确认 → 执行（跳过规划/审阅）".to_string(),
            },
            "coding" => Self {
                intent: intent.to_string(),
                complexity: complexity.to_string(),
                pipeline: "standard".to_string(),
                skip_plan: false,
                skip_review: false,
                requires_human_confirm: true,
                steps_summary: "意图 → 规划 → 审阅 → 人工确认 → 执行".to_string(),
            },
            _ => Self::compute("coding", "complex"),
        }
    }
}

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

        // Register default workflow (4-step pipeline)
        engine.register_workflow(crate::agent::workflow::create_default_workflow());

        // Auto-activate the default workflow
        let _ = engine.activate_workflow(crate::agent::workflow::DEFAULT_WORKFLOW_ID);

        engine
    }

    /// Register a workflow
    pub fn register_workflow(&mut self, workflow: Workflow) {
        let id = workflow.id.clone();
        self.workflows.insert(id, workflow);
    }

    /// Activate a workflow by ID
    pub fn activate_workflow(&mut self, workflow_id: &str) -> Result<(), String> {
        let resolved_id = match workflow_id {
            crate::agent::workflow::LEGACY_WORKFLOW_ID => crate::agent::workflow::DEFAULT_WORKFLOW_ID,
            other => other,
        };
        if let Some(workflow) = self.workflows.get(resolved_id).cloned() {
            tracing::info!("Activating workflow: {}", workflow.name);
            self.current_workflow = Some(workflow);

            // Update session state (use try_lock to avoid blocking in async context)
            if let Ok(mut session) = self.session_state.try_lock() {
                session.current_workflow = resolved_id.to_string();
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
        let step = match self.current_step() {
            Some(s) => s,
            None => return false,
        };
        if !step.allow_code_modification {
            return false;
        }
        // Exploring fast-path lands on Execute but must stay read-only.
        if self.get_current_step_index() == 3
            && Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
                .map(|r| r.intent == "exploring")
                .unwrap_or(false)
        {
            return false;
        }
        true
    }

    /// Get allowed tools for current step.
    /// Returns empty when tools are disabled for this step.
    pub fn get_allowed_tools(&self) -> Vec<String> {
        if let Some(step) = self.current_step() {
            if !step.allow_tool_execution {
                return Vec::new();
            }
            step.allowed_tools.clone()
        } else {
            Vec::new()
        }
    }

    pub fn clear_turn_memory(&self) {
        self.set_variable("_turn_memory", String::new());
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

    /// Roll back from Review to Plan after failed review; preserve plan as PREVIOUS_OUTPUT.
    pub fn rollback_review_to_plan(
        &mut self,
        review_output: &str,
        feedback: &str,
    ) -> Result<(), String> {
        self.set_variable("_step2_output", review_output.to_string());
        self.set_variable("_review_feedback", feedback.to_string());
        if let Some(plan) = self.get_variable("_step1_output") {
            self.set_variable("_prev_output", plan);
        }
        self.go_to_step(1)
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

    /// Reset workflow to first step (clears ephemeral state for a new user round).
    pub fn reset_workflow(&mut self) {
        if self.current_workflow.is_some() {
            if let Ok(mut session) = self.session_state.try_lock() {
                session.current_step_index = 0;
                session.awaiting_user_confirmation = false;
                session.set_variable("_explored_paths", "[]");
                session.set_variable("_exploration_snapshot", "[]");
                session.set_variable("_plan_tracker", "");
                session.set_variable("_route_chat", "");
                session.set_variable("_chat_reply_pending", "");
                session.set_variable("_chat_reply", "");
                session.set_variable("_done_gate_blocks", "");
                session.set_variable("_turn_memory", "");
                session.set_variable("_await_execute_confirm", "");
                for key in [
                    "_step0_output",
                    "_step1_output",
                    "_step2_output",
                    "_step3_output",
                    "_prev_output",
                    "_plan_draft",
                    "_review_feedback",
                ] {
                    session.set_variable(key, "");
                }
            } else {
                tracing::warn!("Failed to acquire session lock for workflow reset");
            }
        }
    }

    /// Start a new user round: archive previous, reset workflow, set current request.
    pub fn begin_user_round(&mut self, user_message: &str) {
        crate::agent::user_round::begin_user_round(self, user_message);
    }

    pub fn user_round_memory_block(&self) -> String {
        crate::agent::user_round::format_user_round_block(self)
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
                if prompt.contains("{EXPLORATION_SNAPSHOT}") {
                    let snap = self.exploration_snapshot_summary();
                    let text = if snap.is_empty() {
                        "（无探索记录）".to_string()
                    } else {
                        snap
                    };
                    prompt = prompt.replace("{EXPLORATION_SNAPSHOT}", &text);
                }
                if prompt.contains("{REVIEW_FEEDBACK}") {
                    let fb = self
                        .get_variable("_review_feedback")
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "（无）".to_string());
                    prompt = prompt.replace("{REVIEW_FEEDBACK}", &fb);
                }
                if prompt.contains("{USER_REQUEST}") {
                    let req = self
                        .get_variable("_current_user_request")
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "（无）".to_string());
                    prompt = prompt.replace("{USER_REQUEST}", &req);
                }
                if prompt.contains("{ROUTING_HINT}") {
                    let hint = self
                        .get_variable("_current_user_request")
                        .map(|u| Self::routing_hint_for_user(&u))
                        .unwrap_or_default();
                    prompt = prompt.replace("{ROUTING_HINT}", &hint);
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

        // Check if code modification is allowed (step + exploring read-only override)
        if !self.allows_code_modification() {
            // Check if this is a code-modifying tool
            let is_code_tool = matches!(tool_name, "file_write" | "edit_file" | "delete_range");

            if is_code_tool {
                // Extract file path from arguments
                let file_path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

                // Check if it's a source code file
                if crate::source_paths::is_source_code_path(file_path) {
                    return Err(format!(
                        "Code modification not allowed in current step. You can only create/modify documentation files (.md, .txt, etc.), not {}. Attempted to modify: {}",
                        crate::source_paths::source_code_guard_hint(),
                        file_path
                    ));
                }
            }
        }

        Ok(())
    }

    /// Check if a file path is a source code file (delegates to shared registry).
    fn is_source_code_file(file_path: &str) -> bool {
        crate::source_paths::is_source_code_path(file_path)
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

    /// Human must confirm after Review (or skip-review) before Execute starts.
    pub fn arm_execute_confirmation(&self) {
        self.set_variable("_await_execute_confirm", "1".to_string());
        self.set_confirmation_flag();
    }

    pub fn is_awaiting_execute_confirmation(&self) -> bool {
        self.get_variable("_await_execute_confirm").as_deref() == Some("1")
    }

    pub fn clear_execute_confirmation(&self) {
        self.set_variable("_await_execute_confirm", String::new());
        self.clear_confirmation_flag();
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
        if step_idx == 1 {
            self.load_plan_tracker(output);
        }
    }

    /// Mark workflow finished (e.g. chat intent — skip Plan/Review/Execute).
    pub fn complete_workflow(&mut self) -> Result<(), String> {
        let total = self
            .current_workflow
            .as_ref()
            .map(|w| w.total_steps())
            .ok_or("No active workflow")?;
        if let Ok(mut session) = self.session_state.try_lock() {
            session.current_step_index = total;
            session.awaiting_user_confirmation = false;
            Ok(())
        } else {
            Err("Failed to acquire session lock".to_string())
        }
    }

    pub fn consume_chat_route(&self) -> bool {
        let v = self.get_variable("_route_chat").unwrap_or_default();
        if v == "1" {
            self.set_variable("_route_chat", String::new());
            true
        } else {
            false
        }
    }

    pub fn set_chat_route(&self) {
        self.set_variable("_route_chat", "1".to_string());
    }

    pub fn set_chat_reply_pending(&self) {
        self.set_variable("_chat_reply_pending", "1".to_string());
    }

    pub fn is_chat_reply_pending(&self) -> bool {
        self.get_variable("_chat_reply_pending").as_deref() == Some("1")
    }

    pub fn clear_chat_reply_pending(&self) {
        self.set_variable("_chat_reply_pending", String::new());
    }

    /// Parse intent JSON and verify `pipeline` matches the canonical routing table.
    pub fn validate_intent_pipeline(assistant_text: &str) -> Result<IntentRouting, String> {
        validate_json_field(
            assistant_text,
            &["intent", "complexity", "pipeline", "routing_reason"],
        )?;
        let json_str = extract_json_block(assistant_text).ok_or_else(|| {
            "❌ 未找到 JSON。请按意图步骤格式输出。".to_string()
        })?;
        let v: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| format!("❌ JSON 解析失败：{e}"))?;
        let intent = v
            .get("intent")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "❌ 缺少 intent 字段。".to_string())?;
        let complexity = v
            .get("complexity")
            .and_then(|x| x.as_str())
            .unwrap_or("complex");
        let pipeline = v
            .get("pipeline")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "❌ 缺少 pipeline 字段。".to_string())?;
        let reason = v
            .get("routing_reason")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if reason.chars().count() < 8 {
            return Err(
                "❌ routing_reason 太短（≥8字）。请说明为何选此 pipeline、跳过或保留哪些步骤。"
                    .to_string(),
            );
        }
        let canonical = IntentRouting::compute(intent, complexity);
        if pipeline != canonical.pipeline {
            return Err(format!(
                "❌ pipeline 应为 \"{}\"（{}），你填了 \"{}\"。\n请根据 intent={intent} complexity={complexity} 修正 pipeline 与 routing_reason。"
                ,
                canonical.pipeline, canonical.steps_summary, pipeline
            ));
        }
        Ok(canonical)
    }

    pub fn intent_routing_from_text(step0: Option<&str>) -> Option<IntentRouting> {
        let (intent, complexity) = Self::parse_intent_meta(step0)?;
        Some(IntentRouting::compute(&intent, &complexity))
    }

    /// Phrasing that means read-only code audit (检查/审查), not implementation.
    pub fn looks_like_read_only_audit(user_text: &str) -> bool {
        let t = user_text.trim();
        if t.is_empty() {
            return false;
        }
        let lower = t.to_lowercase();
        let has_audit = [
            "检查", "审查", "排查", "分析", "看看", "评估", "audit", "review", "inspect", "check",
        ]
        .iter()
        .any(|k| t.contains(k) || lower.contains(k));
        let wants_modify = [
            "修改", "重构", "实现", "修复", "添加", "删除", "改写", "fix", "implement", "refactor",
        ]
        .iter()
        .any(|k| t.contains(k) || lower.contains(k));
        has_audit && !wants_modify
    }

    pub fn routing_hint_for_user(user_text: &str) -> String {
        if Self::looks_like_read_only_audit(user_text) {
            "【路由提示】只读代码检查 → 必须 intent=exploring, pipeline=fast（跳过规划/审阅；人工确认后只读执行；禁止 modify/delete 计划）".to_string()
        } else {
            String::new()
        }
    }

    /// If the user asked for read-only audit but the model chose coding/standard, force exploring/fast.
    pub fn correct_intent_json_for_user(user_text: &str, assistant_text: &str) -> String {
        if !Self::looks_like_read_only_audit(user_text) {
            return assistant_text.to_string();
        }
        if Self::validate_intent_pipeline(assistant_text).is_ok() {
            if let Some(r) = Self::intent_routing_from_text(Some(assistant_text)) {
                if r.intent == "exploring" && r.pipeline == "fast" {
                    return assistant_text.to_string();
                }
            }
        }
        let topic = extract_json_block(assistant_text)
            .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
            .and_then(|v| v.get("topic").and_then(|t| t.as_str()).map(String::from))
            .unwrap_or_else(|| user_text.chars().take(80).collect());
        tracing::info!(
            "[INTENT] Read-only audit request — auto-correcting intent to exploring/fast"
        );
        serde_json::json!({
            "intent": "exploring",
            "complexity": "complex",
            "files": [],
            "topic": topic,
            "pipeline": "fast",
            "routing_reason": "用户请求为只读代码检查，使用 exploring/fast：跳过规划与审阅，人工确认后只读探索输出，不修改文件"
        })
        .to_string()
    }

    /// Reject plans that propose file changes during read-only (exploring) workflows.
    pub fn validate_plan_read_only(json_str: &str) -> Result<(), String> {
        let v: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| format!("❌ plan JSON 解析失败：{e}"))?;
        let Some(plan) = v.get("plan").and_then(|p| p.as_array()) else {
            return Ok(());
        };
        for (i, step) in plan.iter().enumerate() {
            let action = step
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(action.as_str(), "modify" | "delete" | "add" | "create") {
                return Err(format!(
                    "❌ 只读检查任务 plan 步骤 {} 不得使用 action={action}。请改为 explain，且 desc 中说明只分析不修改。",
                    i + 1
                ));
            }
        }
        Ok(())
    }

    /// Read-only exploring and simple coding skip Review.
    pub fn should_skip_review(&self) -> bool {
        Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
            .map(|r| r.skip_review)
            .unwrap_or(false)
    }

    /// Exploring and simple coding skip Plan + Review.
    pub fn should_skip_plan_and_review(&self) -> bool {
        Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
            .map(|r| r.skip_plan)
            .unwrap_or(false)
    }

    /// Build a minimal plan JSON for fast-path Execute (no Plan/Review steps).
    pub fn build_fast_path_plan(intent_text: &str) -> Option<String> {
        let json = extract_json_block(intent_text)?;
        let v: serde_json::Value = serde_json::from_str(&json).ok()?;
        let intent = v.get("intent").and_then(|t| t.as_str()).unwrap_or("coding");
        let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("任务");
        let files: Vec<String> = v
            .get("files")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        if intent == "exploring" {
            let key_files = files.clone();
            let file_hint = if files.is_empty() {
                "项目整体".to_string()
            } else {
                files.join(", ")
            };
            return Some(
                serde_json::json!({
                    "structure_summary": format!("只读代码检查：{topic}（{file_hint}）"),
                    "plan": [{
                        "step": 1,
                        "file": files.first().cloned().unwrap_or_default(),
                        "action": "explain",
                        "target": "项目代码",
                        "desc": format!(
                            "只读检查：{topic}。用 file_list / file_read / code_search 探索，汇总问题与改进建议，不修改任何文件。"
                        ),
                        "verify": "完成分析后输出 ## Done"
                    }],
                    "skills": [],
                    "key_files": key_files
                })
                .to_string(),
            );
        }

        let plan_steps: Vec<serde_json::Value> = if files.is_empty() {
            vec![serde_json::json!({
                "step": 1,
                "file": "",
                "action": "modify",
                "target": "",
                "desc": format!("完成用户请求: {topic}"),
                "verify": "运行项目检查或手动验证"
            })]
        } else {
            files
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    serde_json::json!({
                        "step": i + 1,
                        "file": f,
                        "action": "modify",
                        "target": "",
                        "desc": format!("处理 {f} — {topic}"),
                        "verify": "file_read 确认修改"
                    })
                })
                .collect()
        };

        let key_files = if files.is_empty() {
            Vec::<String>::new()
        } else {
            files.clone()
        };
        let summary = if files.is_empty() {
            format!("快速路径：简单编码任务 — {topic}")
        } else {
            format!(
                "快速路径：简单编码任务，涉及 {} 个文件 — {topic}",
                files.len()
            )
        };

        Some(
            serde_json::json!({
                "structure_summary": summary,
                "plan": plan_steps,
                "skills": [],
                "key_files": key_files
            })
            .to_string(),
        )
    }

    /// Seed synthetic plan + tracker when jumping Intent → Execute.
    pub fn prepare_fast_path_execute(&self, intent_output: &str) {
        if let Some(plan) = Self::build_fast_path_plan(intent_output) {
            self.set_variable("_step1_output", plan.clone());
            self.load_plan_tracker(&plan);
        }
    }

    pub fn parse_intent_meta(step0: Option<&str>) -> Option<(String, String)> {
        let text = step0?;
        let json = extract_json_block(text)?;
        let v: serde_json::Value = serde_json::from_str(&json).ok()?;
        let intent = v.get("intent")?.as_str()?.to_string();
        let complexity = v
            .get("complexity")
            .and_then(|c| c.as_str())
            .unwrap_or("complex")
            .to_string();
        Some((intent, complexity))
    }

    pub fn load_plan_tracker(&self, plan_output: &str) {
        if let Some(tracker) = crate::agent::plan_tracker::load_from_output(plan_output) {
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
        }
    }

    pub fn get_plan_tracker(&self) -> Option<crate::agent::plan_tracker::PlanTracker> {
        self.get_variable("_plan_tracker")
            .and_then(|s| crate::agent::plan_tracker::tracker_from_json(&s))
    }

    pub fn plan_progress_summary(&self) -> String {
        self.get_plan_tracker()
            .map(|t| t.progress_summary())
            .unwrap_or_default()
    }

    pub fn try_mark_plan_step_done(&self, path: &str) -> bool {
        let mut tracker = match self.get_plan_tracker() {
            Some(t) => t,
            None => return false,
        };
        let changed = tracker.try_mark_done_for_path(path);
        if changed {
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
            return true;
        }
        // Plan steps with empty `file` (fast-path / shell tasks) — advance current step.
        if tracker
            .current_step()
            .map(|s| s.file.is_empty())
            .unwrap_or(false)
        {
            return self.try_mark_plan_current_step_done();
        }
        false
    }

    /// Update plan tracker after a successful Execute-step tool call.
    /// Returns true when plan progress changed.
    pub fn record_execute_tool_success(&self, tool_name: &str, arguments: &str) -> bool {
        if self.get_current_step_index() != 3 {
            return false;
        }
        let args: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);

        match tool_name {
            "file_write" | "edit_file" | "delete_range" => args
                .get("path")
                .and_then(|p| p.as_str())
                .map(|path| self.try_mark_plan_step_done(path))
                .unwrap_or(false),
            "file_read" => {
                let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                if path.is_empty() {
                    return false;
                }
                let is_explain = self
                    .get_plan_tracker()
                    .and_then(|t| t.current_step().map(|s| s.action == "explain"))
                    .unwrap_or(false);
                if is_explain {
                    self.try_mark_plan_step_done(path)
                } else {
                    false
                }
            }
            "git_diff" => {
                if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                    if self.try_mark_plan_step_done(path) {
                        return true;
                    }
                }
                self.try_mark_plan_current_step_done()
            }
            "shell_exec" | "load_skill" | "git_status" => self.try_mark_plan_current_step_done(),
            // Read-only exploration / search — do not advance plan steps
            _ => false,
        }
    }

    pub fn plan_progress_message_after_tool(&self, tool_name: &str) -> Option<String> {
        let summary = self.plan_progress_summary();
        if summary.is_empty() {
            return None;
        }
        let label = match tool_name {
            "file_write" | "edit_file" | "delete_range" => "计划项已完成",
            "shell_exec" => "shell 执行完成，计划项已更新",
            "load_skill" => "skill 已加载，计划项已更新",
            "git_status" | "git_diff" => "git 检查完成，计划项已更新",
            "file_read" => "只读步骤已完成",
            _ => "计划项已更新",
        };
        Some(format!(
            "{}\n✅ {label}\n{summary}",
            crate::agent::context_injector::STEP_MEMORY_TAG
        ))
    }

    /// Mark the active plan step done (e.g. after shell_exec).
    pub fn try_mark_plan_current_step_done(&self) -> bool {
        let mut tracker = match self.get_plan_tracker() {
            Some(t) => t,
            None => return false,
        };
        let changed = tracker.mark_current_step_done();
        if changed {
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
        }
        changed
    }

    pub fn verify_hint_for_path(&self, path: &str) -> Option<String> {
        self.get_plan_tracker()
            .and_then(|t| t.verify_hint_for_path(path))
    }

    pub fn check_plan_done_gate(&self) -> Option<String> {
        self.get_plan_tracker().and_then(|t| t.check_done_gate())
    }

    pub fn text_signals_done(text: &str) -> bool {
        text.contains("## Done") || text.contains("【Done】")
    }

    /// Fast/exploring paths use synthetic plans — shell/git may not map to file_write markers.
    pub fn should_skip_plan_done_gate(&self) -> bool {
        Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
            .map(|r| r.intent == "exploring" || r.pipeline == "fast")
            .unwrap_or(false)
    }

    pub fn bump_done_gate_block(&self) -> u32 {
        let n = self
            .get_variable("_done_gate_blocks")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
            .saturating_add(1);
        self.set_variable("_done_gate_blocks", n.to_string());
        n
    }

    pub fn clear_done_gate_blocks(&self) {
        self.set_variable("_done_gate_blocks", String::new());
    }

    pub fn mark_plan_all_done(&self) {
        if let Some(mut tracker) = self.get_plan_tracker() {
            for step in &mut tracker.steps {
                step.status = crate::agent::plan_tracker::StepStatus::Done;
            }
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
        }
    }

    /// Execute step signaled ## Done — advance workflow to complete.
    /// Returns Ok(true) when finished, Err when plan gate blocks (coding tasks).
    pub fn try_complete_execute_on_done(&mut self, assistant_text: &str) -> Result<bool, String> {
        if self.get_current_step_index() != 3 || !Self::text_signals_done(assistant_text) {
            return Ok(false);
        }
        if self.should_skip_plan_done_gate() {
            self.mark_plan_all_done();
        } else if let Some(msg) = self.check_plan_done_gate() {
            return Err(msg);
        }
        match self.advance_step()? {
            false => {
                self.clear_done_gate_blocks();
                Ok(true)
            }
            true => {
                tracing::warn!("[WORKFLOW] ## Done on execute but workflow still has steps — forcing complete");
                self.complete_workflow()?;
                self.clear_done_gate_blocks();
                Ok(true)
            }
        }
    }

    pub fn save_turn_memory(&self, tm: &crate::agent::turn_memory::TurnMemory) {
        self.set_variable(
            "_turn_memory",
            crate::agent::turn_memory::turn_memory_to_json(tm),
        );
    }

    pub fn load_turn_memory(&self) -> Option<crate::agent::turn_memory::TurnMemory> {
        self.get_variable("_turn_memory")
            .and_then(|s| crate::agent::turn_memory::turn_memory_from_json(&s))
    }

    /// Combined durable context for turn start injection.
    pub fn durable_memory_block(&self) -> String {
        crate::agent::memory_bridge::format_durable_memory_block(self)
    }

    /// Retrieve the LLM output from the previous step
    pub fn get_previous_step_output(&self) -> Option<String> {
        self.get_variable("_prev_output")
    }

    /// Normalize a directory path for exploration deduplication.
    pub fn normalize_explore_path(path: &str) -> String {
        let p = path.trim().trim_matches(|c| c == '/' || c == '\\');
        if p.is_empty() { ".".to_string() } else { p.to_lowercase() }
    }

    /// Record that a directory was already listed/read during Plan exploration.
    pub fn record_explored_path(&self, tool: &str, path: &str) {
        let key = format!("{}:{}", tool, Self::normalize_explore_path(path));
        let mut paths = self.get_explored_path_set();
        if paths.insert(key) {
            if let Ok(json) = serde_json::to_string(&paths) {
                self.set_variable("_explored_paths", json);
            }
        }
    }

    /// Check whether this tool+path was already explored in the current workflow.
    pub fn is_path_explored(&self, tool: &str, path: &str) -> bool {
        let key = format!("{}:{}", tool, Self::normalize_explore_path(path));
        self.get_explored_path_set().contains(&key)
    }

    /// Record a tool result into the Plan-step exploration snapshot.
    pub fn record_exploration_result(
        &self,
        working_dir: &std::path::Path,
        tool: &str,
        target: &str,
        raw_result: &str,
    ) {
        if !crate::agent::exploration_snapshot::is_snapshot_tool(tool) {
            return;
        }
        let content = crate::agent::exploration_snapshot::extract_data_content(raw_result);
        let mut entries = self.get_exploration_entries();
        crate::agent::exploration_snapshot::merge_entry(
            &mut entries,
            working_dir,
            tool,
            target,
            &content,
        );
        self.set_variable(
            "_exploration_snapshot",
            crate::agent::exploration_snapshot::entries_to_json(&entries),
        );
    }

    /// Formatted exploration snapshot for Review / Execute steps.
    pub fn exploration_snapshot_summary(&self) -> String {
        let entries = self.get_exploration_entries();
        crate::agent::exploration_snapshot::format_summary(&entries, 24_000)
    }

    fn get_exploration_entries(&self) -> Vec<crate::agent::exploration_snapshot::ExplorationEntry> {
        self.get_variable("_exploration_snapshot")
            .map(|s| crate::agent::exploration_snapshot::entries_from_json(&s))
            .unwrap_or_default()
    }

    /// True when mandatory Plan-step exploration is complete (gates JSON-only mode).
    pub fn plan_exploration_satisfied(&self) -> bool {
        crate::agent::plan_tracker::validate_plan_exploration(
            &self.get_exploration_entries(),
            &self.get_explored_path_set(),
        )
        .is_ok()
    }

    /// Return cached exploration preview when the same tool+path was already run.
    pub fn lookup_exploration_cache(&self, tool: &str, target: &str) -> Option<String> {
        let norm = crate::agent::plan_tracker::normalize_path(target);
        self.get_exploration_entries()
            .into_iter()
            .find(|e| {
                e.tool == tool
                    && crate::agent::plan_tracker::normalize_path(&e.target) == norm
            })
            .map(|e| {
                let mut out = format!(
                    "✅ 【缓存】已探索过 `{target}`（勿重复 {tool}）\n\n{}",
                    e.content
                );
                if let Some(ref rp) = e.ref_path {
                    out.push_str(&format!("\n\n完整快照: `{rp}`"));
                }
                out
            })
    }

    /// What to call next during Plan exploration (reduces repeat-tool loops).
    pub fn plan_exploration_hint(&self) -> String {
        crate::agent::plan_tracker::exploration_next_action(
            &self.get_exploration_entries(),
            &self.get_explored_path_set(),
        )
    }

    /// Whether exploration snapshot already contains a tool result.
    pub fn has_exploration_tool(&self, tool: &str) -> bool {
        self.get_exploration_entries()
            .iter()
            .any(|e| e.tool == tool)
    }

    /// Full plan readiness: field validation + exploration depth + path grounding.
    pub fn validate_plan_ready(&self, json_str: &str) -> Result<(), String> {
        crate::agent::plan_tracker::validate_plan_steps(json_str)?;
        let entries = self.get_exploration_entries();
        let explored = self.get_explored_path_set();
        crate::agent::plan_tracker::validate_plan_exploration(&entries, &explored)?;
        crate::agent::plan_tracker::validate_plan_paths_known(json_str, &entries, &explored)?;
        Ok(())
    }

    /// Human-readable list of explored paths for context injection.
    pub fn explored_paths_summary(&self) -> String {
        let paths = self.get_explored_path_set();
        if paths.is_empty() {
            return String::new();
        }
        let mut items: Vec<String> = paths.into_iter().collect();
        items.sort();
        items.join("\n")
    }

    fn get_explored_path_set(&self) -> std::collections::HashSet<String> {
        self.get_variable("_explored_paths")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
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
    /// - Step 0 (Intent): JSON with "intent" field
    /// - Step 1 (Plan): JSON with "plan" array
    /// - Step 2 (Review): JSON with "safe" + "complete" fields
    /// - Step 3 (Execute): text contains "## Done" or "【Done】"
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

        match step_idx {
            0 => {
                match Self::validate_intent_pipeline(assistant_text) {
                    Ok(routing) => {
                        if routing.intent == "chat" {
                            self.set_chat_route();
                            return (None, None);
                        }
                        if routing.skip_plan {
                            self.prepare_fast_path_execute(assistant_text);
                            return (Some(3), None);
                        }
                        (Some(1), None)
                    }
                    Err(_msg) if had_tool_calls && assistant_text.trim().is_empty() => {
                        (None, None)
                    }
                    Err(msg) => (None, Some(msg)),
                }
            }
            1 => {
                match validate_json_field(assistant_text, &["plan"]) {
                    Ok(_) => {
                        let json_str = extract_json_block(assistant_text).unwrap_or_default();
                        if let Err(msg) = self.validate_plan_ready(&json_str) {
                            return (None, Some(msg));
                        }
                        if Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
                            .map(|r| r.intent == "exploring")
                            .unwrap_or(false)
                        {
                            if let Err(msg) = Self::validate_plan_read_only(&json_str) {
                                return (None, Some(msg));
                            }
                        }
                        self.set_variable("_review_feedback", String::new());
                        let next = if self.should_skip_review() { 3 } else { 2 };
                        (Some(next), None)
                    }
                    Err(_msg) if had_tool_calls && assistant_text.trim().is_empty() => {
                        (None, None)
                    }
                    Err(msg) => (None, Some(msg)),
                }
            }
            2 => {
                match validate_json_field(assistant_text, &["safe", "complete"]) {
                    Ok(_) => {
                        let json_str = extract_json_block(assistant_text).unwrap_or_default();
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let safe = v.get("safe").and_then(|b| b.as_bool()).unwrap_or(false);
                            let complete = v
                                .get("complete")
                                .and_then(|b| b.as_bool())
                                .unwrap_or(false);
                            if !safe || !complete {
                                let issues: Vec<String> = v
                                    .get("issues")
                                    .and_then(|a| a.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|x| x.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let msg = if issues.is_empty() {
                                    "审阅未通过（safe 或 complete 为 false）。".to_string()
                                } else {
                                    format!("审阅未通过:\n{}", issues.join("\n"))
                                };
                                return (Some(1), Some(format!(
                                    "【审阅回退】{msg}\n\n请根据审阅意见修正计划，重新输出完整 plan JSON。"
                                )));
                            }
                        }
                        (Some(3), None)
                    }
                    Err(msg) if looks_like_review_prose(assistant_text) => (
                        Some(1),
                        Some(format!(
                            "❌ 审阅步骤必须输出 JSON，不能输出 Markdown 摘要。\n{msg}\n\n已回退到规划步骤，请修正计划后重新输出 plan JSON。"
                        )),
                    ),
                    // Any other validation failure → rollback to Plan (第二步), not retry Review in place
                    Err(msg) => (
                        Some(1),
                        Some(review_rollback_message(&msg)),
                    ),
                }
            }
            3 => {
                if Self::text_signals_done(assistant_text) && !self.should_skip_plan_done_gate() {
                    if let Some(msg) = self.check_plan_done_gate() {
                        return (None, Some(msg));
                    }
                }
                (None, None)
            }
            _ => (None, None),
        }
    }
}

fn looks_like_review_prose(text: &str) -> bool {
    extract_json_block(text).is_none()
        && (text.contains("计划不完整")
            || text.contains("安全问题")
            || text.contains("审阅未通过")
            || text.contains('⚠'))
}

fn review_rollback_message(detail: &str) -> String {
    format!(
        "【审阅回退】{detail}\n\n请根据审阅意见修正计划，重新输出完整 plan JSON。"
    )
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
pub fn extract_json_block(text: &str) -> Option<String> {
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

#[cfg(test)]
mod advance_tests {
    use super::*;
    use crate::agent::session::SessionState;

    fn test_engine_at_step(step: usize) -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("test")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        session.blocking_lock().current_step_index = step;
        engine
    }

    #[test]
    fn review_incomplete_rolls_back_to_plan() {
        let mut engine = test_engine_at_step(2);
        engine.set_variable("_step1_output", r#"{"plan":[]}"#.to_string());
        let text = r#"{"safe": true, "complete": false, "issues": ["no src/"], "warnings": []}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(1));
        assert!(err.unwrap().contains("审阅回退"));
    }

    #[test]
    fn review_prose_rolls_back_to_plan() {
        let mut engine = test_engine_at_step(2);
        let text = "⚠️ 计划不完整\n  ❌ Step 1 targets 'src/'";
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(1));
        assert!(err.unwrap().contains("JSON"));
    }

    #[test]
    fn review_plan_json_instead_of_review_rolls_back() {
        let mut engine = test_engine_at_step(2);
        let text = r#"{"structure_summary":"x","plan":[{"step":1,"file":"main.rs","action":"explain"}]}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(1));
        assert!(err.unwrap().contains("审阅回退"));
    }

    #[test]
    fn review_pass_advances_to_execute() {
        let mut engine = test_engine_at_step(2);
        let text = r#"{"safe": true, "complete": true, "issues": [], "warnings": []}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(3));
        assert!(err.is_none());
    }

    #[test]
    fn simple_coding_intent_skips_to_execute() {
        let mut engine = test_engine_at_step(0);
        let text = r#"{"intent":"coding","complexity":"simple","files":["src/main.rs"],"topic":"fix typo","pipeline":"fast","routing_reason":"单文件小改动，可跳过规划与审阅直接执行"}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(3));
        assert!(err.is_none());
        assert!(engine.get_variable("_step1_output").is_some());
    }

    #[test]
    fn exploring_intent_skips_plan_and_review() {
        let mut engine = test_engine_at_step(0);
        let text = r#"{"intent":"exploring","complexity":"complex","files":[],"topic":"检查整个项目代码","pipeline":"fast","routing_reason":"只读代码审查，不需要修改计划，跳过规划与审阅"}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(3));
        assert!(err.is_none());
        let plan = engine.get_variable("_step1_output").unwrap();
        assert!(plan.contains("只读"));
    }

    #[test]
    fn exploring_intent_should_skip_review_flag() {
        let mut engine = test_engine_at_step(1);
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"exploring","complexity":"complex","topic":"audit","pipeline":"fast","routing_reason":"只读检查"}"#.to_string(),
        );
        assert!(engine.should_skip_review());
        assert!(engine.should_skip_plan_and_review());
    }

    #[test]
    fn wrong_pipeline_rejected() {
        let mut engine = test_engine_at_step(0);
        let text = r#"{"intent":"exploring","complexity":"complex","topic":"audit","pipeline":"standard","routing_reason":"错误的路径选择应该被拒绝"}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, None);
        assert!(err.unwrap().contains("pipeline"));
    }

    #[test]
    fn complex_coding_intent_goes_to_plan() {
        let mut engine = test_engine_at_step(0);
        let text = r#"{"intent":"coding","complexity":"complex","files":[],"topic":"refactor","pipeline":"standard","routing_reason":"多文件重构需先规划并审阅，不能走快速路径"}"#;
        let (next, err) = engine.advance_on_output(text, false);
        assert_eq!(next, Some(1));
        assert!(err.is_none());
    }

    #[test]
    fn read_only_audit_user_phrase_detected() {
        assert!(WorkflowEngine::looks_like_read_only_audit("检查下整个项目的代码"));
        assert!(!WorkflowEngine::looks_like_read_only_audit("重构 agent 模块"));
    }

    #[test]
    fn read_only_audit_corrects_coding_intent() {
        let user = "检查下整个项目的代码";
        let wrong = r#"{"intent":"coding","complexity":"complex","topic":"code review","pipeline":"standard","routing_reason":"误判为复杂编码需全链路规划审阅"}"#;
        let fixed = WorkflowEngine::correct_intent_json_for_user(user, wrong);
        let mut engine = test_engine_at_step(0);
        let (next, err) = engine.advance_on_output(&fixed, false);
        assert!(err.is_none());
        assert_eq!(next, Some(3));
        assert!(fixed.contains("exploring"));
    }

    #[test]
    fn exploring_plan_rejects_modify_actions() {
        let json = r#"{"plan":[{"step":1,"file":"lib.rs","action":"modify","desc":"remove mod"}]}"#;
        assert!(WorkflowEngine::validate_plan_read_only(json).is_err());
    }

    fn test_engine_at_execute_with_workflow() -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("test")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(crate::agent::workflow::create_default_workflow());
        engine
            .activate_workflow(crate::agent::workflow::DEFAULT_WORKFLOW_ID)
            .unwrap();
        session.blocking_lock().current_step_index = 3;
        engine
    }

    #[test]
    fn coding_fast_path_execute_allows_source_writes() {
        let mut engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"coding","complexity":"simple","files":["src/Foo.java"],"topic":"add validation","pipeline":"fast","routing_reason":"单文件简单改动"}"#.to_string(),
        );
        assert!(engine.allows_code_modification());
        for path in [
            "src/Foo.java",
            "lib/main.py",
            "cmd/app.go",
            "Program.cs",
            "components/App.tsx",
        ] {
            let args = serde_json::json!({"path": path, "content": "..."});
            assert!(
                engine.validate_tool_call("file_write", &args).is_ok(),
                "should allow write for {path}"
            );
        }
    }

    #[test]
    fn exploring_fast_path_execute_blocks_source_file_write() {
        let mut engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"exploring","complexity":"complex","topic":"audit","pipeline":"fast","routing_reason":"只读"}"#.to_string(),
        );
        assert!(!engine.allows_code_modification());
        for path in ["src/Foo.java", "main.py", "app.go", "lib.rs"] {
            let args = serde_json::json!({"path": path, "content": "..."});
            assert!(
                engine.validate_tool_call("file_write", &args).is_err(),
                "should block write for {path}"
            );
        }
    }

    #[test]
    fn exploring_execute_done_skips_plan_gate_in_advance_on_output() {
        let mut engine = test_engine_at_step(3);
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"exploring","complexity":"complex","topic":"audit","pipeline":"fast","routing_reason":"只读"}"#.to_string(),
        );
        let text = "## Done\n\n代码审查完成。";
        let (next, err) = engine.advance_on_output(text, false);
        assert!(next.is_none());
        assert!(err.is_none());
    }

    #[test]
    fn exploring_done_completes_workflow_despite_pending_plan_steps() {
        let mut engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"exploring","complexity":"complex","topic":"audit","pipeline":"fast","routing_reason":"只读"}"#.to_string(),
        );
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"","action":"explain","target":"","desc":"review","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(!engine.is_workflow_complete());
        let done = engine
            .try_complete_execute_on_done("## Done\n\n审查报告。")
            .unwrap();
        assert!(done);
        assert!(engine.is_workflow_complete());
    }

    #[test]
    fn chat_reply_pending_flag() {
        let engine = test_engine_at_step(0);
        assert!(!engine.is_chat_reply_pending());
        engine.set_chat_reply_pending();
        assert!(engine.is_chat_reply_pending());
        engine.clear_chat_reply_pending();
        assert!(!engine.is_chat_reply_pending());
    }

    #[test]
    fn record_execute_shell_marks_current_step() {
        let engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"","action":"modify","target":"","desc":"git commit","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(engine.record_execute_tool_success("shell_exec", r#"{"command":"git commit"}"#));
        let t = engine.get_plan_tracker().unwrap();
        assert_eq!(t.steps[0].status, crate::agent::plan_tracker::StepStatus::Done);
    }

    #[test]
    fn record_execute_load_skill_marks_current_step() {
        let engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"","action":"modify","target":"","desc":"load skill","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(engine.record_execute_tool_success("load_skill", r#"{"skill_name":"coding-workflow"}"#));
    }

    #[test]
    fn record_execute_file_write_empty_plan_file_marks_current() {
        let engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"","action":"create","target":"","desc":"new file","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(engine.record_execute_tool_success(
            "file_write",
            r#"{"path":"docs/new.md","content":"hi"}"#
        ));
    }

    #[test]
    fn record_execute_file_read_explain_marks_matching_path() {
        let engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"src/lib.rs","action":"explain","target":"","desc":"review","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(engine.record_execute_tool_success(
            "file_read",
            r#"{"path":"src/lib.rs"}"#
        ));
    }

    #[test]
    fn record_execute_read_only_search_does_not_mark() {
        let engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"src/lib.rs","action":"modify","target":"","desc":"fix","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(!engine.record_execute_tool_success("code_search", r#"{"query":"foo"}"#));
        assert!(!engine.record_execute_tool_success("file_list", r#"{"path":"."}"#));
    }

    #[test]
    fn fast_coding_done_completes_despite_pending_plan_steps() {
        let mut engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"coding","complexity":"simple","topic":"git commit","pipeline":"fast","routing_reason":"小改"}"#.to_string(),
        );
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"","action":"modify","target":"","desc":"commit","status":"pending"}],"current_index":1}"#.to_string(),
        );
        assert!(!engine.is_workflow_complete());
        let done = engine
            .try_complete_execute_on_done("## Done\n\n提交完成。")
            .unwrap();
        assert!(done);
        assert!(engine.is_workflow_complete());
    }

    #[test]
    fn coding_execute_done_blocked_when_plan_steps_pending() {
        let mut engine = test_engine_at_execute_with_workflow();
        engine.set_variable(
            "_step0_output",
            r#"{"intent":"coding","complexity":"complex","topic":"fix","pipeline":"standard","routing_reason":"多文件需全链路"}"#.to_string(),
        );
        engine.set_variable(
            "_plan_tracker",
            r#"{"steps":[{"index":1,"file":"main.rs","action":"modify","target":"","desc":"fix","status":"pending"}],"current_index":1}"#.to_string(),
        );
        let err = engine
            .try_complete_execute_on_done("## Done")
            .unwrap_err();
        assert!(err.contains("计划") || err.contains("plan") || err.contains("Done"));
        assert!(!engine.is_workflow_complete());
    }
}
