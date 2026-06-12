//! Input handling utilities.
//!
//! Contains helper functions for keyboard input processing.

use crossterm::event::{KeyCode, KeyModifiers};
use ox_core::agent::interrupt::{InterruptAction, InterruptController};
use ox_core::agent::ui_event::{ConfirmationDecision, UiToAgentEvent};
use crate::terminal::app::App;
use crate::terminal::output_pane::OutputLine;

/// Handle navigation keys (arrows, PageUp/Down, Home/End).
pub fn handle_navigation_key(app: &mut App, key: &crossterm::event::KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Up, KeyModifiers::SHIFT) => {
            app.scroll_up(1);
            app.user_scrolled = true;
            app.dirty = true;
        }
        (KeyCode::Down, KeyModifiers::SHIFT) => {
            app.scroll_down(1);
            if app.scroll_offset < 3 {
                app.user_scrolled = false;
            }
            app.dirty = true;
        }
        (KeyCode::Up, _) => {
            app.input.history_up();
            app.dirty = true;
        }
        (KeyCode::Down, _) => {
            app.input.history_down();
            app.dirty = true;
        }
        (KeyCode::Home, _) => {
            app.input.move_home();
            app.dirty = true;
        }
        (KeyCode::End, _) => {
            app.input.move_end();
            app.dirty = true;
        }
        (KeyCode::PageUp, _) => {
            app.scroll_up(10);
            app.user_scrolled = true;
            app.dirty = true;
        }
        (KeyCode::PageDown, _) => {
            app.scroll_down(10);
            if app.scroll_offset < 3 {
                app.user_scrolled = false;
            }
            app.dirty = true;
        }
        _ => {}
    }
}

/// Handle control key shortcuts.
pub fn handle_control_key(app: &mut App, key: &crossterm::event::KeyEvent) -> bool {
    // Returns true if key was handled
    match key.code {
        KeyCode::Char('a') => {
            app.input.move_home();
            app.dirty = true;
            true
        }
        KeyCode::Char('e') => {
            app.input.move_end();
            app.dirty = true;
            true
        }
        KeyCode::Char('u') => {
            app.input.clear_to_home();
            app.dirty = true;
            true
        }
        KeyCode::Char('k') => {
            app.input.clear_to_end();
            app.dirty = true;
            true
        }
        KeyCode::Char('w') => {
            app.input.delete_word();
            app.dirty = true;
            true
        }
        _ => false,
    }
}

/// Handle character input.
pub fn handle_char_input(app: &mut App, ch: char) {
    app.input.insert_char(ch);
    app.dirty = true;
}

/// Handle editing keys (Backspace, Delete, Left, Right).
pub fn handle_editing_key(app: &mut App, key: &crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Backspace => {
            app.input.backspace();
            app.dirty = true;
        }
        KeyCode::Delete => {
            app.input.delete();
            app.dirty = true;
        }
        KeyCode::Left => {
            app.input.move_left();
            app.dirty = true;
        }
        KeyCode::Right => {
            app.input.move_right();
            app.dirty = true;
        }
        _ => {}
    }
}

/// Handle confirmation keys (Y/N/T) - returns true if handled.
pub fn handle_confirmation_key(app: &mut App, key: &crossterm::event::KeyEvent) -> bool {
    let Some(pc) = app.pending_confirmation.take() else {
        return false;
    };

    let decision = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ConfirmationDecision::Allow),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ConfirmationDecision::Deny),
        KeyCode::Char('t') | KeyCode::Char('T') => Some(ConfirmationDecision::TrustAlways),
        _ => None,
    };

    if let Some(decision) = decision {
        if let Some(tx) = &app.ui_to_agent_tx {
            let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                tool_call_id: pc.tool_call_id,
                decision,
            });
            let msg = match decision {
                ConfirmationDecision::Allow => "  -> Allowed",
                ConfirmationDecision::Deny => "  -> Denied",
                ConfirmationDecision::TrustAlways => {
                    app.trusted_all = true;
                    "  -> Trusted all tools for this session. Use /untrust to revoke."
                }
            };
            app.output.push_line(OutputLine::System(msg.to_string()));
        } else {
            app.output.push_line(OutputLine::Error(
                "  -> Error: agent channel closed, cannot confirm".to_string(),
            ));
        }
        app.dirty = true;
        return true;
    }

    // Restore the pending_confirmation if not handled
    app.pending_confirmation = Some(pc);
    false
}

/// Handle interrupt keys (Ctrl+C, Ctrl+D).
pub fn handle_interrupt_key(
    app: &mut App,
    key: &crossterm::event::KeyEvent,
    interrupt_ctrl: &mut InterruptController,
) -> bool {
    match key.code {
        KeyCode::Char('c') => {
            let action = interrupt_ctrl.on_ctrl_c(app.agent_running);
            match action {
                InterruptAction::Shutdown | InterruptAction::ForceQuit => {
                    app.should_quit = true;
                }
                InterruptAction::CancelAgent => {
                    app.agent_running = false;
                    app.workflow_interrupted = true;
                    app.output.push_system("Agent interrupted.");
                    app.status = "Ox".to_string();
                }
            }
            app.dirty = true;
            true
        }
        KeyCode::Char('d') => {
            app.should_quit = true;
            true
        }
        _ => false,
    }
}
