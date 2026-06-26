//! Safety gate — user confirms **dangerous tool execution** (Allow / Deny / TrustAlways).
//!
//! Distinct from [`super::business_gate`] which confirms business outputs (findings scope).

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::message::Message;
use crate::safety::TrustManager;
use crate::tools::SafetyLevel;

use super::engine::WorkflowEngine;
use super::ui_event;

pub use ui_event::ConfirmationDecision as SafetyDecision;

#[derive(Debug, Clone)]
pub struct SafetyGateRequest {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args_summary: String,
    pub safety_level: SafetyLevel,
    pub high_risk_warning: Option<String>,
}

pub struct SafetyGateCancelled;

/// Whether this tool call needs user confirmation before execution.
pub fn should_require_confirmation(
    tool_name: &str,
    safety_level: SafetyLevel,
    path_outside: bool,
    blacklist_warning: bool,
    trust_manager: &TrustManager,
) -> bool {
    if path_outside || blacklist_warning {
        return true;
    }
    match safety_level {
        SafetyLevel::Safe => false,
        SafetyLevel::RequiresConfirmation | SafetyLevel::Dangerous => {
            !trust_manager.can_skip_confirmation(tool_name, safety_level)
        }
    }
}

pub fn build_request(
    tool_call_id: String,
    tool_name: String,
    arguments: &str,
    safety_level: SafetyLevel,
    high_risk_warning: Option<String>,
) -> SafetyGateRequest {
    let args_summary = if arguments.len() > 200 {
        let end = arguments
            .char_indices()
            .take_while(|(i, _)| *i < 200)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!(
            "{}...(truncated)",
            arguments.get(..end).unwrap_or(arguments)
        )
    } else {
        arguments.to_string()
    };
    SafetyGateRequest {
        tool_call_id,
        tool_name,
        args_summary,
        safety_level,
        high_risk_warning,
    }
}

/// Block until the user decides on a pending tool execution (same ReAct turn).
pub async fn await_decision(
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    tool_call_id: &str,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<super::AgentToUiEvent>,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<super::AgentToUiEvent>,
    ),
) -> Result<SafetyDecision, SafetyGateCancelled> {
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(300));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            ev = ui_rx.recv() => {
                match ev {
                    None => return Err(SafetyGateCancelled),
                    Some(ui_event::UiToAgentEvent::ToolConfirmation { tool_call_id: id, decision })
                        if id == tool_call_id =>
                    {
                        return Ok(decision);
                    }
                    Some(ui_event::UiToAgentEvent::Interjection(text)) => {
                        push_interjection(workflow_engine, messages, &text, ui_tx);
                        let is_urgent = text.to_lowercase().contains("stop")
                            || text.to_lowercase().contains("cancel")
                            || text.to_lowercase().contains("中断")
                            || text.to_lowercase().contains("取消");
                        if is_urgent {
                            return Err(SafetyGateCancelled);
                        }
                    }
                    Some(ui_event::UiToAgentEvent::BusinessAck { .. }) => {
                        tracing::warn!(
                            "[SAFETY_GATE] 收到意外的 BusinessAck，business gate 可能超时"
                        );
                    }
                    Some(ui_event::UiToAgentEvent::FinishAck { finished, .. }) => {
                        if finished {
                            return Err(SafetyGateCancelled);
                        }
                    }
                    Some(ui_event::UiToAgentEvent::ScopeConfirmed) => {
                        tracing::warn!("[SAFETY_GATE] 收到意外的 ScopeConfirmed");
                    }
                    _ => {}
                }
            }
            _ = cancel_token.cancelled() => return Err(SafetyGateCancelled),
            () = &mut timeout => {
                tracing::warn!("[SAFETY_GATE] 超时（300秒），自动取消");
                return Err(SafetyGateCancelled);
            }
        }
    }
}

pub fn emit_request(ui_tx: &mpsc::UnboundedSender<super::AgentToUiEvent>, req: &SafetyGateRequest) {
    let _ = ui_tx.send(super::AgentToUiEvent::ToolConfirmationRequest {
        tool_call_id: req.tool_call_id.clone(),
        tool_name: req.tool_name.clone(),
        args_summary: req.args_summary.clone(),
        safety_level: req.safety_level,
        high_risk_warning: req.high_risk_warning.clone(),
    });
}

/// Sync helpers — keep `std::sync::MutexGuard` out of async state machines (Send).
pub fn shell_blacklist_warning(
    trust_manager: &std::sync::Mutex<TrustManager>,
    cmd: &str,
) -> Option<String> {
    let tm = trust_manager.lock().unwrap();
    tm.is_command_blacklisted(cmd)
        .map(|pattern| format!("BLOCKED COMMAND (matches blacklist pattern: \"{pattern}\")"))
}

pub fn needs_confirmation(
    trust_manager: &std::sync::Mutex<TrustManager>,
    tool_name: &str,
    safety_level: SafetyLevel,
    path_outside: bool,
    blacklist_hit: bool,
) -> bool {
    let tm = trust_manager.lock().unwrap();
    should_require_confirmation(tool_name, safety_level, path_outside, blacklist_hit, &tm)
}

pub fn apply_trust_all(trust_manager: &std::sync::Mutex<TrustManager>) {
    let mut tm = trust_manager.lock().unwrap();
    tm.trust_all();
}
