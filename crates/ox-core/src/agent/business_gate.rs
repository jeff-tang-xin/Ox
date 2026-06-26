//! Business-flow gate — user confirms agent **outputs** (findings scope, plans).
//!
//! Distinct from [`super::safety_gate`] which confirms **dangerous tool execution**.
//! Both suspend the same ReAct turn on `ui_rx`; neither emits `TurnDone`.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::message::Message;

use super::engine::WorkflowEngine;
use super::ui_event::{self, BusinessGateKind};

pub const PENDING_SCOPE_KEY: &str = "_business_gate_pending_scope";
pub const SCOPE_ACK_KEY: &str = "_business_gate_scope_ack";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusinessGateResume {
    /// User confirmed scope — continue same turn (edit tools unlocked; safety gate on each edit).
    Acknowledged,
    /// User discussed — LLM responds read-only in same turn.
    Discuss,
    Cancelled,
}

pub fn arm_findings_scope(engine: &WorkflowEngine) {
    engine.set_variable(PENDING_SCOPE_KEY, "1".to_string());
    engine.set_variable(SCOPE_ACK_KEY, String::new());
}

pub fn ack_findings_scope(engine: &WorkflowEngine) {
    if engine.get_variable(SCOPE_ACK_KEY).as_deref() == Some("1") {
        tracing::warn!("[BUSINESS_GATE] scope 已确认，重复触发忽略");
        return;
    }
    engine.set_variable(PENDING_SCOPE_KEY, String::new());
    engine.set_variable(SCOPE_ACK_KEY, "1".to_string());
    super::phase::on_scope_selected(engine);
}

pub fn is_pending_scope(engine: &WorkflowEngine) -> bool {
    engine.get_variable(PENDING_SCOPE_KEY).as_deref() == Some("1")
}

pub fn scope_implementation_unlocked(engine: &WorkflowEngine) -> bool {
    engine.get_variable(SCOPE_ACK_KEY).as_deref() == Some("1")
        || super::phase::get(engine) == super::phase::SingleFlowPhase::Implement
}

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(PENDING_SCOPE_KEY, String::new());
    engine.set_variable(SCOPE_ACK_KEY, String::new());
}

/// Suspend after findings until user confirms scope or discusses.
pub async fn await_findings_scope_gate(
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<super::AgentToUiEvent>,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<super::AgentToUiEvent>,
    ),
) -> BusinessGateResume {
    let _ = ui_tx.send(super::AgentToUiEvent::Status(
        "⏸ 业务流程门禁：等待确认 findings 范围 — 面板选 finding 后 c /confirm；可输入讨论"
            .to_string(),
    ));

    let timeout = tokio::time::sleep(std::time::Duration::from_secs(300));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            ev = ui_rx.recv() => {
                match ev {
                    None => return BusinessGateResume::Cancelled,
                    Some(ui_event::UiToAgentEvent::BusinessAck { kind: BusinessGateKind::FindingsScope })
                    | Some(ui_event::UiToAgentEvent::ScopeConfirmed) => {
                        if let Some(wf) = workflow_engine {
                            if let Ok(engine) = wf.try_lock() {
                                ack_findings_scope(&engine);
                            }
                        }
                        return BusinessGateResume::Acknowledged;
                    }
                    Some(ui_event::UiToAgentEvent::Interjection(text)) => {
                        let trimmed = text.trim();
                        let lower = trimmed.to_lowercase();
                        let is_confirm = trimmed == "c"
                            || trimmed == "/confirm"
                            || trimmed == "/fix"
                            || lower == "确认"
                            || lower == "开始实施";
                        if is_confirm {
                            if let Some(wf) = workflow_engine {
                                if let Ok(engine) = wf.try_lock() {
                                    ack_findings_scope(&engine);
                                }
                            }
                            return BusinessGateResume::Acknowledged;
                        }
                        push_interjection(workflow_engine, messages, &text, ui_tx);
                        let unlocked = workflow_engine
                            .as_ref()
                            .and_then(|wf| wf.try_lock().ok())
                            .is_some_and(|e| scope_implementation_unlocked(&e));
                        if unlocked {
                            return BusinessGateResume::Acknowledged;
                        }
                        return BusinessGateResume::Discuss;
                    }
                    Some(ui_event::UiToAgentEvent::ToolConfirmation { tool_call_id, .. }) => {
                        tracing::warn!(
                            "[BUSINESS_GATE] 收到意外的 ToolConfirmation (id={tool_call_id})，safety gate 可能超时"
                        );
                    }
                    Some(ui_event::UiToAgentEvent::FinishAck { finished, .. }) => {
                        if finished {
                            return BusinessGateResume::Cancelled;
                        }
                    }
                    Some(ui_event::UiToAgentEvent::BusinessAck { kind }) => {
                        if kind != BusinessGateKind::FindingsScope {
                            tracing::warn!(
                                "[BUSINESS_GATE] 收到意外的 BusinessAck (kind={:?})",
                                kind
                            );
                        }
                    }
                }
            }
            _ = cancel_token.cancelled() => return BusinessGateResume::Cancelled,
            () = &mut timeout => {
                tracing::warn!("[BUSINESS_GATE] 超时（300秒），自动取消");
                return BusinessGateResume::Cancelled;
            }
        }
    }
}

/// Suspend after generic `deliver` (non-findings) until user confirms or discusses.
pub async fn await_deliver_gate(
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<super::AgentToUiEvent>,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<super::AgentToUiEvent>,
    ),
    kind: &str,
) -> BusinessGateResume {
    let _ = ui_tx.send(super::AgentToUiEvent::Status(format!(
        "⏸ 交付门禁 ({kind})：c /confirm 确认 · 输入讨论"
    )));

    loop {
        tokio::select! {
            ev = ui_rx.recv() => {
                match ev {
                    None => return BusinessGateResume::Cancelled,
                    Some(ui_event::UiToAgentEvent::BusinessAck { kind: BusinessGateKind::Deliver })
                    | Some(ui_event::UiToAgentEvent::BusinessAck { kind: BusinessGateKind::FindingsScope })
                    | Some(ui_event::UiToAgentEvent::ScopeConfirmed) => {
                        return BusinessGateResume::Acknowledged;
                    }
                    Some(ui_event::UiToAgentEvent::Interjection(text)) => {
                        push_interjection(workflow_engine, messages, &text, ui_tx);
                        return BusinessGateResume::Discuss;
                    }
                    _ => {}
                }
            }
            _ = cancel_token.cancelled() => return BusinessGateResume::Cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionState;
    use crate::agent::workflow::create_default_workflow;

    fn engine() -> WorkflowEngine {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(session);
        engine.register_workflow(create_default_workflow());
        engine
            .activate_workflow(crate::agent::workflow::DEFAULT_WORKFLOW_ID)
            .unwrap();
        engine
    }

    #[test]
    fn arm_sets_pending() {
        let engine = engine();
        arm_findings_scope(&engine);
        assert!(is_pending_scope(&engine));
        assert!(!scope_implementation_unlocked(&engine));
    }
}
