//! Interjection middleware for handling user interruptions.
//!
//! Manages the interjection buffer and processes user input during agent execution.

use crate::terminal::app::App;
use crate::terminal::output_pane::OutputLine;
use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::ui_event::UiToAgentEvent;

/// Process user input as an interjection during agent execution.
pub fn handle_interjection(app: &mut App, text: &str, interjection_buf: &mut InterjectionBuffer) {
    let parked_resume = app
        .workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| e.is_workflow_parked())
        .unwrap_or(false);

    if !parked_resume {
        let blocked = app
            .workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| !e.allows_midflight_interjection())
            .unwrap_or(false);
        if blocked {
            app.output.push_line(OutputLine::System(
                "".to_string(),
            ));
            app.scroll_to_bottom();
            return;
        }
    }

    let priority = if text.starts_with('!') {
        InterjectionPriority::Urgent
    } else {
        InterjectionPriority::Normal
    };
    let content = text.trim_start_matches('!').to_string();

    // Send interjection to agent immediately via channel (buffer only if no active channel)
    if let Some(tx) = &app.ui_to_agent_tx {
        let _ = tx.send(UiToAgentEvent::Interjection(content.clone()));
    } else {
        interjection_buf.push(content.clone(), priority);
    }

    let prefix = if priority == InterjectionPriority::Urgent {
        "(urgent!)"
    } else {
        "(queued)"
    };
    app.output
        .push_line(OutputLine::System(format!("{} {}", prefix, content.trim())));
    app.scroll_to_bottom();
    app.dirty = true;
}
