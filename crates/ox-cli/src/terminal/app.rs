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

/// Workflow state machine for Spec and Council modes
#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowState {
    /// Free exploration mode (default)
    Free,

    /// Spec mode workflow states
    Spec {
        step: SpecWorkflowStep,
        spec_content: String,
    },

    /// Council mode workflow states
    Council {
        step: CouncilWorkflowStep,
        topic: Option<String>,
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

/// Council workflow steps (enforced by code)
#[derive(Debug, Clone, PartialEq)]
pub enum CouncilWorkflowStep {
    /// Step 1: Define discussion topic
    TopicDefinition,
    /// Step 2: Proposal phase (agents submit proposals)
    ProposalPhase,
    /// Step 3: Review phase (critique proposals)
    ReviewPhase,
    /// Step 4: Rebuttal phase (defend proposals)
    RebuttalPhase,
    /// Step 5: Arbitration (final decision)
    Arbitration,
    /// Step 6: Conclusion and summary
    Conclusion,
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

#[derive(Debug)]
pub enum UserInput {
    Text(String),
    SlashCommand { cmd: String, args: String },
    Exit,
}

#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    pub tool_call_id: String,
    #[allow(dead_code)]
    pub tool_name: String,
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

/// Deferred compression: set by handle_key_event, processed by main loop after render.
pub struct PendingCompression {
    pub text: String,
    pub memory_ctx: String,
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
    pub working_dir: String,
    pub cost_summary: String,
    pub message_count: usize,
    pub user_scrolled: bool,
    pub pending_confirmation: Option<PendingConfirmation>,
    pub ui_to_agent_tx:
        Option<tokio::sync::mpsc::UnboundedSender<ox_core::agent::ui_event::UiToAgentEvent>>,
    pub pending_discuss: Option<(String, Option<u8>, bool)>,
    pub last_council_session: Option<ox_core::council::CouncilSession>,
    pub pending_model_switch: Option<String>,
    pub pending_compression: Option<PendingCompression>,
    /// 🆕 Unified pending LLM task (replaces multiple flags)
    pub pending_llm_task: Option<PendingLlmTask>,
    /// Deprecated: use pending_llm_task instead
    #[deprecated(note = "Use pending_llm_task instead")]
    pub pending_spec_planning: Option<String>,
    /// 🚨 Pending smart naming request (LLM-based name generation)
    /// Deprecated: use pending_llm_task instead
    #[deprecated(note = "Use pending_llm_task instead")]
    pub pending_smart_naming: Option<crate::spec_helpers::PendingSmartNaming>,
    /// Flag indicating user requested revision feedback via /O command
    pub pending_revision_feedback: bool,
    /// Flag indicating user approved workflow progression via /Y command
    /// Deprecated: use pending_llm_task instead
    #[deprecated(note = "Use pending_llm_task instead")]
    pub pending_workflow_approval: bool,
    /// Message count at last compression. Used to avoid re-compressing
    /// when no new messages have been added since last compression.
    pub last_compression_msg_count: usize,
    /// Whether compression is currently in progress. Used to prevent
    /// re-entrant compression while an async compression is running.
    pub compression_in_progress: bool,
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

    // Workflow state machine for Spec and Council modes
    pub workflow_state: WorkflowState,

    // Cached workflow display info (updated each tick to avoid locking in render)
    pub workflow_display: Option<WorkflowDisplayInfo>,

    // Backward compatibility fields (deprecated, use workflow_state instead)
    #[deprecated(note = "Use workflow_state instead")]
    pub spec_content: String,
    #[deprecated(note = "Use workflow_state instead")]
    pub spec_active: bool,
    #[deprecated(note = "Use workflow_state instead")]
    pub spec_edit_mode: bool,

    // Workflow engine for Spec and Council modes (wrapped in Arc for sharing)
    pub workflow_engine: Option<Arc<tokio::sync::Mutex<WorkflowEngine>>>,

    // Fields needed by slash command handlers
    /// Session action signaled by slash commands, processed in the main event loop.
    pub session_action: SessionAction,
    /// Compression manager reference for debugging commands.
    pub compression_manager: Option<ox_core::embedding::CompressionManager>,
    /// Provider resolution info for debugging commands.
    pub resolve_info: Option<ox_core::llm::ProviderResolveInfo>,
    /// Flag to clear compressed cache when session is cleaned.
    pub pending_compressed_cache_clear: bool,
}

impl App {
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
            working_dir: String::new(),
            cost_summary: String::new(),
            message_count: 0,
            user_scrolled: false,
            pending_confirmation: None,
            ui_to_agent_tx: None,
            pending_discuss: None,
            last_council_session: None,
            pending_model_switch: None,
            pending_compression: None,
            pending_llm_task: None,  // 🆕 Unified LLM task
            #[allow(deprecated)]
            pending_spec_planning: None,
            #[allow(deprecated)]
            pending_smart_naming: None,
            pending_revision_feedback: false,
            #[allow(deprecated)]
            pending_workflow_approval: false,
            last_compression_msg_count: 0,
            compression_in_progress: false,
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
            workflow_state: WorkflowState::Free,
            workflow_display: None,

            // Backward compatibility fields (deprecated)
            #[allow(deprecated)]
            spec_content: String::new(),
            #[allow(deprecated)]
            spec_active: false,
            #[allow(deprecated)]
            spec_edit_mode: false,

            // Workflow engine (initialized later with session ID)
            workflow_engine: None,

            // Slash command context fields
            session_action: SessionAction::None,
            compression_manager: None,
            resolve_info: None,
            pending_compressed_cache_clear: false,
        }
    }

    pub fn submit_input(&mut self) -> Option<UserInput> {
        let text = self.input.submit();
        if text.is_empty() {
            return None;
        }

        let trimmed = text.trim();
        self.output.push_line(OutputLine::User(trimmed.to_string()));

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
        if self.agent_running && self.spinner_frame != self.last_spinner_frame {
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

    /// Deactivate Spec mode and return to Free mode
    pub fn deactivate_spec_mode(&mut self) {
        self.workflow_state = WorkflowState::Free;
    }

    /// Activate Council mode with topic
    pub fn activate_council_mode(&mut self, topic: Option<String>) {
        self.workflow_state = WorkflowState::Council {
            step: CouncilWorkflowStep::TopicDefinition,
            topic,
        };
    }

    /// Transition to next Council workflow step
    pub fn advance_council_step(&mut self) {
        if let WorkflowState::Council { step, topic } = &self.workflow_state {
            let next_step = match step {
                CouncilWorkflowStep::TopicDefinition => CouncilWorkflowStep::ProposalPhase,
                CouncilWorkflowStep::ProposalPhase => CouncilWorkflowStep::ReviewPhase,
                CouncilWorkflowStep::ReviewPhase => CouncilWorkflowStep::RebuttalPhase,
                CouncilWorkflowStep::RebuttalPhase => CouncilWorkflowStep::Arbitration,
                CouncilWorkflowStep::Arbitration => CouncilWorkflowStep::Conclusion,
                CouncilWorkflowStep::Conclusion => return, // Stay in conclusion
            };
            self.workflow_state = WorkflowState::Council {
                step: next_step,
                topic: topic.clone(),
            };
        }
    }

    /// Deactivate Council mode and return to Free mode
    pub fn deactivate_council_mode(&mut self) {
        self.workflow_state = WorkflowState::Free;
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

        // Activate initial workflow based on current mode
        if self.spec_active || session_meta.workflow_mode == "spec" {
            if let Err(e) = engine.activate_workflow("spec_workflow") {
                tracing::warn!("Failed to activate spec workflow: {}", e);
            } else {
                // 🚨 Restore step index if we're in Spec Mode
                if session_meta.workflow_step_index > 0 && session_meta.workflow_step_index < 6 {
                    // Advance to the saved step
                    for _ in 0..session_meta.workflow_step_index {
                        let _ = engine.advance_step();
                    }
                    tracing::info!("Advanced to step {}/6", session_meta.workflow_step_index + 1);
                }
            }
        } else {
            // Default to free workflow
            if let Err(e) = engine.activate_workflow("free_workflow") {
                tracing::warn!("Failed to activate free workflow: {}", e);
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
                if let Some(workflow) = engine.current_workflow() {
                    // Don't display free_workflow (it's a trivial single-step workflow)
                    if workflow.name == "free_workflow" {
                        self.workflow_display = None;
                        return;
                    }
                    
                    if let Some(step) = engine.current_step() {
                        if let Some((step_num, total_steps)) = engine.get_progress() {
                            // 🚨 Extract requirement name from workflow engine
                            let requirement_name = engine.get_variable("requirement_name");
                            
                            self.workflow_display = Some(WorkflowDisplayInfo {
                                workflow_name: workflow.name.clone(),
                                step_num,
                                total_steps,
                                step_name: step.name.clone(),
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
