//! Key event handler — dispatches keyboard input to appropriate handlers.
//!
//! This module provides a single `handle_key` function that replaces the
//! 19-parameter `handle_key_event` from the original main.rs.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::helpers;
use crate::terminal::app::{App, UnifiedGatePending};

/// Handle a single key event. Returns true if the key was consumed.
///
/// This is a thin dispatcher that delegates to existing helpers in `crate::helpers`.
/// The heavy lifting (text submission, slash commands, LLM invocation) is handled
/// by the event loop after this function returns the input.
pub fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> KeyResult {
    // Unified complete_and_check gates (agent suspended, same turn).
    if let Some(gate) = app.unified_gate.clone() {
        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
            match (&gate, key.code) {
                (UnifiedGatePending::Deliver { .. }, KeyCode::Char('c' | 'C')) => {
                    return KeyResult::UnifiedDeliverConfirm;
                }
                (UnifiedGatePending::Finish { .. }, KeyCode::Char('f' | 'F')) => {
                    return KeyResult::UnifiedFinish(true);
                }
                (UnifiedGatePending::Finish { .. }, KeyCode::Char('c' | 'C')) => {
                    return KeyResult::UnifiedFinish(false);
                }
                _ => {}
            }
        }
    }

    // Scope confirmation shortcuts (no panel — findings shown in chat).
    if app.workflow_awaiting_confirmation == Some(4) && app.park_follow_up_tag.is_none() {
        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
            if matches!(key.code, KeyCode::Char('c' | 'C')) {
                return KeyResult::FindingsConfirm;
            }
            if matches!(key.code, KeyCode::Char('d' | 'D')) {
                return KeyResult::FindingsDiscuss;
            }
        }
    }

    // Fast path: simple printable characters go straight to input buffer
    if let KeyCode::Char(ch) = key.code {
        if !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            if ch != 'y' && ch != 'Y' && ch != 'n' && ch != 'N' && ch != 't' && ch != 'T' {
                app.input.insert_char(ch);
                app.dirty = true;
                return KeyResult::Handled;
            }
        }
    }

    match (key.code, key.modifiers) {
        // Confirmation keys (Y/N/T when pending)
        (KeyCode::Char('y'), KeyModifiers::NONE) | (KeyCode::Char('Y'), KeyModifiers::NONE) => {
            if !helpers::handle_confirmation_key(app, &key) {
                app.input.insert_char('y');
                app.dirty = true;
            }
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) | (KeyCode::Char('N'), KeyModifiers::NONE) => {
            if !helpers::handle_confirmation_key(app, &key) {
                app.input.insert_char('n');
                app.dirty = true;
            }
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) | (KeyCode::Char('T'), KeyModifiers::NONE) => {
            if !helpers::handle_confirmation_key(app, &key) {
                app.input.insert_char('t');
                app.dirty = true;
            }
        }
        // Control keys
        (KeyCode::Char('a'), KeyModifiers::CONTROL)
        | (KeyCode::Char('e'), KeyModifiers::CONTROL)
        | (KeyCode::Char('u'), KeyModifiers::CONTROL)
        | (KeyCode::Char('k'), KeyModifiers::CONTROL)
        | (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
            helpers::handle_control_key(app, &key);
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            // Interrupt handling requires InterruptController — must be handled by caller.
            // We signal the caller to handle it.
            return KeyResult::Interrupt;
        }
        // Enter — submit input (plain Enter submits; Alt+Enter / Ctrl+Enter inserts newline)
        (KeyCode::Enter, KeyModifiers::ALT) | (KeyCode::Enter, KeyModifiers::CONTROL) => {
            app.input.insert_newline();
            app.dirty = true;
            return KeyResult::Handled;
        }
        (KeyCode::Enter, _) => {
            if let Some(input) = app.submit_input() {
                return KeyResult::InputSubmitted(input);
            }
        }
        // Editing keys
        (KeyCode::Backspace, _)
        | (KeyCode::Delete, _)
        | (KeyCode::Left, _)
        | (KeyCode::Right, _) => {
            helpers::handle_editing_key(app, &key);
        }
        // Navigation keys
        (KeyCode::Up, KeyModifiers::SHIFT)
        | (KeyCode::Down, KeyModifiers::SHIFT)
        | (KeyCode::Up, _)
        | (KeyCode::Down, _)
        | (KeyCode::Home, _)
        | (KeyCode::End, _)
        | (KeyCode::PageUp, _)
        | (KeyCode::PageDown, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        // Character input (with modifiers or Y/N/T not in confirmation state)
        (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            helpers::handle_char_input(app, ch);
        }
        _ => {}
    }

    KeyResult::Handled
}

/// Result of processing a key event.
pub enum KeyResult {
    /// Key was handled — no further action needed.
    Handled,
    /// Ctrl+C or Ctrl+D was pressed — caller should handle interrupt.
    Interrupt,
    /// User pressed 1/2/3 on the park follow-up menu.
    ParkMenuShortcut(char),
    /// Toggle finding #N in the findings panel.
    FindingsToggle(u32),
    /// Confirm selected findings scope (same as /confirm).
    FindingsConfirm,
    /// Enter read-only discuss mode (same as /discuss).
    FindingsDiscuss,
    /// Confirm unified deliver gate (`complete_and_check` action=deliver).
    UnifiedDeliverConfirm,
    /// Finish gate: `finished` true = end turn, false = continue.
    UnifiedFinish(bool),
    /// User pressed Enter with text — caller should process the input.
    InputSubmitted(crate::terminal::app::UserInput),
}
