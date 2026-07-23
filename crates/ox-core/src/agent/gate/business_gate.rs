//! Business-flow gate — user confirms agent **outputs** (findings scope, plans).
//!
//! Distinct from [`super::safety_gate`] which confirms **dangerous tool execution**.
//! Both suspend the same ReAct turn on `ui_rx`; neither emits `TurnDone`.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::message::Message;

use super::super::engine::WorkflowEngine;
use super::super::findings;
use super::super::ui_event::{self, BusinessGateKind};

pub const PENDING_SCOPE_KEY: &str = "_business_gate_pending_scope";
pub const SCOPE_ACK_KEY: &str = "_business_gate_scope_ack";
/// Pre-confirmation flag set by the interjection handler before the scope gate
/// opens. Unlike SCOPE_ACK_KEY, this is NOT cleared by arm_findings_scope.
pub const PRE_ACK_KEY: &str = "_gate_pre_acked";

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
    super::super::phase::on_scope_selected(engine);
}

pub fn is_pending_scope(engine: &WorkflowEngine) -> bool {
    // If user pre-confirmed (typed "c" before gate opened), skip the gate.
    if engine.get_variable(PRE_ACK_KEY).as_deref() == Some("1") {
        return false;
    }
    // Only show scope gate if there are PENDING findings
    let has_pending = findings::load_or_migrate(engine)
        .map(|s| s.has_pending_findings())
        .unwrap_or(false);

    has_pending && engine.get_variable(PENDING_SCOPE_KEY).as_deref() == Some("1")
}

pub fn scope_implementation_unlocked(engine: &WorkflowEngine) -> bool {
    engine.get_variable(SCOPE_ACK_KEY).as_deref() == Some("1")
        || super::super::phase::get(engine) == super::super::phase::SingleFlowPhase::Implement
}

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(PENDING_SCOPE_KEY, String::new());
    engine.set_variable(SCOPE_ACK_KEY, String::new());
    engine.set_variable(PRE_ACK_KEY, String::new());
}

/// Suspend after findings until user confirms scope or discusses.
pub async fn await_findings_scope_gate(
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<super::super::AgentToUiEvent>,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<super::super::AgentToUiEvent>,
    ),
) -> BusinessGateResume {
    // If user already pre-confirmed (typed "c" before gate opened), skip wait.
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
        && engine.get_variable(PRE_ACK_KEY).as_deref() == Some("1")
    {
        engine.set_variable(PRE_ACK_KEY, String::new());
        tracing::info!("[BUSINESS_GATE] Pre-ack detected, acknowledging");
        ack_findings_scope(&engine);
        return BusinessGateResume::Acknowledged;
    }

    let _ = ui_tx.send(super::super::AgentToUiEvent::Status(
        "⏸ 业务流程门禁：等待确认 findings 范围 — 面板选 finding 后 c /confirm；可输入讨论"
            .to_string(),
    ));

    // Scan messages for a confirmation that was injected by the main loop's
    // FIX: Increased timeout and added auto-retry on timeout instead of hard cancel
    const INITIAL_TIMEOUT_SECS: u64 = 300;
    const RETRY_TIMEOUT_SECS: u64 = 600; // 10 minutes on retry
    let mut is_retry = false;

    loop {
        let timeout_duration = if is_retry {
            std::time::Duration::from_secs(RETRY_TIMEOUT_SECS)
        } else {
            std::time::Duration::from_secs(INITIAL_TIMEOUT_SECS)
        };
        let timeout = tokio::time::sleep(timeout_duration);
        tokio::pin!(timeout);

        tokio::select! {
            ev = ui_rx.recv() => {
                match ev {
                    None => return BusinessGateResume::Cancelled,
                    Some(ui_event::UiToAgentEvent::BusinessAck { kind: BusinessGateKind::FindingsScope })
                    | Some(ui_event::UiToAgentEvent::ScopeConfirmed) => {
                        if let Some(wf) = workflow_engine
                            && let Ok(engine) = wf.try_lock() {
                                ack_findings_scope(&engine);
                            }
                        return BusinessGateResume::Acknowledged;
                    }
                    Some(ui_event::UiToAgentEvent::Interjection(text)) => {
                        let t = text.trim();
                        let is_confirm = t == "c"
                            || t.starts_with("/confirm")
                            || t.starts_with("/fix")
                            || t.contains("确认")
                            || t.contains("开始实施");
                        if is_confirm {
                            if let Some(wf) = workflow_engine
                                && let Ok(engine) = wf.try_lock() {
                                    ack_findings_scope(&engine);
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
                // FIX: Instead of hard cancel, notify user and allow retry or cancel
                if is_retry {
                    // Already retried once, now really cancel
                    tracing::warn!("[BUSINESS_GATE] 超时（{}秒），自动取消", INITIAL_TIMEOUT_SECS + RETRY_TIMEOUT_SECS);
                    let _ = ui_tx.send(super::super::AgentToUiEvent::Status(
                        "⏸ 等待确认超时（10分钟），自动取消。\n请重新发起任务或输入新需求。".to_string(),
                    ));
                    return BusinessGateResume::Cancelled;
                } else {
                    // First timeout: notify user and allow retry
                    is_retry = true;
                    let _ = ui_tx.send(super::super::AgentToUiEvent::Status(
                        "⏸ 等待确认超时（5分钟），继续等待还是取消？\n输入 c 确认，或输入任意内容讨论。".to_string(),
                    ));
                    // Continue loop for retry period
                    tracing::warn!("[BUSINESS_GATE] 首次超时（300秒），进入重试等待");
                }
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
    ui_tx: &mpsc::UnboundedSender<super::super::AgentToUiEvent>,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<super::super::AgentToUiEvent>,
    ),
    kind: &str,
) -> BusinessGateResume {
    let _ = ui_tx.send(super::super::AgentToUiEvent::Status(format!(
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
                        let t = text.trim();
                        if t == "c" || t.starts_with("/confirm") || t.starts_with("/fix")
                            || t.contains("确认") || t.contains("开始实施")
                        {
                            return BusinessGateResume::Acknowledged;
                        }
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

    /// Persist one Open finding so `is_pending_scope` (which now requires a
    /// pending finding to exist) sees a real scope gate.
    fn seed_pending_finding(engine: &WorkflowEngine) {
        use crate::agent::findings::{Finding, FindingStatus, FindingsStore, Severity};
        let store = FindingsStore {
            summary: "1 issue".into(),
            findings: vec![Finding {
                index: 1,
                severity: Severity::High,
                file: "src/X.java".into(),
                symbol: "doHandle".into(),
                issue: "空指针风险".into(),
                recommendation: "加 null 检查".into(),
                fix_plan: String::new(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: Vec::new(),
        };
        findings::save(engine, &store);
    }

    #[test]
    fn arm_sets_pending() {
        let engine = engine();
        // is_pending_scope requires an actual pending finding, so seed one.
        seed_pending_finding(&engine);
        arm_findings_scope(&engine);
        assert!(is_pending_scope(&engine));
        assert!(!scope_implementation_unlocked(&engine));
    }
}