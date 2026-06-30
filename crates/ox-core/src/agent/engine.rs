use crate::agent::intervention::InterventionRequest;
use crate::agent::session::SessionState;
use crate::agent::workflow::{Workflow, WorkflowStep};
use crate::message::ToolCall;
use std::sync::Arc;

/// Workflow Engine — single-step agent + gatekeeper
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
        let resolved_id = if workflow_id == "four_step_pipeline" {
            crate::agent::workflow::DEFAULT_WORKFLOW_ID
        } else {
            workflow_id
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

    /// Session id for L0 working-memory anchoring.
    pub fn session_id(&self) -> String {
        self.session_state
            .try_lock()
            .map(|s| s.session_id.clone())
            .unwrap_or_else(|_| "default".to_string())
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

    /// Check if tool execution is allowed in current step.
    /// When scope is pending (business gate), still allow read-only tools
    /// so the LLM can file_read during discussion. Write tools blocked separately.
    pub fn allows_tool_execution(&self) -> bool {
        if self.is_single_step() {
            // During scope confirmation, allow tool execution (schema will be filtered
            // to read-only by unified_action's allowed_actions_for_engine).
            // Write/edit tools are blocked individually in validate_single_step_tool.
            return matches!(
                crate::agent::phase::get(self),
                crate::agent::phase::SingleFlowPhase::Receive
                    | crate::agent::phase::SingleFlowPhase::Review
                    | crate::agent::phase::SingleFlowPhase::Implement
                    | crate::agent::phase::SingleFlowPhase::AwaitUser
            );
        }
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
        if self.is_single_step() {
            return crate::agent::business_gate::scope_implementation_unlocked(self);
        }
        self.current_step()
            .map(|s| s.allow_code_modification)
            .unwrap_or(false)
    }

    /// True when running the default single-step agent workflow (no step/tool gating).
    pub fn is_single_step(&self) -> bool {
        self.current_workflow()
            .is_some_and(|w| w.id == crate::agent::workflow::DEFAULT_WORKFLOW_ID)
    }

    fn is_code_modifying_tool(tool_name: &str) -> bool {
        matches!(tool_name, "file_write" | "edit_file" | "delete_range")
    }

    fn validate_single_step_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), String> {
        // Business gate: only block write/edit/shell tools, not read-only tools.
        // LLM must be able to file_read during scope discussion.
        if crate::agent::business_gate::is_pending_scope(self)
            && Self::is_code_modifying_tool(tool_name)
        {
            return Err(
                "⏸️ 业务流程门禁 — 等待用户确认 findings 范围（c /confirm）；讨论请直接输入文字。"
                    .to_string(),
            );
        }
        // NOTE: phase==Complete is intentionally NOT a hard block. `finish` is the
        // LLM's explicit end and yields the turn back to the user; gates/tools must
        // never forbid future actions. The next user round resets the workflow.

        if !self.allows_code_modification() && Self::is_code_modifying_tool(tool_name) {
            return Err(format!(
                "🔒 只读阶段 — 动手前先 finish(finding_json=[...]) 提交计划，用户 c 确认后解锁。禁止 {tool_name}。"
            ));
        }

        if crate::agent::phase::get(self) == crate::agent::phase::SingleFlowPhase::Implement {
            if matches!(tool_name, "file_search" | "file_list") {
                return Err(format!(
                    "实施阶段禁止 {tool_name} — 用 find_symbol/file_read 定位。"
                ));
            }
        }

        crate::agent::read_guard::check(tool_name, args, self)?;

        if tool_name == "file_read" {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
                self.validate_impl_file_read(path, offset)?;
            }
        }

        Ok(())
    }

    pub fn set_task_intent(&self, intent: crate::agent::task_intent::TaskIntent) {
        self.set_variable("_task_intent", intent.as_str().to_string());
    }

    pub fn get_task_intent(&self) -> crate::agent::task_intent::TaskIntent {
        self.get_variable("_task_intent")
            .map(|s| crate::agent::task_intent::TaskIntent::from_stored(&s))
            .unwrap_or(crate::agent::task_intent::TaskIntent::General)
    }

    pub fn clear_turn_provenance(&self) {
        self.set_variable("_explored_paths", "[]".to_string());
        self.set_variable("_exploration_snapshot", "[]".to_string());
        crate::agent::read_guard::clear(self);
    }

    pub fn reset_step_for_fix_reopen(&self) {
        self.set_variable(crate::agent::user_round::ROUND_FINALIZED_KEY, String::new());
        if let Ok(mut session) = self.session_state.try_lock() {
            session.current_step_index = 0;
            session.awaiting_user_confirmation = false;
        }
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
                    workflow
                        .get_step(session.current_step_index)
                        .map(|s| s.name.as_str())
                        .unwrap_or("Unknown")
                );
            } else {
                tracing::info!(
                    "Advanced to step {}/{}: {}",
                    session.current_step_index + 1,
                    workflow.total_steps(),
                    workflow
                        .get_step(session.current_step_index)
                        .map(|s| s.name.as_str())
                        .unwrap_or("Unknown")
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
            session.set_variable("_workflow_guidance", "[]");
            session.set_variable("_execute_report_delivered", "");
            session.set_variable("_execute_handoff", "");
            crate::agent::workflow_session::clear_session_flags(self);
            crate::agent::perception::clear(self);
            crate::agent::workflow_phases::clear_phase(self);
            // FIX: Clear findings store to prevent context pollution across rounds
            crate::agent::findings::clear(self);
            // Clear impl file read counters so new turns don't inherit old limits
            self.clear_impl_files_read();
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
    pub fn workflow_preserves_on_user_input(&self, user_text: &str) -> bool {
        if crate::agent::workflow_session::looks_like_new_task(user_text) {
            return false;
        }
        if self.is_single_step() && self.is_workflow_active() && !self.is_workflow_complete() {
            return true;
        }
        if crate::agent::workflow_session::is_parked(self) {
            return true;
        }
        if !self.is_workflow_active() || self.is_workflow_complete() {
            return false;
        }
        if crate::agent::workflow_phases::get_phase(self)
            == crate::agent::workflow_phases::WorkflowPhase::Act
        {
            return false;
        }
        self.get_current_step_index() > 0
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

    pub fn adopt_execute_interjection(&self, user_text: &str) {
        crate::agent::phase::on_user_message(self, user_text);
    }

    /// Build plan tracker from parked review report; reset per-file read ledger.
    pub fn bootstrap_implementation_plan(&self) {
        crate::agent::workflow_phases::set_phase(
            self,
            crate::agent::workflow_phases::WorkflowPhase::Act,
        );

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
        // Policy B: LLM-driven continuation — relies on actionable substance
        // (findings / failed verify / greenfield) rather than fix-keyword phrasing.
        if !crate::agent::phase::can_reopen_for_fix(self, user_text) {
            return false;
        }
        let r = crate::agent::phase::transition(
            self,
            crate::agent::phase::PhaseEvent::ReopenForFix {
                text: user_text.to_string(),
            },
        );
        self.set_variable(crate::agent::user_round::ROUND_FINALIZED_KEY, String::new());
        r.changed
            || crate::agent::phase::get(self) == crate::agent::phase::SingleFlowPhase::Implement
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
        [
            "cat ",
            "type ",
            "head ",
            "tail ",
            "more ",
            "less ",
            "get-content",
        ]
        .iter()
        .any(|p| lower.contains(p))
    }

    const IMPL_READ_KEY: &str = "_impl_files_read";

    pub fn clear_impl_files_read(&self) {
        self.set_variable(Self::IMPL_READ_KEY, "[]".to_string());
    }

    const REVIEW_HANDOFF_KEY: &str = "_review_handoff_files";

    /// Capture files explored during review BEFORE `enter_implement` clears the
    /// turn memory, so the Implement phase knows they were already read and does
    /// not re-explore them from scratch (fixes review→implement context loss).
    pub fn snapshot_review_handoff(&self) {
        let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for p in self.get_explored_path_set() {
            if !p.trim().is_empty() {
                files.insert(p);
            }
        }
        // Findings files are the highest-signal set — always carry them over.
        if let Some(store) = crate::agent::findings::load_or_migrate(self) {
            for f in &store.findings {
                if !f.file.trim().is_empty() {
                    files.insert(f.file.clone());
                }
            }
        }
        let files: Vec<String> = files.into_iter().collect();
        if files.is_empty() {
            return;
        }
        if let Ok(json) = serde_json::to_string(&files) {
            self.set_variable(Self::REVIEW_HANDOFF_KEY, json);
        }
    }

    /// Files carried over from the review phase (already read, content in context).
    pub fn review_handoff_files(&self) -> Vec<String> {
        self.get_variable(Self::REVIEW_HANDOFF_KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn clear_review_handoff(&self) {
        self.set_variable(Self::REVIEW_HANDOFF_KEY, String::new());
    }

    fn impl_files_read_set(&self) -> std::collections::HashSet<String> {
        self.get_variable(Self::IMPL_READ_KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn impl_file_already_read(&self, path: &str) -> bool {
        let norm = crate::agent::plan_tracker::normalize_path(path);
        self.impl_file_read_count(&norm) > 0
    }

    const IMPL_EDITED_KEY: &str = "_impl_files_edited";

    pub fn record_impl_file_edited(&self, path: &str) {
        let norm = crate::agent::plan_tracker::normalize_path(path);
        let mut list: Vec<String> = self
            .get_variable(Self::IMPL_EDITED_KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if list
            .iter()
            .any(|p| crate::agent::plan_tracker::normalize_path(p) == norm)
        {
            return;
        }
        list.push(path.to_string());
        if let Ok(json) = serde_json::to_string(&list) {
            self.set_variable(Self::IMPL_EDITED_KEY, json);
        }
    }

    /// Implementation phase: allow all reads. Compaction handles context bloat.
    pub fn validate_impl_file_read(&self, _path: &str, _offset: u64) -> Result<(), String> {
        Ok(())
    }

    /// Count how many times a file has been read this implementation round.
    fn impl_file_read_count(&self, norm_path: &str) -> usize {
        let key = &format!("{}:{}", Self::IMPL_READ_KEY, norm_path);
        self.get_variable(key)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
    }

    pub fn record_impl_file_read(&self, path: &str, _arguments: &str) {
        let norm = crate::agent::plan_tracker::normalize_path(path);
        let key = format!("{}:{}", Self::IMPL_READ_KEY, norm);
        let count = self
            .get_variable(&key)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        self.set_variable(&key, (count + 1).to_string());
    }

    pub fn impl_edit_nudge_after_read(&self, _path: &str, _preview: &str) -> Option<String> {
        None
    }

    pub fn should_skip_execute_confirmation(&self, _from_step: usize, _target_step: usize) -> bool {
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
        if self.is_single_step()
            && crate::agent::phase::get(self) == crate::agent::phase::SingleFlowPhase::Implement
        {
            return Some(crate::agent::workflow::IMPLEMENT_TURN_STEP_HINT.to_string());
        }
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
                    prompt = prompt.replace(
                        "{EXECUTE_HANDOFF}",
                        "（单步模式 — 按 [TURN_CONTEXT] 与 findings 执行）",
                    );
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
                        .map(|u| crate::agent::intent_routing::routing_hint_for_user(&u))
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
        // Single-step: registry tools only — no whitelist, phase, or legacy step gates.
        if self.is_single_step() {
            return self.validate_single_step_tool(tool_name, args);
        }

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
                let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
                self.validate_impl_file_read(path, offset)?;
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

    /// Single-step model (index 0) or legacy execute step (index 3).
    pub fn is_task_step(&self) -> bool {
        matches!(self.get_current_step_index(), 0 | 3)
    }

    pub fn current_step_display_label(&self) -> Option<String> {
        self.get_current_step_info()
            .map(|s| format!("{} ({}/{})", s.name, s.current_step, s.total_steps))
    }

    pub fn interjection_should_resume_turn(&self, user_text: &str) -> bool {
        if self.workflow_preserves_on_user_input(user_text) {
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
            self,
            cmd,
            working_dir,
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
            Some("⚠️ 编辑可能未通过语法检查 — 请修复后重试，勿标记为完成。".to_string())
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
        (hits >= 2 && (t.contains("完成") || t.contains("Done"))) || (hits >= 3)
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
    pub fn should_block_execute_reexplore(
        &self,
        tool_calls: &[ToolCall],
        assistant_text: &str,
    ) -> bool {
        if !tool_calls.is_empty() && Self::looks_like_review_report(assistant_text) {
            self.mark_execute_report_delivered();
        }
        if crate::agent::workflow_session::is_implementation_phase(self) {
            return false;
        }
        (self.execute_report_already_delivered() || self.should_park_execute_output(assistant_text))
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

    fn get_explored_path_set(&self) -> std::collections::HashSet<String> {
        self.get_variable("_explored_paths")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn get_execute_review_report(&self) -> Option<String> {
        self.get_variable("_step3_output")
            .filter(|s| !s.trim().is_empty())
            .filter(|s| Self::looks_like_review_report(s))
    }

    pub fn execute_review_report_block(&self, max_chars: usize) -> Option<String> {
        self.get_execute_review_report().map(|report| {
            let snippet: String = report.chars().take(max_chars).collect();
            format!("【审查报告 — park 前输出，用户在此基础上跟进】\n{snippet}")
        })
    }

    pub fn get_all_step_outputs_summary(&self) -> String {
        if self.is_single_step() {
            return self
                .get_previous_step_output()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "（无上一步输出）".to_string());
        }
        let mut summaries = Vec::new();
        let labels = ["意图分类", "任务规划", "审阅计划"];
        for i in 0..3 {
            if let Some(output) = self.get_variable(&format!("_step{i}_output")) {
                if output.trim().is_empty() {
                    continue;
                }
                let label = labels.get(i).copied().unwrap_or("未知");
                let json_or_summary =
                    if let (Some(s), Some(e)) = (output.find('{'), output.rfind('}')) {
                        &output[s..=e]
                    } else {
                        // Char-boundary-safe cap: `&output[..500]` panics mid-UTF-8 char.
                        let mut end = output.len().min(500);
                        while end > 0 && !output.is_char_boundary(end) {
                            end -= 1;
                        }
                        &output[..end]
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

    /// Normalize a directory path for exploration deduplication.
    pub fn normalize_explore_path(path: &str) -> String {
        let p = path.trim().trim_matches(|c| c == '/' || c == '\\');
        if p.is_empty() {
            ".".to_string()
        } else {
            p.to_lowercase()
        }
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
                e.tool == tool && crate::agent::plan_tracker::normalize_path(&e.target) == norm
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
}

/// Extract JSON object from LLM text (handles code fences and inline JSON).
pub fn extract_json_block(text: &str) -> Option<String> {
    // Try code-fenced JSON first
    if let (Some(start), Some(end)) = (text.find("```json"), text.rfind("```")) {
        let inner = &text[start + 7..end].trim();
        if inner.starts_with('{') {
            return Some(inner.to_string());
        }
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
