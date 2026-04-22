use super::input_pane::InputPane;
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

/// Central UI state. All terminal state flows through this struct.
pub struct App {
    pub output: OutputPane,
    pub input: InputPane,
    /// Scroll offset for the output pane (0 = bottom / most recent).
    pub scroll_offset: u16,
    /// Whether the app should quit.
    pub should_quit: bool,
    /// Whether an agent turn is currently running.
    pub agent_running: bool,
    /// Status bar text.
    pub status: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            output: OutputPane::new(),
            input: InputPane::new(),
            scroll_offset: 0,
            should_quit: false,
            agent_running: false,
            status: String::from("Ox v0.1.0"),
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
            prefix: "ox>".to_string(),
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
