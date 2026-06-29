//! Runtime handler for `complete_and_check` tool calls inside `run_agent_turn`.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::message::{Message, ToolCall};
use crate::mcp::gitnexus::GraphResult;
use crate::safety::TrustManager;
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};

use super::AgentToUiEvent;
use super::business_gate;
use super::engine::WorkflowEngine;
use super::safety_gate;
use super::tool_result_envelope::{EnvelopeStatus, ToolResultEnvelope};
use super::ui_event;
use super::unified_action::{self, ActionGate, TOOL_NAME, UnifiedActionRequest, UnifiedRoute};

/// Analyze potential impact before edit_file operations using GitNexus.
/// Returns a summary of affected symbols/functions, or None if unavailable.
pub async fn analyze_edit_impact(
    tool_ctx: &Arc<ToolContext>,
    file_path: &str,
) -> Option<String> {
    let svc = tool_ctx.gitnexus.as_ref()?;
    if !svc.is_ready().await {
        return None;
    }

    // Extract function/method name from file_path if possible
    // For simplicity, we analyze the file itself as the target
    let target = file_path.split('/').last()?.split('\\').last()?;
    if target.is_empty() {
        return None;
    }

    let mut params = crate::mcp::gitnexus::ImpactParams::new(target, "downstream");
    params.max_depth = Some(2);  // Limit depth for performance

    match svc.impact(&params).await {
        Ok(result) if !result.is_error && !result.text.is_empty() => {
            // Parse and summarize the impact
            let summary = summarize_impact(&result.text, target);
            Some(summary)
        }
        _ => None,
    }
}

/// Summarize impact results into a concise message.
fn summarize_impact(impact_text: &str, target: &str) -> String {
    let mut summary = format!("📊 **代码影响分析** (`{}`):\n\n", target);

    // Try to extract key information from the impact result
    if impact_text.contains("risk") {
        if impact_text.contains("LOW") {
            summary.push_str("✅ **风险等级: LOW** - 影响范围较小\n");
        } else if impact_text.contains("MEDIUM") {
            summary.push_str("⚠️ **风险等级: MEDIUM** - 有一定影响范围\n");
        } else if impact_text.contains("HIGH") || impact_text.contains("CRITICAL") {
            summary.push_str("🔴 **风险等级: HIGH/CRITICAL** - 影响范围较大，请谨慎操作\n");
        }
    }

    // Add snippet of affected items
    let lines: Vec<&str> = impact_text.lines().take(10).collect();
    if !lines.is_empty() {
        summary.push_str("**可能影响的代码:**\n");
        for line in lines {
            let trimmed = line.trim();
            if !trimmed.is_empty() && trimmed.len() < 100 {
                summary.push_str(&format!("  • {}\n", trimmed));
            }
        }
    }

    summary
}

/// Analyze git changes using GitNexus detect_changes when finishing a task.
/// Returns a summary of affected files/processes, or None if unavailable.
pub async fn analyze_finish_changes(
    tool_ctx: &Arc<ToolContext>,
) -> Option<String> {
    let svc = tool_ctx.gitnexus.as_ref()?;
    if !svc.is_ready().await {
        return None;
    }

    let params = crate::mcp::gitnexus::DetectChangesParams::default();
    // scope defaults to "unstaged"

    match svc.detect_changes(&params).await {
        Ok(result) if !result.is_error && !result.text.is_empty() => {
            Some(summarize_changes(&result.text))
        }
        _ => None,
    }
}

/// Summarize detect_changes results into a concise message.
fn summarize_changes(changes_text: &str) -> String {
    let mut summary = "📝 **本次修改分析**:\n\n".to_string();

    // Extract key information
    let lines: Vec<&str> = changes_text.lines().take(15).collect();
    for line in &lines {
        let trimmed = line.trim();
        if !trimmed.is_empty() && trimmed.len() < 120 {
            summary.push_str(&format!("  • {}\n", trimmed));
        }
    }

    if lines.is_empty() {
        summary.push_str("  (无未暂存的修改)\n");
    }

    summary
}

/// Analyze API route impact using GitNexus route_map and shape_check.
/// This helps users understand API consumers before making changes.
/// Returns a summary of route consumers and potential mismatches, or None if unavailable.
pub async fn analyze_api_impact(
    tool_ctx: &Arc<ToolContext>,
    route: Option<&str>,
) -> Option<String> {
    let svc = tool_ctx.gitnexus.as_ref()?;
    if !svc.is_ready().await {
        return None;
    }

    // First, get route_map to find consumers
    let route_map_params = crate::mcp::gitnexus::RouteMapParams {
        route: route.map(|s| s.to_string()),
        repo: None,
    };

    let route_result = match svc.route_map(&route_map_params).await {
        Ok(r) if !r.is_error && !r.text.is_empty() => r,
        _ => return None,
    };

    // Then, get shape_check to find response mismatches
    let shape_check_params = crate::mcp::gitnexus::ShapeCheckParams {
        route: route.map(|s| s.to_string()),
        repo: None,
    };

    let shape_result: Option<GraphResult> = match svc.shape_check(&shape_check_params).await {
        Ok(r) if !r.is_error && !r.text.is_empty() => Some(r),
        _ => None,  // shape_check is optional
    };

    Some(summarize_api_impact(&route_result.text, shape_result.as_ref().map(|r| r.text.as_str())))
}

/// Summarize API impact results into a concise message.
fn summarize_api_impact(route_text: &str, shape_text: Option<&str>) -> String {
    let mut summary = String::new();

    // Parse route_map results
    if !route_text.is_empty() {
        summary.push_str("🔗 **API 路由分析**:\n\n");

        // Extract route information
        let lines: Vec<&str> = route_text.lines().take(15).collect();
        for line in lines {
            let trimmed = line.trim();
            if !trimmed.is_empty() && trimmed.len() < 100 {
                // Highlight key information
                if trimmed.contains("Handler") || trimmed.contains("Consumer") || trimmed.contains("->") {
                    summary.push_str(&format!("  → {}\n", trimmed));
                } else {
                    summary.push_str(&format!("  • {}\n", trimmed));
                }
            }
        }
    }

    // Parse shape_check results if available
    if let Some(shape) = shape_text {
        if !shape.is_empty() {
            summary.push_str("\n⚠️ **API 响应不匹配检查**:\n\n");
            let lines: Vec<&str> = shape.lines().take(10).collect();
            for line in lines {
                let trimmed = line.trim();
                if !trimmed.is_empty() && trimmed.len() < 100 {
                    if trimmed.contains("MISMATCH") || trimmed.contains("error") || trimmed.contains("missing") {
                        summary.push_str(&format!("  ⚡ {}\n", trimmed));
                    } else {
                        summary.push_str(&format!("  • {}\n", trimmed));
                    }
                }
            }
        }
    }

    if summary.is_empty() {
        summary.push_str("  (未发现路由信息)\n");
    }

    summary
}

/// Analyze if a rename operation is safe using GitNexus rename in dry-run mode.
/// Returns preview of all references that would be changed, or None if unavailable.
pub async fn preview_rename_impact(
    tool_ctx: &Arc<ToolContext>,
    symbol_name: &str,
    file_path: Option<&str>,
) -> Option<String> {
    let svc = tool_ctx.gitnexus.as_ref()?;
    if !svc.is_ready().await {
        return None;
    }

    let params = crate::mcp::gitnexus::RenameParams {
        symbol_name: Some(symbol_name.to_string()),
        new_name: format!("{}_NEW", symbol_name),  // Placeholder for preview
        file_path: file_path.map(|s| s.to_string()),
        repo: None,
        dry_run: Some(true),  // Preview only
        symbol_uid: None,
    };

    match svc.rename(&params).await {
        Ok(result) if !result.is_error && !result.text.is_empty() => {
            Some(summarize_rename_preview(&result.text, symbol_name))
        }
        _ => None,
    }
}

/// Summarize rename preview results.
fn summarize_rename_preview(rename_text: &str, original_name: &str) -> String {
    let mut summary = format!("🔍 **重命名预览** (`{}`):\n\n", original_name);

    // Count high confidence vs low confidence changes
    let graph_matches = rename_text.matches("graph").count();
    let text_matches = rename_text.matches("text_search").count();

    if graph_matches > 0 || text_matches > 0 {
        summary.push_str(&format!("📍 高可信度引用 (graph): {} 处\n", graph_matches));
        if text_matches > 0 {
            summary.push_str(&format!("⚠️  低可信度引用 (text_search): {} 处\n\n", text_matches));
        } else {
            summary.push_str("\n");
        }
    }

    // Show sample matches
    let lines: Vec<&str> = rename_text.lines().take(10).collect();
    if !lines.is_empty() {
        summary.push_str("**匹配位置:**\n");
        for line in lines {
            let trimmed = line.trim();
            if !trimmed.is_empty() && trimmed.len() < 100 {
                summary.push_str(&format!("  • {}\n", trimmed));
            }
        }
    }

    summary
}

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
                tool_ctx,
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
    tool_ctx: &Arc<ToolContext>,
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

    // ── Onboarding shortcut: no gate, no findings — just end the turn. ──
    if crate::agent::onboarding::is_onboarding_turn(messages) {
        if let Some(wf) = workflow_engine {
            if let Ok(mut engine) = wf.try_lock() {
                let _ = engine.complete_workflow();
            }
        }
        return UnifiedHandleOutcome::TurnDone {
            summary: (!content.is_empty()).then(|| content.clone()),
        };
    }

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

    // ── Analyze git changes when finishing with findings (non-blocking) ──
    if review_json.is_some() {
        // Get tool_ctx from messages if possible, or skip
        let ctx_for_analysis = tool_ctx.clone();
        tokio::spawn(async move {
            if let Some(changes) = analyze_finish_changes(&ctx_for_analysis).await {
                tracing::info!("[DETECT_CHANGES] finish analysis: {}", changes);
                // Could be sent to UI for user awareness
            }
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

    // ── Impact analysis before edit operations ──
    let mut impact_warning = String::new();
    if inner_name == "edit_file" || inner_name == "file_write" || inner_name == "delete_range" {
        if let Some(path) = req.params.get("path").and_then(|p| p.as_str()) {
            // Run impact analysis asynchronously (non-blocking)
            let ctx_clone = Arc::clone(tool_ctx);
            let path_owned = path.to_string();
            tokio::spawn(async move {
                if let Some(impact) = analyze_edit_impact(&ctx_clone, &path_owned).await {
                    tracing::info!("[IMPACT] edit impact analysis: {}", impact);
                    // The impact analysis is logged for reference
                    // Could be sent to UI if needed
                }
            });
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
