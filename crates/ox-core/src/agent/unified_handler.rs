//! Runtime handler for `complete_and_check` tool calls inside `run_agent_turn`.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::message::{Message, ToolCall};
use crate::safety::TrustManager;
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};

use super::AgentToUiEvent;
use super::business_gate;
use super::engine::WorkflowEngine;
use super::safety_gate;
use super::tool_result_envelope::{EnvelopeStatus, ToolResultEnvelope};
use super::ui_event;
use super::unified_action::{self, ActionGate, TOOL_NAME, UnifiedActionRequest, UnifiedRoute};

/// Metadata for turn memory / live update after a delegated tool call.
pub struct DelegateMeta {
    pub inner_tool: String,
    pub inner_args: String,
    pub live_output: String,
}

pub enum UnifiedHandleOutcome {
    /// Push tool_result and continue tool loop.
    Result {
        content: String,
        is_error: bool,
        deferred_system: Vec<String>,
        delegate_meta: Option<DelegateMeta>,
    },
    /// User confirmed finish — caller should TurnDone. Carries the agent's
    /// free-text final summary (if any) so the caller can persist it as an
    /// assistant message in the session, not just preview it in the UI.
    TurnDone { summary: Option<String> },
    /// Cancelled / interrupted.
    Aborted,
}

fn result_err(content: String) -> UnifiedHandleOutcome {
    UnifiedHandleOutcome::Result {
        content: ToolResultEnvelope::err(content).to_compact(),
        is_error: true,
        deferred_system: Vec::new(),
        delegate_meta: None,
    }
}

fn result_ok_envelope(
    value: serde_json::Value,
    deferred_system: Vec<String>,
    delegate_meta: Option<DelegateMeta>,
) -> UnifiedHandleOutcome {
    UnifiedHandleOutcome::Result {
        content: ToolResultEnvelope::ok(value).to_compact(),
        is_error: false,
        deferred_system,
        delegate_meta,
    }
}

pub async fn handle_complete_and_check(
    tc: &ToolCall,
    tool_registry: &ToolRegistry,
    tool_ctx: &Arc<ToolContext>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<AgentToUiEvent>,
    ),
) -> UnifiedHandleOutcome {
    let req = match unified_action::parse_request(&tc.arguments) {
        Ok(r) => r,
        Err(e) => return result_err(e),
    };

    match unified_action::route(&req) {
        UnifiedRoute::Finish => {
            handle_finish(
                tc,
                &req,
                workflow_engine,
                messages,
                ui_tx,
                ui_rx,
                cancel_token,
                push_interjection,
            )
            .await
        }
        UnifiedRoute::DelegateTool => {
            handle_delegate(
                tc,
                &req,
                tool_registry,
                tool_ctx,
                trust_manager,
                workflow_engine,
                messages,
                ui_tx,
                ui_rx,
                cancel_token,
                push_interjection,
            )
            .await
        }
        UnifiedRoute::Unknown => result_err(format!("unknown action: {}", req.action)),
    }
}

/// (Re-)show the findings scope prompt and suspend on the business gate, mapping
/// the user's choice to a handler outcome.
///
/// Used in two places:
/// 1. right after findings are stored (Path 2 of `handle_finish`), and
/// 2. when the model sends a conversational reply while in discussion mode — the
///    scope stays armed so the user can keep discussing or finally confirm `c`.
///
/// The caller is responsible for arming/storing the scope before calling this.
async fn run_findings_scope_gate<F>(
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    push_interjection: F,
) -> UnifiedHandleOutcome
where
    F: Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<AgentToUiEvent>,
    ),
{
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if let Some(store) = crate::agent::findings::load_or_migrate(&engine) {
                let md = crate::agent::presentation::format_findings_card(&store);
                let _ = ui_tx.send(AgentToUiEvent::ScopeConfirmPrompt {
                    summary: format!("{md}\n\nc 确认实施  ·  输入讨论  ·  /discuss"),
                });
            }
        }
    }

    match business_gate::await_findings_scope_gate(
        ui_rx,
        cancel_token,
        workflow_engine,
        messages,
        ui_tx,
        push_interjection,
    )
    .await
    {
        business_gate::BusinessGateResume::Cancelled => UnifiedHandleOutcome::Aborted,
        business_gate::BusinessGateResume::Acknowledged => {
            if let Some(wf) = workflow_engine {
                if let Ok(engine) = wf.try_lock() {
                    // Leaving discussion (if any) — re-enable write tools for implement.
                    crate::agent::workflow_session::clear_feedback_discuss(&engine);
                    crate::agent::phase::confirm_plan_enter_implement(&engine);
                }
            }
            UnifiedHandleOutcome::Result {
                content: ToolResultEnvelope::gate_status(
                    EnvelopeStatus::Confirmed,
                    "business",
                    json!({ "scope": "acknowledged" }),
                )
                .to_compact(),
                is_error: false,
                deferred_system: vec![
                    "✅ 范围已确认 — 进入实施。逐项 file_read → edit_file → 验证；完成后 finish（无 finding_json）。".into(),
                ],
                delegate_meta: None,
            }
        }
        business_gate::BusinessGateResume::Discuss => {
            let in_discuss = workflow_engine
                .as_ref()
                .and_then(|wf| wf.try_lock().ok())
                .is_some_and(|e| crate::agent::workflow_session::is_feedback_discuss(&e));
            let _ = ui_tx.send(AgentToUiEvent::Status("💬 讨论中...".into()));
            // In explicit discussion mode (`/discuss`) the user is having a
            // conversation — answer directly, don't loop back into findings edits.
            // Outside discuss mode a plain interjection means "refine the scope".
            let deferred = if in_discuss {
                "💬 用户在讨论模式提问/反馈：基于已掌握的上下文直接 finish(params.content=回答) 回应；\
                 不要带 finding_json、不要重新探索。回应后会回到范围确认，用户可继续讨论或 c 确认。"
            } else {
                "📋 根据用户反馈更新 finding_json，再次 finish 提交。"
            };
            UnifiedHandleOutcome::Result {
                content: ToolResultEnvelope::gate_status(
                    EnvelopeStatus::Discuss,
                    "business",
                    json!({ "scope": "discuss" }),
                )
                .to_compact(),
                is_error: false,
                deferred_system: vec![deferred.into()],
                delegate_meta: None,
            }
        }
    }
}

/// Single terminal action. Behavior decided purely by presence of `finding_json`:
/// - has finding_json → store findings, open the review gate, wait for user `c`.
///   Confirm → unlock writes and continue the SAME turn (implement). Discuss → return hint.
/// - no finding_json → show `content` in chat, END the turn, wait for new user input.
///   EXCEPTION: a content reply while in discussion mode keeps the scope gate alive.
async fn handle_finish(
    tc: &ToolCall,
    req: &UnifiedActionRequest,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<AgentToUiEvent>,
    ),
) -> UnifiedHandleOutcome {
    let content = unified_action::finish_content(&req.params);
    let review_json = unified_action::finding_json(&req.params);

    // Show free-text (analysis / answer / summary) in chat.
    if !content.is_empty() {
        let _ = ui_tx.send(AgentToUiEvent::DeliverPreview {
            tool_call_id: tc.id.clone(),
            kind: if review_json.is_some() {
                "findings"
            } else {
                "message"
            }
            .to_string(),
            content: content.clone(),
        });
    }

    // ── Path 1: no review content → LLM's explicit end. Hand the turn back to
    // the user WITHOUT locking the session. We finalize the round via
    // `complete_workflow()` (advances the step index so `is_workflow_complete()`
    // becomes true and the NEXT user message resets cleanly) instead of only
    // flipping phase to `Complete` — the latter left `is_workflow_complete()`
    // false and stranded the session in a tools-forbidden limbo. ──
    let Some(review_json) = review_json else {
        // In discussion mode with an armed scope this content is a conversational
        // reply, NOT an end-of-turn — show it (previewed above) and return to the
        // scope gate so the user can keep discussing or confirm `c`. Crucially we
        // do NOT complete the workflow here (that would strand the `c` confirm).
        let discuss_reply = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .is_some_and(|e| {
                crate::agent::workflow_session::is_feedback_discuss(&e)
                    && business_gate::is_pending_scope(&e)
            });
        if discuss_reply {
            return run_findings_scope_gate(
                workflow_engine,
                messages,
                ui_tx,
                ui_rx,
                cancel_token,
                push_interjection,
            )
            .await;
        }
        if let Some(wf) = workflow_engine {
            if let Ok(mut engine) = wf.try_lock() {
                let task = engine
                    .get_variable("_current_user_request")
                    .unwrap_or_default();
                let _ = engine.complete_workflow();
                let _ = ui_tx.send(AgentToUiEvent::WorkflowCompleted {
                    task_description: task,
                    execution_summary: content.clone(),
                });
            }
        }
        return UnifiedHandleOutcome::TurnDone {
            summary: (!content.is_empty()).then(|| content.clone()),
        };
    };

    // ── Path 2: review content present → store findings, then open the gate. ──
    let Some(wf) = workflow_engine else {
        // No workflow engine — nothing to gate against; just end.
        return UnifiedHandleOutcome::TurnDone {
            summary: (!content.is_empty()).then(|| content.clone()),
        };
    };

    {
        let Ok(engine) = wf.try_lock() else {
            return result_err("workflow engine busy".into());
        };
        crate::agent::findings::ensure_from_review_output(&engine, &review_json);
        crate::agent::phase::transition(&engine, crate::agent::phase::PhaseEvent::FindingsStored);
        business_gate::arm_findings_scope(&engine);
    }

    run_findings_scope_gate(
        workflow_engine,
        messages,
        ui_tx,
        ui_rx,
        cancel_token,
        push_interjection,
    )
    .await
}

async fn handle_delegate(
    tc: &ToolCall,
    req: &UnifiedActionRequest,
    tool_registry: &ToolRegistry,
    tool_ctx: &Arc<ToolContext>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    push_interjection: impl Fn(
        &Option<Arc<Mutex<WorkflowEngine>>>,
        &mut Vec<Message>,
        &str,
        &mpsc::UnboundedSender<AgentToUiEvent>,
    ),
) -> UnifiedHandleOutcome {
    let req =
        if let Some(redirected) = crate::agent::tool_args_repair::redirect_recall_file_path(req) {
            tracing::info!(
                "[UNIFIED] recall+path → auto-redirect to file_read: {:?}",
                redirected.params.get("path")
            );
            redirected
        } else {
            req.clone()
        };

    let inner_name = match unified_action::action_to_tool_name(&req.action) {
        Some(n) => n,
        None => {
            return result_err(format!("unknown delegate action: {}", req.action));
        }
    };

    let params_str = req.params.to_string();

    // Pre-execution validation + read guard (parity with legacy tool path).
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if let Err(e) = engine.validate_tool_call(inner_name, &req.params) {
                return result_err(e);
            }
            if let Err(e) = crate::agent::read_guard::check(inner_name, &req.params, &engine) {
                if inner_name == "file_read" {
                    if let Some(path) = req.params.get("path").and_then(|p| p.as_str()) {
                        if let Some(cached) =
                            crate::agent::read_guard::cached_file_read_response(&engine, path)
                        {
                            return result_ok_envelope(
                                json!({
                                    "action": req.action,
                                    "inner_tool": inner_name,
                                    "output": cached,
                                    "cached": true,
                                }),
                                vec![format!(
                                    "📋 ✅ file_read → {path} — 本轮已读过（返回 digest，未重复 IO）"
                                )],
                                Some(DelegateMeta {
                                    inner_tool: inner_name.to_string(),
                                    inner_args: params_str,
                                    live_output: cached.clone(),
                                }),
                            );
                        }
                    }
                }
                return result_err(e);
            }
        }
    }

    let tool = match tool_registry.get(inner_name) {
        Some(t) => t,
        None => {
            return result_err(format!("tool not registered: {inner_name}"));
        }
    };

    let safety_level = tool.safety_level();
    let gate = unified_action::gate_for_action(&req.action, safety_level);

    if gate == ActionGate::Safety {
        let path_outside = req
            .params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|path_str| {
                let resolved = tool_ctx.working_dir.join(path_str);
                !crate::safety::is_path_within_workdir(&resolved, &tool_ctx.working_dir)
            })
            .unwrap_or(false);

        let mut blacklist_warning = None;
        if inner_name == "shell_exec" {
            if let Some(cmd) = req.params.get("command").and_then(|v| v.as_str()) {
                blacklist_warning = safety_gate::shell_blacklist_warning(trust_manager, cmd);
            }
        }

        // Option-2 flow: once the user confirmed the plan/findings scope (or we're in
        // Implement phase), the whole plan is pre-approved — writes/edits/shell auto-run
        // without per-action prompts. Hard exceptions still gate: writing outside the
        // workspace, or a blacklisted (destructive) shell command.
        let scope_unlocked = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| crate::agent::business_gate::scope_implementation_unlocked(&e))
            .unwrap_or(false);

        let confirm = if scope_unlocked {
            path_outside || blacklist_warning.is_some()
        } else {
            safety_gate::needs_confirmation(
                trust_manager,
                inner_name,
                safety_level,
                path_outside,
                blacklist_warning.is_some(),
            )
        };

        if confirm {
            let args_str = req.params.to_string();
            let req_gate = safety_gate::build_request(
                tc.id.clone(),
                format!("{TOOL_NAME}/{inner_name}"),
                &args_str,
                safety_level,
                blacklist_warning,
            );
            safety_gate::emit_request(ui_tx, &req_gate);

            let decision = match safety_gate::await_decision(
                ui_rx,
                cancel_token,
                &tc.id,
                workflow_engine,
                messages,
                ui_tx,
                push_interjection,
            )
            .await
            {
                Ok(d) => d,
                Err(_) => return UnifiedHandleOutcome::Aborted,
            };

            match decision {
                ui_event::ConfirmationDecision::Deny => {
                    return UnifiedHandleOutcome::Result {
                        content: ToolResultEnvelope::gate_status(
                            EnvelopeStatus::Denied,
                            "safety",
                            json!({ "action": req.action }),
                        )
                        .to_compact(),
                        is_error: true,
                        deferred_system: Vec::new(),
                        delegate_meta: None,
                    };
                }
                ui_event::ConfirmationDecision::TrustAlways => {
                    safety_gate::apply_trust_all(trust_manager);
                }
                ui_event::ConfirmationDecision::Allow => {}
            }
        }
    }

    tracing::info!("[DELEGATE] Executing inner tool: {}", inner_name);
    let result = tool.execute(req.params.clone(), tool_ctx).await;
    tracing::info!(
        "[DELEGATE] Tool done: {} (error={}, len={})",
        inner_name,
        result.is_error,
        result.content.len()
    );

    if result.is_error {
        let mut err_text = result.content.clone();
        if inner_name == "edit_file" {
            if let Some(wf) = workflow_engine {
                if let Ok(engine) = wf.try_lock() {
                    if crate::agent::workflow_session::is_implementation_phase(&engine) {
                        if let Some(path) = req.params.get("path").and_then(|p| p.as_str()) {
                            let hint = if engine.impl_file_already_read(path) {
                                "\n\n💡 **edit 恢复：** old_string 须与上条 file_read 内容**逐字一致**（含空格/缩进）。\
                                 缩小到 3–8 行唯一片段重试；先 file_read 该文件再编辑。"
                                    .to_string()
                            } else {
                                format!(
                                    "\n\n💡 **edit 恢复：** 先 `file_read` `{path}`（实施每文件 1 次），\
                                     从返回内容复制 old_string，再 edit_file。"
                                )
                            };
                            err_text.push_str(&hint);
                        }
                    }
                }
            }
        }
        return UnifiedHandleOutcome::Result {
            content: ToolResultEnvelope::err(err_text).to_compact(),
            is_error: true,
            deferred_system: Vec::new(),
            delegate_meta: Some(DelegateMeta {
                inner_tool: inner_name.to_string(),
                inner_args: params_str,
                live_output: result.content,
            }),
        };
    }

    let mut output = result.content.clone();
    let deferred = if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            let (d, out) =
                apply_delegate_success_effects(&engine, tool_ctx, inner_name, &req, &result);
            output = out;
            d
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    result_ok_envelope(
        json!({
            "action": req.action,
            "inner_tool": inner_name,
            "output": output,
        }),
        deferred,
        Some(DelegateMeta {
            inner_tool: inner_name.to_string(),
            inner_args: params_str,
            live_output: output.clone(),
        }),
    )
}

/// Post-success hooks mirroring legacy `mod.rs` tool path.
fn apply_delegate_success_effects(
    engine: &WorkflowEngine,
    tool_ctx: &ToolContext,
    inner_name: &str,
    req: &UnifiedActionRequest,
    result: &ToolOutput,
) -> (Vec<String>, String) {
    let mut deferred = Vec::new();
    let mut output = result.content.clone();
    let params_str = req.params.to_string();
    let result_content = format!("── DATA ({inner_name}) ──\n{output}\n── END DATA ──");

    // (E) A successful code mutation puts the GitNexus index behind the tree.
    // Mark dirty so the next `code_graph` query refreshes before answering.
    if matches!(inner_name, "edit_file" | "file_write" | "delete_range") {
        if let Some(gn) = &tool_ctx.gitnexus {
            gn.mark_dirty();
        }
    }

    if inner_name == "file_read" {
        if let Some(path) = req.params.get("path").and_then(|p| p.as_str()) {
            let offset = req
                .params
                .get("offset")
                .and_then(|o| o.as_u64())
                .unwrap_or(0) as u32;
            crate::agent::read_guard::record_file_read(engine, path);
            crate::agent::tool_digest::record_read(engine, path, &result.content, offset, None);
            // Digest wrapping removed — LLM needs full file content.
        }
    } else if matches!(inner_name, "find_symbol" | "code_search") {
        crate::agent::read_guard::record_symbol_query(engine, inner_name, &req.params);
    }

    let step = engine.get_current_step_index();
    if crate::agent::exploration_snapshot::should_snapshot_for_step(step, inner_name) {
        let target =
            crate::agent::exploration_snapshot::target_from_tool_args(inner_name, &params_str);
        engine.record_exploration_result(
            &tool_ctx.working_dir,
            inner_name,
            &target,
            &result_content,
        );
    }

    let file_info = if matches!(inner_name, "file_write" | "edit_file") {
        req.params
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| format!(" → {p}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let done_label = if matches!(inner_name, "file_write" | "edit_file" | "delete_range") {
        "工具执行成功（清单是否勾选见下方进度）"
    } else {
        "已完成"
    };
    deferred.push(format!("📋 ✅ {inner_name}{file_info} — {done_label}"));

    if matches!(inner_name, "file_list" | "file_read") {
        let path = req
            .params
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or(".");
        if crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::Review {
            engine.record_explored_path(inner_name, path);
        } else if engine.is_task_step() && inner_name == "file_list" {
            engine.record_explored_path(inner_name, path);
        }
    }

    if inner_name == "file_read" && crate::agent::workflow_session::is_implementation_phase(engine)
    {
        if let Some(path) = req.params.get("path").and_then(|p| p.as_str()) {
            engine.record_impl_file_read(path, &params_str);
            if let Some(nudge) = engine.impl_edit_nudge_after_read(path, &result_content) {
                deferred.push(nudge);
            }
        }
    }
    if engine.is_task_step() {
        let (plan_changed, plan_hint) =
            engine.record_execute_tool_success(inner_name, &params_str, &result_content);
        if let Some(hint) = plan_hint {
            deferred.push(hint);
        }
        if plan_changed {
            if let Some(msg) = engine.plan_progress_message_after_tool(inner_name) {
                deferred.push(msg);
            }
        }
        if inner_name == "shell_exec"
            && crate::agent::workflow_session::is_implementation_phase(engine)
        {
            if let Some(cmd) = req.params.get("command").and_then(|c| c.as_str()) {
                let succeeded = !result.is_error;
                crate::agent::post_edit_verification::note_shell_verify_result(
                    engine, cmd, succeeded,
                );
                if succeeded {
                    let idx = engine
                        .get_plan_tracker()
                        .and_then(|t| {
                            t.steps
                                .iter()
                                .find(|s| s.awaiting_verify || !s.verify.is_empty())
                                .map(|s| s.index)
                        })
                        .or_else(|| {
                            crate::agent::findings::load_or_migrate(engine).and_then(|store| {
                                store.scoped_findings().first().map(|finding| finding.index)
                            })
                        });
                    if let Some(idx) = idx {
                        crate::agent::verifier::after_verify_pass(engine, idx);
                    }
                }
            }
        }
        if matches!(inner_name, "edit_file" | "file_write" | "delete_range")
            && crate::agent::workflow_session::is_implementation_phase(engine)
        {
            if let Some(path) = req.params.get("path").and_then(|p| p.as_str()) {
                engine.record_impl_file_edited(path);
                let idx = engine
                    .get_plan_tracker()
                    .and_then(|t| t.current_step().map(|s| s.index))
                    .unwrap_or(1);
                if let Some(note) =
                    crate::agent::verifier::after_edit_note(engine, idx, path, &result_content)
                {
                    deferred.push(note);
                }
                if let Some(verify) = engine.verify_hint_for_path(path) {
                    deferred.push(format!(
                            "📋 计划验证: `{verify}` — 请用 shell_exec 执行（需用户确认），验证通过后再继续下一项。"
                        ));
                }
            }
        }
    }

    (deferred, output)
}
