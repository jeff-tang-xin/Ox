//! Durable memory block — injected at turn start when workspace is not canonical.

use crate::agent::engine::WorkflowEngine;

pub const DURABLE_MEMORY_TAG: &str = "[DURABLE_MEMORY]";

/// Build durable context for the current turn (guidance only — [WORKSPACE] holds task state).
pub fn format_durable_memory_block(engine: &WorkflowEngine) -> String {
    if crate::context::context_slim::is_slim_phase(engine) {
        return String::new();
    }
    if engine.is_workflow_complete() && !crate::agent::phase::fix_impl_session(engine) {
        return String::new();
    }
    if crate::agent::workspace::uses_workspace_memory(engine) {
        return crate::agent::workspace::minimal_durable_addon(engine);
    }
    let guidance = engine.workflow_guidance_block();
    if guidance.is_empty() {
        return String::new();
    }
    format!("{DURABLE_MEMORY_TAG}\n{guidance}")
}

pub fn inject_durable_memory(messages: &mut Vec<crate::message::Message>, block: &str) {
    if block.is_empty() {
        return;
    }
    strip_durable_memory(messages);
    messages.push(crate::message::Message::system(block));
}

pub fn strip_durable_memory(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        !matches!(m, crate::message::Message::System { content } if content.starts_with(DURABLE_MEMORY_TAG))
    });
}