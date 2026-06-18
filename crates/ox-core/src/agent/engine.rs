use crate::agent::intervention::InterventionRequest;
use crate::agent::session::SessionState;
use crate::agent::workflow::{Workflow, WorkflowStep};
use crate::message::ToolCall;
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
            "ops" => Self::ops_fast(intent, complexity),
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

    pub fn ops_fast(intent: &str, complexity: &str) -> Self {
        Self {
            intent: intent.to_string(),
            complexity: complexity.to_string(),
            pipeline: "ops-fast".to_string(),
            skip_plan: true,
            skip_review: true,
            requires_human_confirm: true,
            steps_summary: "运维/发布 → 系统 Preflight → 人工确认 → 执行".to_string(),
        }
    }

    pub fn compute_for_request(user_text: &str, intent: &str, complexity: &str) -> Self {
        if WorkflowEngine::looks_like_ops_task(user_text) {
            Self::ops_fast(intent, complexity)
        } else {
            Self::compute(intent, complexity)
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
        if crate::agent::workflow_session::is_feedback_discuss(self) {
            return false;
        }
        let step = match self.current_step() {
            Some(s) => s,
            None => return false,
        };
        if !step.allow_code_modification {
            return false;
        }
        // Exploring fast-path lands on Execute but must stay read-only until user approves.
        if self.get_current_step_index() == 3
            && Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
                .map(|r| r.intent == "exploring")
                .unwrap_or(false)
            && !crate::agent::workflow_session::is_execute_user_approved(self)
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
                crate::agent::workflow_phases::sync_phase(self);
                Ok(true)
            }
        } else {
            Err("Failed to acquire session lock".to_string())
        }
    }

    /// Check if workflow is complete
    pub fn is_workflow_complete(&self) -> bool {
        if crate::agent::workflow_session::is_parked(self) {
            return false;
        }
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

    /// Clear per-round ephemeral workflow state (keeps step index and user request).
    pub fn clear_ephemeral_workflow_state(&mut self) {
        if self.current_workflow.is_none() {
            return;
        }
        if let Ok(mut session) = self.session_state.try_lock() {
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
            session.set_variable("_clarification_questions", "");
            session.set_variable("_await_clarification", "");
            session.set_variable("_clarification_pending_advance", "");
            session.set_variable("_clarification_kind", "");
            session.set_variable("_park_disambiguation_input", "");
            session.set_variable("_park_follow_up_stage", "");
            session.set_variable("_park_detail_kind", "");
            session.set_variable("_workflow_guidance", "[]");
            session.set_variable("_execute_report_delivered", "");
            crate::agent::workflow_session::clear_session_flags(self);
            crate::agent::execute_handoff::ExecuteHandoff::clear(self);
            crate::agent::perception::clear(self);
            crate::agent::workflow_phases::clear_phase(self);
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
            tracing::warn!("Failed to acquire session lock for ephemeral workflow clear");
        }
    }

    /// Reset workflow to first step (clears ephemeral state for a new user round).
    pub fn reset_workflow(&mut self) {
        if self.current_workflow.is_some() {
            self.clear_ephemeral_workflow_state();
            if let Ok(mut session) = self.session_state.try_lock() {
                session.current_step_index = 0;
                session.set_variable("_round_finalized", "");
                session.set_variable("_round_interrupted", "");
            } else {
                tracing::warn!("Failed to acquire session lock for workflow reset");
            }
        }
    }

    /// Start a new user round: archive previous, reset workflow, set current request.
    pub fn begin_user_round(&mut self, user_message: &str) -> bool {
        crate::agent::user_round::begin_user_round(self, user_message)
    }

    pub fn user_round_memory_block(&self) -> String {
        crate::agent::user_round::format_user_round_block(self)
    }

    /// Suspend after Ctrl+C — preserves step outputs for resume.
    pub fn suspend_on_interrupt(&mut self) -> bool {
        crate::agent::user_round::suspend_on_interrupt(self)
    }

    /// Archive interrupted work when exiting the program.
    pub fn finalize_interrupted_on_exit(&mut self) {
        crate::agent::user_round::finalize_interrupted_on_exit(self);
    }

    /// True when a new user message should correct the current workflow, not restart Intent.
    pub fn workflow_preserves_on_user_input(&self) -> bool {
        if crate::agent::workflow_session::is_parked(self) {
            return true;
        }
        if !self.is_workflow_active() || self.is_workflow_complete() {
            return false;
        }
        // Act 阶段（非 park）不接受中途开放式介入
        if crate::agent::workflow_phases::get_phase(self)
            == crate::agent::workflow_phases::WorkflowPhase::Act
        {
            return false;
        }
        self.get_current_step_index() > 0
            || self.is_awaiting_execute_confirmation()
            || self.is_current_step_waiting_confirmation()
            || self
                .get_variable("_step0_output")
                .is_some_and(|s| !s.trim().is_empty())
    }

    pub fn append_workflow_guidance(&self, text: &str) {
        crate::agent::workflow_guidance::append(self, text);
    }

    pub fn workflow_guidance_block(&self) -> String {
        crate::agent::workflow_guidance::format_block(self)
    }

    pub fn clear_workflow_guidance(&self) {
        crate::agent::workflow_guidance::clear(self);
    }

    pub fn is_workflow_parked(&self) -> bool {
        crate::agent::workflow_session::is_parked(self)
    }

    pub fn park_workflow_awaiting_user(&mut self) -> Result<(), String> {
        Ok(())
    }

    pub fn unpark_workflow(&self) {
        crate::agent::workflow_session::unpark(self);
    }

    /// Resume a parked session from user follow-up (stay on Execute, no Plan re-explore).
    pub fn resume_parked_workflow(&self, user_text: &str) {
        crate::agent::workflow_session::clear_feedback_discuss(self);
        crate::agent::workflow_session::unpark(self);
        self.append_workflow_guidance(user_text);
        if crate::agent::workflow_session::looks_like_implementation_request(user_text) {
            crate::agent::workflow_session::enter_implementation_phase(self);
            self.bootstrap_implementation_plan();
        } else if crate::agent::workflow_session::looks_like_workflow_continuation(user_text) {
            crate::agent::workflow_session::mark_execute_approved(self);
        }
        self.set_variable("_turn_memory", String::new());
        self.clear_execute_report_delivered();
        self.clear_execute_confirmation();
    }

    pub fn adopt_execute_interjection(&self, user_text: &str) {
        self.append_workflow_guidance(user_text);
        if crate::agent::workflow_session::looks_like_implementation_request(user_text) {
            crate::agent::workflow_session::enter_implementation_phase(self);
            self.clear_execute_report_delivered();
            self.bootstrap_implementation_plan();
        }
    }

    /// Build plan tracker from parked review report; reset per-file read ledger.
    pub fn bootstrap_implementation_plan(&self) {
        crate::agent::workflow_phases::set_phase(self, crate::agent::workflow_phases::WorkflowPhase::Act);

        if let Some(findings) = crate::agent::perception::load(self) {
            let tracker = crate::agent::perception::to_plan_tracker(&findings);
            tracing::info!(
                "[IMPL] Loaded {} steps from frozen findings",
                tracker.steps.len()
            );
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
            self.clear_impl_files_read();
            return;
        }

        let report = self
            .get_execute_review_report()
            .or_else(|| self.get_variable("_step3_output"));
        let Some(report) = report.filter(|s| !s.trim().is_empty()) else {
            return;
        };
        if let Some(tracker) = crate::agent::plan_tracker::load_from_review_report(&report) {
            tracing::info!(
                "[IMPL] Loaded {} implementation steps from review report",
                tracker.steps.len()
            );
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
            self.clear_impl_files_read();
        }
    }

    pub fn bootstrap_implementation_plan_from_findings(&self) {
        if let Some(store) = crate::agent::findings::load_or_migrate(self) {
            let only_scoped = !store.active_indices.is_empty();
            let tracker = store.to_plan_tracker(only_scoped);
            self.set_variable(
                "_plan_tracker",
                crate::agent::plan_tracker::tracker_to_json(&tracker),
            );
            self.clear_impl_files_read();
            return;
        }
        self.bootstrap_implementation_plan();
    }

    pub fn sync_plan_from_findings(&self) {
        self.bootstrap_implementation_plan_from_findings();
    }

    /// Re-open workflow after premature ## Done or verify failure.
    pub fn reopen_execute_for_fixes(&mut self, user_text: &str) -> bool {
        if !crate::agent::workflow_session::looks_like_fix_continuation(user_text) {
            return false;
        }
        let had_findings = crate::agent::findings::load_or_migrate(self).is_some();
        let verify_failed =
            crate::agent::post_edit_verification::verify_status_failed(self);
        if !self.is_workflow_complete() && !verify_failed && !had_findings {
            return false;
        }
        self.set_variable(crate::agent::user_round::ROUND_FINALIZED_KEY, String::new());
        if let Ok(mut session) = self.session_state.try_lock() {
            session.current_step_index = 0;
            session.awaiting_user_confirmation = false;
        }
        self.append_workflow_guidance(user_text);
        self.sync_plan_from_findings();
        true
    }

    pub fn has_file_read_snapshot(&self, path: &str) -> bool {
        crate::agent::exploration_snapshot::find_file_read_entry(
            &self.get_exploration_entries(),
            path,
        )
        .is_some()
    }

    pub fn shell_looks_like_file_read(cmd: &str) -> bool {
        let lower = cmd.to_lowercase();
        ["cat ", "type ", "head ", "tail ", "more ", "less ", "get-content"]
            .iter()
            .any(|p| lower.contains(p))
    }

    /// Freeze structured perception at end of perceive phase (park / review complete).
    pub fn freeze_perception_output(&self, output: &str) {
        crate::agent::perception::freeze_from_output(self, output);
        crate::agent::workflow_phases::set_phase(
            self,
            crate::agent::workflow_phases::WorkflowPhase::Think,
        );
    }

    const IMPL_READ_KEY: &str = "_impl_files_read";

    pub fn clear_impl_files_read(&self) {
        self.set_variable(Self::IMPL_READ_KEY, "[]".to_string());
    }

    fn impl_files_read_set(&self) -> std::collections::HashSet<String> {
        self.get_variable(Self::IMPL_READ_KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn impl_file_already_read(&self, path: &str) -> bool {
        let norm = crate::agent::plan_tracker::normalize_path(path);
        self.impl_files_read_set().contains(&norm)
    }

    pub fn record_impl_file_read(&self, path: &str, _arguments: &str) {
        let norm = crate::agent::plan_tracker::normalize_path(path);
        let mut set = self.impl_files_read_set();
        if set.insert(norm) {
            if let Ok(json) = serde_json::to_string(&set) {
                self.set_variable(Self::IMPL_READ_KEY, json);
            }
        }
    }

    /// Implementation phase: one file_read per path; next tool must be edit.
    pub fn validate_impl_file_read(&self, path: &str) -> Result<(), String> {
        if !crate::agent::workflow_session::is_implementation_phase(self) {
            return Ok(());
        }
        if path.trim().is_empty() {
            return Ok(());
        }
        if self.impl_file_already_read(path) {
            return Err(format!(
                "实施阶段 `{path}` 已读过（每文件最多 1 次 file_read）。\
                 请直接对该文件 edit_file / file_write；内容见上一条 ToolResult。"
            ));
        }
        Ok(())
    }

    pub fn impl_edit_nudge_after_read(&self, path: &str, _preview: &str) -> Option<String> {
        if !crate::agent::workflow_session::is_implementation_phase(self) {
            return None;
        }
        let tracker = self.get_plan_tracker()?;
        let step = tracker.step_for_path(path)?;
        Some(format!(
            "⚡ `{path}` 已读取（实施阶段仅此一次）。**立即**对步骤 {} 执行 edit_file 或 file_write：{}\n\
             禁止再次 file_read 同一文件；完成后进入下一项。",
            step.index, step.desc
        ))
    }

    pub fn implementation_execute_prompt_addon(&self) -> String {
        let progress = self.plan_progress_summary();
        let mut parts = vec![
            "【实施阶段规则 — 覆盖上方只读/禁止重读规则】".to_string(),
            "1. 严格按下方【计划进度】清单逐项修改，做完一项再下一项".to_string(),
            "2. 每个源文件最多 file_read **1 次**；读后**下一个 tool 必须是** edit_file 或 file_write".to_string(),
            "3. 禁止空转「需要先读取」；禁止重复输出审查报告".to_string(),
            "4. 全部清单项完成后输出 ## Done".to_string(),
        ];
        if !progress.is_empty() {
            parts.push(progress);
        }
        if let Some(report) = self.get_execute_review_report() {
            let snippet: String = report.chars().take(4000).collect();
            parts.push(format!("【审查报告摘要】\n{snippet}"));
        }
        parts.join("\n")
    }

    pub fn should_skip_execute_confirmation(&self, from_step: usize, target_step: usize) -> bool {
        if target_step != 3 {
            return false;
        }
        // Intent → Execute fast path for read-only exploring: no extra confirm gate
        if from_step == 0 {
            if let Some(r) = Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref()) {
                return r.intent == "exploring" && r.pipeline == "fast";
            }
        }
        false
    }

    pub fn finish_workflow_session(&mut self) -> Result<(), String> {
        crate::agent::workflow_session::clear_session_flags(self);
        self.complete_workflow()
    }

    pub fn looks_like_workflow_continuation(user_text: &str) -> bool {
        crate::agent::workflow_session::looks_like_workflow_continuation(user_text)
    }

    pub fn looks_like_new_task(user_text: &str) -> bool {
        crate::agent::workflow_session::looks_like_new_task(user_text)
    }

    pub fn allows_midflight_interjection(&self) -> bool {
        crate::agent::workflow_phases::allows_midflight_interjection(self)
    }

    pub fn accepts_user_round_input(&self, user_text: &str) -> bool {
        crate::agent::workflow_phases::accepts_user_round_input(self, user_text)
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
                if prompt.contains("{EXECUTE_HANDOFF}") {
                    let block = crate::agent::execute_handoff::ExecuteHandoff::load(self)
                        .map(|h| h.format_for_execute())
                        .unwrap_or_else(|| "（无交接包 — 按前序输出执行）".to_string());
                    prompt = prompt.replace("{EXECUTE_HANDOFF}", &block);
                }
                if prompt.contains("{USER_GUIDANCE}") {
                    let block = self.workflow_guidance_block();
                    prompt = prompt.replace(
                        "{USER_GUIDANCE}",
                        &if block.is_empty() {
                            "（无用户补充说明）".to_string()
                        } else {
                            block
                        },
                    );
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
                if prompt.contains("{WORKFLOW_PHASE}") {
                    prompt = prompt.replace(
                        "{WORKFLOW_PHASE}",
                        &crate::agent::workflow_phases::phase_prompt_addon(self),
                    );
                } else {
                    let phase_addon = crate::agent::workflow_phases::phase_prompt_addon(self);
                    if !phase_addon.is_empty() {
                        prompt.push_str("\n\n");
                        prompt.push_str(&phase_addon);
                    }
                }
                if prompt.contains("{FINDINGS_SCHEMA}") {
                    let schema = if self.is_perceive_execute() {
                        crate::agent::workflow_phases::FINDINGS_JSON_SCHEMA
                    } else {
                        ""
                    };
                    prompt = prompt.replace("{FINDINGS_SCHEMA}", schema);
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

        if tool_name == "file_read" {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                self.validate_impl_file_read(path)?;
            }
        }

        crate::agent::workflow_phases::validate_act_tool(self, tool_name)?;
        crate::agent::workflow_session::validate_feedback_discuss_tool(self, tool_name)?;

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

    /// Single-step model (index 0) or legacy execute step (index 3).
    pub fn is_task_step(&self) -> bool {
        matches!(self.get_current_step_index(), 0 | 3)
    }

    pub fn current_step_display_label(&self) -> Option<String> {
        self.get_current_step_info()
            .map(|s| format!("{} ({}/{})", s.name, s.current_step, s.total_steps))
    }

    pub fn interjection_should_resume_turn(&self, user_text: &str) -> bool {
        if self.workflow_preserves_on_user_input() {
            return true;
        }
        if crate::agent::workflow_session::looks_like_fix_continuation(user_text) {
            return self.is_workflow_complete()
                || crate::agent::post_edit_verification::verify_status_failed(self);
        }
        false
    }

    pub fn apply_workflow_command(
        &mut self,
        input: &str,
        working_dir: Option<&std::path::Path>,
    ) -> Option<crate::agent::workflow_command::CommandOutcome> {
        let cmd = crate::agent::workflow_command::parse(input)?;
        Some(crate::agent::workflow_command::apply_with_cwd(
            self, cmd, working_dir,
        ))
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
        crate::agent::user_round::finalize_completed_round(self);
        self.clear_workflow_guidance();
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
    pub fn validate_intent_pipeline(
        assistant_text: &str,
        user_text: Option<&str>,
    ) -> Result<IntentRouting, String> {
        Self::parse_intent_output(assistant_text, user_text).map(|p| p.routing)
    }

    /// Full intent parse including optional requirement-clarification gate.
    pub fn parse_intent_output(
        assistant_text: &str,
        user_text: Option<&str>,
    ) -> Result<crate::agent::requirement_clarification::IntentParseResult, String> {
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
        let canonical = match user_text {
            Some(u) => IntentRouting::compute_for_request(u, intent, complexity),
            None => IntentRouting::compute(intent, complexity),
        };
        if pipeline != canonical.pipeline {
            return Err(format!(
                "❌ pipeline 应为 \"{}\"（{}），你填了 \"{}\"。\n请根据 intent={intent} complexity={complexity} 修正 pipeline 与 routing_reason。"
                ,
                canonical.pipeline, canonical.steps_summary, pipeline
            ));
        }
        let (needs_clarification, clarification_questions) =
            crate::agent::requirement_clarification::extract_clarification(&v);
        Ok(crate::agent::requirement_clarification::IntentParseResult {
            routing: canonical,
            needs_clarification,
            clarification_questions,
        })
    }

    pub fn is_awaiting_clarification(&self) -> bool {
        crate::agent::requirement_clarification::is_awaiting(self)
    }

    pub fn clarification_markdown(&self) -> String {
        crate::agent::requirement_clarification::format_markdown(self)
    }

    pub fn is_park_disambiguation_awaiting(&self) -> bool {
        crate::agent::requirement_clarification::is_park_disambiguation(self)
    }

    pub fn arm_park_follow_up_menu(&self) {
        crate::agent::requirement_clarification::arm_park_follow_up_menu(self);
    }

    pub fn arm_park_disambiguation(&self, _pending_input: &str) {
        self.arm_park_follow_up_menu();
    }

    pub fn finish_park_disambiguation(
        &self,
        answer: &str,
    ) -> Result<crate::agent::requirement_clarification::ParkFollowUpOutcome, String> {
        crate::agent::requirement_clarification::resolve_park_follow_up(self, answer)
    }

    /// Resume parked session for feedback / clarification (no implementation phase).
    pub fn resume_parked_feedback(&self, user_text: &str) {
        crate::agent::workflow_session::unpark(self);
        self.append_workflow_guidance(user_text);
        crate::agent::workflow_session::enter_feedback_discuss(self);
        crate::agent::workflow_session::mark_execute_approved(self);
        self.set_variable("_turn_memory", String::new());
        self.clear_execute_report_delivered();
        self.clear_execute_confirmation();
    }

    /// Apply park follow-up choice; returns resume block for Continue / Feedback.
    pub fn apply_park_disambiguation_resolution(
        &mut self,
        resolution: crate::agent::requirement_clarification::ParkDisambiguationResolution,
    ) -> Option<String> {
        use crate::agent::requirement_clarification::ParkDisambiguationResolution;
        let prior = self
            .get_variable("_step3_output")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.get_previous_step_output());
        match resolution {
            ParkDisambiguationResolution::ContinuePrevious { follow_up } => {
                self.resume_parked_workflow(&follow_up);
                Some(crate::agent::workflow_session::resume_message(
                    &follow_up,
                    prior.as_deref(),
                ))
            }
            ParkDisambiguationResolution::Feedback { text } => {
                self.resume_parked_feedback(&text);
                let block = format!(
                    "[TASK_SESSION_RESUME — 用户对审查结论发表意见/澄清；请基于审查报告回应，**勿**从 Intent/Plan 重来]\n{text}"
                );
                if let Some(out) = prior.as_deref() {
                    Some(format!(
                        "{block}\n\n【审查报告摘要】\n{}",
                        out.chars().take(8000).collect::<String>()
                    ))
                } else {
                    Some(block)
                }
            }
            ParkDisambiguationResolution::NewTask { task } => {
                let _ = self.finish_workflow_session();
                self.begin_user_round(&task);
                None
            }
        }
    }

    pub fn apply_clarification_answer(&self, answer: &str) {
        crate::agent::requirement_clarification::apply_answer(self, answer);
    }

    pub fn clear_clarification_gate(&self) {
        crate::agent::requirement_clarification::clear_gate(self);
    }

    pub fn clarification_pending_advance(&self) -> usize {
        crate::agent::requirement_clarification::pending_advance_step(self)
    }

    /// After user answers Intent clarification: advance workflow to Plan (1) or Execute (3).
    pub fn finish_clarification_and_advance(&mut self, answer: &str) -> Result<usize, String> {
        crate::agent::requirement_clarification::validate_intent_clarification_answer(answer)?;
        self.apply_clarification_answer(answer);
        let target = self.clarification_pending_advance();
        self.clear_clarification_gate();
        let _ = self.advance_to_step(Some(target));
        if target == 3 {
            if let Some(intent_out) = self.get_variable("_step0_output") {
                self.prepare_fast_path_execute(&intent_out);
            }
        }
        Ok(target)
    }

    pub fn build_execute_confirmation_markdown(&self, review_text: &str, from_step: usize) -> String {
        const HINT: &str = "\
---\n\n\
> **审阅已完成 — 请确认后执行**\n\
> - 输入修改意见 → 回到规划重新生成\n\
> - 输入 `ok` / `继续` / `确认` → 开始执行";
        let plan_raw = self
            .get_variable("_step1_output")
            .unwrap_or_else(|| self.get_previous_step_output().unwrap_or_default());
        let plan_md = Self::format_step_output_for_confirm(1, &plan_raw, "📋 任务规划");
        let review_md = if from_step == 2 {
            Self::format_step_output_for_confirm(2, review_text, "🛡️ 审阅计划")
        } else if from_step == 0 {
            let exploring = Self::parse_intent_meta(self.get_variable("_step0_output").as_deref())
                .map(|(intent, _)| intent == "exploring")
                .unwrap_or(false);
            if exploring {
                "✅ **只读检查快速路径** — 跳过规划/审阅，确认后直接探索并输出分析".to_string()
            } else {
                "✅ **快速路径** — 简单改动跳过规划/审阅，直接进入人工确认".to_string()
            }
        } else {
            "✅ **自动审阅已跳过**（只读检查或简单编码任务）".to_string()
        };
        format!("{plan_md}\n\n---\n\n{review_md}\n\n{HINT}")
    }

    fn format_step_output_for_confirm(step_idx: usize, raw: &str, title: &str) -> String {
        if raw.trim().is_empty() {
            return format!("### {title}\n\n（无输出）");
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(
            &extract_json_block(raw).unwrap_or_else(|| raw.to_string()),
        ) {
            let pretty = serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_string());
            let snippet: String = pretty.chars().take(4000).collect();
            format!("### {title}\n\n```json\n{snippet}\n```")
        } else {
            let snippet: String = raw.chars().take(4000).collect();
            format!("### {title}\n\n{snippet}")
        }
    }

    pub fn arm_execute_confirmation_with_markdown(&self, markdown: &str, from_step: usize) {
        let handoff = crate::agent::execute_handoff::ExecuteHandoff::freeze(
            self,
            markdown,
            from_step,
            false,
        );
        handoff.save(self);
        self.arm_execute_confirmation();
    }

    pub fn intent_routing_from_text(step0: Option<&str>) -> Option<IntentRouting> {
        let (intent, complexity) = Self::parse_intent_meta(step0)?;
        Some(IntentRouting::compute(&intent, &complexity))
    }

    /// User text can force ops-fast even when step0 says coding/simple/fast.
    pub fn effective_routing(user_text: &str, step0: Option<&str>) -> Option<IntentRouting> {
        if Self::looks_like_ops_task(user_text) {
            let (intent, complexity) =
                Self::parse_intent_meta(step0).unwrap_or(("ops".into(), "simple".into()));
            return Some(IntentRouting::ops_fast(&intent, &complexity));
        }
        Self::intent_routing_from_text(step0)
    }

    pub fn effective_routing_for_engine(&self) -> Option<IntentRouting> {
        let user = self.get_variable("_current_user_request").unwrap_or_default();
        Self::effective_routing(&user, self.get_variable("_step0_output").as_deref())
    }

    /// Git tag / release / push — route to ops-fast, not file-edit fast path.
    pub fn looks_like_ops_task(user_text: &str) -> bool {
        let t = user_text.trim();
        if t.is_empty() {
            return false;
        }
        let lower = t.to_lowercase();
        [
            "git tag",
            "打 tag",
            "打tag",
            "tag ",
            "release",
            "发布",
            "push",
            "推送",
            "deploy",
            "部署",
            "changelog",
            "版本号",
            "发版",
            "git push",
        ]
        .iter()
        .any(|k| t.contains(k) || lower.contains(k))
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
        if Self::looks_like_ops_task(user_text) {
            if let Ok(r) = Self::validate_intent_pipeline(assistant_text, Some(user_text)) {
                if r.pipeline == "ops-fast" {
                    return assistant_text.to_string();
                }
            }
            let topic = extract_json_block(assistant_text)
                .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                .and_then(|v| v.get("topic").and_then(|t| t.as_str()).map(String::from))
                .unwrap_or_else(|| user_text.chars().take(80).collect());
            tracing::info!("[INTENT] Ops/release request — auto-correcting to ops-fast");
            return serde_json::json!({
                "intent": "ops",
                "complexity": "simple",
                "files": [],
                "topic": topic,
                "pipeline": "ops-fast",
                "routing_reason": "用户请求为 git tag/发布/推送类运维操作，使用 ops-fast：系统 Preflight 探测后人工确认再执行"
            })
            .to_string();
        }
        if !Self::looks_like_read_only_audit(user_text) {
            return assistant_text.to_string();
        }
        if Self::validate_intent_pipeline(assistant_text, Some(user_text)).is_ok() {
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
    pub fn build_fast_path_plan(intent_text: &str, user_request: Option<&str>) -> Option<String> {
        let json = extract_json_block(intent_text)?;
        let v: serde_json::Value = serde_json::from_str(&json).ok()?;
        let intent = v.get("intent").and_then(|t| t.as_str()).unwrap_or("coding");
        let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("任务");
        let user_text = user_request.unwrap_or(topic);

        if intent == "ops" || Self::looks_like_ops_task(user_text) {
            return Some(Self::build_ops_fast_plan(topic, user_text));
        }

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

    fn build_ops_fast_plan(topic: &str, user_text: &str) -> String {
        serde_json::json!({
            "structure_summary": format!("运维/发布任务：{topic}"),
            "probes": [
                {
                    "id": "git_tags",
                    "command": "git tag -l --sort=-v:refname",
                    "purpose": "现有 tag 列表（确认命名规则与最新版本）"
                },
                {
                    "id": "git_head",
                    "command": "git rev-parse --short HEAD",
                    "purpose": "当前 HEAD 提交"
                },
                {
                    "id": "git_status",
                    "command": "git status -sb",
                    "purpose": "工作区与分支状态"
                }
            ],
            "plan": [{
                "step": 1,
                "action": "shell",
                "command": "",
                "desc": format!("按用户请求执行：{user_text}"),
                "verify": "命令成功且 ## Done"
            }],
            "skills": [],
            "key_files": []
        })
        .to_string()
    }

    /// Seed synthetic plan + tracker when jumping Intent → Execute.
    pub fn prepare_fast_path_execute(&self, intent_output: &str) {
        let user = self.get_variable("_current_user_request");
        if let Some(plan) = Self::build_fast_path_plan(intent_output, user.as_deref()) {
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

    /// Update plan tracker after a successful task-step tool call.
    pub fn record_execute_tool_success(
        &self,
        tool_name: &str,
        arguments: &str,
        result_content: &str,
    ) -> (bool, Option<String>) {
        if !self.is_task_step() {
            return (false, None);
        }
        let args: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);

        let changed = match tool_name {
            "file_write" | "edit_file" | "delete_range" => args
                .get("path")
                .and_then(|p| p.as_str())
                .map(|path| self.try_mark_plan_step_done(path))
                .unwrap_or(false),
            "file_read" => {
                let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                if path.is_empty() {
                    false
                } else {
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
            }
            "git_diff" => {
                if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                    if self.try_mark_plan_step_done(path) {
                        true
                    } else {
                        self.try_mark_plan_current_step_done()
                    }
                } else {
                    self.try_mark_plan_current_step_done()
                }
            }
            "shell_exec" | "load_skill" | "git_status" => self.try_mark_plan_current_step_done(),
            _ => false,
        };

        let hint = if tool_name == "edit_file"
            && (result_content.contains("Syntax error") || result_content.contains("AST Syntax"))
        {
            Some(
                "⚠️ 编辑可能未通过语法检查 — 请修复后重试，勿标记为完成。".to_string(),
            )
        } else {
            None
        };
        (changed, hint)
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
        if text.contains("## Done") || text.contains("【Done】") {
            return true;
        }
        if text.contains("## 完成") || text.contains("【完成】") {
            return true;
        }
        // Trailing completion line (common in Chinese reviews)
        if let Some(last) = text.lines().map(str::trim).filter(|l| !l.is_empty()).last() {
            if last == "完成"
                || last == "**完成**"
                || last.ends_with("完成。")
                || last.contains("审查完成")
                || last.contains("检查完成")
            {
                return true;
            }
        }
        false
    }

    /// Structured code-review report without explicit ## Done (exploring execute).
    pub fn looks_like_review_report(text: &str) -> bool {
        let t = text.trim();
        if t.chars().count() < 180 {
            return false;
        }
        let markers = [
            "优先级",
            "建议",
            "问题",
            "审查",
            "风险",
            "结论",
            "High",
            "Medium",
            "Low",
            "| ---",
            "| 高",
            "| 中",
            "| 低",
            "改进",
            "缺陷",
        ];
        let hits = markers.iter().filter(|m| t.contains(*m)).count();
        (hits >= 2 && (t.contains("完成") || t.contains("Done")))
            || (hits >= 3)
    }

    fn is_explaining_execute(&self) -> bool {
        self.is_perceive_execute()
    }

    /// Execute step in read-only perceive mode — disabled in single-step model.
    pub fn is_perceive_execute(&self) -> bool {
        false
    }

    /// Whether Execute output should park — disabled in single-step model.
    pub fn should_park_execute_output(&self, _text: &str) -> bool {
        false
    }

    /// Run gatekeeper pipeline when the model signals ## Done.
    pub fn run_done_gates(
        &self,
        text: &str,
        had_code_changes: bool,
    ) -> crate::agent::gatekeeper::GateReport {
        crate::agent::gatekeeper::standard_pipeline().run(&crate::agent::gatekeeper::GateCtx {
            engine: self,
            assistant_text: text,
            touched_files: &[],
            had_code_changes,
        })
    }

    /// Standard coding workflow: ## Done → gatekeeper must pass.
    pub fn should_finish_execute_workflow(&self, text: &str) -> bool {
        if !Self::text_signals_done(text) || self.is_workflow_complete() {
            return false;
        }
        matches!(
            self.run_done_gates(text, false),
            crate::agent::gatekeeper::GateReport::Pass
        )
    }

    pub fn mark_execute_report_delivered(&self) {
        self.set_variable("_execute_report_delivered", "1".to_string());
    }

    pub fn execute_report_already_delivered(&self) -> bool {
        if crate::agent::workflow_session::is_implementation_phase(self) {
            return false;
        }
        if self.get_variable("_execute_report_delivered").as_deref() == Some("1") {
            return true;
        }
        // Prior review in _step3_output — only block re-explore while still in read-only phase.
        self.get_variable("_step3_output")
            .is_some_and(|s| Self::looks_like_review_report(&s))
    }

    /// Block file_read/code_search after a review report (read-only execute phase only).
    pub fn should_block_execute_reexplore(&self, tool_calls: &[ToolCall], assistant_text: &str) -> bool {
        if !tool_calls.is_empty()
            && Self::looks_like_review_report(assistant_text)
        {
            self.mark_execute_report_delivered();
        }
        if crate::agent::workflow_session::is_implementation_phase(self) {
            return false;
        }
        (self.execute_report_already_delivered()
            || self.should_park_execute_output(assistant_text))
            && Self::tool_calls_are_reexplore_only(tool_calls)
    }

    pub fn clear_execute_report_delivered(&self) {
        self.set_variable("_execute_report_delivered", String::new());
    }

    /// Cache lookup for Execute-step read tools (snapshot + explored paths).
    pub fn lookup_execute_exploration_cache(
        &self,
        working_dir: &std::path::Path,
        tool: &str,
        arguments: &str,
    ) -> Option<String> {
        let target = crate::agent::exploration_snapshot::target_from_tool_args(tool, arguments);
        if tool == "file_read" {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) {
                if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                    let entries = self.get_exploration_entries();
                    if crate::agent::exploration_snapshot::find_file_read_entry(&entries, path)
                        .is_some()
                        || self.is_path_explored("file_read", path)
                    {
                        return Some(crate::agent::exploration_snapshot::resolve_file_read_cache(
                            working_dir,
                            &entries,
                            path,
                            arguments,
                        ));
                    }
                }
            }
        }
        if let Some(hit) = self.lookup_exploration_cache(working_dir, tool, &target) {
            return Some(hit);
        }
        if matches!(tool, "code_search" | "find_symbol" | "file_search") {
            if self.is_path_explored(tool, &target) {
                return self.lookup_exploration_cache(working_dir, tool, &target);
            }
        }
        None
    }

    pub fn tool_calls_are_reexplore_only(tool_calls: &[ToolCall]) -> bool {
        !tool_calls.is_empty()
            && tool_calls.iter().all(|tc| {
                matches!(
                    tc.name.as_str(),
                    "file_read" | "file_list" | "code_search" | "find_symbol" | "file_search"
                )
            })
    }

    /// Fast/exploring paths use synthetic plans — shell/git may not map to file_write markers.
    pub fn should_skip_plan_done_gate(&self) -> bool {
        Self::intent_routing_from_text(self.get_variable("_step0_output").as_deref())
            .map(|r| r.intent == "exploring" || r.pipeline == "fast" || r.pipeline == "ops-fast")
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

    /// Execute step output complete — park session for user follow-up (no workflow teardown).
    pub fn try_complete_execute_on_done(&mut self, assistant_text: &str) -> Result<bool, String> {
        if self.get_current_step_index() != 3 || !self.should_park_execute_output(assistant_text) {
            return Ok(false);
        }
        if Self::looks_like_review_report(assistant_text) {
            self.mark_execute_report_delivered();
        }
        if self.is_perceive_execute() {
            self.freeze_perception_output(assistant_text);
        }
        if self.should_skip_plan_done_gate() {
            self.mark_plan_all_done();
        } else if let Some(msg) = self.check_plan_done_gate() {
            return Err(msg);
        }
        self.park_workflow_awaiting_user()?;
        self.clear_done_gate_blocks();
        Ok(true)
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

    /// Store preflight probe output (before execute confirmation).
    pub fn record_preflight_result(
        &self,
        working_dir: &std::path::Path,
        target: &str,
        raw_result: &str,
    ) {
        let content = crate::agent::exploration_snapshot::extract_data_content(raw_result);
        let mut entries = self.get_exploration_entries();
        crate::agent::exploration_snapshot::merge_entry(
            &mut entries,
            working_dir,
            "preflight",
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
    pub fn lookup_exploration_cache(
        &self,
        working_dir: &std::path::Path,
        tool: &str,
        target: &str,
    ) -> Option<String> {
        if tool == "file_read" {
            let path = crate::agent::exploration_snapshot::file_path_from_target(target);
            let entries = self.get_exploration_entries();
            if crate::agent::exploration_snapshot::find_file_read_entry(&entries, path).is_some() {
                let args = serde_json::json!({ "path": path }).to_string();
                return Some(crate::agent::exploration_snapshot::resolve_file_read_cache(
                    working_dir,
                    &entries,
                    path,
                    &args,
                ));
            }
        }

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

    /// Full review report from Execute (exploring fast-path), stored at park time.
    pub fn get_execute_review_report(&self) -> Option<String> {
        self.get_variable("_step3_output")
            .filter(|s| !s.trim().is_empty())
            .filter(|s| Self::looks_like_review_report(s))
    }

    /// Durable block for LLM — park 前的审查报告（实施阶段每轮注入）。
    pub fn execute_review_report_block(&self, max_chars: usize) -> Option<String> {
        self.get_execute_review_report().map(|report| {
            let snippet: String = report.chars().take(max_chars).collect();
            format!("【审查报告 — park 前输出，用户在此基础上跟进】\n{snippet}")
        })
    }

    /// Build an aggregated summary of all previous steps for the Execute step
    pub fn get_all_step_outputs_summary(&self) -> String {
        let mut summaries = Vec::new();
        let labels = ["意图分类", "任务规划", "审阅计划"];
        for i in 0..3 {
            if let Some(output) = self.get_variable(&format!("_step{}_output", i)) {
                if output.trim().is_empty() {
                    continue;
                }
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
        if let Some(report) = self.get_execute_review_report() {
            let snippet: String = report.chars().take(6000).collect();
            summaries.push(format!("Step 4: 只读审查报告\n{snippet}"));
        }
        if summaries.is_empty() {
            "（无上一步输出）".to_string()
        } else {
            summaries.join("\n\n")
        }
    }

    /// Snapshot task + step outputs for skill reflection before workflow reset.
    pub fn snapshot_for_skill_reflect(&self) -> (String, String) {
        let task_description = self
            .get_variable("_current_user_request")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Unknown task".to_string());
        let summary = self.get_all_step_outputs_summary();
        let execution_summary = if summary == "（无上一步输出）" {
            String::new()
        } else {
            summary
        };
        (task_description, execution_summary)
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
                let user = self.get_variable("_current_user_request");
                match Self::parse_intent_output(assistant_text, user.as_deref()) {
                    Ok(parsed) => {
                        if parsed.routing.intent == "chat" {
                            self.set_chat_route();
                            return (None, None);
                        }
                        self.set_previous_output(assistant_text);
                        if parsed.needs_clarification {
                            let pending = if parsed.routing.skip_plan { 3 } else { 1 };
                            crate::agent::requirement_clarification::arm_gate(
                                self,
                                &parsed.clarification_questions,
                                pending,
                            );
                            return (None, None);
                        }
                        if parsed.routing.skip_plan {
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

