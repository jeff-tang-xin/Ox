//! Agent event handler — processes `AgentToUiEvent` variants from the agent task.
//!
//! Each event type has a dedicated handler function. The main event loop
//! dispatches to these functions rather than containing 500+ lines of inline matching.

use std::sync::Arc;
use tokio::sync::mpsc;

use ox_core::agent::AgentToUiEvent;
use ox_core::config::OxConfig;
use ox_core::context::ContextBuilder;
use ox_core::cost::CostTracker;
use ox_core::llm::LlmProvider;
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};

use crate::helpers;
use crate::terminal::app::{
    App, FindingsPanelState, ParkFollowUpTag, PendingConfirmation, PendingSkillDraft,
};
use crate::terminal::output_pane::OutputLine;

/// Cancel a running agent (if any), reset interrupt token, bump turn id. Returns the new turn id.
pub fn prepare_agent_spawn(
    app: &mut App,
    interrupt_ctrl: &mut ox_core::agent::interrupt::InterruptController,
) -> u64 {
    if app.agent_running {
        interrupt_ctrl.token().cancel();
        app.ui_to_agent_tx = None;
    }
    interrupt_ctrl.reset();
    app.agent_turn_id = app.agent_turn_id.wrapping_add(1);
    app.agent_turn_id
}

/// Result of handling an agent event — tells the event loop what to do next.
pub enum HandleResult {
    /// Normal processing — continue the event loop.
    Normal,
    /// Interjection was processed — trigger a new agent turn.
    InterjectionTriggered {
        text: String,
        turn_messages: Vec<Message>,
    },
    /// Background session completed — no further action needed.
    BackgroundDone,
}

/// Handle a single TextChunk event.
pub fn handle_text_chunk(app: &mut App, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    // Clear stale gate confirmation state — LLM is responding, gate is resolved.
    app.workflow_awaiting_confirmation = None;
    app.unified_gate = None;
    app.output.collapse_thinking();
    app.output.push_streaming_chunk(text);
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle streaming reasoning / thinking tokens.
pub fn handle_reasoning_chunk(app: &mut App, text: &str) {
    app.output.push_reasoning_chunk(text);
    app.dirty = true;
}

/// Handle a single ToolStart event.
pub fn handle_tool_start(app: &mut App, name: &str, detail: &Option<String>) {
    app.output.note_tool_activity(name);
    let (display_name, display_detail) = if name.starts_with("complete_and_check:") {
        let action = name.replace("complete_and_check:", "");
        let clean_detail = detail
            .as_ref()
            .and_then(|d| {
                let v: serde_json::Value = serde_json::from_str(d).ok()?;
                let params = v.get("params")?;
                let op = params.get("op").and_then(|o| o.as_str()).unwrap_or("");
                let path = params.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let mut parts = Vec::new();
                if !op.is_empty() {
                    parts.push(format!("op={}", op));
                }
                if !path.is_empty() {
                    parts.push(format!("path={}", path));
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(", "))
                }
            });
        (action, clean_detail)
    } else {
        (name.to_string(), detail.clone())
    };
    app.output.push_line(OutputLine::Tool {
        name: display_name,
        detail: display_detail,
    });
    app.output.invalidate_cache();
    app.scroll_to_bottom();
    app.dirty = true;
}

/// Handle a single ToolResult event.
pub fn handle_tool_result(
    app: &mut App,
    name: &str,
    output: &str,
    is_error: bool,
    target_session: &Session,
) {
    let summary = helpers::summarize_tool_result(name, output);
    app.output.push_line(OutputLine::ToolResult {
        name: name.to_string(),
        summary,
        is_error,
    });

    // Register file writes for implicit feedback tracking
    if name == "file_write" && !is_error {
        if let Some(path_str) = helpers::extract_file_path_from_output(output) {
            if let Ok(path) = std::path::PathBuf::from(&path_str).canonicalize() {
                if let Some(content) =
                    helpers::extract_last_file_write_content(&target_session.messages)
                {
                    app.override_detector.register_write(path.clone(), &content);
                    app.total_file_writes += 1;
                    tracing::debug!(
                        "[IMPLICIT FEEDBACK] Registered write: {:?}, total: {}",
                        path,
                        app.total_file_writes
                    );
                }
            }
        }
    }

    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.user_scrolled = false;
    app.dirty = true;
}

/// Handle a single ToolProgress event.
pub fn handle_tool_progress(
    app: &mut App,
    tool_call_id: String,
    tool_name: String,
    message: String,
    progress_percent: Option<u8>,
) {
    let progress_display = if let Some(percent) = progress_percent {
        format!("[{}] {} ({}%)", tool_name, message, percent)
    } else {
        format!("[{}] {}", tool_name, message)
    };
    app.output.push_tool_log(tool_call_id, progress_display);
    app.scroll_to_bottom();
    app.dirty = true;
}

/// Handle the TurnDone event — the most complex handler.
///
/// Parses Plan/Done blocks, persists messages, records cost, triggers
/// knowledge extraction, evaluates implicit feedback, and drains interjections.
#[allow(clippy::too_many_arguments)]
pub fn handle_turn_done(
    app: &mut App,
    turn_id: u64,
    session: &mut Session,
    background_session: &mut Option<Session>,
    new_messages: &[Message],
    usage: &ox_core::message::TokenUsage,
    has_provider: bool,
    rt_env: &mut RuntimeEnvironment,
    tool_registry: &Arc<ToolRegistry>,
    cost_tracker: &mut CostTracker,
    model_name: &str,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    _agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    _tool_ctx: &mut Arc<ToolContext>,
    config: &OxConfig,
    interrupt_ctrl: &mut ox_core::agent::interrupt::InterruptController,
    interjection_buf: &mut ox_core::agent::interjection::InterjectionBuffer,
    context_builder: &ContextBuilder,
    context_window: u32,
    _agent_config: &Arc<ox_core::config::AgentConfig>,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    _provider: &Option<Arc<dyn LlmProvider>>,
    system_prompt: &str,
) -> HandleResult {
    use std::collections::HashSet;

    if turn_id != 0 && turn_id != app.agent_turn_id {
        tracing::info!(
            "Ignoring stale TurnDone (id {turn_id}, current {})",
            app.agent_turn_id
        );
        return HandleResult::Normal;
    }

    app.output.finalize_streaming();

    if let Some(ref wf) = app.workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if ox_core::agent::phase::get(&engine)
                != ox_core::agent::phase::SingleFlowPhase::AwaitUser
            {
                // Already in implement / complete — don't re-open the confirm UI.
            } else {
                // Findings shown as markdown in chat — no panel.
                let card = ox_core::agent::findings::load_or_migrate(&engine)
                    .filter(|s| !s.findings.is_empty())
                    .map(|s| ox_core::agent::presentation::format_findings_card(&s));
                if let Some(text) = card {
                    app.output.push_line(OutputLine::Markdown(text));
                }
            }
        }
    }

    let interrupt_boundary = if app.workflow_interrupted {
        let boundary = if let Some(ref wf) = app.workflow_engine {
            if let Ok(mut engine) = wf.try_lock() {
                if engine.suspend_on_interrupt() {
                    let task = engine
                        .get_variable("_current_user_request")
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| "（进行中的任务）".to_string());
                    Some(ox_core::agent::user_round::format_interrupt_boundary_message(&task))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        app.clear_workflow_confirmation();
        app.output.push_system(
            "⏹️ 已中断 — 本轮未完成（不触发 Skill 反思）。继续任务直接输入；换新任务请用「新任务」或 /new。",
        );
        boundary
    } else {
        None
    };

    let project_root = rt_env.effective_project_root();
    let main_session_msgs = session.messages.clone();

    // Determine target session (background or active)
    let target_session = background_session.as_mut().unwrap_or(session);

    let onboarding_completed = ox_core::agent::onboarding::onboarding_files_complete(&project_root)
        && ox_core::agent::onboarding::is_onboarding_turn(&main_session_msgs)
        && ox_core::agent::onboarding::turn_signals_onboarding_done(new_messages);

    // ── Parse Plan/Done blocks ──
    let mut plan_files: Vec<String> = Vec::new();
    let mut done_files: Vec<String> = Vec::new();
    for msg in new_messages {
        if let Message::Assistant { content, .. } = msg {
            // Match ## Plan
            if let Some(plan_start) = content.find("\n## Plan").or_else(|| {
                if content.starts_with("## Plan") {
                    Some(0)
                } else {
                    None
                }
            }) {
                let plan_start = if content.starts_with("## Plan") {
                    0
                } else {
                    plan_start + 1
                };
                let plan_text = &content[plan_start..];
                let plan_end = plan_text.find("\n## Done").unwrap_or(plan_text.len());
                for line in plan_text[..plan_end].lines().skip(1) {
                    let t = line.trim();
                    if t.starts_with("- File:") || t.starts_with("- **File:**") {
                        let f = t
                            .trim_start_matches("- File:")
                            .trim_start_matches("- **File:**")
                            .trim()
                            .trim_matches('`');
                        plan_files.push(f.to_string());
                    }
                }
            }
            // Match ## Done
            if let Some(done_start) = content.find("\n## Done").or_else(|| {
                if content.starts_with("## Done") {
                    Some(0)
                } else {
                    None
                }
            }) {
                let done_start = if content.starts_with("## Done") {
                    0
                } else {
                    done_start + 1
                };
                let done_text = &content[done_start..];
                for line in done_text.lines().skip(1).take(6) {
                    let t = line.trim();
                    if t.starts_with("- Created:") || t.starts_with("- Modified:") {
                        let entry = t
                            .trim_start_matches("- Created:")
                            .trim_start_matches("- Modified:")
                            .trim();
                        if let Some(path) = entry.trim_matches('`').split('`').next() {
                            let path = path.split(" — ").next().unwrap_or(path).trim();
                            if !path.is_empty() {
                                done_files.push(path.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Show Plan/Done summaries
    if !plan_files.is_empty() {
        let names: Vec<_> = plan_files
            .iter()
            .map(|f| f.rsplit('/').next().unwrap_or(f))
            .collect();
        app.output
            .push_line(OutputLine::System(format!("📋 Plan: {}", names.join(", "))));
    }

    // Update plan tracking state
    use crate::terminal::app::{PlanItem, PlanItemStatus};
    app.plan_items = plan_files
        .iter()
        .map(|f| PlanItem {
            file: f.clone(),
            status: if done_files.contains(f) {
                PlanItemStatus::Done
            } else {
                PlanItemStatus::Pending
            },
        })
        .collect();
    for item in &mut app.plan_items {
        if !plan_files.contains(&item.file) && item.status == PlanItemStatus::Pending {
            item.status = PlanItemStatus::Cancelled;
        }
    }

    // Persist plan to session metadata
    let plan_data: Vec<serde_json::Value> = app
        .plan_items
        .iter()
        .map(|p| {
            serde_json::json!({
                "file": p.file,
                "status": match p.status {
                    PlanItemStatus::Done => "done",
                    PlanItemStatus::Pending => "pending",
                    PlanItemStatus::Cancelled => "cancelled",
                }
            })
        })
        .collect();
    if let Ok(json) = serde_json::to_string(&plan_data) {
        target_session.meta.plan_json = json;
    }

    helpers::refresh_header_info(app, rt_env, has_provider);

    if !done_files.is_empty() {
        let status = if !plan_files.is_empty() {
            let planned: HashSet<_> = plan_files.iter().collect();
            let done: HashSet<_> = done_files.iter().collect();
            let missing: Vec<_> = planned.difference(&done).collect();
            if missing.is_empty() { "✅" } else { "⚠️" }
        } else {
            ""
        };
        let names: Vec<_> = done_files
            .iter()
            .map(|f| f.rsplit('/').next().unwrap_or(f))
            .collect();
        app.output.push_line(OutputLine::System(format!(
            "{status} Done: {}",
            names.join(", ")
        )));
    } else if !plan_files.is_empty() {
        app.output.push_line(OutputLine::System(
            "⏳ Awaiting verification...".to_string(),
        ));
    }

    // Auto-reload skills after modifying .ox/skills/
    if done_files.iter().any(|f| f.contains(".ox/skills/")) {
        let _ = tool_registry.reload_skills(rt_env);
        let count = tool_registry.get_skills_list().len();
        app.output.push_system(&format!(
            "🧠 Skills reloaded ({} skill(s) now active)",
            count
        ));
    }

    // Token usage + cost display
    let total_tokens = usage.prompt_tokens + usage.completion_tokens;
    let cost_this_turn = ox_core::cost::estimate_cost(model_name, usage);
    let context_info = if let Some((compressed_msgs, source_count)) = compressed_cache {
        let current_total = target_session.messages.len();
        let recent_msgs = current_total.saturating_sub(*source_count);
        format!(
            " | Context: {} compressed + {} recent = {} total msgs",
            compressed_msgs.len(),
            recent_msgs,
            current_total
        )
    } else {
        let current_total = target_session.messages.len();
        format!(" | Context: {} msgs (no compression)", current_total)
    };
    app.output.push_line(OutputLine::System(format!(
        "\n💰 Token Usage: {} prompt + {} completion = {} total | Cost: ${:.4}{}",
        usage.prompt_tokens, usage.completion_tokens, total_tokens, cost_this_turn, context_info
    )));

    // Save new messages to session
    for msg in new_messages {
        if let Err(e) = target_session.append_message(msg.clone()) {
            tracing::error!("Failed to persist message: {e}");
        }
    }

    // Terminate a successfully completed round with an explicit, machine-detectable
    // boundary. Without it, a finished round left only a trail of tool results
    // (e.g. `file_read` dumps) as the tail, so on later turns the LLM could not tell
    // from message history whether that work had completed — and re-explored or
    // treated stale results as pending. Only on genuine finish (workflow complete,
    // not interrupted); timeouts/aborts leave the workflow active and get no marker.
    if interrupt_boundary.is_none() {
        let completed_task = app.workflow_engine.as_ref().and_then(|wf| {
            wf.try_lock()
                .ok()
                .filter(|e| e.is_workflow_complete())
                .map(|e| e.get_variable("_current_user_request").unwrap_or_default())
        });
        if let Some(task) = completed_task {
            let already = matches!(
                target_session.messages.last(),
                Some(Message::System { content })
                    if ox_core::agent::user_round::is_complete_boundary(content)
            );
            if !already {
                let summary = new_messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        Message::Assistant { content, .. } if !content.trim().is_empty() => {
                            Some(content.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let react_summary = String::new();
                let marker =
                    ox_core::agent::user_round::format_complete_boundary_message(&task, &summary, &react_summary);
                if let Err(e) = target_session.append_message(Message::system(marker)) {
                    tracing::error!("Failed to persist completion boundary: {e}");
                }
            }
        }
    }
    cost_tracker.record(model_name, usage);

    // KnowledgeEngine turn recording removed — memory now lives in ox_core::memory + session files.

    // Implicit feedback: evaluate satisfaction
    let explicit_rate = if app.explicit_feedback_count > 0 {
        app.good_feedback_count as f64 / app.explicit_feedback_count as f64
    } else {
        0.5
    };
    let tool_success_rate = helpers::calculate_tool_success_rate(&target_session.messages);
    let code_accept_rate = app.ema_manager.get_value("code_accept_rate").unwrap_or(0.8);
    let has_explicit = app.explicit_feedback_count >= 5;
    let _satisfaction_score = app.rollback_manager.calculate_satisfaction_score(
        explicit_rate,
        tool_success_rate,
        code_accept_rate,
        has_explicit,
    );

    // Handle background session completion
    if background_session.is_some() {
        *background_session = None;
        app.output
            .push_system("Background session completed and saved.");
        app.dirty = true;
        return HandleResult::BackgroundDone;
    }

    // Normal completion
    app.agent_running = false;
    flush_queued_skill_draft(app);

    if let Some(boundary) = interrupt_boundary {
        let _ = session.append_message(Message::system(&boundary));
    }

    if app.workflow_awaiting_confirmation.is_none() && app.pending_skill_draft.is_none() {
        app.status = String::new();
    }
    app.pending_confirmation = None;
    app.message_count = session.messages.len();
    app.cost_summary = cost_tracker.summary_short();
    interrupt_ctrl.reset();
    app.ui_to_agent_tx = None;

    // Queued interjections: buffer is fallback when live channel was unavailable
    let interjections_vec: Vec<String> = interjection_buf.drain();
    if !interjections_vec.is_empty() {
        for inj_text in &interjections_vec {
            app.output.push_line(OutputLine::System(format!(
                "💬 (fallback queued) {}",
                inj_text.trim()
            )));
        }
        if let Some(last) = interjections_vec.last() {
            let mut resume = false;
            if let Some(ref wf) = app.workflow_engine {
                if let Ok(mut engine) = wf.try_lock() {
                    if engine.is_workflow_complete() {
                        resume = engine.reopen_execute_for_fixes(last);
                    }
                    if engine.interjection_should_resume_turn(last) {
                        if !resume {
                            engine.adopt_execute_interjection(last);
                        }
                        resume = true;
                    } else if engine.workflow_preserves_on_user_input(last) {
                        engine.append_workflow_guidance(last);
                        resume = true;
                    }
                }
            }
            if resume {
                let _ = session.append_message(Message::user(last));

                let turn_messages = crate::helpers::build_context_with_option(
                    context_builder,
                    system_prompt,
                    &session.messages,
                    context_window,
                    config.context.use_refined_context,
                );

                app.scroll_to_bottom();
                app.dirty = true;
                app.message_count = session.messages.len();
                app.cost_summary = cost_tracker.summary_short();

                return HandleResult::InterjectionTriggered {
                    text: last.clone(),
                    turn_messages,
                };
            }
        }
    }

    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;

    // First-time onboarding: agent ran without workflow — reset CLI engine & skip auto-spawn
    if onboarding_completed {
        let task = ox_core::agent::onboarding::extract_onboarding_task(&main_session_msgs);
        let summary = new_messages
            .iter()
            .find_map(|m| {
                if let Message::Assistant { content, .. } = m {
                    Some(content.chars().take(1000).collect::<String>())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| task.clone());
        if let Some(ref wf) = app.workflow_engine {
            if let Ok(mut engine) = wf.try_lock() {
                if let Err(e) = ox_core::agent::onboarding::finalize_cli_workflow_after_onboarding(
                    &mut engine,
                    session,
                    &task,
                ) {
                    tracing::warn!("Onboarding workflow reset failed: {e}");
                }
            }
        }
        app.suppress_workflow_autospawn = true;
        app.workflow_display = None;
        app.output.push_system(
            "✅ 首次 Skill 创建完成。下一条消息将从 **Intent（意图识别）** 开始新任务。",
        );
        let _ = _agent_tx.send(AgentToUiEvent::WorkflowCompleted {
            task_description: task,
            execution_summary: summary,
        });
    }

    HandleResult::Normal
}

/// Handle Error event.
pub fn handle_error(app: &mut App, err: &str, background_session: &mut Option<Session>) {
    app.output.finalize_streaming();
    app.output.push_error(&format!("{err}"));
    if background_session.is_some() {
        *background_session = None;
    } else {
        app.agent_running = false;
        flush_queued_skill_draft(app);
        if app.pending_skill_draft.is_none() {
            app.status = String::new();
        }
        app.ui_to_agent_tx = None;
    }
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle Status update event.
pub fn handle_status(app: &mut App, status: String) {
    // Once the LLM request is in flight, show a live thinking row (reasoning stream or status carousel).
    if status.contains("Calling LLM") || status.contains("Thinking") {
        app.output.touch_thinking_status(&status);
    } else if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.status = status;
    app.dirty = true;
}

/// Handle formatted plan ready for user review.
pub fn handle_plan_review_ready(app: &mut App, markdown: &str) {
    app.output.finalize_streaming();
    app.output
        .push_line(OutputLine::Markdown(markdown.to_string()));
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle `complete_and_check` deliver preview — business gate (non-findings).
pub fn handle_deliver_preview(app: &mut App, tool_call_id: &str, kind: &str, content: &str) {
    app.output.finalize_streaming();
    if kind != "findings" && !content.is_empty() {
        app.output
            .push_line(OutputLine::Markdown(content.to_string()));
    }
    if kind != "findings" {
        app.unified_gate = Some(crate::terminal::app::UnifiedGatePending::Deliver {
            tool_call_id: tool_call_id.to_string(),
            kind: kind.to_string(),
        });
        app.workflow_awaiting_confirmation = Some(crate::terminal::app::UNIFIED_GATE_DELIVER_STEP);
        app.status = format!("⏸ 交付门禁 ({kind})：c 确认 · 可输入讨论");
    }
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle `complete_and_check` finish preview — user must ack end/continue.
pub fn handle_finish_preview(app: &mut App, tool_call_id: &str, summary: &str) {
    app.output.finalize_streaming();
    if !summary.is_empty() {
        app.output
            .push_line(OutputLine::Markdown(summary.to_string()));
    }
    app.unified_gate = Some(crate::terminal::app::UnifiedGatePending::Finish {
        tool_call_id: tool_call_id.to_string(),
    });
    app.workflow_awaiting_confirmation = Some(crate::terminal::app::UNIFIED_GATE_FINISH_STEP);
    app.status = "⏸ finish 门禁：f 结束本轮 · c 继续".to_string();
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Send unified deliver business ack to suspended agent.
pub fn send_unified_deliver_ack(app: &mut App) -> bool {
    use ox_core::agent::ui_event::{BusinessGateKind, UiToAgentEvent};
    if let Some(tx) = &app.ui_to_agent_tx {
        if tx
            .send(UiToAgentEvent::BusinessAck {
                kind: BusinessGateKind::Deliver,
            })
            .is_ok()
        {
            app.clear_workflow_confirmation();
            app.status = "已确认交付 — agent 继续".to_string();
            app.dirty = true;
            return true;
        }
    }
    false
}

/// Send finish gate ack (`finished` = user ends turn).
pub fn send_unified_finish_ack(app: &mut App, finished: bool) -> bool {
    use ox_core::agent::ui_event::UiToAgentEvent;
    if let Some(tx) = &app.ui_to_agent_tx {
        if tx
            .send(UiToAgentEvent::FinishAck {
                finished,
                note: None,
            })
            .is_ok()
        {
            app.clear_workflow_confirmation();
            app.status = if finished {
                "用户确认结束本轮".to_string()
            } else {
                "继续本轮 — agent 恢复".to_string()
            };
            app.dirty = true;
            return true;
        }
    }
    false
}

/// Handle workflow paused for user confirmation.
pub fn handle_workflow_awaiting_confirmation(app: &mut App, step_idx: usize, message: &str) {
    app.output.finalize_streaming();
    if !message.is_empty() {
        app.output
            .push_line(OutputLine::Markdown(message.to_string()));
    }
    app.workflow_awaiting_confirmation = Some(step_idx);
    if step_idx == 4 {
        app.park_follow_up_tag = None;
    }
    app.agent_running = false;
    flush_queued_skill_draft(app);
    app.status = match step_idx {
        0 => "❓ 请具体回答澄清问题（模糊回复会继续追问）".to_string(),
        2 => "⏸️ 请确认后开始执行（输入 ok/继续/确认，或输入修改意见）".to_string(),
        4 => "⏸️ 1-9 切换 finding · c 确认 · d 讨论 · n 新任务".to_string(),
        _ => "⏸️ 等待确认".to_string(),
    };
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Map typed menu keywords to input tag (when user types 意见/新任务 instead of 1/2/3).
pub fn park_tag_from_menu_answer(answer: &str) -> Option<ParkFollowUpTag> {
    match answer.trim() {
        "1" | "继续" | "continue" | "resume" | "执行" | "修复" => {
            Some(ParkFollowUpTag::Continue)
        }
        "2" | "意见" | "反馈" | "澄清" | "说明" | "feedback" => {
            Some(ParkFollowUpTag::Feedback)
        }
        "3" | "新任务" | "new" | "/new" => Some(ParkFollowUpTag::NewTask),
        _ => None,
    }
}

/// Build panel state from engine (no App borrow).
fn findings_panel_from_engine(
    engine: &ox_core::agent::engine::WorkflowEngine,
) -> Option<FindingsPanelState> {
    let store = ox_core::agent::findings::load_or_migrate(engine)?;
    Some(FindingsPanelState {
        summary: ox_core::agent::presentation::panel_summary(&store),
        rows: store.progress_rows(),
    })
}

// Panel removed — findings are shown as markdown in chat.
pub fn refresh_findings_panel(_app: &mut App, _engine: &ox_core::agent::engine::WorkflowEngine) {}
pub fn toggle_finding_in_panel(app: &mut App, n: u32) {
    // Just show a brief status — no panel to toggle.
    app.output.push_system(&format!("已切换 finding #{n} 范围"));
    app.dirty = true;
}

/// Engine-side gates that should keep the UI in confirmation mode.
pub fn engine_has_workflow_gate(engine: &ox_core::agent::engine::WorkflowEngine) -> bool {
    engine.is_current_step_waiting_confirmation()
}

/// True when free-text input should wait for a gate (single-step: rarely used).
pub fn workflow_input_blocked_by_gate(
    _app: &App,
    engine: &ox_core::agent::engine::WorkflowEngine,
) -> bool {
    engine.is_current_step_waiting_confirmation()
}

/// Apply a workflow slash command from the TUI (returns handled, spawn_workflow).
pub fn apply_workflow_slash(
    app: &mut App,
    text: &str,
    working_dir: &std::path::Path,
) -> (bool, bool) {
    let Some(wf) = app.workflow_engine.clone() else {
        return (false, false);
    };
    let Ok(mut engine) = wf.try_lock() else {
        return (false, false);
    };
    let parsed_cmd = ox_core::agent::workflow_command::parse(text);
    let Some(outcome) = engine.apply_workflow_command(text, Some(working_dir)) else {
        return (false, false);
    };
    use ox_core::agent::workflow_command::{CommandOutcome, WorkflowCommand};
    let mut spawn_workflow = false;
    let mut clear_confirm = false;
    let mut refresh_panel = false;
    let mut system_msgs: Vec<String> = Vec::new();
    let scope_confirmed = matches!(parsed_cmd, Some(WorkflowCommand::ConfirmScope));
    let enter_discuss = matches!(parsed_cmd, Some(WorkflowCommand::EnterDiscuss));
    let new_task = matches!(parsed_cmd, Some(WorkflowCommand::NewTask(_)));
    let panel_cmd = matches!(
        parsed_cmd,
        Some(
            WorkflowCommand::ToggleFinding(_)
                | WorkflowCommand::SelectFindings(_)
                | WorkflowCommand::SelectAllFindings
                | WorkflowCommand::ShrinkScope(_)
                | WorkflowCommand::ShowFindings
        )
    );
    match outcome {
        CommandOutcome::Applied(Some(msg)) => {
            system_msgs.push(msg);
            if scope_confirmed || new_task {
                if scope_confirmed && app.agent_running {
                    if let Some(tx) = &app.ui_to_agent_tx {
                        use ox_core::agent::ui_event::{BusinessGateKind, UiToAgentEvent};
                        let sent = tx
                            .send(UiToAgentEvent::BusinessAck {
                                kind: BusinessGateKind::FindingsScope,
                            })
                            .is_ok()
                            || tx.send(UiToAgentEvent::ScopeConfirmed).is_ok();
                        if sent {
                            spawn_workflow = false;
                        } else {
                            spawn_workflow = true;
                        }
                    } else {
                        spawn_workflow = true;
                    }
                } else {
                    spawn_workflow = true;
                }
                clear_confirm = true;
            } else if enter_discuss {
                clear_confirm = true;
            } else if panel_cmd {
                refresh_panel = true;
            }
        }
        CommandOutcome::Applied(None) | CommandOutcome::Ignored => {}
    }
    let panel = if refresh_panel {
        findings_panel_from_engine(&engine)
    } else {
        None
    };
    let mode_banner = if scope_confirmed {
        Some(ox_core::agent::phase::workspace_mode_event(&engine))
    } else {
        None
    };
    drop(engine);
    for msg in system_msgs {
        app.output.push_system(&msg);
    }
    if clear_confirm {
        app.clear_workflow_confirmation();
    } else if enter_discuss {
        enter_findings_discuss_mode(app);
    } else if panel.is_some() {
        app.park_follow_up_tag = None;
        app.workflow_awaiting_confirmation = Some(4);
    }
    if let Some((mode, banner)) = mode_banner {
        if !banner.is_empty() {
            app.output.push_system(&banner);
        }
        app.workflow_phase_line = mode;
    }
    app.dirty = true;
    (true, spawn_workflow)
}

/// Enter read-only discuss mode from findings panel (`d` / `/discuss`).
pub fn enter_findings_discuss_mode(app: &mut App) {
    app.workflow_awaiting_confirmation = None;
    app.park_follow_up_tag = Some(ParkFollowUpTag::Feedback);
    if let Some(ref wf) = app.workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            ox_core::agent::workflow_session::enter_feedback_discuss(&engine);
        }
    }
    app.output
        .push_system("💬 讨论模式（只读）— 在下方输入意见或问题，不会修改代码、不会进入实施。");
    app.dirty = true;
}

/// Enter new-task mode from findings panel (`n` / park menu 3).
pub fn enter_findings_new_task_mode(app: &mut App) {
    app.clear_workflow_confirmation();
    app.park_follow_up_tag = Some(ParkFollowUpTag::NewTask);
    app.output
        .push_system("🆕 新任务 — 在下方描述新需求（Enter 提交后将结束当前 workflow）。");
    app.dirty = true;
}

/// Park / findings menu shortcuts.
pub fn handle_park_menu_shortcut(app: &mut App, choice: char) {
    match choice {
        '1' => {
            app.workflow_awaiting_confirmation = None;
            app.findings_panel = None;
            app.park_follow_up_tag = Some(ParkFollowUpTag::Continue);
            app.output
                .push_system("🔧 继续修复 — 说明范围（如「修复 1,2」）或输入 /confirm 确认实施。");
            app.dirty = true;
        }
        '2' => enter_findings_discuss_mode(app),
        '3' => enter_findings_new_task_mode(app),
        _ => {}
    }
}

/// Handle ToolConfirmationRequest event.
pub fn handle_tool_confirmation(
    app: &mut App,
    tool_call_id: String,
    tool_name: String,
    args_summary: String,
    safety_level: ox_core::tools::SafetyLevel,
    high_risk_warning: &Option<String>,
) {
    let warning_str = high_risk_warning
        .as_ref()
        .map(|w| format!(" [{}]", w))
        .unwrap_or_default();
    app.output.push_line(OutputLine::Tool {
        name: format!(
            "Confirm {} {:?}{}: {}",
            tool_name, safety_level, warning_str, args_summary
        ),
        detail: None,
    });
    app.output.push_line(OutputLine::System(
        "  [Y] Allow / [N] Deny / [T] Trust always".to_string(),
    ));
    app.pending_confirmation = Some(PendingConfirmation {
        tool_call_id,
        tool_name,
    });
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle ToolOutputChunk event.
pub fn handle_tool_output_chunk(app: &mut App, chunk: &str) {
    app.output.push_streaming_chunk(chunk);
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle BudgetExceeded event.
pub fn handle_budget_exceeded(app: &mut App, total_tokens: u32, estimated_cost: String) {
    app.output.push_line(OutputLine::System(format!(
        "Token limit reached: {} tokens, est. cost: {}. Continue? [Y/N]",
        total_tokens, estimated_cost
    )));
    app.pending_confirmation = Some(PendingConfirmation {
        tool_call_id: "__budget__".into(),
        tool_name: "budget".into(),
    });
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle IterationLimitReached event.
pub fn handle_iteration_limit(app: &mut App, iteration: u32) {
    app.output.push_line(OutputLine::System(format!(
        "⏸️ 本轮已执行 {iteration} 步 ReAct 循环（安全上限），暂停等待确认。\n\
         • **Y** — 继续执行（计数重置，再跑一批）\n\
         • **N** — 停止本轮（可稍后输入继续修复）\n\
         （此处无 T；T 仅用于工具授权确认）"
    )));
    app.pending_confirmation = Some(PendingConfirmation {
        tool_call_id: "__iteration_limit__".into(),
        tool_name: "iteration_limit".into(),
    });
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle WorkingDirChanged event.
pub fn handle_working_dir_changed(
    app: &mut App,
    session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    new_dir: std::path::PathBuf,
    has_provider: bool,
    config: &OxConfig,
    gitnexus: Option<Arc<ox_core::mcp::GitNexusService>>,
) -> Option<Arc<ToolContext>> {
    use ox_core::runtime;

    let target = new_dir.display().to_string();
    match runtime::change_directory(rt_env, &target) {
        runtime::DirectoryChangeResult::Success {
            new_dir,
            project_changed,
        } => {
            app.output.push_line(OutputLine::System(format!(
                "Working directory: {}",
                new_dir.display()
            )));
            helpers::refresh_header_info(app, rt_env, has_provider);

            let working_dir_str = new_dir.to_string_lossy().to_string();
            if let Err(e) = session.update_working_dir(&working_dir_str) {
                tracing::warn!("Failed to update session working dir: {}", e);
            }

            let new_tool_ctx = Arc::new(
                ToolContext::new(
                    rt_env.clone(),
                    new_dir.clone(),
                    Arc::new(config.clone()),
                )
                .with_gitnexus(gitnexus),
            );

            if project_changed {
                app.output
                    .push_system(&format!("Project boundary changed: {}", new_dir.display()));
            }

            Some(new_tool_ctx)
        }
        _ => None,
    }
}

/// Handle WorkflowCompleted event.
pub fn handle_workflow_completed(
    app: &mut App,
    session: &Session,
    provider: &Option<Arc<dyn LlmProvider>>,
    rt_env: &RuntimeEnvironment,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    task_description: String,
    execution_summary: String,
    agent_config: &Arc<ox_core::config::AgentConfig>,
) {
    app.clear_workflow_confirmation();
    app.agent_running = false;
    tracing::info!(
        "[AUTO-REFLECT] Workflow completed. Task: {}, Summary: {}",
        task_description,
        execution_summary
    );

    // KnowledgeEngine episode/consolidation removed — workflow completion no longer stores memory here.

    if !agent_config.skill_reflect_enabled {
        return;
    }

    let Some(provider) = provider.clone() else {
        app.output.push_system("ℹ️ 未配置模型 — 跳过 Skill 反思");
        return;
    };

    let threshold = ox_core::agent::skill_reflect_buffer::SkillReflectBuffer::clamp_threshold(
        agent_config.skill_reflect_rounds,
    );
    let root = rt_env.effective_project_root();
    let pending = ox_core::agent::skill_reflect_buffer::SkillReflectBuffer::load(&root, threshold);
    let next_round = pending.round_count() + 1;

    app.output.push_system(&format!(
        "🧠 任务完成 — 后台提炼 Skill 经验（第 {next_round}/{threshold} 轮，每轮自动存草稿）"
    ));
    app.status = format!("🧠 Skill 反思 {next_round}/{threshold}…");
    app.dirty = true;

    let tx = agent_tx.clone();
    let messages = session.messages.clone();
    let task_desc = task_description.clone();
    let exec_summary = execution_summary.clone();
    let project_root = root.clone();
    let reflect_rounds = agent_config.skill_reflect_rounds;

    tokio::spawn(async move {
        let _ = tx.send(AgentToUiEvent::Status(
            "🧠 正在分析本次任务经验…".to_string(),
        ));

        let reflector =
            match ox_core::agent::auto_reflect::AutoReflector::new(provider, &project_root) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(AgentToUiEvent::Error(format!("Skill 反思初始化失败: {e}")));
                    return;
                }
            };

        match reflector
            .reflect_on_workflow(&task_desc, &exec_summary, &messages)
            .await
        {
            Ok(ox_core::agent::auto_reflect::ReflectOutcome::Draft {
                skill_id,
                content,
                description,
            }) => {
                let threshold =
                    ox_core::agent::skill_reflect_buffer::SkillReflectBuffer::clamp_threshold(
                        reflect_rounds,
                    );
                let mut buffer = ox_core::agent::skill_reflect_buffer::SkillReflectBuffer::load(
                    &project_root,
                    threshold,
                );
                match buffer.append_round(
                    &project_root,
                    &task_desc,
                    &skill_id,
                    &content,
                    &description,
                ) {
                    Ok((round, ready)) => {
                        let task_summary = task_desc.chars().take(60).collect::<String>();
                        let _ = tx.send(AgentToUiEvent::SkillReflectRoundSaved {
                            round,
                            threshold: buffer.threshold,
                            task_summary,
                        });
                        if ready {
                            let (merged_id, merged_content, merged_desc) =
                                buffer.build_merged_draft();
                            if let Err(e) = buffer.clear(&project_root) {
                                tracing::warn!("[SKILL-REFLECT] Failed to clear buffer: {e}");
                            }
                            let _ = tx.send(AgentToUiEvent::SkillDraftReady {
                                skill_id: merged_id,
                                content: merged_content,
                                description: merged_desc,
                            });
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(AgentToUiEvent::Error(format!(
                            "保存 Skill 反思草稿失败: {e}"
                        )));
                    }
                }
            }
            Ok(ox_core::agent::auto_reflect::ReflectOutcome::Skipped { reason }) => {
                let _ = tx.send(AgentToUiEvent::Status(format!(
                    "ℹ️ Skill 反思跳过: {reason}"
                )));
            }
            Err(e) => {
                let _ = tx.send(AgentToUiEvent::Error(format!("Skill 反思失败: {e}")));
            }
        }
    });
}

/// Present skill draft for user review (or queue if agent is busy).
pub fn present_skill_draft(app: &mut App, skill_id: String, content: String, description: String) {
    let draft = PendingSkillDraft {
        skill_id,
        content,
        description,
    };
    if app.agent_running {
        tracing::info!(
            "[SKILL-DRAFT] Agent busy — queued review for `{}`",
            draft.skill_id
        );
        app.queued_skill_draft = Some(draft);
        app.output.push_line(OutputLine::System(
            "💡 聚合 Skill 草稿已就绪 — 当前任务结束后将提示你确认保存。".into(),
        ));
        app.status = "💡 Skill 草稿待确认（任务结束后弹出）".into();
        app.dirty = true;
        return;
    }
    show_skill_draft_prompt(app, draft);
}

/// Flush queued skill draft after agent turn completes.
pub fn flush_queued_skill_draft(app: &mut App) {
    if app.agent_running {
        return;
    }
    if let Some(draft) = app.queued_skill_draft.take() {
        show_skill_draft_prompt(app, draft);
    }
}

fn show_skill_draft_prompt(app: &mut App, draft: PendingSkillDraft) {
    let skill_id = draft.skill_id.clone();
    app.output.finalize_streaming();
    app.output.push_line(OutputLine::Markdown(format!(
        "## 🧠 建议保存 Skill: `{skill_id}`\n\n{}\n\n---\n\n预览（前 800 字）：\n\n{}",
        draft.description,
        draft.content.chars().take(800).collect::<String>()
    )));
    app.output.push_line(OutputLine::System(
        "已聚合多轮任务反思。输入 **ok** / **保存** 写入 `.ox/skills/`；**取消** / **忽略** 丢弃。\n\
         直接输入新任务也会忽略此建议并继续对话。".into(),
    ));
    app.pending_skill_draft = Some(draft);
    app.status = "💡 Skill 聚合草稿 — ok 保存 / 直接输入继续".into();
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle one reflection round saved to disk.
pub fn handle_skill_reflect_round_saved(
    app: &mut App,
    round: usize,
    threshold: usize,
    task_summary: &str,
) {
    app.output.push_line(OutputLine::System(format!(
        "✅ 第 {round}/{threshold} 轮 Skill 反思已存草稿（`.ox/skills/.drafts/`）— {task_summary}"
    )));
    if round < threshold {
        app.status = format!("🧠 Skill 反思 {round}/{threshold}（草稿已保存）");
    } else {
        app.status = "💡 Skill 聚合完成 — 等待确认".into();
    }
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle skill draft ready — show preview and wait for user confirmation.
pub fn handle_skill_draft_ready(
    app: &mut App,
    skill_id: String,
    content: String,
    description: String,
) {
    present_skill_draft(app, skill_id, content, description);
}
