pub mod auto_reflect; // 🆕 Auto-reflection for skill generation
pub mod collaboration;
pub mod completion; // Machine-verifiable completion receipt
pub mod engine;
pub mod error_recovery; // 🆕 Build/test failure auto-fix
pub mod exploration_snapshot; // Plan-step tool results for cross-step handoff
pub mod findings; // Canonical findings store (review → park → implement)
#[cfg(test)]
mod flow_e2e;
pub mod gate; // Validation / safety / guard primitives (Done gates, business, safety, loop guards)
pub mod git_undo; // Git checkout undo per finding
pub mod intent_routing;
pub mod interjection;
pub mod interrupt;
pub mod intervention;
pub mod mod_builders;
pub mod onboarding; // Stub: auto skill generation removed, API compat only
pub mod perception; // Structured findings from perceive phase
pub mod phase; // Review → Fix → Done phase transitions
pub mod plan_tracker; // Execute-step plan progress
pub mod post_edit_verification; // AST feedback + language verify gate
pub mod presentation; // Executive summary formatting for findings
pub mod progress;
pub mod session;
pub mod skill_reflect_buffer;
pub mod task_canvas;
pub mod task_intent;
pub mod think_stream; // Route  / reasoning_content to Think pane
pub mod tool_args_repair;
pub mod tool_digest; // Semantic file_read digests
pub mod tool_executor; // 🆕 Tool detail display + error formatting
pub mod tool_graph; // Phase-aware [TOOL_ROUTE] injection
pub mod tool_result;
pub mod tool_result_envelope;
pub mod turn_state; // Typed per-turn budget (P1 rewrite scaffold)
pub mod ui_event;
pub mod unified_action;
pub mod unified_handler;
pub mod user_round; // Per-user-message round segmentation
pub mod verifier; // Post-edit read-only verifier pass
pub mod workflow;
pub mod workflow_command; // /fix /pause /confirm slash commands
pub mod workflow_guidance; // Mid-workflow user corrections without restart
pub mod workflow_session; // Park / resume persistent task session
pub mod workspace; // Single [WORKSPACE] LLM context block // Single-flow E2E integration tests

pub use engine::StepDisplayInfo;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::gate::{business_gate, explore_reflect, repeat_guard, safety_gate};
use crate::config::AgentConfig;
use crate::llm::{LlmProvider, LlmStreamEvent};
use crate::message::{Message, TokenUsage, ToolCall};
use crate::safety::TrustManager;
use crate::safety::injection;
use crate::tools::{SafetyLevel, ToolContext, ToolRegistry};
/// Callback that appends any pending interjection text as a message.
type PushInterjectionFn = fn(
    &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    &mut Vec<Message>,
    &str,
    &mpsc::UnboundedSender<AgentToUiEvent>,
);

/// Events sent from the agent to the UI.
#[derive(Debug, Clone)]
pub enum AgentToUiEvent {
    /// Streaming text from LLM.
    TextChunk(String),
    /// Streaming reasoning / thinking content (DeepSeek reasoning_content, etc.).
    ReasoningChunk(String),
    /// Agent is calling a tool.
    ToolStart {
        name: String,
        id: String,
        detail: Option<String>,
    },
    /// Tool execution result.
    ToolResult {
        name: String,
        output: String,
        is_error: bool,
    },
    /// Agent turn completed — carries new messages and accumulated token usage.
    TurnDone {
        /// Monotonic id from UI spawn; stale turns are ignored.
        turn_id: u64,
        new_messages: Vec<Message>,
        usage: TokenUsage,
    },
    /// Error during agent turn.
    Error(String),
    /// Status update (e.g. "Thinking...", "Running tool...").
    Status(String),
    /// Persistent system line for the scrollback (e.g. background GitNexus
    /// readiness). Unlike `Status`, this is appended to the transcript, not the
    /// transient bottom line.
    SystemNotice(String),
    /// Request user confirmation for tool execution.
    ToolConfirmationRequest {
        tool_call_id: String,
        tool_name: String,
        /// Argument summary (sanitized, truncated).
        args_summary: String,
        safety_level: SafetyLevel,
        /// High-risk command warning (only for shell_exec).
        high_risk_warning: Option<String>,
    },
    /// Incremental tool output chunk (for streaming tools like shell_exec).
    ToolOutputChunk { tool_call_id: String, chunk: String },
    /// Real-time tool execution progress (for long-running operations).
    ToolProgress {
        tool_call_id: String,
        tool_name: String,
        /// Progress message (e.g., "Writing chunk 3/5...")
        message: String,
        /// Optional progress percentage (0-100)
        progress_percent: Option<u8>,
    },
    /// Budget exceeded — request user confirmation to continue.
    BudgetExceeded {
        total_tokens: u32,
        estimated_cost: String,
    },
    /// Agent detected a working directory change (e.g. shell cd).
    WorkingDirChanged(std::path::PathBuf),
    /// Agent reached the iteration limit and is asking user to continue.
    IterationLimitReached { iteration: u32 },
    /// Workflow completed — trigger auto-reflection to update Skills.
    WorkflowCompleted {
        /// Task description (user's original request)
        task_description: String,
        /// Execution summary (what was done)
        execution_summary: String,
    },
    /// Workflow paused after ## Done — waiting for user follow-up in the same session.
    WorkflowParked { message: String },
    /// Formatted plan ready for user review (rendered as Markdown).
    PlanReviewReady { markdown: String },
    /// Workflow paused — waiting for user confirmation or feedback.
    WorkflowAwaitingConfirmation { step_idx: usize, message: String },
    /// Findings list after review park — user selects scope via /fix or UI.
    FindingsPanel {
        summary: String,
        rows: Vec<crate::agent::findings::FindingProgressRow>,
    },
    /// Awaiting user to confirm implementation scope (/confirm).
    ScopeConfirmPrompt { summary: String },
    /// Workspace mode changed (review / parked / impl / discuss / paused).
    WorkspaceModeChanged {
        mode: String,
        /// Banner for output pane (empty if unchanged / no transition).
        banner: String,
    },
    /// Generated skill draft awaiting user confirmation before save.
    SkillDraftReady {
        skill_id: String,
        content: String,
        description: String,
    },
    /// One workflow reflection round saved to disk (not yet asking user to confirm).
    SkillReflectRoundSaved {
        round: usize,
        threshold: usize,
        task_summary: String,
    },
    /// `complete_and_check` deliver action — preview before business gate.
    DeliverPreview {
        tool_call_id: String,
        kind: String,
        content: String,
    },
    /// `complete_and_check` finish action — awaiting user end/continue.
    FinishPreview {
        tool_call_id: String,
        summary: String,
    },
}

/// Persist in-turn tool log to workflow session (survives TurnDone → next spawn).
fn persist_turn_memory(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    turn_memory: &crate::memory::turn_memory::TurnMemory,
) {
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
    {
        engine.save_turn_memory(turn_memory);
    }
}

/// Digest a reasoning blob for re-injection: keep the head and (more important)
/// the tail, since a thought's conclusion / next-step decision is usually last.
/// `max_chars` is the total budget; under it the text is returned whole.
fn digest_reasoning(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    if chars.len() <= max_chars {
        return chars.into_iter().collect();
    }
    // Bias toward the tail: 40% head, 60% tail.
    let head_len = (max_chars * 2) / 5;
    let tail_len = max_chars.saturating_sub(head_len);
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();
    format!("{head}\n…(中间省略)…\n{tail}")
}

/// Build reasoning fallback for react_log recording.
/// In tool mode the LLM often produces no <think> tags, so we synthesise
/// a plausible "why" from the tool call arguments or the visible text.
/// This ensures react_log always carries a meaningful reasoning field
/// so subsequent turns can reconstruct the decision chain.
pub fn build_reasoning_fallback(
    reasoning_content: &str,
    tool_name: &str,
    tool_arguments: &str,
    full_text: &str,
    unified_tool_mode: bool,
) -> String {
    if !reasoning_content.trim().is_empty() {
        return reasoning_content.to_string();
    }

    // Try to extract a meaningful intent from tool arguments
    if !tool_name.is_empty() && !tool_arguments.is_empty() {
        if unified_tool_mode && tool_name == crate::agent::unified_action::TOOL_NAME {
            if let Ok(req) = crate::agent::unified_action::parse_request(tool_arguments) {
                let action = req.action;
                let target = req
                    .params
                    .get("path")
                    .or_else(|| req.params.get("name"))
                    .or_else(|| req.params.get("target"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.chars().take(80).collect::<String>())
                    .unwrap_or_else(|| "?".to_string());
                let args_preview: String = tool_arguments.chars().take(120).collect();
                return format!(
                    "(意图推断) LLM 决定执行 `{action}` 操作，目标: {target}，参数: {args_preview}"
                );
            }
        } else {
            let target_json: Option<serde_json::Value> = serde_json::from_str(tool_arguments).ok();
            let target: String = target_json
                .as_ref()
                .and_then(|v| {
                    v.get("path")
                        .or_else(|| v.get("name"))
                        .or_else(|| v.get("params"))
                })
                .map(|x| {
                    if let Some(s) = x.as_str() {
                        s.chars().take(80).collect()
                    } else {
                        x.to_string().chars().take(80).collect()
                    }
                })
                .unwrap_or_else(|| "?".to_string());
            let args_preview: String = tool_arguments.chars().take(120).collect();
            return format!(
                "(意图推断) LLM 调用 `{tool_name}`，目标: {target}，参数: {args_preview}"
            );
        }
    }

    // If no tool, try to extract from full text
    let visible = crate::agent::think_stream::visible_only(full_text);
    if !visible.trim().is_empty() {
        let preview: String = visible.chars().take(150).collect();
        return format!("(文本推断) LLM 输出文本: {preview}");
    }

    "(本轮无显式思考过程，仅工具调用)".to_string()
}

/// Deliver a user interjection into the live message list (workflow-aware).
fn push_interjection_message(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    text: &str,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) {
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
    {
        // Pin durable user constraints to the blackboard before any routing,
        // so a rule stated mid-task survives compaction and phase switches.
        if crate::memory::blackboard::looks_like_constraint(text) {
            crate::memory::blackboard::add_constraint(&engine, text);
        }
        if !engine.allows_midflight_interjection() {
            if crate::agent::workflow_session::looks_like_fix_continuation(text)
                || text.trim().starts_with("/fix")
            {
                let result = crate::agent::phase::on_user_message(&engine, text);
                notify_workspace_state_if_changed(ui_tx, &engine, &result);
                user_round::set_turn_user_input(&engine, text);
                let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                    "💬 User (Act 修复介入): {}",
                    text.trim().chars().take(120).collect::<String>()
                )));
                // Inject the last assistant message as reference so the LLM
                // doesn't need to re-read the history
                let last_assistant: String = messages
                    .iter()
                    .rev()
                    .filter_map(|m| {
                        if let Message::Assistant { content, .. } = m {
                            if !content.is_empty() {
                                Some(content.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .next()
                    .unwrap_or_default();
                let last_analysis = if last_assistant.chars().count() > 800 {
                    format!("{}…", last_assistant.chars().take(800).collect::<String>())
                } else {
                    last_assistant
                };
                let directive = if !last_analysis.is_empty() {
                    format!(
                        "【直接实施】用户要求你按上一轮的分析结果直接实施。\
                             你上一轮的分析原文:\n{} \
                             \n直接按此方案 edit_file 改代码，不要重新读文件、不要重新探索。",
                        last_analysis
                    )
                } else {
                    "【直接实施】用户要求你按上一轮的分析结果直接实施。\
                         直接 edit_file 改代码，不要重新读文件。"
                        .to_string()
                };
                messages.push(Message::system(&directive));
                return;
            }
            tracing::info!("[WORKFLOW] Blocked mid-flight interjection in Act phase");
            let _ = ui_tx.send(AgentToUiEvent::Status(String::new()));
            return;
        }
    }

    let sanitized = if injection::is_suspicious(text) {
        let result = injection::detect(text);
        let categories: Vec<String> = result
            .matches
            .iter()
            .map(|m| format!("{:?}", m.category))
            .collect();
        tracing::warn!(
            "🛡️ Prompt injection detected in interjection: categories={:?}, text={:?}",
            categories,
            text.chars().take(100).collect::<String>()
        );
        messages.push(Message::system(
            "⚠️ The following user input was sanitized for potential prompt injection:\n",
        ));
        injection::sanitize(text)
    } else {
        text.to_string()
    };

    let sanitized_for_user = sanitized.clone();
    let formatted = if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if engine.workflow_preserves_on_user_input(&sanitized)
                || crate::agent::phase::can_pivot_to_fix(&engine, &sanitized)
            {
                let result = crate::agent::phase::on_user_message(&engine, &sanitized);
                notify_workspace_state_if_changed(ui_tx, &engine, &result);
                user_round::set_turn_user_input(&engine, &sanitized);
                crate::agent::workflow_guidance::format_interjection_message(&engine, &sanitized)
            } else {
                sanitized
            }
        } else {
            sanitized
        }
    } else {
        sanitized
    };

    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
    {
        user_round::set_turn_user_input(&engine, &sanitized_for_user);
    }

    messages.push(Message::user(&formatted));
    let _ = ui_tx.send(AgentToUiEvent::Status(format!(
        "💬 User (workflow 介入): {}",
        sanitized_for_user
            .trim()
            .chars()
            .take(120)
            .collect::<String>()
    )));
}

fn notify_workspace_state(
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    engine: &crate::agent::engine::WorkflowEngine,
    result: &crate::agent::phase::TransitionResult,
) {
    let line = crate::agent::phase::workspace_status_line(engine);
    let banner = if result.changed {
        crate::agent::phase::take_pending_user_banner(engine)
    } else {
        String::new()
    };
    let _ = ui_tx.send(AgentToUiEvent::WorkspaceModeChanged { mode: line, banner });
}

fn notify_workspace_state_if_changed(
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    engine: &crate::agent::engine::WorkflowEngine,
    result: &crate::agent::phase::TransitionResult,
) {
    if result.changed {
        notify_workspace_state(ui_tx, engine, result);
    }
}

/// Run a complete agent turn: LLM -> tool_calls -> execute -> loop -> text.
///
/// Takes owned data so it can be spawned into a `tokio::spawn` task.
/// New messages produced during the turn are returned via `TurnDone`.
fn emit_turn_done(
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    turn_id: u64,
    new_messages: Vec<Message>,
    usage: TokenUsage,
) {
    tracing::info!(
        "[TURN_DONE] turn_id={}, new_messages={}, prompt_tokens={}, completion_tokens={}",
        turn_id,
        new_messages.len(),
        usage.prompt_tokens,
        usage.completion_tokens,
    );
    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
        turn_id,
        new_messages,
        usage,
    });
}

/// Capture review findings and transition to AwaitUser.
/// Returns true when the agent should suspend at the scope-confirm gate (same turn, no TurnDone).
fn try_capture_review_findings(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    full_text: &str,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> bool {
    let Some(engine_arc) = workflow_engine else {
        return false;
    };
    let Ok(engine) = engine_arc.try_lock() else {
        return false;
    };
    let phase = crate::agent::phase::get(&engine);
    let review_capture = matches!(
        phase,
        crate::agent::phase::SingleFlowPhase::Receive
            | crate::agent::phase::SingleFlowPhase::Review
    );
    if !review_capture {
        return false;
    }
    if !crate::agent::engine::WorkflowEngine::looks_like_review_report(full_text)
        && crate::agent::perception::extract_from_text(full_text).is_none()
    {
        return false;
    }
    crate::agent::findings::ensure_from_review_output(&engine, full_text);
    let result =
        crate::agent::phase::transition(&engine, crate::agent::phase::PhaseEvent::FindingsStored);
    notify_workspace_state_if_changed(ui_tx, &engine, &result);
    if let Some(store) = crate::agent::findings::load_or_migrate(&engine)
        && !store.findings.is_empty()
    {
        let _ = ui_tx.send(AgentToUiEvent::FindingsPanel {
            summary: crate::agent::presentation::panel_summary(&store),
            rows: store.progress_rows(),
        });
    }
    if result.phase == crate::agent::phase::SingleFlowPhase::AwaitUser {
        // Don't re-arm if already confirmed
        if !crate::agent::gate::business_gate::scope_implementation_unlocked(&engine) {
            crate::agent::gate::business_gate::arm_findings_scope(&engine);
            if let Some(store) = crate::agent::findings::load_or_migrate(&engine) {
                let summary = store.scope_confirm_summary();
                let _ = ui_tx.send(AgentToUiEvent::ScopeConfirmPrompt {
                    summary: summary.clone(),
                });
                let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                    "✅ 审查 findings 已记录 — {summary}\n请在面板选择范围后按 c 或 /confirm"
                )));
            } else {
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "✅ 审查 findings 已记录 — 请在面板选择范围后按 c 或 /confirm".to_string(),
                ));
            }
        }
        return true;
    }
    false
}

fn strip_tool_call_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<tool_call>") {
            in_tag = true;
            i += b"<tool_call>".len();
        } else if in_tag && bytes[i..].starts_with(b"</tool_call>") {
            in_tag = false;
            i += b"</tool_call>".len();
        } else if !in_tag {
            let ch = text[i..]
                .chars()
                .next()
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            out.push(ch);
            i += ch.len_utf8();
        } else {
            i += 1;
        }
    }
    out
}

fn extract_action_from_xml(text: &str) -> Option<String> {
    let pattern = "<arg_key>action</arg_key><arg_value>";
    let start = text.find(pattern)?;
    let value_start = start + pattern.len();
    let end = text[value_start..].find("</arg_value>")?;
    Some(text[value_start..value_start + end].to_string())
}

fn refresh_turn_memory_for_implement(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
) {
    let Some(wf) = workflow_engine else {
        return;
    };
    let Ok(engine) = wf.try_lock() else {
        return;
    };
    let task = user_round::get_turn_user_input(&engine)
        .or_else(|| engine.get_variable("_current_user_request"))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "实施修复".to_string());
    // Review → Implement is continuous: keep the in-flight turn memory (tool log +
    // decisions built during review) and only refresh the task anchor. Previously

    // this reset to a blank TurnMemory, which — combined with enter_implement's
    // clear — made the model forget everything it had just explored and re-read.
    if turn_memory.user_task.trim().is_empty() || turn_memory.user_task != task {
        turn_memory.user_task = task;
    }
    if let Some(saved) = engine.load_turn_memory() {
        turn_memory.merge_from(saved);
    }
}

/// Strip all injection blocks from previous iterations in one pass.
/// Replaces individual strip_prior_* calls in context_injector.
pub fn strip_all_injection_blocks(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        let Message::System { content } = m else {
            return true;
        };
        let c = content.as_str();
        // All known injection tags — one pass, one retain
        !(c.starts_with("[TURN_CONTEXT]")
            || c.starts_with("[TURN_MEMORY]")
            || c.starts_with("[STEP_MEMORY]")
            || c.starts_with("[USER_ROUND]")
            || c.starts_with("[DURABLE_MEMORY]")
            || c.starts_with("[TURN_INPUT]")
            || c.starts_with(crate::memory::memory_offload::MEMORY_GRAPH_TAG)
            || c.starts_with("[WORKSPACE]")
            || c.starts_with("[UNIFIED_ROUTE]")
            || c.starts_with("[TOOL_ROUTE]")
            || c.starts_with("[PHASE]")
            || c.starts_with("[PHASE_SWITCH]")
            || c.starts_with("[ROUND_MEMORY]")
            || c.starts_with("【输出纪律")
            || c.starts_with(crate::skill::policy::SKILL_ROUTE_TAG))
    });
}

// Legacy inject_slim_context + helpers moved to:
//   - crate::agent::mod_builders (build_task_anchor_block, build_workspace_block, etc.)
//   - crate::context::assembler::ContextAssembler (new unified entry point)

// Legacy code moved to mod_builders.rs and context/assembler.rs

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_turn(
    provider: Arc<dyn LlmProvider>,
    role_providers: collaboration::RoleProviders,
    mut messages: Vec<Message>,
    tool_registry: Arc<ToolRegistry>,
    tool_ctx: Arc<ToolContext>,
    ui_tx: mpsc::UnboundedSender<AgentToUiEvent>,
    mut ui_rx: mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: CancellationToken,
    trust_manager: Arc<std::sync::Mutex<TrustManager>>,
    agent_config: Arc<AgentConfig>,
    planning_mode: bool,
    workflow_engine: Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    turn_id: u64,
) {
    let unified_tool_mode = agent_config.unified_tool_mode;
    let tool_schemas = tool_registry.schemas_for_agent(unified_tool_mode);
    let mut tool_ctx = tool_ctx; // Allow reassignment on cd

    // Track new messages produced during this turn for returning to the caller.
    let mut new_messages: Vec<Message> = Vec::new();
    let mut total_usage = TokenUsage::default();

    const MAX_SAME_TOOL_CALLS: u32 = 5; // Maximum times the same tool can be called in one turn

    // Fresh symbol-search dedup each agent spawn (workflow vars may survive across sessions).
    if let Some(wf) = &workflow_engine
        && let Ok(engine) = wf.try_lock()
    {
        crate::agent::gate::read_guard::clear_symbol_queries(&engine);
    }

    // Result: user task resolved via workflow engine -> _current_user_request -> last User message
    let user_task = resolve_user_task(&workflow_engine, &messages);

    let mut turn_memory = init_turn_memory(&workflow_engine, &mut messages, user_task.as_deref());

    let mut iteration = 0u32;
    let mut budget = crate::agent::turn_state::TurnBudget::with_total_explore(init_total_explore(
        &workflow_engine,
    ));
    let mut repeat_guard = repeat_guard::RepeatGuard::new();
    let mut unified_parse_error_streak = 0u32;
    let mut findings_deliver_error_streak = 0u32;
    // Bounded recovery for API errors (e.g. ARK 400 on an oversized/malformed
    // body): trim context + retry the same iteration instead of aborting the
    // whole turn. Capped so a persistent error can't spin forever.
    let mut api_error_recovery_streak = 0u32;
    const MAX_API_ERROR_RECOVERY: u32 = 2;
    let mut tools_used_this_turn: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Hide findings JSON from UI stream during review-phase single-step turns.

    loop {
        // Check cancellation before each LLM call.
        if cancel_token.is_cancelled() {
            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
            break;
        }

        let _ = ui_tx.send(AgentToUiEvent::Status(if iteration == 0 {
            "🧠 Thinking...".to_string()
        } else {
            format!("🧠 Thinking... (iteration {})", iteration + 1)
        }));

        // Check for queued interjections before LLM call (extracted to drain_interjections_pre_llm)
        drain_interjections_pre_llm(
            &mut ui_rx,
            &workflow_engine,
            &mut messages,
            &ui_tx,
            push_interjection_message,
        );

        // Prepare context for LLM call (extracted to prepare_llm_context)
        prepare_llm_context(
            &mut messages,
            &mut turn_memory,
            &workflow_engine,
            iteration,
            user_task.as_deref().unwrap_or(""),
            unified_tool_mode,
            &tool_ctx.memory_store,
            budget.explore_streak,
            budget.total_explore,
            budget.impl_streak,
        );

        // Single-step model: always show assistant output to the user.
        let pre_llm_step_idx = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.get_current_step_index())
            .unwrap_or(0);

        // Stream LLM response -- P5.3: extracted to dispatch_llm()
        let dispatch = dispatch_llm(
            &provider,
            &role_providers,
            &workflow_engine,
            &messages,
            &tool_schemas,
            unified_tool_mode,
            planning_mode,
            iteration,
            &ui_tx,
            &cancel_token,
        )
        .await;
        let active_provider = dispatch.active_provider;
        let mut llm_rx = dispatch.llm_rx;
        let mut stream_handle = dispatch.stream_handle;

        // Collect the full response -- P5.3: extracted to collect_response()
        let collect = collect_response(
            &mut llm_rx,
            &mut stream_handle,
            &cancel_token,
            &ui_tx,
            &workflow_engine,
            &mut messages,
            &mut new_messages,
            &mut total_usage,
            turn_id,
            user_task.as_deref().unwrap_or(""),
            unified_tool_mode,
            &mut api_error_recovery_streak,
            MAX_API_ERROR_RECOVERY,
        )
        .await;
        let (full_text, reasoning_content, mut tool_calls, last_prompt_tokens) = match collect {
            LlmCollectOutcome::TurnAborted => return,
            LlmCollectOutcome::RetryIteration => continue,
            LlmCollectOutcome::Collected {
                full_text,
                reasoning_content,
                tool_calls,
                last_prompt_tokens,
            } => (full_text, reasoning_content, tool_calls, last_prompt_tokens),
        };

        // ── Unified budget offload ──
        run_memory_offload(
            last_prompt_tokens,
            &mut messages,
            &workflow_engine,
            &tool_ctx,
            &ui_tx,
            &active_provider,
        )
        .await;

        // Repair malformed / empty tool arguments (GLM empty JSON, XML hallucinations).
        repair_and_extract_tool_calls(&mut tool_calls, &full_text, &reasoning_content);

        // Review findings + business gate (extracted to handle_review_findings)
        match handle_review_findings(
            unified_tool_mode,
            &workflow_engine,
            &full_text,
            &ui_tx,
            &mut ui_rx,
            &cancel_token,
            &mut messages,
            &mut new_messages,
            &mut turn_memory,
            &mut tools_used_this_turn,
            pre_llm_step_idx,
            push_interjection_message,
        )
        .await
        {
            ReviewFindingsOutcome::Break => break,
            ReviewFindingsOutcome::Continue => continue,
            ReviewFindingsOutcome::Proceed => {}
        }

        if tool_calls.is_empty()
            && handle_empty_tool_calls(
                &full_text,
                &reasoning_content,
                &tool_ctx,
                &workflow_engine,
                user_task.as_deref(),
                &mut messages,
                &mut new_messages,
                &turn_memory,
                &ui_tx,
                turn_id,
                total_usage.clone(),
            )
            .await
        {
            return;
        }

        // Classify tool calls: detect truncated arguments and infinite loops.
        // Extracted into `classify_tool_calls()` for testability -- pure computation,
        // no side effects beyond resetting truncated arguments to `{}`.
        let classification = classify_tool_calls(&mut tool_calls, MAX_SAME_TOOL_CALLS);
        let truncated_ids = classification.truncated_ids;
        let exceeded_loop_limit_ids = classification.exceeded_loop_limit_ids;
        let tool_loop_keys = classification.tool_loop_keys;
        let temp_counts = classification.temp_counts;

        let execute_step = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.is_task_step())
            .unwrap_or(false);

        // Single-step model: always show the assistant's text to the user
        // (perception filter strips machine-only findings JSON when present).
        let display = execute_user_display(
            &workflow_engine,
            pre_llm_step_idx,
            &crate::agent::think_stream::visible_only(&full_text),
        );

        // Fold reasoning digest into content so it survives into next turn.
        let content_with_reasoning = build_content_with_reasoning(&display, &reasoning_content);
        // 🪞 Reflect-FIRST guard (extracted to check_reflection_skip)
        if check_reflection_skip(
            &tool_calls,
            unified_tool_mode,
            &workflow_engine,
            user_task.as_deref().unwrap_or(""),
            &mut budget,
            &ui_tx,
            &content_with_reasoning,
            &mut messages,
            &mut new_messages,
            &mut turn_memory,
        ) {
            iteration += 1;
            continue;
        }

        // Keep ALL tool_calls on the assistant message so every ToolResult has a matching id.
        // (Filtering caused orphaned ToolResults → API auto-fix → context amnesia.)
        let assistant_msg = Message::Assistant {
            content: content_with_reasoning,
            tool_calls: tool_calls.clone(),
            reasoning_content: None,
        };
        new_messages.push(assistant_msg.clone());
        messages.push(assistant_msg);

        record_turn_decision(
            &tool_calls,
            &full_text,
            &reasoning_content,
            unified_tool_mode,
            &mut turn_memory,
        );

        // Record LLM decision to react_log (extracted to record_llm_decision)
        record_llm_decision(
            &tool_calls,
            &tool_ctx,
            &workflow_engine,
            &user_task,
            &turn_memory,
            &full_text,
            &reasoning_content,
            unified_tool_mode,
        )
        .await;

        // ── Context Offloader: created once and reused across all tools in this iteration ──
        let mut offloader = crate::context::context_offloader::ContextOffloader::new(
            &tool_ctx.working_dir,
            &format!("session_{}", iteration),
        );

        // System notes during tool batch — deferred until all ToolResults are appended
        // (OpenAI requires Assistant.tool_calls → ToolResults with no messages between).
        let mut deferred_tool_system: Vec<String> = Vec::new();

        // Execute each tool call.
        tracing::info!(
            "[AGENT] Starting tool execution: {} tool(s) in batch",
            tool_calls.len()
        );
        for tc in &tool_calls {
            // Check cancellation before each tool execution.
            tracing::info!("[AGENT] Executing tool: {} (id={})", tc.name, tc.id);
            if cancel_token.is_cancelled() {
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "Interrupted before tool execution.".to_string(),
                ));
                break;
            }

            // Unified parse error check (extracted to check_unified_parse_error)
            match check_unified_parse_error(
                tc,
                unified_tool_mode,
                &mut messages,
                &mut new_messages,
                &mut turn_memory,
                &mut unified_parse_error_streak,
                &ui_tx,
                turn_id,
                &total_usage,
                iteration,
            ) {
                UnifiedParseOutcome::Skip => continue,
                UnifiedParseOutcome::TurnDone => return,
                UnifiedParseOutcome::Proceed => {}
            }

            // Unified handler dispatch (extracted to handle_unified_tool_call)
            match handle_unified_tool_call(
                tc,
                unified_tool_mode,
                &tool_registry,
                &tool_ctx,
                &trust_manager,
                &workflow_engine,
                &mut messages,
                &ui_tx,
                &mut ui_rx,
                &cancel_token,
                push_interjection_message,
                turn_id,
                &mut new_messages,
                &total_usage,
                iteration,
                &tool_calls,
                &mut unified_parse_error_streak,
                &mut findings_deliver_error_streak,
                &mut deferred_tool_system,
                &mut turn_memory,
                &user_task,
                &full_text,
                &reasoning_content,
            )
            .await
            {
                UnifiedDispatchOutcome::Continue => continue,
                UnifiedDispatchOutcome::TurnDone => return,
                UnifiedDispatchOutcome::NotHandled => {}
            }

            // Loop guard + truncation check (extracted to check_loop_and_truncation_guards)
            if check_loop_and_truncation_guards(
                tc,
                &exceeded_loop_limit_ids,
                &truncated_ids,
                &tool_loop_keys,
                &temp_counts,
                execute_step,
                &workflow_engine,
                &mut messages,
                &mut new_messages,
                &mut turn_memory,
                &user_task,
                &full_text,
                &reasoning_content,
                unified_tool_mode,
                tool_ctx.memory_store.as_ref(),
                &ui_tx,
            ) {
                continue;
            }
            let _ = ui_tx.send(AgentToUiEvent::Status(format!("Running tool: {}", tc.name)));

            // Workflow validation (extracted to check_workflow_validation)
            if check_workflow_validation(
                tc,
                &workflow_engine,
                &mut messages,
                &mut new_messages,
                &mut turn_memory,
                unified_tool_mode,
                &ui_tx,
            )
            .await
            {
                continue;
            }

            // Send detailed ToolStart for UI display
            let tool_detail = tool_executor::extract_tool_detail(&tc.name, &tc.arguments);
            // Always send ToolStart to UI (detail is optional)
            let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                name: tc.name.clone(),
                id: tc.id.clone(),
                detail: tool_detail,
            });

            // Tool registry lookup (extracted to lookup_tool_or_error)
            let tool = match lookup_tool_or_error(
                tc,
                &tool_registry,
                &mut messages,
                &mut new_messages,
                &ui_tx,
            ) {
                Some(t) => t,
                None => continue,
            };

            // ── Safety check before execution ──
            match check_safety_gate(
                tc,
                tool,
                &tool_ctx,
                &trust_manager,
                &workflow_engine,
                &cancel_token,
                &ui_tx,
                &mut ui_rx,
                &mut messages,
                &mut new_messages,
                &user_task,
                &full_text,
                &reasoning_content,
                unified_tool_mode,
                turn_id,
                &total_usage,
            )
            .await
            {
                SafetyGateOutcome::Allow => {}
                SafetyGateOutcome::Skip => continue,
                SafetyGateOutcome::TurnDone => return,
            }

            // Parse tool arguments (extracted to parse_tool_args)
            let args = match parse_tool_args(tc, &mut messages, &mut new_messages, &ui_tx) {
                Ok(v) => v,
                Err(()) => continue,
            };
            // Check for queued interjections before tool execution.
            drain_interjections_pre_tool(
                &mut ui_rx,
                &workflow_engine,
                &mut messages,
                &ui_tx,
                push_interjection_message,
            );

            // file_write path validation (extracted to check_file_write_missing_path)
            if check_file_write_missing_path(tc, &args, &mut messages, &mut new_messages, &ui_tx) {
                continue;
            }

            // Execute tool with retry (extracted to execute_tool_with_retry)
            let result = execute_tool_with_retry(tc, &args, tool, &tool_ctx, &ui_tx).await;
            // Log + sanitize + update working dir (extracted to post_tool_log_and_sanitize)
            let (sanitized_content, new_tool_ctx) =
                post_tool_log_and_sanitize(tc, &args, &result, &tool_ctx, &ui_tx);
            if let Some(ctx) = new_tool_ctx {
                tool_ctx = Arc::new(ctx);
            }

            // Offload + notify + record decision (extracted to offload_and_record)
            let offloaded = offload_and_record(
                tc,
                &result,
                &sanitized_content,
                iteration as usize,
                &mut offloader,
                &mut turn_memory,
                &ui_tx,
            );

            // Record to SQLite react_log (extracted to record_react_log)
            record_react_log(
                tc,
                &result,
                &tool_ctx,
                &workflow_engine,
                &user_task,
                &turn_memory,
                &full_text,
                &reasoning_content,
                unified_tool_mode,
            )
            .await;

            let mut result_content = format!(
                "── DATA ({}) ──\n{}\n── END DATA ──",
                tc.name,
                offloaded.to_context_message()
            );

            // Record read/symbol queries to workflow engine (extracted to record_read_queries)
            record_read_queries(tc, &result, &workflow_engine);

            // Snapshot + record tool result to turn memory (extracted to snapshot_and_record_turn)
            snapshot_and_record_turn(
                tc,
                &result,
                &result_content,
                &tool_ctx.working_dir,
                &workflow_engine,
                &mut turn_memory,
            );

            // Verify shell result + edit_file error hint (extracted to post_verify_and_hint)
            post_verify_and_hint(
                tc,
                &result,
                &sanitized_content,
                &workflow_engine,
                &mut result_content,
            );

            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: result_content.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);

            // Status log + plan tracker + verify-after-edit (extracted to post_success_updates)
            post_success_updates(
                tc,
                &result,
                &result_content,
                &workflow_engine,
                unified_tool_mode,
                &mut deferred_tool_system,
                &mut tools_used_this_turn,
            );
        } // end for tc

        if post_batch_processing(
            &mut deferred_tool_system,
            &mut messages,
            &mut new_messages,
            &mut offloader,
            &tool_calls,
            &workflow_engine,
            &tool_ctx,
            unified_tool_mode,
            &mut turn_memory,
            &ui_tx,
            turn_id,
            &mut total_usage,
            &mut repeat_guard,
            &full_text,
        ) {
            return;
        }

        // Loop back to call LLM again with tool results.
        persist_turn_memory(&workflow_engine, &turn_memory);
        iteration += 1;
    }

    persist_turn_memory(&workflow_engine, &turn_memory);
    // Loop exited via break (cancellation or user declined to continue).
    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
}

/// Post-batch processing after all tool calls in an iteration have completed.
///
/// Handles: deferred system messages, orphan pruning, canvas injection,
/// post-edit checks, error recovery, repeated-failure hand-off, offloader cleanup,
/// and repeat guard.
///
/// Returns `true` if the turn should end (caller must `return`).
#[allow(clippy::too_many_arguments)]
fn post_batch_processing(
    deferred_tool_system: &mut Vec<String>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    offloader: &mut crate::context::context_offloader::ContextOffloader,
    tool_calls: &[ToolCall],
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    tool_ctx: &Arc<ToolContext>,
    unified_tool_mode: bool,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    turn_id: u64,
    total_usage: &mut TokenUsage,
    repeat_guard: &mut repeat_guard::RepeatGuard,
    full_text: &str,
) -> bool {
    for note in std::mem::take(deferred_tool_system) {
        messages.push(Message::system(&note));
    }

    // 鈹€鈹€ Post-hoc fix: remove orphaned tool_calls from latest Assistant msg 鈹€鈹€
    prune_orphaned_tool_calls(&mut [messages, new_messages]);

    // 馃椇锔?Inject task canvas if any results were offloaded
    if let Some(canvas_ctx) = offloader.get_canvas_context() {
        messages.push(Message::system(&canvas_ctx));
    }

    // 馃毃 AST recovery + verify hints + Done reminder
    if !tool_calls.is_empty() {
        run_post_edit_checks(
            tool_calls,
            messages,
            new_messages,
            workflow_engine,
            tool_ctx,
            unified_tool_mode,
        );

        // 馃攧 Auto-fix: if build/test failed, inject error for self-repair
        error_recovery::check_and_recover(
            messages,
            new_messages,
            tool_calls,
            tool_ctx.gitnexus.as_ref(),
        );

        // Repeated-failure hand-off
        if check_repeated_failure_handoff(
            workflow_engine,
            messages,
            new_messages,
            turn_memory,
            ui_tx,
            turn_id,
            total_usage,
        ) {
            return true;
        }
    }

    // Clean up old offloaded refs, keeping at most the 50 most recent ones.
    if let Err(e) = offloader.cleanup_old_refs(50) {
        tracing::warn!("Failed to clean up old refs: {}", e);
    }

    // Repeat guard
    if check_repeat_guard(
        repeat_guard,
        full_text,
        messages,
        new_messages,
        turn_memory,
        workflow_engine,
        ui_tx,
        turn_id,
        total_usage,
    ) {
        return true;
    }

    false
}

/// Resolve the user task from three fallback sources (in priority order):
/// 1. `user_round::get_turn_user_input` (explicit turn-scoped input)
/// 2. `_current_user_request` workflow variable
/// 3. Last `Message::User` in the conversation
///
/// Returns `None` if all sources are empty.
fn resolve_user_task(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &[Message],
) -> Option<String> {
    workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .and_then(|e| user_round::get_turn_user_input(&e))
        .or_else(|| {
            workflow_engine
                .as_ref()
                .and_then(|wf| wf.try_lock().ok())
                .and_then(|e| e.get_variable("_current_user_request"))
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            messages.iter().rev().find_map(|m| {
                if let Message::User { content } = m {
                    Some(content.clone())
                } else {
                    None
                }
            })
        })
}

/// Initialize TurnMemory and inject saved state + memory blocks into messages.
///
/// Side effects: mutates `messages` by injecting user_round and durable memory blocks.
/// Also resets gate failures and verify failures for the new turn.
fn init_turn_memory(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    user_task: Option<&str>,
) -> crate::memory::turn_memory::TurnMemory {
    let mut turn_memory = crate::memory::turn_memory::TurnMemory::new(user_task.unwrap_or(""));
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            crate::agent::gate::reset_failures(&engine);
            post_edit_verification::reset_verify_failures(&engine);
            if let Some(saved) = engine.load_turn_memory() {
                turn_memory.merge_from(saved);
            }
            let block = engine.user_round_memory_block();
            if !block.is_empty() {
                user_round::inject_user_round(messages, &block);
            }
            let block = engine.durable_memory_block();
            if !block.is_empty() {
                crate::memory::memory_bridge::inject_durable_memory(messages, &block);
            }
        } else {
            tracing::warn!(
                "[run_agent_turn] Failed to acquire workflow_engine lock for memory injection"
            );
        }
    }
    turn_memory
}

/// Load `total_explore` counter from the workflow engine, resetting to 0
/// if a new task is detected.
fn init_total_explore(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
) -> u32 {
    workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| {
            let val = e.get_counter("_total_explore");
            let user_req = e.get_variable("_current_user_request").unwrap_or_default();
            if crate::agent::workflow_session::looks_like_new_task(&user_req) {
                e.set_counter("_total_explore", 0);
                tracing::info!("[EXPLORE_RESET] total_explore reset to 0 (new task detected)");
                0u32
            } else {
                val
            }
        })
        .unwrap_or(0)
}

// ─── P5.3: Extracted from run_agent_turn ───

/// Output of `dispatch_llm` -- everything the caller needs to collect the stream.
struct LlmDispatch {
    active_provider: Arc<dyn LlmProvider>,
    stream_handle: tokio::task::JoinHandle<()>,
    llm_rx: mpsc::UnboundedReceiver<LlmStreamEvent>,
}

/// Outcome of `collect_response` -- tells the caller how to proceed.
enum LlmCollectOutcome {
    /// Normal completion with full response.
    Collected {
        full_text: String,
        reasoning_content: String,
        tool_calls: Vec<ToolCall>,
        last_prompt_tokens: u32,
    },
    /// API client error (400/413/422) -- context was trimmed, retry this iteration.
    RetryIteration,
    /// Fatal error or timeout -- TurnDone already emitted, caller should return.
    TurnAborted,
}

/// P5.3: Select active provider, filter tool schemas, and spawn the LLM stream task.
///
/// Extracted verbatim from `run_agent_turn` lines 868-974.
#[allow(clippy::too_many_arguments)]
async fn dispatch_llm(
    provider: &Arc<dyn LlmProvider>,
    role_providers: &collaboration::RoleProviders,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &[Message],
    tool_schemas: &[crate::llm::ToolSchema],
    unified_tool_mode: bool,
    planning_mode: bool,
    iteration: u32,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    cancel_token: &CancellationToken,
) -> LlmDispatch {
    let (llm_tx, llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

    let active_provider = if let Some(engine_arc) = workflow_engine {
        let engine = engine_arc.lock().await;
        let picked = role_providers.pick(provider, &engine);
        if role_providers.enabled {
            let role = role_providers.role_label(&engine);
            let name = picked.model_name();
            if name != provider.model_name() {
                let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                    "🤝 协作模型 [{role}]: {name}"
                )));
            }
        }
        picked
    } else {
        provider.clone()
    };

    let provider_clone = Arc::clone(&active_provider);
    let msgs = messages.to_vec();

    // Filter tool schemas based on current workflow step
    let workflow_blocks_planning = if let Some(engine_arc) = workflow_engine {
        engine_arc.lock().await.is_workflow_active()
    } else {
        false
    };

    let schemas: Vec<_> = if unified_tool_mode {
        if planning_mode && iteration == 0 && !workflow_blocks_planning {
            vec![]
        } else if let Some(engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;
            if !engine.allows_tool_execution() {
                Vec::new()
            } else {
                crate::agent::unified_action::unified_tool_schemas_for_engine(&engine)
            }
        } else {
            crate::agent::unified_action::unified_tool_schemas()
        }
    } else if planning_mode && iteration == 0 && !workflow_blocks_planning {
        vec![]
    } else if let Some(engine_arc) = workflow_engine {
        let engine = engine_arc.lock().await;
        if !engine.allows_tool_execution() {
            Vec::new()
        } else if engine.is_single_step() {
            let allowed = crate::agent::tool_graph::allowed_tool_names(&engine);
            crate::agent::tool_graph::filter_tool_schemas(tool_schemas, &allowed)
        } else {
            tool_schemas.to_vec()
        }
    } else {
        tool_schemas.to_vec()
    };

    // 📝 LOG REQUEST CONTEXT (debug level - expensive, iterates all messages)
    tracing::debug!("\n{}", "=".repeat(80));
    tracing::debug!("🤖 LLM REQUEST CONTEXT (Iteration {})", iteration + 1);
    tracing::debug!("{}", "=".repeat(80));
    tracing::debug!("Total messages: {}", msgs.len());

    // Show system prompt preview (debug level)
    if let Some(first_msg) = msgs.first()
        && let Message::System { content } = first_msg
    {
        tracing::debug!(
            "📋 SYSTEM PROMPT LENGTH: {} characters",
            content.chars().count()
        );
    }
    tracing::debug!(
        "Enabled tools: {}",
        schemas
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    tracing::debug!("{}", "=".repeat(80));

    let mut llm_opts = crate::llm::StreamOptions::default();
    if !schemas.is_empty() {
        llm_opts.tool_choice = Some(crate::llm::ToolChoice::Auto);
        llm_opts.parallel_tool_calls = Some(true);
    }
    let cancel_clone = cancel_token.clone();
    let llm_tx_err = llm_tx.clone();
    let schemas_clone = schemas.clone();
    let stream_handle = tokio::spawn(async move {
        tokio::select! {
            result = provider_clone.stream_chat(&msgs, &schemas_clone, llm_tx, llm_opts) => {
                if let Err(e) = result {
                    tracing::error!("LLM stream error: {e}");
                    let _ = llm_tx_err.send(LlmStreamEvent::Error(format!("Stream failed: {e}")));
                }
            }
            _ = cancel_clone.cancelled() => {}
        }
    });

    LlmDispatch {
        active_provider,
        stream_handle,
        llm_rx,
    }
}

/// P5.3: Collect the LLM streamed response (text + reasoning + tool_calls).
///
/// Extracted verbatim from `run_agent_turn` lines 976-1198.
/// Returns `Collected` on normal completion, `RetryIteration` for API error
/// recovery (caller should `continue` the loop), or `TurnAborted` if TurnDone
/// was already emitted (caller should `return`).
#[allow(clippy::too_many_arguments)]
async fn collect_response(
    llm_rx: &mut mpsc::UnboundedReceiver<LlmStreamEvent>,
    stream_handle: &mut tokio::task::JoinHandle<()>,
    cancel_token: &CancellationToken,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    total_usage: &mut TokenUsage,
    turn_id: u64,
    user_task: &str,
    unified_tool_mode: bool,
    api_error_recovery_streak: &mut u32,
    max_api_error_recovery: u32,
) -> LlmCollectOutcome {
    const LLM_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

    let mut full_text = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut current_tool_args: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let use_findings_stream = review_stream_filter_inner(workflow_engine);
    let mut findings_stream =
        use_findings_stream.then(crate::agent::perception::FindingsStreamFilter::new);
    let mut think_stream = crate::agent::think_stream::ThinkTagStreamFilter::new();
    let mut last_stream_completion_tokens = 0u32;
    let mut last_prompt_tokens = 0u32;

    while let Some(event) = tokio::select! {
        ev = llm_rx.recv() => ev,
        _ = cancel_token.cancelled() => {
            tracing::warn!("[AGENT] ⚠️ Cancellation token triggered, stopping LLM stream");
            None
        }
        _ = tokio::time::sleep(LLM_RESPONSE_TIMEOUT) => {
            tracing::error!(
                "[AGENT] ⏱️ LLM response timed out after {:?}",
                LLM_RESPONSE_TIMEOUT
            );
            stream_handle.abort();
            let _ = ui_tx.send(AgentToUiEvent::Status(
                "⏱️ LLM 响应超时 (180s) - 请重试或简化请求".to_string(),
            ));
            let boundary = crate::agent::user_round::format_interrupt_boundary_message(user_task);
            new_messages.push(crate::message::Message::system(&boundary));
            messages.push(crate::message::Message::system(&boundary));
            emit_turn_done(ui_tx, turn_id, std::mem::take(new_messages), total_usage.clone());
            return LlmCollectOutcome::TurnAborted;
        }
    } {
        match event {
            LlmStreamEvent::TextDelta(text) => {
                let (reasoning_delta, visible_delta) = think_stream.push(&text);
                if let Some(r) = reasoning_delta.filter(|s| !s.is_empty()) {
                    reasoning_content.push_str(&r);
                    let _ = ui_tx.send(AgentToUiEvent::ReasoningChunk(r));
                }
                let visible_piece = visible_delta.unwrap_or_default();
                let clean_visible = strip_tool_call_xml(&visible_piece);
                if clean_visible.len() < visible_piece.len()
                    && let Some(action) = extract_action_from_xml(&visible_piece)
                {
                    let _ = ui_tx.send(AgentToUiEvent::Status(format!("🔄 {} ...", action)));
                }
                if let Some(ref mut filter) = findings_stream {
                    if let Some(visible) = filter.push(&clean_visible)
                        && !visible.is_empty()
                    {
                        let _ = ui_tx.send(AgentToUiEvent::TextChunk(visible));
                    }
                } else if !clean_visible.is_empty() {
                    let _ = ui_tx.send(AgentToUiEvent::TextChunk(clean_visible));
                }
                full_text.push_str(&text);
            }
            LlmStreamEvent::ReasoningDelta(text) => {
                reasoning_content.push_str(&text);
                let _ = ui_tx.send(AgentToUiEvent::ReasoningChunk(text));
            }
            LlmStreamEvent::ToolCallStart { id, name } => {
                tracing::debug!("[AGENT] LLM requested tool: {} (id={})", name, id);
                if unified_tool_mode {
                    let tool_display = if name == crate::agent::unified_action::TOOL_NAME {
                        "准备执行...".to_string()
                    } else {
                        name.clone()
                    };
                    let _ = ui_tx.send(AgentToUiEvent::Status(format!("🔄 {tool_display}")));
                }
                current_tool_args.insert(id.clone(), String::new());
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: String::new(),
                });
            }
            LlmStreamEvent::ToolCallArgumentsDelta { id, delta } => {
                if let Some(args) = current_tool_args.get_mut(&id) {
                    let was_empty = args.is_empty();
                    args.push_str(&delta);
                    if was_empty
                        && unified_tool_mode
                        && let Ok(action) = serde_json::from_str::<
                            crate::agent::unified_action::UnifiedActionRequest,
                        >(args)
                    {
                        let _ =
                            ui_tx.send(AgentToUiEvent::Status(format!("🔄 {} ...", action.action)));
                    }
                }
                if let Some(tc) = tool_calls.iter_mut().find(|tc| tc.id == id) {
                    tc.arguments.push_str(&delta);
                }
            }
            LlmStreamEvent::ToolCallEnd { .. } => {}
            LlmStreamEvent::Done { usage } => {
                last_stream_completion_tokens = usage.completion_tokens;
                tracing::info!(
                    "[AGENT] ✅ LLM stream completed (prompt: {}, completion: {}, total: {})",
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.total_tokens
                );
                total_usage.prompt_tokens += usage.prompt_tokens;
                total_usage.completion_tokens += usage.completion_tokens;
                total_usage.total_tokens += usage.total_tokens;
                last_prompt_tokens = usage.prompt_tokens;

                // 📝 LOG RESPONSE SUMMARY (debug level)
                tracing::debug!("\n{}", "-".repeat(80));
                tracing::debug!("📤 LLM RESPONSE SUMMARY");
                tracing::debug!("{}", "-".repeat(80));
                if !full_text.is_empty() {
                    let preview = if full_text.chars().count() > 300 {
                        format!("{}...", full_text.chars().take(300).collect::<String>())
                    } else {
                        full_text.clone()
                    };
                    tracing::debug!("Text response: {}", preview.replace('\n', "\\n"));
                }
                if !tool_calls.is_empty() {
                    tracing::debug!(
                        "Tool calls: {}",
                        tool_calls
                            .iter()
                            .map(|tc| { format!("{}({})", tc.name, tc.id) })
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    for tc in &tool_calls {
                        let args_preview = if tc.arguments.chars().count() > 200 {
                            format!("{}...", tc.arguments.chars().take(200).collect::<String>())
                        } else {
                            tc.arguments.clone()
                        };
                        tracing::debug!(
                            "  - {} [{}]: {}",
                            tc.name,
                            tc.id,
                            args_preview.replace('\n', "\\n")
                        );
                    }
                } else {
                    tracing::debug!("No tool calls");
                }
                tracing::debug!("{}", "-".repeat(80));

                break;
            }
            LlmStreamEvent::Error(err) => {
                tracing::error!("LLM error: {}", err);
                stream_handle.abort();

                let is_client_api_error = err.contains("API error 400")
                    || err.contains("API error 413")
                    || err.contains("API error 422");
                if is_client_api_error && *api_error_recovery_streak < max_api_error_recovery {
                    *api_error_recovery_streak += 1;
                    tracing::warn!(
                        "[AGENT] API error recovery {}/{}: trimming context and retrying",
                        *api_error_recovery_streak,
                        max_api_error_recovery
                    );
                    let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                        "⚠️ API 拒绝请求（{}/{}）- 正在裁剪上下文后重试…",
                        *api_error_recovery_streak, max_api_error_recovery
                    )));
                    crate::context::sanitize_tool_pairs(messages);
                    crate::context::filter_noisy_messages(messages);
                    crate::memory::memory_offload::hard_trim_public(messages);
                    return LlmCollectOutcome::RetryIteration;
                }

                let _ = ui_tx.send(AgentToUiEvent::Error(err));
                emit_turn_done(
                    ui_tx,
                    turn_id,
                    std::mem::take(new_messages),
                    total_usage.clone(),
                );
                return LlmCollectOutcome::TurnAborted;
            }
        }
    }

    // Wait for the stream task to finish, but don't block forever.
    tokio::select! {
        _ = &mut *stream_handle => {}
        _ = cancel_token.cancelled() => {
            stream_handle.abort();
        }
    }

    if let Some(ref mut filter) = findings_stream
        && let Some(tail) = filter.flush_tail()
    {
        let _ = ui_tx.send(AgentToUiEvent::TextChunk(tail));
    }

    let _ = last_stream_completion_tokens; // used for logging above

    LlmCollectOutcome::Collected {
        full_text,
        reasoning_content,
        tool_calls,
        last_prompt_tokens,
    }
}

/// Inner helper -- mirrors the nested `review_stream_filter` fn defined inside
/// `run_agent_turn`. Kept as a standalone so `collect_response` can call it.
fn review_stream_filter_inner(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
) -> bool {
    workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .is_some_and(|e| e.is_single_step() && !crate::agent::phase::is_implementation_phase(&e))
}

/// Heuristically determine if a JSON parse error is likely due to truncation.
///
/// Truncation typically manifests as:
/// - EOF errors (unexpected end of input)
/// - Missing closing brackets/braces
/// - Incomplete string literals
fn is_likely_json_truncation(json_str: &str, error: &serde_json::Error) -> bool {
    let error_msg = error.to_string();

    // Common truncation indicators
    let truncation_patterns = [
        "EOF",                 // End of file unexpectedly
        "expected `,` or `}`", // Missing closing brace
        "expected `,` or `]`", // Missing closing bracket
        "expected `\"`",       // Unclosed string
        "control character",   // Cut off in middle of content
        "invalid escape",      // Truncated escape sequence
    ];

    // Check if error message matches truncation patterns
    let is_eof_error = truncation_patterns
        .iter()
        .any(|pattern| error_msg.contains(pattern));

    // Additional heuristic: check if the JSON looks incomplete
    let trimmed = json_str.trim();
    let has_unclosed_structure = (trimmed.matches('{').count() > trimmed.matches('}').count()) ||
        (trimmed.matches('[').count() > trimmed.matches(']').count()) ||
        // Ends with incomplete syntax
        trimmed.ends_with(',') ||
        trimmed.ends_with(':') ||
        // Has unclosed quote
        !trimmed.matches('"').count().is_multiple_of(2);

    is_eof_error || has_unclosed_structure
}

/// Result of classifying tool calls for truncation and loop-limit violations.
///
/// This is a pure computation over the LLM's tool-call batch -- no side effects,
/// no I/O. The caller uses the sets to skip execution of bad tool calls and
/// return error messages to the LLM.
pub(crate) struct ToolCallClassification {
    /// Tool-call IDs whose arguments were truncated (incomplete JSON).
    /// Their `arguments` have been reset to `{}` so they parse cleanly.
    pub truncated_ids: std::collections::HashSet<String>,
    /// Tool-call IDs that exceeded the same-tool call limit (`MAX_SAME_TOOL_CALLS`).
    pub exceeded_loop_limit_ids: std::collections::HashSet<String>,
    /// Maps tool-call ID -> dedup loop key (used for error messages later).
    pub tool_loop_keys: std::collections::HashMap<String, String>,
    /// Maps loop key -> call count (used for error messages later).
    pub temp_counts: std::collections::HashMap<String, u32>,
}

/// Classify tool calls for two failure modes:
///
/// 1. **Truncation**: when `finish_reason="length"`, arguments may be incomplete
///    JSON. We detect this heuristically and mark the ID so the caller skips
///    execution and returns an error to the LLM. The arguments are reset to `{}`
///    so downstream parsing doesn't choke.
///
/// 2. **Infinite loop**: when the same tool (same name + same dedup key) is
///    called more than `MAX_SAME_TOOL_CALLS` times in one LLM response.
///
/// This function mutates `tool_calls` in place (resetting truncated arguments)
/// and returns the classification sets for the caller to consult during
/// execution.
pub(crate) fn classify_tool_calls(
    tool_calls: &mut [ToolCall],
    max_same_tool_calls: u32,
) -> ToolCallClassification {
    let mut truncated_ids = std::collections::HashSet::new();
    let mut exceeded_loop_limit_ids = std::collections::HashSet::new();
    let mut temp_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut tool_loop_keys: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // Phase 1: truncation detection
    for tc in tool_calls.iter_mut() {
        if !tc.arguments.trim().is_empty() {
            match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                Ok(_) => {}
                Err(e) => {
                    if is_likely_json_truncation(&tc.arguments, &e) {
                        tracing::warn!(
                            "Truncated tool arguments for '{}' (len {}, error: {}), will return error to LLM",
                            tc.name,
                            tc.arguments.len(),
                            e
                        );
                        truncated_ids.insert(tc.id.clone());
                        tc.arguments = "{}".to_string();
                    } else {
                        tracing::debug!(
                            "Invalid JSON for '{}' but not truncation (error: {}), will handle later",
                            tc.name,
                            e
                        );
                    }
                }
            }
        }
    }

    // Phase 2: loop-limit detection
    for tc in tool_calls.iter() {
        let loop_key = tool_loop_key(&tc.name, &tc.arguments);
        tool_loop_keys.insert(tc.id.clone(), loop_key.clone());
        let count = temp_counts.entry(loop_key).or_insert(0);
        *count += 1;
        if *count > max_same_tool_calls {
            exceeded_loop_limit_ids.insert(tc.id.clone());
        }
    }

    ToolCallClassification {
        truncated_ids,
        exceeded_loop_limit_ids,
        tool_loop_keys,
        temp_counts,
    }
}

/// Replace the latest review report instead of stacking duplicate full reports.
fn upsert_review_report_assistant(messages: &mut Vec<Message>, new_msg: &Message) {
    let Message::Assistant {
        content: new_content,
        tool_calls: new_tc,
        ..
    } = new_msg
    else {
        messages.push(new_msg.clone());
        return;
    };
    if !new_tc.is_empty() {
        messages.push(new_msg.clone());
        return;
    }
    if !crate::agent::engine::WorkflowEngine::looks_like_review_report(new_content) {
        messages.push(new_msg.clone());
        return;
    }
    crate::agent::gate::idle_narrative::strip_idle_hints(messages);
    if let Some(Message::Assistant {
        content: prev,
        tool_calls: prev_tc,
        ..
    }) = messages.last()
        && prev_tc.is_empty()
        && crate::agent::engine::WorkflowEngine::looks_like_review_report(prev)
    {
        messages.pop();
    }
    messages.push(new_msg.clone());
}

/// Hide machine-only findings JSON; show prose / markdown report.
/// `format_for_user_display` is a no-op when the text has no findings payload.
fn execute_user_display(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    _step_idx: usize,
    text: &str,
) -> String {
    let filter = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| !crate::agent::phase::is_implementation_phase(&e))
        .unwrap_or(false);
    if filter {
        crate::agent::perception::format_for_user_display(text)
    } else {
        text.to_string()
    }
}

/// Dedup key for same-tool loop detection (file_read includes offset/limit).
pub fn tool_loop_key(name: &str, arguments: &str) -> String {
    if name == crate::agent::unified_action::TOOL_NAME {
        return crate::agent::unified_action::tool_loop_key(arguments);
    }
    match name {
        "file_list" => {
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    v.get("path")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| ".".to_string());
            format!(
                "file_list:{}",
                crate::agent::engine::WorkflowEngine::normalize_explore_path(&path)
            )
        }
        "file_read" => {
            let v = serde_json::from_str::<serde_json::Value>(arguments).ok();
            let path = v
                .as_ref()
                .and_then(|j| j.get("path").and_then(|p| p.as_str()))
                .unwrap_or("?");
            let offset = v
                .as_ref()
                .and_then(|j| j.get("offset").and_then(|o| o.as_u64()))
                .unwrap_or(0);
            let limit = v
                .as_ref()
                .and_then(|j| j.get("limit").and_then(|l| l.as_u64()))
                .unwrap_or(200);
            format!(
                "file_read:{}@{}+{}",
                crate::agent::engine::WorkflowEngine::normalize_explore_path(path),
                offset,
                limit
            )
        }
        other => {
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| {
                    v.get("path")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                });
            if let Some(path) = path {
                format!(
                    "{}:{}",
                    other,
                    crate::agent::engine::WorkflowEngine::normalize_explore_path(&path)
                )
            } else {
                other.to_string()
            }
        }
    }
}

/// Remove think tags from text. LLMs sometimes include thinking content in tool
/// arguments, which breaks JSON parsing.
fn clean_think_tags(text: &str) -> String {
    use regex::Regex;

    static THINK_PATTERN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?si)<(?:think|redacted_thinking)[^>]*>.*?</(?:think|redacted_thinking)>")
            .unwrap()
    });

    static UNCLOSED_THINK: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?si)<(?:think|redacted_thinking)[^>]*>.*$").unwrap()
    });

    let result = THINK_PATTERN.replace_all(text, "");
    UNCLOSED_THINK.replace_all(&result, "").to_string()
}

/// Evaluate whether the pre-execution reflection guard should fire.
///
/// This is the **reflect-FIRST guard** -- it runs *before* this turn's tools
/// execute. Two separate loops are caught:
///
/// - **Exploration**: read-after-read without ever acting (threshold in
///   `explore_reflect::evaluate`).
/// - **Implementation**: plan confirmed, but drifting into no-edit turns
///   instead of editing (threshold in `explore_reflect::evaluate_impl`).
///
/// When a threshold trips, the caller discards this turn's chosen tool batch,
/// records the reasoning as a tool-call-free assistant message, and injects the
/// returned prompt -- forcing the model to re-decide with the reflection in view.
///
/// Returns `Some(prompt)` when the reflection should fire, `None` when the
/// tool batch should proceed to execution.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_reflection(
    tool_calls: &[ToolCall],
    unified_tool_mode: bool,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    user_task: &str,
    budget: &mut crate::agent::turn_state::TurnBudget,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> Option<String> {
    // Resolve each tool call to its inner tool name (unified mode parses
    // the action from the complete_and_check wrapper).
    let turn_tool_names: Vec<String> = tool_calls
        .iter()
        .map(|tc| {
            if unified_tool_mode {
                crate::agent::unified_action::parse_request(&tc.arguments)
                    .ok()
                    .and_then(|r| {
                        crate::agent::unified_action::action_to_tool_name(&r.action)
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| tc.name.clone())
            } else {
                tc.name.clone()
            }
        })
        .collect();

    // Does this batch contain a finish action? A finish batch is treated as
    // progress and never skipped.
    let had_finish = tool_calls.iter().any(|tc| {
        if unified_tool_mode {
            crate::agent::unified_action::parse_request(&tc.arguments)
                .ok()
                .map(|r| {
                    matches!(
                        crate::agent::unified_action::route(&r),
                        crate::agent::unified_action::UnifiedRoute::Finish
                    )
                })
                .unwrap_or(false)
        } else {
            tc.name == "finish"
        }
    });

    let in_impl_phase = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| crate::agent::phase::is_implementation_phase(&e))
        .unwrap_or(false);

    // Information-gain signal: does this turn's read-only batch surface anything
    // NEW? A discovering turn is real progress and resets the exploration streak.
    let made_discovery = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|engine| {
            tool_calls.iter().any(|tc| {
                let (inner_name, inner_args) = if unified_tool_mode {
                    match crate::agent::unified_action::parse_request(&tc.arguments) {
                        Ok(r) => (
                            crate::agent::unified_action::action_to_tool_name(&r.action)
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| tc.name.clone()),
                            r.params,
                        ),
                        Err(_) => (
                            tc.name.clone(),
                            serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({})),
                        ),
                    }
                } else {
                    (
                        tc.name.clone(),
                        serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({})),
                    )
                };
                crate::agent::gate::read_guard::is_discovery_call(&engine, &inner_name, &inner_args)
            })
        })
        .unwrap_or(true);

    // Implementation phase -> impl guard; otherwise the exploration guard.
    let action = if in_impl_phase {
        explore_reflect::evaluate_impl(
            &mut budget.impl_streak,
            &mut budget.impl_reflected,
            &turn_tool_names,
            had_finish,
            user_task,
        )
    } else {
        let converge = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| explore_reflect::ConvergeMode::from_intent(e.get_task_intent()))
            .unwrap_or(explore_reflect::ConvergeMode::SubmitPlan);
        let action = explore_reflect::evaluate(
            &mut budget.explore_streak,
            &mut budget.explore_reflected,
            &mut budget.total_explore,
            &turn_tool_names,
            had_finish,
            made_discovery,
            user_task,
            converge,
        );
        if let Some(wf) = workflow_engine
            && let Ok(engine) = wf.try_lock()
        {
            engine.set_counter("_total_explore", budget.total_explore);
        }
        action
    };

    match action {
        explore_reflect::ReflectAction::Continue => None,
        explore_reflect::ReflectAction::Reflect(prompt) => {
            let label = if in_impl_phase {
                "🛠️ 实施反思检查点 - 提示模型停止泛读、立即动手。"
            } else {
                "🪞 探索反思检查点 - 提示模型盘点已知信息后动手。"
            };
            tracing::info!(
                "[REFLECT] Pre-exec reflect (impl_phase={in_impl_phase}, explore_streak={}, impl_streak={}) - skipping this tool batch",
                budget.explore_streak,
                budget.impl_streak
            );
            let _ = ui_tx.send(AgentToUiEvent::Status(label.to_string()));
            Some(prompt)
        }
        explore_reflect::ReflectAction::Stop(handoff) => {
            let gate_msg = format!(
                "{handoff}\n\n\
                 **c** 继续探索\n\
                 **其他** 结束本轮"
            );
            let _ = ui_tx.send(AgentToUiEvent::Status(
                "⏸️ 探索预算耗尽 - c 继续 · 其他结束".to_string(),
            ));
            budget.explore_streak = 0;
            budget.total_explore = 0;
            Some(gate_msg)
        }
    }
}

// ── Safety gate extraction ──────────────────────────────────────────────────
// P5.2 step 1: extract the pre-execution confirmation gate (163 lines) into
// a standalone async function. The loop body calls this, then matches on the
// returned `SafetyGateOutcome` to decide whether to execute, skip, or end
// the turn.

/// Outcome of the pre-execution safety gate check.
enum SafetyGateOutcome {
    /// Tool is approved, proceed to execute.
    Allow,
    /// Tool was denied; error already pushed to messages/ui_tx. Skip to next.
    Skip,
    /// Turn is over (`emit_turn_done` already called). Caller must `return`.
    TurnDone,
}

/// Checks whether the tool call needs user confirmation (based on safety
/// level, path containment, and shell blacklist), awaits the user's decision,
/// and records the outcome.
///
/// - `Allow`  -> proceed with tool execution
/// - `Skip`   -> user denied; error msg already in messages/new_messages/ui_tx
/// - `TurnDone` -> cancelled or aborted; `emit_turn_done` already sent
#[allow(clippy::too_many_arguments)]
async fn check_safety_gate(
    tc: &ToolCall,
    tool: &dyn crate::tools::Tool,
    tool_ctx: &crate::tools::ToolContext,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    cancel_token: &CancellationToken,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    user_task: &Option<String>,
    full_text: &str,
    reasoning_content: &str,
    unified_tool_mode: bool,
    turn_id: u64,
    total_usage: &crate::message::TokenUsage,
) -> SafetyGateOutcome {
    tracing::info!("[AGENT] Processing tool call: {} (id: {})", tc.name, tc.id);
    tracing::info!("[AGENT] About to check safety level for: {}", tc.name);
    let safety_level = tool.safety_level();
    tracing::info!("[AGENT] Safety level for {}: {:?}", tc.name, safety_level);

    // Check if tool args reference a path outside working directory.
    let path_outside =
        if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
            if let Some(path_str) = args_val.get("path").and_then(|v| v.as_str()) {
                let resolved = tool_ctx.working_dir.join(path_str);
                !crate::safety::is_path_within_workdir(&resolved, &tool_ctx.working_dir)
            } else {
                false
            }
        } else {
            false
        };

    let mut blacklist_warning: Option<String> = None;
    if tc.name == "shell_exec"
        && let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        && let Some(cmd) = args_val.get("command").and_then(|v| v.as_str())
    {
        blacklist_warning = safety_gate::shell_blacklist_warning(trust_manager, cmd);
    }

    let should_confirm = safety_gate::needs_confirmation(
        trust_manager,
        &tc.name,
        safety_level,
        path_outside,
        blacklist_warning.is_some(),
    );

    if !should_confirm {
        return SafetyGateOutcome::Allow;
    }

    tracing::info!("[SAFETY_GATE] Tool {} requires confirmation", tc.name);
    let high_risk_warning = if tc.name == "shell_exec" {
        if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
            if let Some(cmd) = args_val.get("command").and_then(|v| v.as_str()) {
                let mut warning = None;
                if crate::safety::is_high_risk_command(cmd) {
                    warning = Some("HIGH RISK COMMAND".to_string());
                }
                if let Some(ref bw) = blacklist_warning {
                    warning = Some(match warning {
                        Some(mut w) => {
                            w.push_str(" + ");
                            w.push_str(bw);
                            w
                        }
                        None => bw.clone(),
                    });
                }
                warning
            } else {
                blacklist_warning.clone()
            }
        } else {
            blacklist_warning.clone()
        }
    } else {
        None
    };

    let req = safety_gate::build_request(
        tc.id.clone(),
        tc.name.clone(),
        &tc.arguments,
        safety_level,
        high_risk_warning,
    );
    safety_gate::emit_request(ui_tx, &req);

    let decision = match safety_gate::await_decision(
        ui_rx,
        cancel_token,
        &tc.id,
        workflow_engine,
        messages,
        ui_tx,
        push_interjection_message,
    )
    .await
    {
        Ok(d) => d,
        Err(safety_gate::SafetyGateCancelled) => {
            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
            let nm = std::mem::take(new_messages);
            emit_turn_done(ui_tx, turn_id, nm, total_usage.clone());
            return SafetyGateOutcome::TurnDone;
        }
    };

    match decision {
        ui_event::ConfirmationDecision::Deny => {
            tracing::info!("[AGENT] User denied tool: {}", tc.name);
            let error_msg = "User denied tool execution".to_string();
            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: error_msg.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
            // Record user denial to react_log
            if let Some(ref ms) = tool_ctx.memory_store {
                let (session_id, task_desc) =
                    react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
                let target_json: Option<serde_json::Value> =
                    serde_json::from_str(&tc.arguments).ok();
                let target = target_json
                    .as_ref()
                    .and_then(|v| {
                        v.get("params")
                            .or_else(|| v.get("path"))
                            .or_else(|| v.get("name"))
                    })
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let decision = format!("👤 用户拒绝执行: {}", tc.name);
                let assistant_text = {
                    let raw = crate::agent::think_stream::visible_only(full_text);
                    if raw.trim().is_empty() {
                        "(用户拒绝了此工具执行)".into()
                    } else {
                        raw
                    }
                };
                let reasoning_fallback = build_reasoning_fallback(
                    reasoning_content,
                    &tc.name,
                    &tc.arguments,
                    full_text,
                    unified_tool_mode,
                );
                let _ = ms.record_react(
                    &session_id,
                    &task_desc,
                    &tc.name,
                    &target,
                    "denied",
                    &decision,
                    &assistant_text,
                    &reasoning_fallback,
                    &error_msg,
                );
            }
            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: error_msg,
                is_error: true,
            });
            SafetyGateOutcome::Skip
        }
        ui_event::ConfirmationDecision::TrustAlways => {
            tracing::info!("[AGENT] User trusted all tools");
            safety_gate::apply_trust_all(trust_manager);
            SafetyGateOutcome::Allow
        }
        ui_event::ConfirmationDecision::Allow => {
            tracing::info!("[AGENT] User allowed tool: {}", tc.name);
            SafetyGateOutcome::Allow
        }
    }
}

// ── Loop / truncation guard extraction ─────────────────────────────────────
// P5.2 step 2: extract the infinite-loop and truncation guard checks (118
// lines) into a standalone function. Both paths end with `continue`, so the
// function returns `true` (skip) or `false` (proceed).

/// Returns `true` if the tool call should be skipped (error already pushed to
/// messages/new_messages/ui_tx/turn_memory). Covers two cases:
/// - Infinite loop: same tool called too many times in one turn.
/// - Truncated args: arguments were cut off by the LLM output window.
#[allow(clippy::too_many_arguments)]
fn check_loop_and_truncation_guards(
    tc: &ToolCall,
    exceeded_loop_limit_ids: &std::collections::HashSet<String>,
    truncated_ids: &std::collections::HashSet<String>,
    tool_loop_keys: &std::collections::HashMap<String, String>,
    temp_counts: &std::collections::HashMap<String, u32>,
    execute_step: bool,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    user_task: &Option<String>,
    full_text: &str,
    reasoning_content: &str,
    unified_tool_mode: bool,
    memory_store: Option<&Arc<crate::memory::store::MemoryStore>>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> bool {
    if exceeded_loop_limit_ids.contains(&tc.id) {
        let loop_key = tool_loop_keys
            .get(&tc.id)
            .cloned()
            .unwrap_or_else(|| tc.name.clone());
        let call_count = temp_counts.get(&loop_key).copied().unwrap_or(0);
        tracing::error!(
            "🚨 INFINITE LOOP DETECTED: {} called {} times in one turn. Stopping.",
            loop_key,
            call_count
        );
        let hint = if tc.name == "file_read" && execute_step {
            "\n5. 大文件用 file_read 的 offset/limit 分段读取（例如 offset=200, limit=200）"
        } else {
            ""
        };
        let error_msg = format!(
            "❌ Infinite Loop Detected:\n\
             `{loop_key}` has been called {call_count} times in this LLM response.\n\
             This suggests the AI is stuck in a loop.\n\n\
             💡 Solutions:\n\
             1. Try a different approach to solve the problem\n\
             2. Break the task into smaller steps\n\
             3. Provide more specific instructions\n\
             4. Use /clear to start fresh if needed{hint}",
            hint = hint
        );
        let result_msg = Message::ToolResult {
            tool_call_id: tc.id.clone(),
            content: error_msg.clone(),
        };
        new_messages.push(result_msg.clone());
        messages.push(result_msg);
        turn_memory.record_tool(&tc.name, &tc.arguments, false);
        if let Some(ms) = memory_store {
            let (session_id, task_desc) =
                react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
            let target_json: Option<serde_json::Value> = serde_json::from_str(&tc.arguments).ok();
            let target = target_json
                .as_ref()
                .and_then(|v| {
                    v.get("params")
                        .or_else(|| v.get("path"))
                        .or_else(|| v.get("name"))
                })
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let decision = format!(
                "🚨 检测到无限循环: {} 已调用 {} 次，强制阻止",
                loop_key, call_count
            );
            let assistant_text = {
                let raw = crate::agent::think_stream::visible_only(full_text);
                if raw.trim().is_empty() {
                    "(LLM 陷入循环，被系统阻止)".into()
                } else {
                    raw
                }
            };
            let reasoning_fallback = build_reasoning_fallback(
                reasoning_content,
                &tc.name,
                &tc.arguments,
                full_text,
                unified_tool_mode,
            );
            let _ = ms.record_react(
                &session_id,
                &task_desc,
                &tc.name,
                &target,
                "blocked",
                &decision,
                &assistant_text,
                &reasoning_fallback,
                &error_msg,
            );
        }
        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
            name: tc.name.clone(),
            output: error_msg,
            is_error: true,
        });
        return true;
    }
    if truncated_ids.contains(&tc.id) {
        let error_msg = build_truncation_error(&tc.name, &tc.arguments);
        tracing::warn!(
            "Tool '{}' (id={}) had truncated arguments ({} bytes). Sending error to LLM.",
            tc.name,
            tc.id,
            tc.arguments.len()
        );
        let result_msg = Message::ToolResult {
            tool_call_id: tc.id.clone(),
            content: error_msg.clone(),
        };
        new_messages.push(result_msg.clone());
        messages.push(result_msg);
        turn_memory.record_tool(&tc.name, &tc.arguments, false);
        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
            name: tc.name.clone(),
            output: error_msg,
            is_error: true,
        });
        return true;
    }
    false
}

// ── Workflow validation extraction ──────────────────────────────────────────
// P5.2 step 3: extract the read_guard + validate_tool_call checks (72 lines)
// into a standalone async function. Returns `true` if the tool call should be
// skipped (error/cached result already pushed to messages/new_messages/ui_tx).

/// Returns `true` if the tool call should be skipped.
/// - read_guard blocked: cached file_read result or error already pushed.
/// - workflow step validation failed: error already pushed.
#[allow(clippy::too_many_arguments)]
async fn check_workflow_validation(
    tc: &ToolCall,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    unified_tool_mode: bool,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> bool {
    let Some(engine_arc) = workflow_engine else {
        return false;
    };
    let engine = engine_arc.lock().await;

    // Parse tool arguments for validation
    let args_value = if !tc.arguments.trim().is_empty() {
        serde_json::from_str::<serde_json::Value>(&tc.arguments).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Read guard: duplicate file_read / shell-as-read
    if let Err(e) = crate::agent::gate::read_guard::check(&tc.name, &args_value, &engine) {
        if tc.name == "file_read"
            && let Some(path) = args_value.get("path").and_then(|p| p.as_str())
            && let Some(cached) =
                crate::agent::gate::read_guard::cached_file_read_response(&engine, path)
        {
            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: cached.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
            turn_memory.record_tool(&tc.name, &tc.arguments, true);
            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: cached,
                is_error: false,
            });
            return true;
        }
        let result_msg = Message::ToolResult {
            tool_call_id: tc.id.clone(),
            content: format!("❌ {e}"),
        };
        new_messages.push(result_msg.clone());
        messages.push(result_msg);
        turn_memory.record_tool(&tc.name, &tc.arguments, false);
        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
            name: tc.name.clone(),
            output: e.clone(),
            is_error: true,
        });
        return true;
    }

    // Validate tool call against current workflow step
    if let Err(e) = engine.validate_tool_call(&tc.name, &args_value) {
        tracing::warn!("Workflow validation failed for tool '{}': {}", tc.name, e);
        let directive = if unified_tool_mode {
            "\n\n💡 该 action 当前不可用。请改用 [WORKSPACE] 允许的 action，或 finish。"
        } else {
            "\n\n💡 该工具当前不可用。请改用其它工具，或完成时输出 ## Done。"
        };
        let result_msg = Message::ToolResult {
            tool_call_id: tc.id.clone(),
            content: format!("❌ {}\n{}", e, directive),
        };
        new_messages.push(result_msg.clone());
        messages.push(result_msg);
        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
            name: tc.name.clone(),
            output: e,
            is_error: true,
        });
        return true;
    }

    false
}

// ── file_write path validation extraction ─────────────────────────────────────
/// Returns `true` if the tool call should be skipped (error already pushed).
/// Checks that file_write has a path/filename/file_id parameter.
fn check_file_write_missing_path(
    tc: &ToolCall,
    args: &serde_json::Value,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> bool {
    if tc.name != "file_write" {
        return false;
    }
    let has_path = args.get("path").is_some();
    let has_filename = args.get("filename").is_some();
    let has_file_id = args.get("file_id").is_some();
    if has_path || has_filename || has_file_id {
        return false;
    }
    let error_msg = "❌ CRITICAL ERROR: Missing 'path' parameter for file_write!\n\n\
                     💡 For NEW files, you MUST provide a COMPLETE path:\n\
                     • Include directory structure (e.g., 'src/utils/helper.rs')\n\
                     • NOT just filename (e.g., 'helper.rs' is WRONG)\n\n\
                     📝 Correct Examples:\n\
                     {\"path\": \"src/main.rs\", \"content\": \"...\"}\n\
                     {\"path\": \"docs/guide.md\", \"content\": \"...\"}\n\
                     {\"path\": \"tests/unit_test.rs\", \"content\": \"...\"}\n\n\
                     ❌ Wrong Example:\n\
                     {\"content\": \"...\"} ← NO PATH PROVIDED!\n\
                     {\"filename\": \"main.rs\"} ← Only works for EXISTING files!";
    let result_msg = Message::ToolResult {
        tool_call_id: tc.id.clone(),
        content: error_msg.to_string(),
    };
    new_messages.push(result_msg.clone());
    messages.push(result_msg);
    let _ = ui_tx.send(AgentToUiEvent::ToolResult {
        name: tc.name.clone(),
        output: error_msg.to_string(),
        is_error: true,
    });
    true
}

// ── Tool registry lookup extraction ───────────────────────────────────────────
/// Look up a tool from the registry. Returns `Some(tool)` if found, or `None`
/// if the tool name is unknown (error already pushed to messages/ui_tx).
fn lookup_tool_or_error<'a>(
    tc: &ToolCall,
    tool_registry: &'a crate::tools::ToolRegistry,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> Option<&'a dyn crate::tools::Tool> {
    tracing::info!("[AGENT] About to get tool object for: {}", tc.name);
    match tool_registry.get(&tc.name) {
        Some(t) => {
            tracing::info!("[AGENT] Tool object retrieved for: {}", tc.name);
            Some(t)
        }
        None => {
            let tool_names: Vec<String> = tool_registry
                .names()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let error_msg = tool_executor::build_unknown_tool_error(&tc.name, &tool_names);
            tracing::warn!("Unknown tool requested: '{}'", tc.name);
            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: error_msg.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: error_msg,
                is_error: true,
            });
            None
        }
    }
}

// ── Tool argument parsing extraction ──────────────────────────────────────────
/// Parse tool-call arguments into a `serde_json::Value`.
/// Returns `Ok(value)` on success, or `Err(())` if parsing failed
/// (error already pushed to messages/new_messages/ui_tx).
fn parse_tool_args(
    tc: &ToolCall,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> Result<serde_json::Value, ()> {
    if tc.arguments.trim().is_empty() {
        // LLM sent no arguments - treat as empty object (common for no-param tools).
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    // Clean think tags from arguments before parsing
    let cleaned_args = clean_think_tags(&tc.arguments);
    match serde_json::from_str(&cleaned_args) {
        Ok(v) => Ok(v),
        Err(parse_err) => {
            let error_msg = build_arg_parse_error(&tc.name, &parse_err);
            tracing::warn!(
                "Tool argument parse error for '{}': {} | Raw: {}",
                tc.name,
                parse_err,
                {
                    if tc.arguments.chars().count() > 100 {
                        tc.arguments.chars().take(100).collect::<String>()
                    } else {
                        tc.arguments.clone()
                    }
                }
            );
            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: error_msg.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: error_msg,
                is_error: true,
            });
            Err(())
        }
    }
}

// ── Tool execution + retry extraction ──────────────────────────────────────────
/// Send a progress event, build the progress-callback context, execute the tool,
/// and retry once on transient failures (file_write/shell_exec/web_fetch).
async fn execute_tool_with_retry(
    tc: &ToolCall,
    args: &serde_json::Value,
    tool: &dyn crate::tools::Tool,
    tool_ctx: &crate::tools::ToolContext,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> crate::tools::ToolOutput {
    // Send toolProgress event to indicate execution starting
    let progress_msg = match tc.name.as_str() {
        "file_write" => "Starting file write...",
        "file_read" => "Reading file...",
        "shell_exec" => "Executing command...",
        "code_search" => "Searching code...",
        "edit_file" => "Editing file...",
        "delete_range" => "Deleting range...",
        "find_symbol" => "Finding symbols...",
        _ => "Executing...",
    };
    let _ = ui_tx.send(AgentToUiEvent::ToolProgress {
        tool_call_id: tc.id.clone(),
        tool_name: tc.name.clone(),
        message: progress_msg.to_string(),
        progress_percent: Some(0),
    });

    tracing::info!("[AGENT] About to execute tool: {} (id: {})", tc.name, tc.id);
    // Create a tool context with progress callback for real-time updates
    let ui_tx_clone = ui_tx.clone();
    let _tool_call_id_clone = tc.id.clone();
    let _tool_name_clone = tc.name.clone();
    let tool_ctx_with_progress = Arc::new(crate::tools::ToolContext::with_progress_callback(
        tool_ctx.runtime.clone(),
        tool_ctx.working_dir.clone(),
        tool_ctx.config.clone(),
        tc.id.clone(),
        move |progress: crate::tools::ToolProgress| {
            let _ = ui_tx_clone.send(AgentToUiEvent::ToolProgress {
                tool_call_id: progress.tool_call_id,
                tool_name: progress.tool_name,
                message: progress.message,
                progress_percent: progress.progress_percent,
            });
        },
    ));

    tracing::info!("[AGENT] Executing tool.execute() for: {}", tc.name);
    let mut result = tool.execute(args.clone(), &tool_ctx_with_progress).await;
    // Retry once for transient failures on write/network tools
    if result.is_error && matches!(tc.name.as_str(), "file_write" | "shell_exec" | "web_fetch") {
        tracing::warn!(
            "[AGENT] Tool {} failed, retrying once: {}",
            tc.name,
            result.content
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        result = tool.execute(args.clone(), &tool_ctx_with_progress).await;
    }
    tracing::info!(
        "[AGENT] Tool execution completed: {}, is_error: {}",
        tc.name,
        result.is_error
    );
    result
}

// ── Tool I/O logging + sanitization extraction ─────────────────────────────────
/// Log tool I/O, send completion progress, update working dir if changed,
/// and sanitize untrusted tool output.
/// Returns `(sanitized_content, Option<ToolContext>)` where the second element
/// is a new ToolContext if the tool changed the working directory.
fn post_tool_log_and_sanitize(
    tc: &ToolCall,
    args: &serde_json::Value,
    result: &crate::tools::ToolOutput,
    tool_ctx: &crate::tools::ToolContext,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> (String, Option<crate::tools::ToolContext>) {
    // ── Full tool I/O logging for debugging ──
    let args_preview: String =
        serde_json::to_string_pretty(args).unwrap_or_else(|_| format!("{:?}", args));
    let result_preview: String = if result.content.len() > 8000 {
        let head: String = result.content.chars().take(8000).collect();
        format!(
            "{head}... (truncated, total {} chars)",
            result.content.len()
        )
    } else {
        result.content.clone()
    };
    tracing::info!(
        "[TOOL_IO] {} | args={} | error={} | output={}",
        tc.name,
        args_preview,
        result.is_error,
        result_preview
    );

    // Send completion progress event only if tool executed successfully
    if !result.is_error {
        let _ = ui_tx.send(AgentToUiEvent::ToolProgress {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            message: "Completed".to_string(),
            progress_percent: Some(100),
        });
    }

    // If the tool changed working directory, update tool_ctx and notify UI.
    let new_ctx = if let Some(new_dir) = result.new_working_dir.clone() {
        let ctx = crate::tools::ToolContext::new(
            tool_ctx.runtime.clone(),
            new_dir.clone(),
            tool_ctx.config.clone(),
        );
        let _ = ui_tx.send(AgentToUiEvent::WorkingDirChanged(new_dir));
        Some(ctx)
    } else {
        None
    };

    // 🛡️ Untrusted tool output: injection scan + data banner
    let sanitized_content = if matches!(
        tc.name.as_str(),
        "web_fetch" | "file_read" | "shell_exec" | "git_diff" | "code_search"
    ) && !result.is_error
    {
        crate::agent::tool_result::wrap_for_llm(&tc.name, &result.content, false)
    } else if result.is_error {
        crate::agent::tool_result::wrap_for_llm(&tc.name, &result.content, true)
    } else {
        result.content.clone()
    };

    (sanitized_content, new_ctx)
}

// ── Context offloading + decision recording extraction ────────────────────────
/// Offload large tool results, send UI notifications, and record the
/// decision to turn memory. Returns the `OffloadedResult`.
fn offload_and_record(
    tc: &ToolCall,
    result: &crate::tools::ToolOutput,
    sanitized_content: &str,
    step_index: usize,
    offloader: &mut crate::context::context_offloader::ContextOffloader,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> crate::context::context_offloader::OffloadedResult {
    // ── Context Offloading: only offload shell_exec (build logs can be huge) ──
    // file_read results are essential context - never offload
    let offload_threshold: usize = if tc.name == "shell_exec" {
        4000
    } else {
        usize::MAX // Never offload non-shell_exec results
    };
    let offloaded = offloader.process_result(
        &tc.name,
        &tc.arguments,
        sanitized_content,
        step_index,
        offload_threshold,
    );

    // Send notification about offloading
    if offloaded.is_offloaded {
        let path_display = offloaded
            .ref_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "?".to_string());
        let _ = ui_tx.send(AgentToUiEvent::Status(format!(
            "📄 Result offloaded to: {path_display}",
        )));
    }

    let _ = ui_tx.send(AgentToUiEvent::ToolResult {
        name: tc.name.clone(),
        output: offloaded.to_context_message(),
        is_error: result.is_error,
    });

    // Record decision BEFORE react_log (so react_log reflects current tool, not previous)
    {
        let dec_target_json: Option<serde_json::Value> = serde_json::from_str(&tc.arguments).ok();
        let dec_target: String = dec_target_json
            .as_ref()
            .and_then(|v| {
                v.get("params")
                    .or_else(|| v.get("path"))
                    .or_else(|| v.get("name"))
            })
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "?".to_string());
        let observation: String =
            crate::agent::exploration_snapshot::extract_data_content(&result.content)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ")
                .chars()
                .take(260)
                .collect();
        let status = if result.is_error { "失败" } else { "成功" };
        turn_memory.record_decision(format!(
            "你刚才执行 {}({}) {status}; 观察到: {}; 后续避免重复同一查询",
            tc.name, dec_target, observation
        ));
    }

    offloaded
}

// ── Read query recording extraction ───────────────────────────────────────────
/// Record file_read and find_symbol/code_search results to the workflow engine
/// for gate/digest tracking.
fn record_read_queries(
    tc: &ToolCall,
    result: &crate::tools::ToolOutput,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
) {
    if tc.name == "file_read"
        && !result.is_error
        && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
    {
        if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
            let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as u32;
            if let Some(engine_arc) = workflow_engine
                && let Ok(engine) = engine_arc.try_lock()
            {
                crate::agent::gate::read_guard::record_file_read(&engine, path);
                crate::agent::tool_digest::record_read(
                    &engine,
                    path,
                    &result.content,
                    offset,
                    None,
                );
            }
        }
    } else if matches!(tc.name.as_str(), "find_symbol" | "code_search")
        && !result.is_error
        && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        && let Some(engine_arc) = workflow_engine
        && let Ok(engine) = engine_arc.try_lock()
    {
        crate::agent::gate::read_guard::record_symbol_query(&engine, &tc.name, &args);
    }
}

// ── Snapshot + turn memory recording extraction ────────────────────────────────
/// Snapshot exploration results to the workflow engine and record the tool
/// result + decision to turn memory, then persist.
fn snapshot_and_record_turn(
    tc: &ToolCall,
    result: &crate::tools::ToolOutput,
    result_content: &str,
    working_dir: &std::path::Path,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
) {
    // Snapshot tool results for Plan / Execute step iteration memory
    if !result.is_error
        && let Some(engine_arc) = workflow_engine
        && let Ok(engine) = engine_arc.try_lock()
    {
        let step = engine.get_current_step_index();
        if crate::agent::exploration_snapshot::should_snapshot_for_step(step, &tc.name) {
            let target =
                crate::agent::exploration_snapshot::target_from_tool_args(&tc.name, &tc.arguments);
            engine.record_exploration_result(working_dir, &tc.name, &target, result_content);
        }
    }

    turn_memory.record_tool_with_result(
        &tc.name,
        &tc.arguments,
        !result.is_error,
        Some(result_content),
    );
    let target = crate::agent::exploration_snapshot::target_from_tool_args(&tc.name, &tc.arguments);
    let observation: String =
        crate::agent::exploration_snapshot::extract_data_content(result_content)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ")
            .chars()
            .take(260)
            .collect();
    let status = if result.is_error { "失败" } else { "成功" };
    turn_memory.record_decision(format!(
        "你刚才执行 {}({}) {status}; 观察到: {}; 后续避免重复同一查询",
        tc.name, target, observation
    ));
    persist_turn_memory(workflow_engine, turn_memory);
}

// ── Post-verify + edit hint extraction ─────────────────────────────────────────
/// After tool execution: check shell_exec verify results, and if edit_file
/// failed, append a recovery hint to `result_content`.
fn post_verify_and_hint(
    tc: &ToolCall,
    result: &crate::tools::ToolOutput,
    sanitized_content: &str,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    result_content: &mut String,
) {
    if tc.name == "shell_exec"
        && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        && let Some(cmd) = args.get("command").and_then(|c| c.as_str())
    {
        let succeeded = post_edit_verification::shell_result_success(sanitized_content);
        if let Some(engine_arc) = workflow_engine
            && let Ok(engine) = engine_arc.try_lock()
        {
            post_edit_verification::note_shell_verify_result(&engine, cmd, succeeded);
            if succeeded
                && let Some(idx) = engine.get_plan_tracker().and_then(|t| {
                    t.steps
                        .iter()
                        .find(|s| !s.verify.is_empty() && s.awaiting_verify)
                        .map(|s| s.index)
                })
            {
                crate::agent::verifier::after_verify_pass(&engine, idx);
            }
        }
    }

    if result.is_error
        && tc.name == "edit_file"
        && let Some(engine_arc) = workflow_engine
        && let Ok(engine) = engine_arc.try_lock()
        && crate::agent::phase::is_implementation_phase(&engine)
        && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        && let Some(path) = args.get("path").and_then(|p| p.as_str())
    {
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
        result_content.push_str(&hint);
    }
}

// ── Post-success status log + plan tracker extraction ─────────────────────────
/// After a successful tool execution: push status log, update plan tracker,
/// record explored paths, and add verify-after-edit prompts.
/// All deferred system messages are pushed to `deferred_tool_system`.
fn post_success_updates(
    tc: &ToolCall,
    result: &crate::tools::ToolOutput,
    result_content: &str,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    unified_tool_mode: bool,
    deferred_tool_system: &mut Vec<String>,
    tools_used_this_turn: &mut std::collections::HashSet<String>,
) {
    // 📋 Status log: tell LLM what it just accomplished (critical for multi-step awareness)
    if !result.is_error {
        let tool_name = tc.name.clone();
        let file_info = if matches!(tool_name.as_str(), "file_write" | "edit_file") {
            serde_json::from_str::<serde_json::Value>(&tc.arguments)
                .ok()
                .and_then(|v| {
                    v.get("path")
                        .and_then(|p| p.as_str())
                        .map(|s| s.to_string())
                })
                .map(|p| format!(" -> {}", p))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let done_label = if matches!(
            tool_name.as_str(),
            "file_write" | "edit_file" | "delete_range"
        ) {
            "工具执行成功（清单是否勾选见下方进度）"
        } else {
            "已完成"
        };
        deferred_tool_system.push(format!(
            "📋 ✅ {tool_name}{file_info} - {done_label}",
            tool_name = tool_name,
            file_info = file_info,
            done_label = done_label
        ));
        tools_used_this_turn.insert(tool_name.clone());

        // Track explored paths during Plan only (Execute may re-read files)
        if matches!(tool_name.as_str(), "file_list" | "file_read")
            && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
            if let Some(engine_arc) = workflow_engine
                && let Ok(engine) = engine_arc.try_lock()
                && (crate::agent::phase::get(&engine)
                    == crate::agent::phase::SingleFlowPhase::Review
                    || (engine.is_task_step() && tool_name == "file_list"))
            {
                engine.record_explored_path(&tool_name, path);
            }
        }

        // Execute: update plan tracker for completing tools
        if let Some(engine_arc) = workflow_engine
            && let Ok(engine) = engine_arc.try_lock()
        {
            if engine.is_task_step() {
                if tool_name == "file_read"
                    && crate::agent::phase::is_implementation_phase(&engine)
                    && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    && let Some(path) = args.get("path").and_then(|p| p.as_str())
                {
                    engine.record_impl_file_read(path, &tc.arguments);
                    if let Some(nudge) = engine.impl_edit_nudge_after_read(path, result_content) {
                        deferred_tool_system.push(nudge);
                    }
                }
                let (plan_changed, plan_hint) =
                    engine.record_execute_tool_success(&tool_name, &tc.arguments, result_content);
                if let Some(hint) = plan_hint {
                    deferred_tool_system.push(hint);
                }
                if plan_changed
                    && let Some(msg) = engine.plan_progress_message_after_tool(&tool_name)
                {
                    deferred_tool_system.push(msg);
                }
                if matches!(
                    tool_name.as_str(),
                    "edit_file" | "file_write" | "delete_range"
                ) && crate::agent::phase::is_implementation_phase(&engine)
                    && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    && let Some(path) = args.get("path").and_then(|p| p.as_str())
                {
                    engine.record_impl_file_edited(path);
                    let idx = engine
                        .get_plan_tracker()
                        .and_then(|t| t.current_step().map(|s| s.index))
                        .unwrap_or(1);
                    if let Some(note) =
                        crate::agent::verifier::after_edit_note(&engine, idx, path, result_content)
                    {
                        deferred_tool_system.push(note);
                    }
                }
            }
            if matches!(
                tool_name.as_str(),
                "file_write" | "edit_file" | "delete_range"
            ) && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                && let Some(path) = args.get("path").and_then(|p| p.as_str())
                && let Some(verify) = engine.verify_hint_for_path(path)
            {
                deferred_tool_system.push(format!(
                                    "📋 计划验证: `{verify}` - 请用 shell_exec 执行（需用户确认），验证通过后再继续下一项。"
                                ));
            }
        }
    }

    // 📖 Verify-after-edit: prompt LLM to verify changes
    if matches!(
        tc.name.as_str(),
        "edit_file" | "delete_range" | "file_write"
    ) && !result.is_error
    {
        let is_skill = tc.arguments.contains(".ox/skills/");

        let is_execute_step = workflow_engine
            .as_ref()
            .is_some_and(|wf| wf.try_lock().is_ok_and(|e| e.is_task_step()));

        if is_execute_step && is_skill {
            deferred_tool_system.push(if unified_tool_mode {
                "✅ 文件已写入。若全部完成，调用 complete_and_check(action=finish, params={summary:\"...\"})。".to_string()
            } else {
                "✅ 文件已写入。如果所有需要的文件都已完成，输出 `## Done` 结束。".to_string()
            });
        }
    } // verify-after-edit
}

// ── Repeated-failure hand-off extraction ───────────────────────────────────────
/// Check if the same verify has failed N times in a row. If so, stop
/// auto-retrying, emit a hand-off message, and return `true` (TurnDone).
/// Returns `false` to continue normally.
fn check_repeated_failure_handoff(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    turn_id: u64,
    total_usage: &crate::message::TokenUsage,
) -> bool {
    let repeated_failure = workflow_engine.as_ref().and_then(|wf| {
        wf.try_lock().ok().and_then(|e| {
            if post_edit_verification::should_stop_on_repeated_failure(&e) {
                let streak = post_edit_verification::verify_fail_streak(&e);
                let cmd = e
                    .get_variable(post_edit_verification::VERIFY_CMD_KEY)
                    .unwrap_or_default();
                Some((streak, cmd))
            } else {
                None
            }
        })
    });
    if let Some((streak, cmd)) = repeated_failure {
        let cmd_line = if cmd.is_empty() {
            String::new()
        } else {
            format!("\n验证命令: `{cmd}`")
        };
        let handoff = format!(
            "## Failed\n已连续 {streak} 次验证未通过，停止自动重试，交给你判断。{cmd_line}\n\
             请查看上面最近的报错：可能是改法方向不对、缺少依赖，或需要你补充信息。"
        );
        let _ = ui_tx.send(AgentToUiEvent::Status(format!(
            "🛑 连续 {streak} 次验证失败 - 暂停本轮，等待你的指示。"
        )));
        messages.push(Message::system(&handoff));
        new_messages.push(Message::system(&handoff));
        if let Some(wf) = workflow_engine
            && let Ok(engine) = wf.try_lock()
        {
            post_edit_verification::reset_verify_failures(&engine);
        }
        persist_turn_memory(workflow_engine, turn_memory);
        emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
        true
    } else {
        false
    }
}

// ── Repeat guard extraction ───────────────────────────────────────────────────
/// Check for degenerate repeated-output loops. Returns `true` if the turn
/// should be aborted (Stop), `false` to continue.
#[allow(clippy::too_many_arguments)]
fn check_repeat_guard(
    repeat_guard: &mut crate::agent::repeat_guard::RepeatGuard,
    full_text: &str,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    turn_id: u64,
    total_usage: &crate::message::TokenUsage,
) -> bool {
    match repeat_guard.observe(&crate::agent::think_stream::visible_only(full_text)) {
        repeat_guard::RepeatAction::Continue => {}
        repeat_guard::RepeatAction::Nudge(nudge) => {
            let _ = ui_tx.send(AgentToUiEvent::Status(
                "🔁 检测到重复思考 - 提示模型发出具体动作。".to_string(),
            ));
            messages.push(Message::system(&nudge));
        }
        repeat_guard::RepeatAction::Stop(handoff) => {
            let _ = ui_tx.send(AgentToUiEvent::Status(
                "🛑 连续重复思考无法推进 - 暂停本轮，等待你的指示。".to_string(),
            ));
            messages.push(Message::system(&handoff));
            new_messages.push(Message::system(&handoff));
            persist_turn_memory(workflow_engine, turn_memory);
            emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
            return true;
        }
    }
    false
}

// ── Unified parse error extraction ────────────────────────────────────────────

/// Outcome of `check_unified_parse_error`.
enum UnifiedParseOutcome {
    /// Args are valid; proceed with unified handler.
    Proceed,
    /// Args invalid; error pushed, skip this tool.
    Skip,
    /// 5th consecutive parse error; TurnDone emitted, caller should return.
    TurnDone,
}

/// Check for empty/invalid `complete_and_check` arguments when in unified tool mode.
/// Returns `Proceed` if args are valid, `Skip` if invalid (error pushed),
/// or `TurnDone` if 5 consecutive errors (turn aborted).
#[allow(clippy::too_many_arguments)]
fn check_unified_parse_error(
    tc: &ToolCall,
    unified_tool_mode: bool,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    unified_parse_error_streak: &mut u32,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    turn_id: u64,
    total_usage: &crate::message::TokenUsage,
    iteration: u32,
) -> UnifiedParseOutcome {
    if !unified_tool_mode || tc.name != crate::agent::unified_action::TOOL_NAME {
        return UnifiedParseOutcome::Proceed;
    }
    let args_empty = tc.arguments.trim().is_empty();
    let args_invalid = !tc.arguments.trim().is_empty()
        && serde_json::from_str::<serde_json::Value>(&tc.arguments).is_err();
    if !args_empty && !args_invalid {
        return UnifiedParseOutcome::Proceed;
    }
    let reason = if args_empty {
        "参数为空"
    } else {
        "参数不是合法 JSON"
    };
    let error_msg = format!(
        "❌ complete_and_check {reason}。\n\n\
         必须发送合法 JSON，例如：\n\
         {{\"action\":\"file_read\",\"params\":{{\"path\":\"src/main.rs\"}}}}\n\n\
         禁止空或非法 arguments；每轮必须包含 action 与 params。"
    );
    let result_msg = Message::ToolResult {
        tool_call_id: tc.id.clone(),
        content: error_msg.to_string(),
    };
    new_messages.push(result_msg.clone());
    messages.push(result_msg);
    turn_memory.record_tool(&tc.name, &tc.arguments, true);
    let _ = ui_tx.send(AgentToUiEvent::ToolResult {
        name: tc.name.clone(),
        output: error_msg.to_string(),
        is_error: true,
    });
    *unified_parse_error_streak += 1;
    tracing::warn!(
        "[UNIFIED_PARSE_ERROR] streak={} reason={} args_len={} iteration={}",
        *unified_parse_error_streak,
        reason,
        tc.arguments.len(),
        iteration,
    );
    if *unified_parse_error_streak >= 3 {
        messages.push(Message::system(
            "⚠️ 已连续 3 次空/无效 complete_and_check 参数。\
             必须发送合法 JSON：{\"action\":\"…\",\"params\":{…}}\n\
             例如 action=file_read, action=edit_file, action=finish",
        ));
    }
    if *unified_parse_error_streak >= 5 {
        let _ = ui_tx.send(AgentToUiEvent::Status(
            "⏹️ 连续 5 次无效 complete_and_check - 强制结束本轮".to_string(),
        ));
        emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
        return UnifiedParseOutcome::TurnDone;
    }
    UnifiedParseOutcome::Skip
}

// ── Unified handler dispatch extraction ─────────────────────────────────────

/// Outcome of `handle_unified_tool_call`.
enum UnifiedDispatchOutcome {
    /// Tool handled; proceed to next tool in the batch.
    Continue,
    /// Turn is done (finish/timeout/abort); caller should return immediately.
    TurnDone,
    /// Not a unified tool call; caller should proceed with the non-unified path.
    NotHandled,
}

/// Handle a `complete_and_check` tool call in unified tool mode.
///
/// This is the largest single tool-dispatch path: it wraps
/// `handle_complete_and_check` with a 300s timeout, processes the
/// `UnifiedHandleOutcome` (Result / TurnDone / Aborted), records
/// ReAct logs, and manages parse-error / findings-error streaks.
#[allow(clippy::too_many_arguments)]
async fn handle_unified_tool_call(
    tc: &ToolCall,
    unified_tool_mode: bool,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    push_interjection_message: PushInterjectionFn,
    turn_id: u64,
    new_messages: &mut Vec<Message>,
    total_usage: &crate::message::TokenUsage,
    iteration: u32,
    tool_calls: &[ToolCall],
    unified_parse_error_streak: &mut u32,
    findings_deliver_error_streak: &mut u32,
    deferred_tool_system: &mut Vec<String>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    user_task: &Option<String>,
    full_text: &str,
    reasoning_content: &str,
) -> UnifiedDispatchOutcome {
    if !unified_tool_mode || tc.name != crate::agent::unified_action::TOOL_NAME {
        return UnifiedDispatchOutcome::NotHandled;
    }
    let action_hint = crate::agent::unified_action::parse_request(&tc.arguments)
        .map(|r| r.action)
        .unwrap_or_else(|_| "?".into());
    let _ = ui_tx.send(AgentToUiEvent::ToolStart {
        name: format!("{}:{action_hint}", crate::agent::unified_action::TOOL_NAME),
        id: tc.id.clone(),
        detail: Some(tc.arguments.chars().take(200).collect()),
    });

    tracing::info!("[UNIFIED_CALL] Entering handle_complete_and_check...");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        crate::agent::unified_handler::handle_complete_and_check(
            tc,
            tool_registry,
            tool_ctx,
            trust_manager,
            workflow_engine,
            messages,
            ui_tx,
            ui_rx,
            cancel_token,
            push_interjection_message,
        ),
    )
    .await;
    let outcome = match result {
        Ok(outcome) => {
            tracing::info!("[UNIFIED_CALL] Completed normally");
            outcome
        }
        Err(_elapsed) => {
            let action_hint = tc.arguments.chars().take(100).collect::<String>();
            tracing::error!(
                "[UNIFIED_CALL] TIMEOUT after 300s - aborting | iteration={} | tool_calls_in_turn={} | action_hint={}",
                iteration,
                tool_calls.len(),
                action_hint
            );
            let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                "⏱️ 操作超时 (300s) - 强制结束 | 已重试 {} 次",
                iteration
            )));
            emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
            return UnifiedDispatchOutcome::TurnDone;
        }
    };
    match outcome {
        crate::agent::unified_handler::UnifiedHandleOutcome::Result {
            content,
            is_error,
            deferred_system,
            delegate_meta,
        } => {
            tracing::info!(
                "[UNIFIED_OUTCOME] Result: error={}, content_len={}",
                is_error,
                content.len()
            );
            if is_error {
                if content.contains("empty arguments") || content.contains("invalid JSON") {
                    *unified_parse_error_streak += 1;
                    if *unified_parse_error_streak >= 3 {
                        messages.push(Message::system(
                            "⚠️ 已连续 3 次空/无效 complete_and_check 参数。\
                             必须发送合法 JSON：{\"action\":\"…\",\"params\":{…}}",
                        ));
                    }
                    if *unified_parse_error_streak >= 5 {
                        let _ = ui_tx.send(AgentToUiEvent::Status(
                            "⏹️ 连续 5 次无效 complete_and_check - 强制结束本轮".to_string(),
                        ));
                        emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
                        return UnifiedDispatchOutcome::TurnDone;
                    }
                }
            } else {
                *unified_parse_error_streak = 0;
            }
            if is_error && tc.arguments.contains("\"finding") {
                *findings_deliver_error_streak += 1;
                if *findings_deliver_error_streak >= 3 {
                    messages.push(Message::system(
                        "⚠️ 连续 3 次 finding_json 格式错误。改用 finish(params.content=...) 先汇报分析。",
                    ));
                    *findings_deliver_error_streak = 0;
                }
            }
            deferred_tool_system.extend(deferred_system);

            let content_preview: String = if content.len() > 8000 {
                let head: String = content.chars().take(8000).collect();
                format!("{head}... (truncated, {} total)", content.len())
            } else {
                content.clone()
            };
            let args_preview: String = tc.arguments.chars().take(500).collect();
            tracing::debug!(
                "[UNIFIED_IO] complete_and_check | args={} | error={} | result={}",
                args_preview,
                is_error,
                content_preview
            );

            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: content.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
            if let Some(meta) = delegate_meta {
                turn_memory.record_tool_with_result(
                    &meta.inner_tool,
                    &meta.inner_args,
                    !is_error,
                    Some(&content),
                );
                let target = crate::agent::exploration_snapshot::target_from_tool_args(
                    &meta.inner_tool,
                    &meta.inner_args,
                );
                let observation: String =
                    crate::agent::exploration_snapshot::extract_data_content(&content)
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" | ")
                        .chars()
                        .take(260)
                        .collect();
                let status = if is_error { "失败" } else { "成功" };
                turn_memory.record_decision(format!(
                    "你刚才执行 {}({}) {status}; 观察到: {}; 后续避免重复同一查询",
                    meta.inner_tool, target, observation
                ));
                let _ = (&meta.inner_args, &meta.live_output);
                if let Some(ref ms) = tool_ctx.memory_store {
                    let (session_id, _react_task) =
                        react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
                    let decision = turn_memory
                        .decisions
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "本轮仅工具调用".into());
                    let outcome_str = if is_error { "error" } else { "ok" };
                    let _ = record_react_tool(
                        ms.as_ref(),
                        &session_id,
                        user_task.as_deref().unwrap_or(""),
                        &meta.inner_tool,
                        &target,
                        outcome_str,
                        &decision,
                        full_text,
                        reasoning_content,
                        unified_tool_mode,
                        &meta.inner_args,
                        &meta.live_output,
                    )
                    .await;
                }
            } else {
                turn_memory.record_tool(&tc.name, &tc.arguments, is_error);
                if let Some(ref ms) = tool_ctx.memory_store {
                    let (session_id, task_desc) =
                        react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
                    let target_json: Option<serde_json::Value> =
                        serde_json::from_str(&tc.arguments).ok();
                    let target = target_json
                        .as_ref()
                        .and_then(|v| {
                            v.get("params")
                                .or_else(|| v.get("path"))
                                .or_else(|| v.get("name"))
                        })
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let decision = turn_memory
                        .decisions
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "本轮仅工具调用".into());
                    let outcome_str = if is_error { "error" } else { "ok" };
                    let _ = record_react_tool(
                        ms.as_ref(),
                        &session_id,
                        &task_desc,
                        &tc.name,
                        &target,
                        outcome_str,
                        &decision,
                        full_text,
                        reasoning_content,
                        unified_tool_mode,
                        &tc.arguments,
                        &content,
                    )
                    .await;
                }
            }
            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: content,
                is_error,
            });
        }
        crate::agent::unified_handler::UnifiedHandleOutcome::TurnDone { summary } => {
            let finish_content_text = summary.clone().unwrap_or_default();
            if let Some(summary) = summary {
                let summary = summary.trim();
                if !summary.is_empty() {
                    match new_messages
                        .iter_mut()
                        .rev()
                        .find(|m| matches!(m, Message::Assistant { .. }))
                    {
                        Some(Message::Assistant { content, .. }) if content.trim().is_empty() => {
                            *content = summary.to_string();
                        }
                        _ => new_messages.push(Message::assistant(summary)),
                    }
                }
            }
            if let Some(ref ms) = tool_ctx.memory_store {
                let (session_id, task_desc) =
                    react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
                let decision = turn_memory
                    .decisions
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "finish: round completed".into());
                let _ = record_react_tool(
                    ms.as_ref(),
                    &session_id,
                    &task_desc,
                    tc.name.as_str(),
                    finish_content_text.as_str(),
                    "ok",
                    &decision,
                    full_text,
                    reasoning_content,
                    unified_tool_mode,
                    &tc.arguments,
                    &finish_content_text,
                )
                .await;
            }
            if let Some(wf) = workflow_engine
                && let Ok(engine) = wf.try_lock()
            {
                crate::memory::round_memory::append_round(
                    &engine,
                    crate::memory::round_memory::RoundRecord {
                        round_id: iteration,
                        user_intent: user_task.clone().unwrap_or_default(),
                        actions_summary: turn_memory.tool_names_summary(),
                        deliverables_summary: "finish confirmed".into(),
                        gate_outcomes: vec!["finish:user_finished".into()],
                    },
                );
            }
            emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
            return UnifiedDispatchOutcome::TurnDone;
        }
        crate::agent::unified_handler::UnifiedHandleOutcome::Aborted => {
            emit_turn_done(ui_tx, turn_id, new_messages.clone(), total_usage.clone());
            return UnifiedDispatchOutcome::TurnDone;
        }
    }
    persist_turn_memory(workflow_engine, turn_memory);
    UnifiedDispatchOutcome::Continue
}

// ── ReAct log helpers ──────────────────────────────────────────────────────
// These three helpers eliminate ~150 lines of repeated boilerplate across the
// 8 react_log call sites in `run_agent_turn`. Each site was extracting the same
// session_id/task_desc/target/assistant_text/reasoning_fallback values.

/// Extract (session_id, task_desc) from the workflow engine. Both values come
/// from the same engine lock; this collapses 8× duplicated try_lock chains.
fn react_log_ids(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    fallback_task: &str,
) -> (String, String) {
    let session_id = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| e.session_id())
        .unwrap_or_else(|| "default".to_string());
    let task_desc = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .and_then(|e| e.get_variable("_current_user_request"))
        .unwrap_or_else(|| fallback_task.to_string());
    (session_id, task_desc)
}

/// Build the assistant_text field for react_log: prefer visible text, fall back
/// to a reasoning summary, else a placeholder.
fn react_log_assistant_text(full_text: &str, reasoning_content: &str) -> String {
    let raw = crate::agent::think_stream::visible_only(full_text);
    if raw.trim().is_empty() {
        if !reasoning_content.trim().is_empty() {
            format!("(思考摘要) {reasoning_content}")
        } else {
            "(本轮仅工具调用，无可见文本)".into()
        }
    } else {
        raw
    }
}

/// Record a single tool execution to react_log. This covers the common case
/// (tool executed, has a result). Used by both the unified-handler path (with
/// delegate_meta) and the legacy tool path.
#[allow(clippy::too_many_arguments)]
async fn record_react_tool(
    ms: &crate::memory::store::MemoryStore,
    session_id: &str,
    task_desc: &str,
    tool_name: &str,
    target: &str,
    outcome: &str,
    decision: &str,
    full_text: &str,
    reasoning_content: &str,
    unified_tool_mode: bool,
    raw_args: &str,
    live_output: &str,
) {
    let assistant_text = react_log_assistant_text(full_text, reasoning_content);
    let reasoning_fallback = build_reasoning_fallback(
        reasoning_content,
        tool_name,
        raw_args,
        full_text,
        unified_tool_mode,
    );
    let _ = ms.record_react(
        session_id,
        task_desc,
        tool_name,
        target,
        outcome,
        decision,
        &assistant_text,
        &reasoning_fallback,
        live_output,
    );
}

/// Drain queued interjections before each tool execution (simpler variant).
///
/// Unlike `drain_interjections_pre_llm`, this does not check for confirmation
/// messages -- it simply forwards interjection text and handles pre-ack for
/// ScopeConfirmed / BusinessAck events.
fn drain_interjections_pre_tool(
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    push_interjection_message: PushInterjectionFn,
) {
    while let Ok(ev) = ui_rx.try_recv() {
        match ev {
            ui_event::UiToAgentEvent::Interjection(text) => {
                push_interjection_message(workflow_engine, messages, &text, ui_tx);
            }
            ui_event::UiToAgentEvent::ScopeConfirmed
            | ui_event::UiToAgentEvent::BusinessAck { .. } => {
                if let Some(wf) = workflow_engine
                    && let Ok(engine) = wf.try_lock()
                {
                    engine.set_variable(
                        crate::agent::gate::business_gate::PRE_ACK_KEY,
                        "1".to_string(),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Drain queued interjections before the LLM call.
///
/// Detects confirmation messages ("c", "/confirm", "/fix", etc.) and sets
/// pre-ack so the scope gate skips waiting. Also handles ScopeConfirmed /
/// BusinessAck events by setting pre-ack.
fn drain_interjections_pre_llm(
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    push_interjection_message: PushInterjectionFn,
) {
    while let Ok(ev) = ui_rx.try_recv() {
        match ev {
            ui_event::UiToAgentEvent::Interjection(text) => {
                let trimmed = text.trim();
                let is_confirm = trimmed == "c"
                    || trimmed.starts_with("/confirm")
                    || trimmed.starts_with("/fix")
                    || trimmed.contains("确认")
                    || trimmed.contains("开始实施");
                if is_confirm
                    && let Some(wf) = workflow_engine
                    && let Ok(engine) = wf.try_lock()
                {
                    engine.set_variable(
                        crate::agent::gate::business_gate::PRE_ACK_KEY,
                        "1".to_string(),
                    );
                }
                push_interjection_message(workflow_engine, messages, &text, ui_tx);
            }
            ui_event::UiToAgentEvent::ScopeConfirmed
            | ui_event::UiToAgentEvent::BusinessAck { .. } => {
                if let Some(wf) = workflow_engine
                    && let Ok(engine) = wf.try_lock()
                {
                    engine.set_variable(
                        crate::agent::gate::business_gate::PRE_ACK_KEY,
                        "1".to_string(),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Check if reflection should fire, and if so, inject reflection prompt
/// and return `true` (caller should continue the loop).
///
/// When a reflection threshold trips, this DISCARDS the current tool batch,
/// records the reasoning as a tool-call-free assistant message, and injects
/// the reflection prompt.
#[allow(clippy::too_many_arguments)]
fn check_reflection_skip(
    tool_calls: &[ToolCall],
    unified_tool_mode: bool,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    user_task: &str,
    budget: &mut crate::agent::turn_state::TurnBudget,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    content_with_reasoning: &str,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
) -> bool {
    let Some(prompt) = evaluate_reflection(
        tool_calls,
        unified_tool_mode,
        workflow_engine,
        user_task,
        budget,
        ui_tx,
    ) else {
        return false;
    };
    let reasoning_only = Message::Assistant {
        content: content_with_reasoning.to_string(),
        tool_calls: Vec::new(),
        reasoning_content: None,
    };
    new_messages.push(reasoning_only.clone());
    messages.push(reasoning_only);
    messages.push(Message::system(&prompt));
    new_messages.push(Message::system(&prompt));
    persist_turn_memory(workflow_engine, turn_memory);
    true
}

/// Outcome of `handle_review_findings`.
enum ReviewFindingsOutcome {
    /// Gate cancelled; break the main loop.
    Break,
    /// Gate acknowledged or discuss; continue the main loop.
    Continue,
    /// No review findings captured; proceed with tool execution.
    Proceed,
}

/// Handle non-unified review findings capture + business gate.
///
/// If the LLM's response contains review findings and we're not in unified
/// mode, this captures the findings, opens the business scope gate, and
/// returns the gate's resume decision.
#[allow(clippy::too_many_arguments)]
async fn handle_review_findings(
    unified_tool_mode: bool,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    full_text: &str,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    ui_rx: &mut mpsc::UnboundedReceiver<ui_event::UiToAgentEvent>,
    cancel_token: &CancellationToken,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    tools_used_this_turn: &mut std::collections::HashSet<String>,
    pre_llm_step_idx: usize,
    push_interjection_message: PushInterjectionFn,
) -> ReviewFindingsOutcome {
    if unified_tool_mode || !try_capture_review_findings(workflow_engine, full_text, ui_tx) {
        return ReviewFindingsOutcome::Proceed;
    }
    let visible = crate::agent::think_stream::visible_only(full_text);
    let content_for_session = execute_user_display(workflow_engine, pre_llm_step_idx, &visible);
    let msg = Message::Assistant {
        content: content_for_session,
        tool_calls: Vec::new(),
        reasoning_content: None,
    };
    upsert_review_report_assistant(messages, &msg);
    upsert_review_report_assistant(new_messages, &msg);

    match business_gate::await_findings_scope_gate(
        ui_rx,
        cancel_token,
        workflow_engine,
        messages,
        ui_tx,
        push_interjection_message,
    )
    .await
    {
        business_gate::BusinessGateResume::Cancelled => ReviewFindingsOutcome::Break,
        business_gate::BusinessGateResume::Acknowledged => {
            refresh_turn_memory_for_implement(workflow_engine, turn_memory);
            tools_used_this_turn.clear();
            persist_turn_memory(workflow_engine, turn_memory);
            ReviewFindingsOutcome::Continue
        }
        business_gate::BusinessGateResume::Discuss => {
            messages.push(Message::system(
                "📋 用户提供了反馈。请根据反馈更新 findings/计划，重新提交。禁止直接进入实施。",
            ));
            persist_turn_memory(workflow_engine, turn_memory);
            ReviewFindingsOutcome::Continue
        }
    }
}

/// Prepare messages and turn memory for the next LLM call.
///
/// Syncs turn memory from message scan, collapses redundant idle narration,
/// assembles context window, sanitizes tool pairs, and strips reasoning.
/// Returns `slim_in_impl_phase` flag for downstream use.
#[allow(clippy::too_many_arguments)]
fn prepare_llm_context(
    messages: &mut Vec<Message>,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    iteration: u32,
    user_task: &str,
    unified_tool_mode: bool,
    memory_store: &Option<std::sync::Arc<crate::memory::store::MemoryStore>>,
    explore_streak: u32,
    total_explore: u32,
    impl_streak: u32,
) -> bool {
    // Sync turn memory from full message scan (survives compaction)
    let include_writes = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| e.is_task_step())
        .unwrap_or(true);
    turn_memory.sync_from_messages(messages, include_writes);
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
        && let Some(ti) = user_round::get_turn_user_input(&engine)
    {
        turn_memory.user_task = ti;
    }

    // Workflow: collapse repeated idle narration (keeps LLM context lean)
    if workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .is_some_and(|e| e.is_workflow_active())
    {
        crate::agent::gate::idle_narrative::collapse_redundant_idle(messages);
    }

    // ── Unified context assembly ──
    let slim_in_impl_phase = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| crate::agent::phase::is_implementation_phase(&e))
        .unwrap_or(false);
    crate::context::assembler::ContextAssembler::assemble(
        messages,
        user_task,
        iteration,
        turn_memory,
        workflow_engine,
        unified_tool_mode,
        memory_store,
        &workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.session_id().to_string())
            .unwrap_or_default(),
        explore_streak,
        total_explore,
        impl_streak,
        slim_in_impl_phase,
    );

    crate::context::sanitize_tool_pairs(messages);
    crate::agent::think_stream::prepare_messages_for_llm(messages);

    slim_in_impl_phase
}

/// Record the LLM's decision BEFORE tool execution to the SQLite react_log.
/// This captures the tool batch intent (not yet executed) for cross-round memory.
#[allow(clippy::too_many_arguments)]
async fn record_llm_decision(
    tool_calls: &[ToolCall],
    tool_ctx: &Arc<ToolContext>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    user_task: &Option<String>,
    turn_memory: &crate::memory::turn_memory::TurnMemory,
    full_text: &str,
    reasoning_content: &str,
    unified_tool_mode: bool,
) {
    let actions_summary = tool_calls
        .iter()
        .map(|tc| {
            if unified_tool_mode && tc.name == crate::agent::unified_action::TOOL_NAME {
                crate::agent::unified_action::parse_request(&tc.arguments)
                    .map(|req| req.action)
                    .unwrap_or_else(|_| tc.name.clone())
            } else {
                tc.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(ref ms) = tool_ctx.memory_store {
        let (session_id, task_desc) =
            react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
        let first_tool = tool_calls.first();
        let (tool_name, tool_target) = first_tool
            .map(|tc| extract_tool_name_and_target(tc, unified_tool_mode))
            .unwrap_or(("(no tool)".to_string(), String::new()));
        let decision = turn_memory
            .decisions
            .last()
            .cloned()
            .unwrap_or_else(|| format!("LLM 选择执行: {}", actions_summary));
        let _ = record_react_tool(
            ms.as_ref(),
            &session_id,
            &task_desc,
            &tool_name,
            &tool_target,
            "llm_decision",
            &decision,
            full_text,
            reasoning_content,
            unified_tool_mode,
            first_tool.map(|tc| tc.arguments.as_str()).unwrap_or(""),
            &format!("(待执行) {} 个工具调用", tool_calls.len()),
        )
        .await;
    }
}

/// Record a legacy (non-unified) tool execution to the SQLite react_log.
/// This is the simple path: tool name + raw args + result content.
#[allow(clippy::too_many_arguments)]
async fn record_react_log(
    tc: &ToolCall,
    result: &crate::tools::ToolOutput,
    tool_ctx: &Arc<ToolContext>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    user_task: &Option<String>,
    turn_memory: &crate::memory::turn_memory::TurnMemory,
    full_text: &str,
    reasoning_content: &str,
    unified_tool_mode: bool,
) {
    if let Some(ref ms) = tool_ctx.memory_store {
        let (session_id, task) = react_log_ids(workflow_engine, user_task.as_deref().unwrap_or(""));
        let target_json: Option<serde_json::Value> = serde_json::from_str(&tc.arguments).ok();
        let target = target_json
            .as_ref()
            .and_then(|v| {
                v.get("params")
                    .or_else(|| v.get("path"))
                    .or_else(|| v.get("name"))
            })
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let outcome = if result.is_error { "error" } else { "ok" };
        let decision = turn_memory
            .decisions
            .last()
            .cloned()
            .unwrap_or_else(|| "本轮仅工具调用".into());
        let _ = record_react_tool(
            ms.as_ref(),
            &session_id,
            &task,
            &tc.name,
            &target,
            outcome,
            &decision,
            full_text,
            reasoning_content,
            unified_tool_mode,
            &tc.arguments,
            &result.content,
        )
        .await;
    }
}

/// Repair malformed / empty tool-call arguments and extract XML-style tool
/// calls from text content (GLM models emit `<tool_call>` XML as text instead of
/// using the OpenAI function-calling protocol).
fn repair_and_extract_tool_calls(
    tool_calls: &mut Vec<ToolCall>,
    full_text: &str,
    reasoning_content: &str,
) {
    let fallback_blob = format!("{full_text}\n{reasoning_content}");
    let fallbacks = [fallback_blob.as_str()];
    for tc in tool_calls.iter_mut() {
        tc.arguments = crate::agent::tool_args_repair::recover_tool_call_arguments(
            &tc.name,
            &tc.arguments,
            &fallbacks,
        );
    }

    if tool_calls.is_empty() {
        let extracted = crate::agent::tool_args_repair::extract_xml_tool_calls(full_text);
        if !extracted.is_empty() {
            tracing::info!(
                "[XML_EXTRACT] Extracted {} tool call(s) from <tool_call> XML in text content",
                extracted.len()
            );
            *tool_calls = extracted;
        }
    }
}

/// Run the unified budget offload check. When the API's real prompt-token count
/// crosses the adaptive threshold, cluster the un-archived ReAct log into
/// memory-graph nodes, persist them, and placeholder the old ReAct messages.
#[allow(clippy::too_many_arguments)]
async fn run_memory_offload(
    last_prompt_tokens: u32,
    messages: &mut Vec<Message>,
    workflow_engine: &Option<
        std::sync::Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>,
    >,
    tool_ctx: &ToolContext,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    active_provider: &Arc<dyn LlmProvider>,
) {
    if last_prompt_tokens == 0 {
        return;
    }
    let Some(ref ms) = tool_ctx.memory_store else {
        return;
    };
    let (session_id, fail_streak, cooldown) = {
        let session_id = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.session_id())
            .unwrap_or_else(|| "default".to_string());
        let fail_streak = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .and_then(|e| e.get_variable(crate::memory::memory_offload::OFFLOAD_FAIL_VAR))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let cooldown = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .and_then(|e| e.get_variable(crate::memory::memory_offload::OFFLOAD_COOLDOWN_VAR))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        (session_id, fail_streak, cooldown)
    };
    let gitnexus_available = if let Some(svc) = tool_ctx.gitnexus.as_ref() {
        svc.is_ready().await
    } else {
        false
    };
    let context_window = active_provider.context_window_size();
    let summarizer = tool_ctx.summarizer.clone();
    let ui_tx_offload = ui_tx.clone();
    let (outcome, new_streak, new_cooldown) =
        crate::memory::memory_offload::offload_if_over_budget(
            last_prompt_tokens,
            context_window,
            messages,
            summarizer,
            active_provider,
            ms,
            &session_id,
            fail_streak,
            cooldown,
            gitnexus_available,
            |s| {
                let _ = ui_tx_offload.send(AgentToUiEvent::Status(s));
            },
        )
        .await;
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
    {
        engine.set_variable(
            crate::memory::memory_offload::OFFLOAD_FAIL_VAR,
            new_streak.to_string(),
        );
        engine.set_variable(
            crate::memory::memory_offload::OFFLOAD_COOLDOWN_VAR,
            new_cooldown.to_string(),
        );
        if matches!(
            outcome,
            crate::memory::memory_offload::OffloadOutcome::Archived { .. }
        ) {
            let block = crate::memory::memory_offload::build_memory_graph_block(ms, &session_id);
            engine.set_variable(crate::memory::memory_offload::MEMORY_GRAPH_VAR, block);
        }
    }
}

/// Build a content string that folds a short reasoning digest into the
/// visible display text. GLM-style models put nearly all their analysis inside
/// `<think>` and emit only a tool_call as visible output; dropping the reasoning
/// every turn means the model can't see WHY it did what it did last turn,
/// driving re-exploration. We fold a short head+tail digest into the content
/// so it survives into the next turn's context without bloating tokens.
fn build_content_with_reasoning(display: &str, reasoning_content: &str) -> String {
    let reasoning_digest = {
        let r = crate::agent::think_stream::visible_only(reasoning_content);
        let r = if r.is_empty() {
            reasoning_content.trim().to_string()
        } else {
            r
        };
        if r.is_empty() {
            String::new()
        } else {
            digest_reasoning(&r, 320)
        }
    };
    if reasoning_digest.is_empty() {
        display.to_string()
    } else if display.trim().is_empty() {
        format!("(本轮思考) {reasoning_digest}")
    } else {
        format!("{display}\n(本轮思考) {reasoning_digest}")
    }
}

/// Handle the case where the LLM produced only text (no tool calls).
/// Records to react_log, pushes assistant message, emits turn done.
/// Always returns `true` so the caller knows to `return`.
#[allow(clippy::too_many_arguments)]
async fn handle_empty_tool_calls(
    full_text: &str,
    reasoning_content: &str,
    tool_ctx: &ToolContext,
    workflow_engine: &Option<
        std::sync::Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>,
    >,
    user_task: Option<&str>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    turn_memory: &crate::memory::turn_memory::TurnMemory,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    turn_id: u64,
    total_usage: TokenUsage,
) -> bool {
    let visible = crate::agent::think_stream::visible_only(full_text);
    let reasoning = crate::agent::think_stream::visible_only(reasoning_content);

    if let Some(ref ms) = tool_ctx.memory_store {
        let (session_id, task_desc) = react_log_ids(workflow_engine, user_task.unwrap_or(""));
        let assistant_text = if visible.trim().is_empty() {
            reasoning.clone()
        } else {
            visible.clone()
        };
        let decision = if !reasoning.is_empty() {
            reasoning.chars().take(200).collect()
        } else {
            "LLM 输出文本".to_string()
        };
        let _ = ms.record_react(
            &session_id,
            &task_desc,
            "(llm_text)",
            "",
            "ok",
            &decision,
            &assistant_text,
            reasoning_content,
            "LLM 纯文本输出，本轮结束",
        );
    }

    let msg_content = if visible.trim().is_empty() {
        reasoning_content.to_string()
    } else {
        visible.clone()
    };
    let msg = Message::Assistant {
        content: msg_content,
        tool_calls: Vec::new(),
        reasoning_content: Some(reasoning_content.to_string()),
    };
    new_messages.push(msg.clone());
    messages.push(msg);

    let _ = ui_tx.send(AgentToUiEvent::Status(
        "✅ LLM 输出完成，本轮结束".to_string(),
    ));
    persist_turn_memory(workflow_engine, turn_memory);
    emit_turn_done(ui_tx, turn_id, std::mem::take(new_messages), total_usage);
    true
}

/// Build a summary of the LLM's visible text and tool actions for this turn,
/// then record it as a decision in turn_memory.
fn record_turn_decision(
    tool_calls: &[ToolCall],
    full_text: &str,
    reasoning_content: &str,
    unified_tool_mode: bool,
    turn_memory: &mut crate::memory::turn_memory::TurnMemory,
) {
    let mut visible_summary: String = crate::agent::think_stream::visible_only(full_text)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" | ");
    visible_summary = visible_summary.chars().take(260).collect();
    if visible_summary.trim().is_empty() && !reasoning_content.trim().is_empty() {
        let r = crate::agent::think_stream::visible_only(reasoning_content);
        let r = if r.is_empty() {
            reasoning_content.to_string()
        } else {
            r
        };
        visible_summary = digest_reasoning(&r, 260);
    }
    let actions_summary = tool_calls
        .iter()
        .map(|tc| {
            if unified_tool_mode && tc.name == crate::agent::unified_action::TOOL_NAME {
                crate::agent::unified_action::parse_request(&tc.arguments)
                    .map(|req| req.action)
                    .unwrap_or_else(|_| tc.name.clone())
            } else {
                tc.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if !actions_summary.is_empty() {
        turn_memory.record_decision(format!(
            "你刚才选择动作: {actions_summary}; 当时的可见依据: {visible_summary}"
        ));
    }
}

/// Extract a human-readable (name, target) pair from a tool call for react_log.
/// Handles both unified-mode and native tool calls.
fn extract_tool_name_and_target(tc: &ToolCall, unified_tool_mode: bool) -> (String, String) {
    if unified_tool_mode && tc.name == crate::agent::unified_action::TOOL_NAME {
        crate::agent::unified_action::parse_request(&tc.arguments)
            .ok()
            .map(|req| {
                let target = req
                    .params
                    .get("path")
                    .or_else(|| req.params.get("name"))
                    .or_else(|| req.params.get("target"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                (req.action, target)
            })
            .unwrap_or_else(|| (tc.name.clone(), String::new()))
    } else {
        let target_json: Option<serde_json::Value> = serde_json::from_str(&tc.arguments).ok();
        let target = target_json
            .as_ref()
            .and_then(|v| v.get("path").or_else(|| v.get("name")))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        (tc.name.clone(), target)
    }
}

/// Remove tool_calls from the latest Assistant message that have no matching
/// ToolResult (tools skipped by validation/safety/truncation/loop-limit).
/// Also remove the Assistant message entirely if it became empty.
fn prune_orphaned_tool_calls(messages: &mut [&mut Vec<Message>]) {
    let all_result_ids: std::collections::HashSet<String> = messages
        .iter()
        .flat_map(|msgs| {
            msgs.iter().filter_map(|m| {
                if let Message::ToolResult { tool_call_id, .. } = m {
                    Some(tool_call_id.clone())
                } else {
                    None
                }
            })
        })
        .collect();
    for msgs in messages {
        if let Some(last_assistant_pos) = msgs
            .iter()
            .rposition(|m| matches!(m, Message::Assistant { .. }))
            && let Message::Assistant { tool_calls, .. } = &mut msgs[last_assistant_pos]
        {
            let before = tool_calls.len();
            tool_calls.retain(|tc| all_result_ids.contains(&tc.id));
            if tool_calls.len() != before {
                tracing::info!(
                    "[POST-FILTER] Removed {} orphaned tool_calls from latest Assistant msg ({} -> {})",
                    before - tool_calls.len(),
                    before,
                    tool_calls.len()
                );
            }
        }
        if let Some(pos) = msgs.iter().rposition(|m| matches!(m, Message::Assistant { content, tool_calls, .. } if content.is_empty() && tool_calls.is_empty())) {
            msgs.remove(pos);
        }
    }
}

/// Run post-edit verification checks: AST recovery, edit tracking, verify hints,
/// and Done reminder injection. Mutates `messages` with deferred system notes.
#[allow(clippy::too_many_arguments)]
fn run_post_edit_checks(
    tool_calls: &[ToolCall],
    messages: &mut Vec<Message>,
    new_messages: &[Message],
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    tool_ctx: &ToolContext,
    unified_tool_mode: bool,
) {
    let has_write = tool_calls.iter().any(|tc| {
        matches!(
            tc.name.as_str(),
            "file_write" | "edit_file" | "delete_range"
        )
    });
    let has_ast = post_edit_verification::tool_batch_has_ast_issues(new_messages, tool_calls);
    post_edit_verification::check_ast_and_recover(messages, new_messages, tool_calls);

    let execute_coding = workflow_engine.as_ref().is_some_and(|wf| {
        wf.try_lock()
            .map(|e| e.is_task_step() && !e.is_perceive_execute())
            .unwrap_or(false)
    });
    if execute_coding {
        let project_root = tool_ctx
            .runtime
            .project_root
            .clone()
            .unwrap_or_else(|| tool_ctx.working_dir.clone());
        if let Some(engine_arc) = workflow_engine
            && let Ok(engine) = engine_arc.try_lock()
        {
            post_edit_verification::track_edits_and_verify_plan(
                &engine,
                &project_root,
                tool_calls,
                new_messages,
                true,
            );
            if !has_ast && let Some(hint) = post_edit_verification::verify_hint_message(&engine) {
                messages.push(Message::system(&hint));
            }
        }
    }

    if has_write && !has_ast {
        let verify_blocking = workflow_engine.as_ref().and_then(|wf| {
            wf.try_lock()
                .ok()
                .and_then(|e| post_edit_verification::check_execute_done_gate(&e))
        });
        let ast_pending = workflow_engine.as_ref().and_then(|wf| {
            wf.try_lock()
                .ok()
                .and_then(|e| e.get_variable("_ast_pending"))
                .filter(|s| !s.is_empty())
        });
        if verify_blocking.is_none() && ast_pending.is_none() {
            messages.push(Message::system(if unified_tool_mode {
                "Files were modified. Run verify via shell_exec if needed, then complete_and_check(action=finish, params={summary:\"...\"}). 3 lines max in summary."
            } else {
                "Files were modified. Run project verify if not done yet, then output ## Done with what changed and verify result. 3 lines max."
            }));
        }
    }
}

/// Build a contextual error message for tool arguments that were truncated
/// (incomplete JSON). Produces tool-specific guidance for file_write and
/// edit_file, or a generic message for other tools.
fn build_truncation_error(name: &str, arguments: &str) -> String {
    let is_file_write = name == "file_write";
    let is_edit_file = name == "edit_file";
    let content_length = arguments.len();

    if is_file_write && content_length > 10000 {
        format!(
            "❌ Content Too Large - Arguments Truncated:\n\
             The 'content' parameter appears to be too large ({:.1} KB).\n\
             This usually happens when trying to write a large file in one call.\n\n\
             💡 Solutions (choose one):\n\n\
             1️⃣ Retry the request:\n\
                The system will automatically handle large files (>1 MB) using chunked writes.\n\
                Just resend the complete content without worrying about size.\n\n\
             2️⃣ Split into multiple operations:\n\
                - Write first part: {{\"path\": \"file.txt\", \"content\": \"part1...\"}}\n\
                - Use edit_file to append/modify remaining parts\n\n\
             3️⃣ Use edit_file for modifications:\n\
                If modifying existing file, use search/replace instead of rewriting entire file\n\n\
             📝 Note: Files >1 MB are automatically written in 512 KB chunks",
            content_length as f64 / 1024.0
        )
    } else if is_edit_file && content_length > 500 {
        let partial_info =
            if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(arguments) {
                let path = args_val
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<not specified>");
                let has_search = args_val.get("search").is_some();
                let has_replace = args_val.get("replace").is_some();
                format!(
                    "\n\n📋 Partial arguments received:\n\
                 • path: {}\n\
                 • search: {}\n\
                 • replace: {}",
                    path,
                    if has_search {
                        "✅ present (may be truncated)"
                    } else {
                        "❌ missing"
                    },
                    if has_replace {
                        "✅ present (may be truncated)"
                    } else {
                        "❌ missing"
                    }
                )
            } else {
                "".to_string()
            };

        format!(
            "❌ Arguments Truncated - edit_file parameters incomplete:\n\
             Your search/replace content was too long and got truncated ({:.1} KB).\n\
             This usually happens when including too many lines of code context.\n\n\
             💡 How to fix:\n\
             1️⃣ Use SHORTER search strings:\n\
                - Include only 2-3 unique lines that uniquely identify the code\n\
                - Use distinctive identifiers (method names, variable names)\n\
                - Example: {{\"search\": \"fn process_order() {{\n    let order = validate();\"}}\n\n\
             2️⃣ Use file_read first:\n\
                - Read the file to see exact line numbers\n\
                - Copy the EXACT text including whitespace\n\
                - Use line numbers to ensure you have unique context\n\n\
             3️⃣ Break into multiple patches:\n\
                - Instead of one large patch, make 2-3 smaller edit_file calls\n\
                - Each patch should change <50% of the file\n\
                - Or use file_write to rewrite the entire file\n{}\n\n\
             📝 Example of good search string (2-3 lines):\n\
             {{\"path\": \"src/main.rs\", \"search\": \"fn calculate() {{\n    let result = a + b;\", \"replace\": \"fn calculate() {{\n    let result = a * b;\"}}",
            content_length as f64 / 1024.0,
            partial_info
        )
    } else {
        format!(
            "❌ JSON Truncation Error for tool '{}':\n\
             Arguments were truncated (incomplete JSON). This usually happens when:\n\
             • The response exceeded the token limit\n\
             • The content was cut off during transmission\n\n\
             💡 How to fix:\n\
             • Retry with a shorter or more concise request\n\
             • Break large operations into smaller steps\n\
             • Ensure complete JSON syntax with all brackets/braces closed\n\n\
             📝 Example of complete JSON:\n\
             {{\"path\": \"output.txt\", \"content\": \"Hello World\"}}\n\n\
             Please retry with complete arguments.",
            name
        )
    }
}

/// Build a contextual error message for tool arguments that failed JSON parsing.
fn build_arg_parse_error(name: &str, parse_err: &serde_json::Error) -> String {
    let example = match name {
        "file_read" => "{\"path\": \"src/main.rs\", \"limit\": 100}",
        "file_write" => "{\"path\": \"output.txt\", \"content\": \"Hello World\"}",
        "edit_file" => {
            "{\"path\": \"src/lib.rs\", \"old_string\": \"...\", \"new_string\": \"...\"}"
        }
        "shell_exec" => "{\"command\": \"ls -la\", \"timeout_ms\": 5000}",
        "file_search" => "{\"pattern\": \"*.rs\", \"path\": \"src/\"}",
        "code_search" => "{\"pattern\": \"fn main\", \"path\": \"src/\"}",
        "code_graph" => {
            "{\"op\": \"impact\", \"target\": \"funcName\", \"direction\": \"upstream\"}"
        }
        _ => "{ /* check tool documentation */ }",
    };
    format!(
        "❌ JSON Parse Error for tool '{}':\n{}\n\n\
         💡 How to fix:\n\
         • Ensure valid JSON syntax (no trailing commas)\n\
         • Quote all keys and string values with double quotes\n\
         • Escape special characters in strings\n\
         • Check for missing brackets or braces\n\n\
         📝 Correct format example:\n\
         {}\n\n\
         Please retry with corrected arguments.",
        name, parse_err, example
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tc(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }
    }

    #[test]
    fn classify_valid_json_no_truncation() {
        let mut tcs = vec![
            make_tc("a", "file_read", r#"{"path":"src/main.rs"}"#),
            make_tc(
                "b",
                "edit_file",
                r#"{"path":"x","old_string":"a","new_string":"b"}"#,
            ),
        ];
        let cls = classify_tool_calls(&mut tcs, 5);
        assert!(cls.truncated_ids.is_empty());
        assert!(cls.exceeded_loop_limit_ids.is_empty());
        assert_eq!(cls.temp_counts.len(), 2); // two distinct keys
    }

    #[test]
    fn classify_truncated_args_detected_and_reset() {
        let mut tcs = vec![
            // Incomplete JSON: missing closing brace
            make_tc("t1", "file_write", r#"{"path":"out.txt","content":"hello"#),
        ];
        let cls = classify_tool_calls(&mut tcs, 5);
        assert!(cls.truncated_ids.contains("t1"));
        // Arguments should be reset to {}
        assert_eq!(tcs[0].arguments, "{}");
    }

    #[test]
    fn classify_invalid_but_not_truncated_passes_through() {
        let mut tcs = vec![
            // Valid JSON shape but not truncated -- trailing comma (not truncation)
            make_tc("x1", "shell_exec", r#"{"command":"ls",}"#),
        ];
        let cls = classify_tool_calls(&mut tcs, 5);
        assert!(cls.truncated_ids.is_empty());
        // Arguments NOT reset (left as-is for normal error handling)
        assert_eq!(tcs[0].arguments, r#"{"command":"ls",}"#);
    }

    #[test]
    fn classify_loop_limit_exceeded() {
        let mut tcs = vec![
            make_tc("c1", "file_read", r#"{"path":"a"}"#),
            make_tc("c2", "file_read", r#"{"path":"a"}"#),
            make_tc("c3", "file_read", r#"{"path":"a"}"#),
        ];
        // Limit = 2: 3rd call exceeds
        let cls = classify_tool_calls(&mut tcs, 2);
        assert!(cls.exceeded_loop_limit_ids.contains("c3"));
        assert!(!cls.exceeded_loop_limit_ids.contains("c1"));
        assert!(!cls.exceeded_loop_limit_ids.contains("c2"));
        // Same dedup key for all three
        assert_eq!(cls.temp_counts.len(), 1);
        assert_eq!(cls.temp_counts.values().next().copied().unwrap_or(0), 3);
    }

    #[test]
    fn classify_different_paths_no_loop() {
        let mut tcs = vec![
            make_tc("d1", "file_read", r#"{"path":"a.rs"}"#),
            make_tc("d2", "file_read", r#"{"path":"b.rs"}"#),
            make_tc("d3", "file_read", r#"{"path":"c.rs"}"#),
        ];
        let cls = classify_tool_calls(&mut tcs, 2);
        assert!(cls.exceeded_loop_limit_ids.is_empty());
        // Three distinct keys
        assert_eq!(cls.temp_counts.len(), 3);
    }

    #[test]
    fn classify_empty_args_not_truncated() {
        let mut tcs = vec![make_tc("e1", "file_list", "")];
        let cls = classify_tool_calls(&mut tcs, 5);
        assert!(cls.truncated_ids.is_empty());
        // Empty args still get a loop key
        assert_eq!(cls.tool_loop_keys.len(), 1);
    }

    #[test]
    fn truncation_error_file_write_large() {
        let args = "x".repeat(11000);
        let msg = build_truncation_error("file_write", &args);
        assert!(msg.contains("Content Too Large"));
    }

    #[test]
    fn truncation_error_edit_file_long() {
        let args = "x".repeat(600);
        let msg = build_truncation_error("edit_file", &args);
        assert!(msg.contains("Arguments Truncated"));
        assert!(msg.contains("edit_file"));
    }

    #[test]
    fn truncation_error_generic() {
        let args = "{partial";
        let msg = build_truncation_error("code_search", &args);
        assert!(msg.contains("JSON Truncation Error"));
        assert!(msg.contains("code_search"));
    }

    #[test]
    fn arg_parse_error_known_tool() {
        let args = "{bad json}";
        let err = serde_json::from_str::<serde_json::Value>(args).unwrap_err();
        let msg = build_arg_parse_error("file_read", &err);
        assert!(msg.contains("JSON Parse Error"));
        assert!(msg.contains("file_read"));
        assert!(msg.contains("src/main.rs"));
    }

    #[test]
    fn arg_parse_error_unknown_tool() {
        let args = "{bad json}";
        let err = serde_json::from_str::<serde_json::Value>(args).unwrap_err();
        let msg = build_arg_parse_error("my_custom_tool", &err);
        assert!(msg.contains("JSON Parse Error"));
        assert!(msg.contains("my_custom_tool"));
        assert!(msg.contains("check tool documentation"));
    }
}
