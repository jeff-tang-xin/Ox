use super::input_pane::InputPane;
use super::markdown::MarkdownRenderer;
use super::output_pane::{OutputLine, OutputPane};

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
    #[allow(dead_code)]
    pub filename: String,
    pub info: String,
    pub is_active: bool,
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
    pub ui_to_agent_tx: Option<tokio::sync::mpsc::UnboundedSender<ox_core::agent::ui_event::UiToAgentEvent>>,
    pub pending_discuss: Option<(String, Option<u8>, bool)>,
    pub last_council_session: Option<ox_core::council::CouncilSession>,
    pub pending_model_switch: Option<String>,
    pub pending_compression: Option<PendingCompression>,
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
    /// Chat area bounds for mouse scroll detection (x, y, width, height)
    pub chat_area: Option<(u16, u16, u16, u16)>,
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
            last_compression_msg_count: 0,
            compression_in_progress: false,
            trusted_all: false,
            header_info: Vec::new(),
            sessions: Vec::new(),
            sidebar_width: 22,
            last_spinner_frame: 0,
            chat_area: None,
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
}
