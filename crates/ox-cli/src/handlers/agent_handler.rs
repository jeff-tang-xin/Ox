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
use ox_core::knowledge::KnowledgeEngine;
use ox_core::llm::LlmProvider;
use ox_core::memory::MemoryManager;
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};

use crate::terminal::app::{App, PendingConfirmation};
use crate::terminal::output_pane::OutputLine;
use crate::helpers;

/// Result of handling an agent event — tells the event loop what to do next.
pub enum HandleResult {
    /// Normal processing — continue the event loop.
    Normal,
    /// Interjection was processed — trigger a new agent turn.
    InterjectionTriggered {
        text: String,
        memory_ctx: String,
        turn_messages: Vec<Message>,
    },
    /// Background session completed — no further action needed.
    BackgroundDone,
}

/// Handle a single TextChunk event.
pub fn handle_text_chunk(app: &mut App, text: &str) {
    app.output.push_streaming_chunk(text);
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle a single ToolStart event.
pub fn handle_tool_start(app: &mut App, name: &str, detail: &Option<String>) {
    if detail.is_some() {
        let mut updated = false;
        for line in app.output.lines.iter_mut().rev() {
            if let OutputLine::Tool { name: n, detail: d_ref } = line {
                if *n == name {
                    *d_ref = detail.clone();
                    updated = true;
                    break;
                }
            }
        }
        if !updated {
            app.output.push_line(OutputLine::Tool {
                name: name.to_string(),
                detail: detail.clone(),
            });
        }
        app.output.invalidate_cache();
    } else {
        app.output.push_line(OutputLine::Tool {
            name: name.to_string(),
            detail: None,
        });
    }
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
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
                if let Some(content) = helpers::extract_last_file_write_content(&target_session.messages)
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
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle the TurnDone event — the most complex handler.
///
/// Parses Plan/Done blocks, persists messages, records cost, triggers
/// knowledge extraction, evaluates implicit feedback, and drains interjections.
#[allow(clippy::too_many_arguments)]
pub fn handle_turn_done(
    app: &mut App,
    session: &mut Session,
    background_session: &mut Option<Session>,
    new_messages: &[Message],
    usage: &ox_core::message::TokenUsage,
    has_provider: bool,
    rt_env: &mut RuntimeEnvironment,
    tool_registry: &Arc<ToolRegistry>,
    knowledge_engine: &Arc<tokio::sync::RwLock<KnowledgeEngine>>,
    cost_tracker: &mut CostTracker,
    model_name: &str,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    _agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    _tool_ctx: &mut Arc<ToolContext>,
    config: &OxConfig,
    memory: &Arc<MemoryManager>,
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

    app.output.finalize_streaming();

    // Determine target session (background or active)
    let target_session = background_session.as_mut().unwrap_or(session);

    // ── Parse Plan/Done blocks ──
    let mut plan_files: Vec<String> = Vec::new();
    let mut done_files: Vec<String> = Vec::new();
    for msg in new_messages {
        if let Message::Assistant { content, .. } = msg {
            // Match ## Plan
            if let Some(plan_start) = content
                .find("\n## Plan")
                .or_else(|| if content.starts_with("## Plan") { Some(0) } else { None })
            {
                let plan_start =
                    if content.starts_with("## Plan") { 0 } else { plan_start + 1 };
                let plan_text = &content[plan_start..];
                let plan_end = plan_text
                    .find("\n## Done")
                    .unwrap_or(plan_text.len());
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
            if let Some(done_start) = content
                .find("\n## Done")
                .or_else(|| if content.starts_with("## Done") { Some(0) } else { None })
            {
                let done_start =
                    if content.starts_with("## Done") { 0 } else { done_start + 1 };
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
        app.output
            .push_line(OutputLine::System(format!("{status} Done: {}", names.join(", "))));
    } else if !plan_files.is_empty() {
        app.output
            .push_line(OutputLine::System("⏳ Awaiting verification...".to_string()));
    }

    // Auto-reload skills after modifying .ox/skills/
    if done_files.iter().any(|f| f.contains(".ox/skills/")) {
        let _ = tool_registry.reload_skills(rt_env);
        let count = tool_registry.get_skills_list().len();
        app.output
            .push_system(&format!("🧠 Skills reloaded ({} skill(s) now active)", count));
    }

    // Token usage + cost display
    let total_tokens = usage.prompt_tokens + usage.completion_tokens;
    let cost_this_turn = ox_core::cost::estimate_cost(model_name, usage);
    let context_info =
        if let Some((compressed_msgs, source_count)) = compressed_cache {
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
    cost_tracker.record(model_name, usage);

    // Async: store turn summary + extract facts via KnowledgeEngine
    let knowledge_for_turn = Arc::clone(knowledge_engine);
    let new_msgs_for_store = new_messages.to_vec();
    let pid_for_store = rt_env.project_id.clone();
    let lang_for_store = rt_env.project_language.clone();
    tokio::spawn(async move {
        let mut engine = knowledge_for_turn.write().await;
        if let Some(last_user) = new_msgs_for_store.iter().rev().find_map(|m| {
            if let Message::User { content } = m {
                Some(content.as_str())
            } else {
                None
            }
        }) {
            let _ = engine.record_turn(
                "current",
                &format!(
                    "User asked: {}",
                    last_user.chars().take(200).collect::<String>()
                ),
                None,
                None,
                vec![],
                false,
            );
        }
        if let Some(summary) =
            ox_core::context::refinement::generate_memory_summary(&new_msgs_for_store)
        {
            let _ = engine.record_atomic_fact(
                &summary.format_for_storage(),
                "BestPractice",
                Some(&pid_for_store),
                &lang_for_store,
                "RefinedSummary",
            );
        }
    });

    // Implicit feedback: evaluate satisfaction
    let explicit_rate = if app.explicit_feedback_count > 0 {
        app.good_feedback_count as f64 / app.explicit_feedback_count as f64
    } else {
        0.5
    };
    let tool_success_rate = helpers::calculate_tool_success_rate(&target_session.messages);
    let code_accept_rate = app
        .ema_manager
        .get_value("code_accept_rate")
        .unwrap_or(0.8);
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
    app.status = String::new();
    app.pending_confirmation = None;
    app.message_count = session.messages.len();
    app.cost_summary = cost_tracker.summary_short();
    interrupt_ctrl.reset();
    app.ui_to_agent_tx = None;

    // Process queued interjections
    let interjections_vec: Vec<String> = interjection_buf.drain();
    if !interjections_vec.is_empty() {
        for inj_text in &interjections_vec {
            app.output
                .push_line(OutputLine::User(format!("(queued) {}", inj_text)));
        }
        if let Some(last) = interjections_vec.last() {
            let _ = session.append_message(Message::user(last));

            // Build context for interjection
            let memory_nodes =
                memory.retrieve(last, &Some(rt_env.project_id.as_str()), 5);
            let accessed_ids: Vec<&str> =
                memory_nodes.iter().map(|n| n.id.as_str()).collect();
            memory.reinforce_accessed(&accessed_ids);
            let memory_ctx = memory.format_memory_context(&memory_nodes, false);

            let turn_messages = crate::helpers::build_context_with_option(
                context_builder,
                system_prompt,
                &memory_ctx,
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
                memory_ctx,
                turn_messages,
            };
        }
    }

    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
    HandleResult::Normal
}

/// Handle Error event.
pub fn handle_error(
    app: &mut App,
    err: &str,
    background_session: &mut Option<Session>,
) {
    app.output.finalize_streaming();
    app.output.push_error(&format!("{err}"));
    if background_session.is_some() {
        *background_session = None;
    } else {
        app.agent_running = false;
        app.status = String::new();
        app.ui_to_agent_tx = None;
    }
    if !app.user_scrolled {
        app.scroll_to_bottom();
    }
    app.dirty = true;
}

/// Handle Status update event.
pub fn handle_status(app: &mut App, status: String) {
    app.status = status;
    app.dirty = true;
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
pub fn handle_budget_exceeded(
    app: &mut App,
    total_tokens: u32,
    estimated_cost: String,
) {
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
        "Agent reached {} iterations. Continue? [Y] Yes / [N] Stop",
        iteration
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
    knowledge_engine: &Arc<tokio::sync::RwLock<KnowledgeEngine>>,
) -> Option<Arc<ToolContext>> {
    use ox_core::runtime;

    let target = new_dir.display().to_string();
    match runtime::change_directory(rt_env, &target) {
        runtime::DirectoryChangeResult::Success {
            new_dir,
            project_changed,
        } => {
            app.output
                .push_line(OutputLine::System(format!("Working directory: {}", new_dir.display())));
            helpers::refresh_header_info(app, rt_env, has_provider);

            let working_dir_str = new_dir.to_string_lossy().to_string();
            if let Err(e) = session.update_working_dir(&working_dir_str) {
                tracing::warn!("Failed to update session working dir: {}", e);
            }

            let new_tool_ctx = Arc::new(ToolContext::new(
                rt_env.clone(),
                new_dir.clone(),
                Arc::new(config.clone()),
                Arc::clone(knowledge_engine),
            ));

            if project_changed {
                app.output.push_system(&format!(
                    "Project boundary changed: {}",
                    new_dir.display()
                ));
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
) {
    tracing::info!(
        "[AUTO-REFLECT] Workflow completed. Task: {}, Summary: {}",
        task_description,
        execution_summary
    );

    if let Some(llm_provider) = provider {
        let project_root = rt_env.working_dir.clone();
        match ox_core::agent::auto_reflect::AutoReflector::new(
            Arc::clone(llm_provider),
            &project_root,
        ) {
            Ok(reflector) => {
                app.output.push_line(OutputLine::System(
                    "\n🤖 Auto-reflection in progress...".to_string(),
                ));
                let conversation_history = session.messages.clone();
                let tx_clone = agent_tx.clone();
                tokio::spawn(async move {
                    match reflector
                        .reflect_on_workflow(
                            &task_description,
                            &execution_summary,
                            &conversation_history,
                        )
                        .await
                    {
                        Ok(Some(skill_id)) => {
                            let _ = tx_clone.send(AgentToUiEvent::Status(format!(
                                "✅ Skill created: {}",
                                skill_id
                            )));
                        }
                        Ok(None) => {
                            tracing::debug!(
                                "[AUTO-REFLECT] No skill generated"
                            );
                        }
                        Err(e) => {
                            tracing::error!("[AUTO-REFLECT] Reflection failed: {}", e);
                            let _ = tx_clone.send(AgentToUiEvent::Error(format!(
                                "Auto-reflection failed: {}",
                                e
                            )));
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!("[AUTO-REFLECT] Failed to initialize: {}", e);
                app.output.push_line(OutputLine::System(
                    "⚠️ Auto-reflection unavailable (initialization failed)".to_string(),
                ));
            }
        }
    } else {
        app.output.push_line(OutputLine::System(
            "⚠️ Auto-reflection unavailable (no LLM provider)".to_string(),
        ));
    }
}
