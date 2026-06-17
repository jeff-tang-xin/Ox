use super::input_pane::InputPane;
use super::markdown::MarkdownRenderer;
use super::output_pane::{OutputLine, OutputPane};
use ox_core::agent::engine::WorkflowEngine;
use ox_core::agent::session::SessionState;
use std::sync::Arc;

/// Session action signaled by slash commands, processed in the main event loop.
#[derive(Debug, Clone, Default)]
pub enum SessionAction {
    #[default]
    None,
    New,
    Resume {
        filename: String,
    },
    /// Smart switch: go to next session, or reverse if at end
    SwitchNext,
}

/// Workflow state machine (Spec file overlay; agent always runs 4-step pipeline)
#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowState {
    /// Default: 4-step pipeline active
    Pipeline,

    /// Spec mode workflow states (legacy UI overlay)
    Spec {
        step: SpecWorkflowStep,
        spec_content: String,
    },
}

/// Spec workflow steps (enforced by code)
#[derive(Debug, Clone, PartialEq)]
pub enum SpecWorkflowStep {
    /// Step 1: Analyze requirements and classify task type
    RequirementAnalysis,
    /// Step 2: Generate spec.md (for complex tasks)
    GeneratingSpec,
    /// Step 3: Wait for user confirmation on spec
    AwaitingSpecConfirmation,
    /// Step 4: Generate task.md
    GeneratingTask,
    /// Step 5: Wait for final confirmation before execution
    AwaitingTaskConfirmation,
    /// Step 6: Execute code (tool calls allowed)
    Executing,
}


/// Cached workflow display information (to avoid locking in render loop)
#[derive(Debug, Clone)]
pub struct WorkflowDisplayInfo {
    pub workflow_name: String,
    pub step_num: usize,
    pub total_steps: usize,
    pub step_name: String,
    pub step_prompt: Option<String>,
    pub allows_code_modification: bool,
    /// 🚨 Requirement name for Spec mode (e.g., "order-optimization")
    pub requirement_name: Option<String>,
}

/// Unified LLM task to be processed in the main event loop
#[derive(Debug, Clone)]
pub enum PendingLlmTask {
    /// Generate skill from description
    SkillCreate { prompt: String, description: String },
    /// Spec planning
    SpecPlanning { spec_content: String },
    /// Workflow approval (Y command)
    WorkflowApproval { context: String },
    /// Smart naming for requirement
    SmartNaming { content: String },
}

/// Plan item — parsed from LLM ## Plan block, tracked against ## Done
#[derive(Debug, Clone)]
pub struct PlanItem {
    pub file: String,
    pub status: PlanItemStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanItemStatus {
    Pending,
    Done,
    Cancelled,
}

#[derive(Debug)]
pub enum UserInput {
    Text(String),
    SlashCommand { cmd: String, args: String },
    Exit,
}

/// Park follow-up menu choice shown as an input tag after pressing 1/2/3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkFollowUpTag {
    Continue,
    /// Read-only discussion — no implementation.
    Feedback,
    NewTask,
}

#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    pub tool_call_id: String,
    #[allow(dead_code)]
    pub tool_name: String,
}

#[derive(Debug, Clone)]
pub struct PendingSkillDraft {
    pub skill_id: String,
    pub content: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// Session file name (e.g., "session_001.jsonl")
    pub id: String,
    /// Project ID this session belongs to
    pub project_id: String,
    /// Display info (time, message count, etc.)
    pub info: String,
    pub is_active: bool,
}

impl SessionEntry {
    /// Get full path to session file
    pub fn full_path(&self, sessions_root: &std::path::Path) -> std::path::PathBuf {
        sessions_root.join(&self.project_id).join(&self.id)
    }
    
    /// Get display name with project prefix
    pub fn display_name(&self) -> String {
        format!("[{}] {}", self.project_id, self.info)
    }
}

pub struct App {
    pub output: OutputPane,
    pub input: InputPane,
    pub md_renderer: MarkdownRenderer,
    pub scroll_offset: u16,
    pub should_quit: bool,
    pub agent_running: bool,
    pub status: String,
    pub dirty: bool,
    pub spinner_frame: u64,
    pub model_name: String,
    /// Short embedding model label (e.g. `multilingual-e5-small`).
    pub embedding_model: String,
    pub working_dir: String,
    pub cost_summary: String,
    pub message_count: usize,
    pub user_scrolled: bool,
    pub pending_confirmation: Option<PendingConfirmation>,
    /// Workflow step index awaiting user confirmation (e.g. Plan review at step 1).
    pub workflow_awaiting_confirmation: Option<usize>,
    /// After park menu shortcut 1/2/3 — tag shown in the input pane until submit.
    pub park_follow_up_tag: Option<ParkFollowUpTag>,
    /// Skill draft awaiting user confirmation before save.
    pub pending_skill_draft: Option<PendingSkillDraft>,
    /// Skill review queued while agent is still running.
    pub queued_skill_draft: Option<PendingSkillDraft>,
    pub ui_to_agent_tx:
        Option<tokio::sync::mpsc::UnboundedSender<ox_core::agent::ui_event::UiToAgentEvent>>,
    pub pending_model_switch: Option<String>,
    /// 🆕 Unified pending LLM task (replaces multiple flags)
    pub pending_llm_task: Option<PendingLlmTask>,
    /// Flag indicating user requested revision feedback via /O command
    pub pending_revision_feedback: bool,
    pub trusted_all: bool,
    pub header_info: Vec<String>,
    pub sessions: Vec<SessionEntry>,
    pub sidebar_width: u16,
    /// Track last spinner frame to avoid unnecessary renders
    pub last_spinner_frame: u64,

    // Implicit feedback system
    pub override_detector: ox_core::feedback::CodeOverrideDetector,
    pub ema_manager: ox_core::feedback::Emamanager,
    pub rollback_manager: ox_core::feedback::RollbackManager,

    // Tracking counters for implicit feedback
    pub total_file_writes: u32,
    pub accepted_file_writes: u32,
    pub explicit_feedback_count: u32,
    pub good_feedback_count: u32,

    // Workflow state machine for Spec mode
    pub workflow_state: WorkflowState,

    // Cached workflow display info (updated each tick to avoid locking in render)
    pub workflow_display: Option<WorkflowDisplayInfo>,

    // Workflow engine (wrapped in Arc for sharing)
    pub workflow_engine: Option<Arc<tokio::sync::Mutex<WorkflowEngine>>>,

    // 🆕 Plan tracking: parsed from LLM ## Plan / ## Done blocks
    pub plan_items: Vec<PlanItem>,

    /// Flag set when user interrupts the agent (Ctrl+C). Prevents auto-spawning
    /// the next workflow step after the interrupted turn completes.
    pub workflow_interrupted: bool,

    // Fields needed by slash command handlers
    /// Session action signaled by slash commands, processed in the main event loop.
    pub session_action: SessionAction,
    /// Provider resolution info for debugging commands.
    pub resolve_info: Option<ox_core::llm::ProviderResolveInfo>,
    /// Unified knowledge engine (AST + memory + vector store)
    pub knowledge_engine: Option<Arc<tokio::sync::RwLock<ox_core::knowledge::KnowledgeEngine>>>,

    // Indexing progress
    /// Whether background indexing is still in progress
    pub indexing: bool,
    /// Current index phase: `parsing`, `embedding`, or empty.
    pub index_phase: String,
    /// Receiver for indexing progress updates: (files_processed, symbols_indexed)
    pub index_progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(usize, usize)>>,
    /// AST walk: files processed / total
    pub index_parse_done: usize,
    pub index_parse_total: usize,
    /// Symbols extracted so far (AST phase)
    pub index_symbols: usize,
    /// BERT embed: entities done / total (separate from file counts)
    pub index_embed_done: usize,
    pub index_embed_total: usize,
}

impl App {
    /// Active phase progress for UI (done, total).
    pub fn index_progress_counts(&self) -> (usize, usize) {
        if self.index_phase == "embedding" {
            let total = self.index_embed_total.max(1);
            (self.index_embed_done.min(total), total)
        } else {
            let total = self.index_parse_total.max(1);
            (self.index_parse_done.min(total), total)
        }
    }

    pub fn index_has_progress(&self) -> bool {
        if self.index_phase == "embedding" {
            self.index_embed_total > 0
        } else {
            self.index_parse_total > 0
        }
    }

    pub fn new() -> Self {
        Self {
            output: OutputPane::new(),
            input: InputPane::new(),
            md_renderer: MarkdownRenderer::new(),
            scroll_offset: 0,
            should_quit: false,
            agent_running: false,
            status: String::new(),
            dirty: true,
            spinner_frame: 0,
            model_name: String::new(),
            embedding_model: String::new(),
            working_dir: String::new(),
            cost_summary: String::new(),
            message_count: 0,
            user_scrolled: false,
            pending_confirmation: None,
            workflow_awaiting_confirmation: None,
            park_follow_up_tag: None,
            pending_skill_draft: None,
            queued_skill_draft: None,
            ui_to_agent_tx: None,
            pending_model_switch: None,
            pending_llm_task: None,  // 🆕 Unified LLM task
            pending_revision_feedback: false,
            trusted_all: false,
            header_info: Vec::new(),
            sessions: Vec::new(),
            sidebar_width: 22,
            last_spinner_frame: 0,

            // Implicit feedback system initialization
            override_detector: ox_core::feedback::CodeOverrideDetector::new(300), // 5 min window
            ema_manager: ox_core::feedback::Emamanager::new(0.2),                 // alpha = 0.2
            rollback_manager: ox_core::feedback::RollbackManager::new(),
            total_file_writes: 0,
            accepted_file_writes: 0,
            explicit_feedback_count: 0,
            good_feedback_count: 0,

            // Workflow state machine (default to Free mode)
            workflow_state: WorkflowState::Pipeline,
            workflow_display: None,

            // Workflow engine (initialized later with session ID)
            workflow_engine: None,

            // Plan tracking
            plan_items: Vec::new(),

            // Interrupt tracking
            workflow_interrupted: false,

            // Slash command context fields
            session_action: SessionAction::None,
            resolve_info: None,
            knowledge_engine: None,

            // Indexing progress
            indexing: false,
            index_phase: String::new(),
            index_progress_rx: None,
            index_parse_done: 0,
            index_parse_total: 0,
            index_symbols: 0,
            index_embed_done: 0,
            index_embed_total: 0,
        }
    }

    pub fn clear_workflow_confirmation(&mut self) {
        self.workflow_awaiting_confirmation = None;
        self.park_follow_up_tag = None;
    }

    pub fn submit_input(&mut self) -> Option<UserInput> {
        let text = self.input.submit();
        if text.is_empty() {
            return None;
        }

        let trimmed = text.trim();
        let display = match self.park_follow_up_tag {
            Some(ParkFollowUpTag::Feedback) => format!("[意见] {trimmed}"),
            Some(ParkFollowUpTag::NewTask) => format!("[新任务] {trimmed}"),
            Some(ParkFollowUpTag::Continue) => format!("[继续] {trimmed}"),
            None => trimmed.to_string(),
        };
        self.output.push_line(OutputLine::User(display));
        self.park_follow_up_tag = None;

        if let Some(stripped) = trimmed.strip_prefix('/') {
            let mut parts = stripped.splitn(2, char::is_whitespace);
            let cmd = parts.next().unwrap_or("").to_string();
            let args = parts.next().unwrap_or("").trim().to_string();

            if cmd == "exit" {
                return Some(UserInput::Exit);
            }

            return Some(UserInput::SlashCommand { cmd, args });
        }

        Some(UserInput::Text(text))
    }

    pub fn scroll_up(&mut self, delta: u16) {
        // Scroll offset = lines from bottom being shown. 0 = bottom, max = top.
        self.scroll_offset = self.scroll_offset.saturating_add(delta);
    }

    pub fn scroll_down(&mut self, delta: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(delta);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    #[allow(dead_code)]
    pub fn get_max_scroll(&self) -> u16 {
        // Approximate max scroll based on total line count
        let total_lines = self.output.lines.len() as u16 * 3; // rough estimate of wrapped lines
        total_lines.saturating_sub(10).min(500)
    }

    /// Check if render is needed, considering spinner animation
    pub fn needs_render(&self) -> bool {
        if self.dirty {
            return true;
        }
        // Only re-render for spinner if frame actually changed
        if (self.agent_running || self.indexing) && self.spinner_frame != self.last_spinner_frame {
            return true;
        }
        false
    }

    /// Mark that spinner frame has been processed for rendering
    pub fn mark_spinner_rendered(&mut self) {
        self.last_spinner_frame = self.spinner_frame;
    }

    // ===== Workflow State Machine Helpers =====

    /// Check if currently in Spec mode
    pub fn is_spec_mode(&self) -> bool {
        matches!(self.workflow_state, WorkflowState::Spec { .. })
    }

    /// Get current spec content (if in Spec mode)
    pub fn get_spec_content(&self) -> Option<&str> {
        match &self.workflow_state {
            WorkflowState::Spec { spec_content, .. } if !spec_content.is_empty() => {
                Some(spec_content)
            }
            _ => None,
        }
    }

    /// Activate Spec mode with initial requirement
    pub fn activate_spec_mode(&mut self, requirement: String) {
        self.workflow_state = WorkflowState::Spec {
            step: SpecWorkflowStep::RequirementAnalysis,
            spec_content: requirement,
        };
    }

    /// Transition to next Spec workflow step
    pub fn advance_spec_step(&mut self) {
        if let WorkflowState::Spec { step, spec_content } = &self.workflow_state {
            let next_step = match step {
                SpecWorkflowStep::RequirementAnalysis => SpecWorkflowStep::GeneratingSpec,
                SpecWorkflowStep::GeneratingSpec => SpecWorkflowStep::AwaitingSpecConfirmation,
                SpecWorkflowStep::AwaitingSpecConfirmation => SpecWorkflowStep::GeneratingTask,
                SpecWorkflowStep::GeneratingTask => SpecWorkflowStep::AwaitingTaskConfirmation,
                SpecWorkflowStep::AwaitingTaskConfirmation => SpecWorkflowStep::Executing,
                SpecWorkflowStep::Executing => return, // Stay in executing
            };
            self.workflow_state = WorkflowState::Spec {
                step: next_step,
                spec_content: spec_content.clone(),
            };
        }
    }

    /// Deactivate Spec overlay and return to default pipeline UI state
    pub fn deactivate_spec_mode(&mut self) {
        self.workflow_state = WorkflowState::Pipeline;
    }

    /// Initialize workflow engine (called after session is created)
    pub fn init_workflow_engine(&mut self, session_id: &str, session_meta: &ox_core::message::SessionMeta) {
        // 🚨 Restore workflow state from persisted metadata
        let mut session_state = SessionState::new(session_id);
        
        // Restore workflow mode and step index if available
        if !session_meta.workflow_mode.is_empty() {
            session_state.current_mode = session_meta.workflow_mode.clone();
            session_state.current_workflow = session_meta.workflow_id.clone();
            session_state.current_step_index = session_meta.workflow_step_index;
            
            tracing::info!(
                "Restored workflow state: mode={}, workflow={}, step={}",
                session_state.current_mode,
                session_state.current_workflow,
                session_state.current_step_index
            );
        }
        
        // Restore requirement name if available
        if let Some(ref req_name) = session_meta.requirement_name {
            session_state.set_variable("requirement_name", req_name);
            tracing::info!("Restored requirement name: {}", req_name);
        }
        
        let session_state_arc = Arc::new(tokio::sync::Mutex::new(session_state));
        let mut engine = WorkflowEngine::new(session_state_arc);

        // Always use the 4-step pipeline (migrate legacy "free" sessions)
        let workflow_id = if session_meta.workflow_id.is_empty()
            || session_meta.workflow_mode == "free"
        {
            ox_core::agent::workflow::DEFAULT_WORKFLOW_ID
        } else {
            session_meta.workflow_id.as_str()
        };
        if let Err(e) = engine.activate_workflow(workflow_id) {
            tracing::warn!("Failed to activate workflow '{}': {}", workflow_id, e);
        } else if session_meta.workflow_step_index > 0
            && session_meta.workflow_mode != "free"
        {
            let total = engine
                .current_workflow()
                .map(|w| w.total_steps())
                .unwrap_or(0);
            if session_meta.workflow_step_index < total {
                for _ in 0..session_meta.workflow_step_index {
                    let _ = engine.advance_step();
                }
                tracing::info!(
                    "Restored workflow step {}/{}",
                    session_meta.workflow_step_index + 1,
                    total
                );
            }
        }

        self.workflow_engine = Some(Arc::new(tokio::sync::Mutex::new(engine)));
    }

    /// Get cloned Arc reference to workflow engine (for passing to async tasks)
    pub fn workflow_engine_arc(&self) -> Option<Arc<tokio::sync::Mutex<WorkflowEngine>>> {
        self.workflow_engine.clone()
    }

    /// Update cached workflow display info (call from main loop tick)
    pub fn update_workflow_display(&mut self) {
        if let Some(ref engine_arc) = self.workflow_engine {
            // Use try_lock to avoid blocking - if locked, skip this update
            if let Ok(engine) = engine_arc.try_lock() {
                if engine.current_workflow().is_some() {
                    if let Some(step) = engine.current_step() {
                        if let Some((step_num, total_steps)) = engine.get_progress() {
                            // 🚨 Extract requirement name from workflow engine
                            let requirement_name = engine.get_variable("requirement_name");
                            
                            self.workflow_display = Some(WorkflowDisplayInfo {
                                workflow_name: if step.display_status.is_empty() { step.name.clone() } else { step.display_status.clone() },
                                step_num,
                                total_steps,
                                step_name: if step.display_status.is_empty() { step.name.clone() } else { step.display_status.clone() },
                                step_prompt: engine.get_step_system_prompt(),
                                allows_code_modification: engine.allows_code_modification(),
                                requirement_name,
                            });
                            return;
                        }
                    }
                }
            }
        }
        // If no workflow or lock failed, clear the display
        self.workflow_display = None;
    }
}
