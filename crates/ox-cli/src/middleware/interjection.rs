//! Interjection middleware for handling user interruptions.
//!
//! Manages the interjection buffer and processes user input during agent execution.

use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::ui_event::UiToAgentEvent;
use crate::terminal::app::App;
use crate::terminal::output_pane::OutputLine;

/// Process user input as an interjection during agent execution.
pub fn handle_interjection(
    app: &mut App,
    text: &str,
    interjection_buf: &mut InterjectionBuffer,
) {
    let priority = if text.starts_with('!') {
        InterjectionPriority::Urgent
    } else {
        InterjectionPriority::Normal
    };
    let content = text.trim_start_matches('!').to_string();

    // Send interjection to agent immediately via channel
    if let Some(tx) = &app.ui_to_agent_tx {
        let _ = tx.send(UiToAgentEvent::Interjection(content.clone()));
    }

    // Also buffer locally for fallback display
    interjection_buf.push(content.clone(), priority);

    let prefix = if priority == InterjectionPriority::Urgent {
        "(urgent!)"
    } else {
        "(queued)"
    };
    app.output.push_line(OutputLine::System(format!(
        "{} {}",
        prefix,
        content.trim()
    )));
    app.scroll_to_bottom();
    app.dirty = true;
}
