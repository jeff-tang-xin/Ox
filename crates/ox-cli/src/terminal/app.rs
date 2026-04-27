use super::input_pane::InputPane;
use super::markdown::MarkdownRenderer;
use super::output_pane::{OutputLine, OutputPane};

/// What the user submitted from the input pane.
#[derive(Debug)]
pub enum UserInput {
    /// Regular text to send to the agent.
    Text(String),
    /// A /slash command.
    SlashCommand { cmd: String, args: String },
    /// Exit signal (/exit or Ctrl+D).
    Exit,
}

/// Pending tool confirmation request from the agent.
#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    pub tool_call_id: String,
    pub tool_name: String,
}

/// Central UI state. All terminal state flows through this struct.
pub struct App {
    pub output: OutputPane,
    pub input: InputPane,
    /// Markdown renderer — created once, reused across all frames.
    pub md_renderer: MarkdownRenderer,
    /// Scroll offset for the output pane (0 = bottom / most recent).
    pub scroll_offset: u16,
    /// Whether the app should quit.
    pub should_quit: bool,
    /// Whether an agent turn is currently running.
    pub agent_running: bool,
    /// Status bar text.
    pub status: String,
    /// Whether the UI needs re-rendering (dirty flag).
    pub dirty: bool,
    /// Spinner animation frame counter.
    pub spinner_frame: u64,
    // ── Status bar info ──
    /// Current model name (for status bar display).
    pub model_name: String,
    /// Working directory short display.
    pub working_dir: String,
    /// Token cost summary string.
    pub cost_summary: String,
    /// Message count in current session.
    pub message_count: usize,
    /// Whether user has manually scrolled up (disables auto-scroll-to-bottom).
    pub user_scrolled: bool,
    // ── Confirmation state ──
    /// Pending tool confirmation request (if any).
    pub pending_confirmation: Option<PendingConfirmation>,
    /// UI→Agent channel sender for sending confirmations.
    pub ui_to_agent_tx: Option<tokio::sync::mpsc::UnboundedSender<ox_core::agent::ui_event::UiToAgentEvent>>,
    /// Pending council discuss request: (question, rounds, verbose).
    pub pending_discuss: Option<(String, Option<u8>, bool)>,
    /// Last completed council session (for /council last).
    pub last_council_session: Option<ox_core::council::CouncilSession>,
    /// Pending model switch request (model name).
    pub pending_model_switch: Option<String>,
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
            status: String::from("Ox v0.1.0"),
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
        }
    }

    /// Handle Enter key: parse input and return what the user submitted.
    pub fn submit_input(&mut self) -> Option<UserInput> {
        let text = self.input.submit();
        if text.is_empty() {
            return None;
        }

        let trimmed = text.trim();

        // Echo user input to output pane.
        self.output.push_line(OutputLine::Styled {
            prefix: "You".to_string(),
            content: trimmed.to_string(),
        });

        // Parse slash commands.
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

    /// Scroll the output pane up by N lines.
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Scroll the output pane down by N lines (towards most recent).
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Reset scroll to bottom (most recent output).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }
}
