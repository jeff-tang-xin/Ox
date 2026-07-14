pub mod auto_reflect; // 🆕 Auto-reflection for skill generation
pub mod blackboard; // Cross-turn always-on-top user constraints (anti-drift)
pub mod business_gate; // User confirms outputs (findings scope)
pub mod collaboration;
pub mod completion; // Machine-verifiable completion receipt
pub mod context_injector; // 🆕 Task anchoring + knowledge re-injection
pub mod context_offloader;
pub mod context_slim; // Implement-phase context diet
pub mod enforcer;
pub mod engine;
pub mod error_recovery; // 🆕 Build/test failure auto-fix
pub mod exploration_snapshot; // Plan-step tool results for cross-step handoff
pub mod explore_reflect; // Explore-but-never-act loop guard (reflect-then-stop)
pub mod findings; // Canonical findings store (review → park → implement)
#[cfg(test)]
mod flow_e2e;
pub mod gatekeeper; // ## Done validation pipeline (not user business gate)
pub mod git_undo; // Git checkout undo per finding
pub mod idle_narrative; // Cross-step idle prose detection + output discipline
pub mod intent_routing;
pub mod interjection;
pub mod interrupt;
pub mod intervention;
pub mod memory_bridge; // Cross-turn durable memory injection
pub mod memory_offload; // Budget-triggered memory-graph offload (unified compaction)
pub mod onboarding; // First-time project skill generation
pub mod perception; // Structured findings from perceive phase
pub mod phase; // Review → Fix → Done phase transitions
pub mod plan_tracker; // Execute-step plan progress
pub mod post_edit_verification; // AST feedback + language verify gate
pub mod presentation; // Executive summary formatting for findings
pub mod progress;
pub mod read_guard;
pub mod repeat_guard; // Degenerate repeated-output loop guard (content-level)
pub mod round_memory;
pub mod safety_gate; // User confirms dangerous tool execution
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
pub mod turn_memory; // In-turn tool log + message compaction
pub mod ui_event;
pub mod unified_action;
pub mod unified_handler;
pub mod user_round; // Per-user-message round segmentation
pub mod verifier; // Post-edit read-only verifier pass
pub mod workflow;
pub mod workflow_command; // /fix /pause /confirm slash commands
pub mod workflow_guidance; // Mid-workflow user corrections without restart
pub mod workflow_phases; // 感知 → 思考 → 执行 phase state machine
pub mod workflow_session; // Park / resume persistent task session
pub mod workspace; // Single [WORKSPACE] LLM context block // Single-flow E2E integration tests

pub use engine::StepDisplayInfo;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::llm::{LlmProvider, LlmStreamEvent};
use crate::message::{Message, TokenUsage, ToolCall};
use crate::safety::TrustManager;
use crate::safety::injection;
use crate::tools::{SafetyLevel, ToolContext, ToolRegistry};

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
    turn_memory: &turn_memory::TurnMemory,
) {
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock() {
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

/// Deliver a user interjection into the live message list (workflow-aware).
fn push_interjection_message(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    text: &str,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) {
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock() {
            // Pin durable user constraints to the blackboard before any routing,
            // so a rule stated mid-task survives compaction and phase switches.
            if crate::agent::blackboard::looks_like_constraint(text) {
                crate::agent::blackboard::add_constraint(&engine, text);
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
                    let last_assistant: String = messages.iter().rev()
                        .filter_map(|m| {
                            if let Message::Assistant { content, .. } = m {
                                if !content.is_empty() { Some(content.clone()) } else { None }
                            } else { None }
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
                         直接 edit_file 改代码，不要重新读文件。".to_string()
                    };
                    messages.push(Message::system(&directive));
                    return;
                }
                tracing::info!("[WORKFLOW] Blocked mid-flight interjection in Act phase");
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    crate::agent::workflow_phases::act_interjection_blocked_message().to_string(),
                ));
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
        && let Ok(engine) = wf.try_lock() {
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
        && !store.findings.is_empty() {
            let _ = ui_tx.send(AgentToUiEvent::FindingsPanel {
                summary: crate::agent::presentation::panel_summary(&store),
                rows: store.progress_rows(),
            });
        }
    if result.phase == crate::agent::phase::SingleFlowPhase::AwaitUser {
        // Don't re-arm if already confirmed
        if !crate::agent::business_gate::scope_implementation_unlocked(&engine) {
            crate::agent::business_gate::arm_findings_scope(&engine);
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
            let ch = text[i..].chars().next().unwrap_or(char::REPLACEMENT_CHARACTER);
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
    turn_memory: &mut turn_memory::TurnMemory,
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
fn strip_all_injection_blocks(messages: &mut Vec<Message>) {
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
            || c.starts_with(crate::agent::memory_offload::MEMORY_GRAPH_TAG)
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

/// Build & inject a single compact `[TURN_CONTEXT]` block.
/// Replaces 7+ separate injection blocks that polluted context every iteration.
fn inject_slim_context(
    messages: &mut Vec<Message>,
    user_task: &str,
    iteration: u32,
    turn_memory: &turn_memory::TurnMemory,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    unified_tool_mode: bool,
    memory_store: &Option<Arc<crate::memory::store::MemoryStore>>,
    session_id: &str,
    explore_streak: u32,
    total_explore: u32,
    impl_streak: u32,
    in_impl_phase: bool,
) {
    // ── 1. Strip all prior injection blocks in ONE pass ──
    strip_all_injection_blocks(messages);

    // ── 1b. Rebuild [USER_ROUND] if stripped ──
    let has_user_round = messages.iter().any(|m| match m {
        Message::System { content } => content.contains("[USER_ROUND]"),
        Message::User { content } => content.contains("[USER_ROUND]"),
        Message::Assistant { content, .. } => content.contains("[USER_ROUND]"),
        Message::ToolResult { content, .. } => content.contains("[USER_ROUND]"),
    });
    if !has_user_round && !user_task.is_empty() {
        let user_round = format!("[USER_ROUND]\n{}\n[/USER_ROUND]", user_task);
        messages.insert(0, Message::system(&user_round));
    }

    // ── 2. Build compact block from helpers ──
    let mut block = String::with_capacity(1200);

    // Implement phase gets wider memory windows (long edit tasks need more trace).
    let slim_phase = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| crate::agent::context_slim::is_slim_phase(&e))
        .unwrap_or(false);

    // Memory-graph pinned at the very top (only present after an offload has
    // archived nodes this session). Highest-priority anchor, like the blackboard.
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
            && let Some(graph) = engine.get_variable(crate::agent::memory_offload::MEMORY_GRAPH_VAR)
                && !graph.trim().is_empty() {
                    block.push_str(&graph);
                    block.push_str("\n\n");
                }

    block.push_str(&build_task_anchor_block(
        user_task,
        iteration,
        turn_memory,
        workflow_engine,
        explore_streak,
        total_explore,
        impl_streak,
        in_impl_phase,
    ));
    block.push_str(&build_tool_trace_block(turn_memory, slim_phase));

    // ── ReAct timeline: cross-turn action history ──
    if let Some(ms) = memory_store {
        if let Ok(timeline) = ms.get_react_timeline(session_id, 30) {
            if !timeline.trim().is_empty() {
                block.push_str("🔄 ReAct Timeline:\n");
                block.push_str(&timeline);
                block.push('\n');
            }
        }
    }

    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock() {
            block.push_str(&build_workspace_block(&engine, unified_tool_mode));
        }

    messages.push(Message::system(&block));
}

// ═══════════════════════════════════════════════════════════════════
//  inject_slim_context helpers — each builds one section of [TURN_CONTEXT]
// ═══════════════════════════════════════════════════════════════════

/// Section 1: task anchor + blackboard + phase/progress ripple.
fn build_task_anchor_block(
    user_task: &str,
    iteration: u32,
    turn_memory: &turn_memory::TurnMemory,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    explore_streak: u32,
    total_explore: u32,
    impl_streak: u32,
    in_impl_phase: bool,
) -> String {
    let mut b = String::new();
    let task: String = user_task.chars().take(300).collect();
    let ellipsis = if task.len() < user_task.len() { "…" } else { "" };
    b.push_str(&format!("[TURN_CONTEXT]\n🎯 任务: {task}{ellipsis}\n"));

    // Constraint blackboard — always visible
    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock() {
            let bb = crate::agent::blackboard::block(&engine);
            if !bb.is_empty() {
                b.push_str(&bb);
                b.push('\n');
            }
        }

    // Progress + phase ripple
    let tool_count = turn_memory.entries.len();
    let mut plan_recap = String::new();
    // Convergence action for the gauge depends on task intent: a review submits a
    // plan (writes locked); a fix/general task edits directly (writes unlocked); a
    // Q&A answers. Default to SubmitPlan when no engine (conservative — never nudges
    // an edit when we can't confirm writes are unlocked).
    let mut converge = crate::agent::explore_reflect::ConvergeMode::SubmitPlan;
    let phase_line = if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            plan_recap = engine.plan_progress_summary();
            converge =
                crate::agent::explore_reflect::ConvergeMode::from_intent(engine.get_task_intent());
            format_phase_ripple(&crate::agent::phase::get(&engine), &engine)
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    if phase_line.is_empty() {
        b.push_str(&format!("📍 iteration {} · 工具 {} 次\n", iteration + 1, tool_count));
    } else {
        b.push_str(&format!("📍 iteration {} · 工具 {} 次 · {phase_line}\n", iteration + 1, tool_count));
    }

    // 🔍 Exploration/implementation budget gauge — makes the cost of continued
    // exploration visible every turn (not just when a reflection fires), turning
    // "look once more" from a free habit into a visible choice.
    b.push_str(&crate::agent::explore_reflect::budget_gauge(
        explore_streak,
        total_explore,
        impl_streak,
        in_impl_phase,
        converge,
    ));

    // Todo-list recap
    if !plan_recap.is_empty() {
        b.push('\n');
        b.push_str(&plan_recap);
        b.push('\n');
    }
    b
}

/// Section 2: tool trace — edited files + recent tool log + decisions.
fn build_tool_trace_block(
    turn_memory: &turn_memory::TurnMemory,
    slim_phase: bool,
) -> String {
    let mut b = String::new();
    const EDIT_TOOLS: [&str; 3] = ["file_write", "edit_file", "delete_range"];
    let is_edit = |tool: &str| EDIT_TOOLS.contains(&tool);

    // Edited files with counts
    if turn_memory.entries.iter().any(|e| is_edit(&e.tool)) {
        let mut order: Vec<String> = Vec::new();
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for e in turn_memory.entries.iter().filter(|e| is_edit(&e.tool)) {
            let target: String = e.target.chars().take(120).collect();
            if counts.insert(target.clone(), counts.get(&target).copied().unwrap_or(0) + 1).is_none() {
                order.push(target);
            }
        }
        b.push_str("\n✏️ 你本轮已修改的文件 (勿重复编辑):\n");
        for target in &order {
            let n = counts.get(target).copied().unwrap_or(1);
            if n > 1 {
                b.push_str(&format!("  · {target} (已编辑 {n} 次)\n"));
            } else {
                b.push_str(&format!("  · {target}\n"));
            }
        }
    }

    // Recent tool log
    if !turn_memory.entries.is_empty() {
        let read_window = if slim_phase { 12 } else { 8 };
        let edits: Vec<&turn_memory::TurnMemoryEntry> =
            turn_memory.entries.iter().filter(|e| is_edit(&e.tool)).collect();
        let all_reads: Vec<&turn_memory::TurnMemoryEntry> =
            turn_memory.entries.iter().filter(|e| !is_edit(&e.tool)).collect();
        let read_start = all_reads.len().saturating_sub(read_window);
        let recent_reads = &all_reads[read_start..];
        let combined: Vec<&turn_memory::TurnMemoryEntry> =
            edits.into_iter().chain(recent_reads.iter().copied()).collect();
        if !combined.is_empty() {
            b.push_str("\n你刚才已经执行过:\n");
            for e in combined {
                let icon = if e.outcome == "ok" || e.outcome.starts_with("ok") { "✅" } else { "⚠️" };
                b.push_str(&format!("  {icon} {}({}) → {}\n", e.tool,
                    e.target.chars().take(80).collect::<String>(),
                    e.outcome.chars().take(160).collect::<String>()));
            }
        }
    }

    // Decisions
    if !turn_memory.decisions.is_empty() {
        let window = if slim_phase { 8 } else { 4 };
        b.push_str("\n你刚才形成的判断（非原始 think 摘要）:\n");
        for d in turn_memory.decisions.iter().rev().take(window).rev() {
            b.push_str(&format!("  - {}\n", d.chars().take(220).collect::<String>()));
        }
    }
    b
}

/// Section 3: workspace-derived guidance — required_action, scope gate, review handoff, durable memory.
fn build_workspace_block(
    engine: &crate::agent::engine::WorkflowEngine,
    unified_tool_mode: bool,
) -> String {
    let mut b = String::new();

    // Required action from workspace
    if crate::agent::phase::should_inject_workspace(engine)
        && let Some(ws) = crate::agent::workspace::WorkflowWorkspace::build(engine) {
            let action_text = if unified_tool_mode {
                format_required_action_one_liner_unified(&ws.required_action)
            } else {
                format_required_action_one_liner(&ws.required_action)
            };
            b.push_str(&format!("\n下一步: {action_text}\n"));
        }

    // Scope gate
    if crate::agent::business_gate::is_pending_scope(engine) {
        b.push_str("\n⏸️ 门禁: 等待用户 c /confirm 确认范围\n");
        if let Some(store) = crate::agent::findings::load_or_migrate(engine)
            && !store.findings.is_empty() {
                b.push_str("\n📋 当前 findings (用户按编号讨论):\n");
                for f in &store.findings {
                    let icon = if store.active_indices.is_empty() || store.active_indices.contains(&f.index) { "☐" } else { "⊘" };
                    b.push_str(&format!("  {icon} #{} [{}] {} — {}\n", f.index,
                        f.severity.label(),
                        f.file.rsplit('/').next().unwrap_or(&f.file),
                        f.issue.chars().take(80).collect::<String>()));
                }
            }
    }

    // Implement phase: review handoff files
    if crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::Implement {
        let mut files: Vec<String> = engine.review_handoff_files();
        if files.is_empty()
            && let Some(store) = crate::agent::findings::load_or_migrate(engine) {
                files = store.findings.iter().filter(|f| !f.file.is_empty()).map(|f| f.file.clone()).collect();
            }
        if !files.is_empty() {
            b.push_str("\n📂 审查阶段已读文件（内容在上文，直接编辑，勿重新探索）:\n");
            let mut seen = std::collections::HashSet::new();
            for f in &files {
                if seen.insert(f.clone()) {
                    b.push_str(&format!("  · {f}\n"));
                }
            }
        }
    }

    // Durable memory fallback (when workspace is NOT active)
    if !crate::agent::phase::should_inject_workspace(engine) {
        let dm = engine.durable_memory_block();
        if !dm.is_empty() {
            b.push_str(&format!("\n记忆: {}\n", dm.chars().take(400).collect::<String>()));
        }
        let ur = engine.user_round_memory_block();
        if !ur.is_empty() {
            b.push_str(&format!("\n上下文: {}\n", ur.chars().take(200).collect::<String>()));
        }
    }

    // Historical batch memory is now handled entirely by the [MEMORY_GRAPH]
    // block (pinned at top of context after an offload) + `recall #<id>` node
    // replay. The former `_prev_impl_edits` / `_memory_history` /
    // `_last_session_summary` timeline was retired with the unified offload path.

    b
}

/// Compact phase-location hint for `[TURN_CONTEXT]` — tells the LLM where it is
/// in the explore→confirm→implement→finish pipeline.
fn format_phase_ripple(
    phase: &crate::agent::phase::SingleFlowPhase,
    engine: &crate::agent::engine::WorkflowEngine,
) -> String {
    match phase {
        crate::agent::phase::SingleFlowPhase::Receive
        | crate::agent::phase::SingleFlowPhase::Review => {
            let has_findings = crate::agent::findings::load_or_migrate(engine)
                .is_some_and(|s| !s.findings.is_empty());
            if has_findings {
                "🔍 已探索 → finish(finding_json) 确认".to_string()
            } else {
                "🔍 探索代码".to_string()
            }
        }
        crate::agent::phase::SingleFlowPhase::AwaitUser => {
            if crate::agent::business_gate::scope_implementation_unlocked(engine) {
                "✏️ 已确认 → 开始实施".to_string()
            } else {
                "⏸️ 等待确认".to_string()
            }
        }
        crate::agent::phase::SingleFlowPhase::Implement => {
            if let Some(store) = crate::agent::findings::load_or_migrate(engine) {
                let done = store
                    .findings
                    .iter()
                    .filter(|f| f.status == crate::agent::findings::FindingStatus::Done)
                    .count();
                let total = store.findings.len();
                if total > 0 {
                    format!("✏️ 实施中 ({done}/{total})")
                } else {
                    "✏️ 实施中".to_string()
                }
            } else {
                "✏️ 实施中".to_string()
            }
        }
        crate::agent::phase::SingleFlowPhase::Complete => "✅ 完成".to_string(),
    }
}

fn format_required_action_one_liner(action: &crate::agent::workspace::RequiredAction) -> String {
    match action {
        crate::agent::workspace::RequiredAction::Explore { hint } => {
            format!("探索 — {hint}")
        }
        crate::agent::workspace::RequiredAction::ReadFile {
            path,
            finding_index,
            ..
        } => {
            format!("file_read finding #{finding_index}: `{path}`")
        }
        crate::agent::workspace::RequiredAction::EditFile {
            path,
            finding_index,
        } => {
            format!("edit_file finding #{finding_index}: `{path}`")
        }
        crate::agent::workspace::RequiredAction::Verify {
            command,
            finding_index,
        } => {
            let cmd: String = command.chars().take(80).collect();
            format!("验证 finding #{finding_index}: `{cmd}`")
        }
        crate::agent::workspace::RequiredAction::EmitFindingsAndDone => {
            "finish(finding_json) 提交计划".into()
        }
        crate::agent::workspace::RequiredAction::EmitCompletionReceipt => {
            "finish(content) 收尾结束".into()
        }
        crate::agent::workspace::RequiredAction::AwaitUser => "等待用户确认范围".into(),
        crate::agent::workspace::RequiredAction::DiscussOnly => "讨论模式 — finish(content)".into(),
    }
}

fn format_required_action_one_liner_unified(
    action: &crate::agent::workspace::RequiredAction,
) -> String {
    match action {
        crate::agent::workspace::RequiredAction::Explore { hint } => {
            format!("find_symbol(name=目标符号) → file_read(path, offset) — {hint}")
        }
        crate::agent::workspace::RequiredAction::ReadFile {
            path,
            finding_index,
            ..
        } => {
            format!(
                "先 find_symbol 定位 #{finding_index} 对应方法 → 再 file_read(path={path}, offset=行号)"
            )
        }
        crate::agent::workspace::RequiredAction::EditFile {
            path,
            finding_index,
        } => {
            format!("complete_and_check(action=edit_file, path={path}) — finding #{finding_index}")
        }
        crate::agent::workspace::RequiredAction::Verify {
            command,
            finding_index,
        } => {
            let cmd: String = command.chars().take(80).collect();
            format!(
                "complete_and_check(action=shell_exec, command={cmd}) — finding #{finding_index}"
            )
        }
        crate::agent::workspace::RequiredAction::EmitFindingsAndDone => {
            "complete_and_check(action=finish, params.finding_json=[...])".into()
        }
        crate::agent::workspace::RequiredAction::EmitCompletionReceipt => {
            "complete_and_check(action=finish, params.content=...)".into()
        }
        crate::agent::workspace::RequiredAction::AwaitUser => {
            "等待用户确认 — 禁止 complete_and_check".into()
        }
        crate::agent::workspace::RequiredAction::DiscussOnly => {
            "complete_and_check(action=finish, params.content=...)".into()
        }
    }
}

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
        && let Ok(engine) = wf.try_lock() {
            crate::agent::read_guard::clear_symbol_queries(&engine);
        }

    // 🎯 Anchor to the **current turn user input** (not session history)
    let user_task: Option<String> = workflow_engine
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
        });

    let mut turn_memory = turn_memory::TurnMemory::new(user_task.as_deref().unwrap_or(""));
    if let Some(wf) = &workflow_engine {
        // FIX: Add warning when lock acquisition fails
        if let Ok(engine) = wf.try_lock() {
            crate::agent::gatekeeper::reset_failures(&engine);
            post_edit_verification::reset_verify_failures(&engine);
            if let Some(saved) = engine.load_turn_memory() {
                turn_memory.merge_from(saved);
            }
            // Intent is set at user-round boundary; do not re-classify each LLM iteration.
            let block = engine.user_round_memory_block();
            if !block.is_empty() {
                user_round::inject_user_round(&mut messages, &block);
            }
            let block = engine.durable_memory_block();
            if !block.is_empty() {
                memory_bridge::inject_durable_memory(&mut messages, &block);
            }
        } else {
            tracing::warn!("[run_agent_turn] Failed to acquire workflow_engine lock for memory injection");
        }
    }

    let mut iteration = 0u32;
    let mut idle_streak = 0u32;
    let mut content_only_streak = 0u32;
    let mut explore_streak = 0u32;
    let mut explore_reflected = false;
    // Cumulative exploration hard ceiling — discovery does NOT reset this; only a
    // real edit/finish does. Backstop against unbounded breadth-first wandering.
    let mut total_explore = 0u32;
    // Implementation-phase reflection (consecutive no-edit turns once the plan is confirmed).
    let mut impl_streak = 0u32;
    let mut impl_reflected = false;
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
    fn review_stream_filter(
        workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    ) -> bool {
        workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .is_some_and(|e| {
                e.is_single_step() && !crate::agent::workflow_session::is_implementation_phase(&e)
            })
    }

    loop {
        // Check cancellation before each LLM call.
        if cancel_token.is_cancelled() {
            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
            break;
        }

        // No automatic per-turn iteration cap: the ReAct loop runs until the agent
        // calls finish (LLM-driven termination) or the user stops it (N / Ctrl+C).
        // The only remaining auto-stops are genuine safety nets: same-tool loop
        // detection, the per-call 120s timeout, and empty-arg streak guards.

        let _ = ui_tx.send(AgentToUiEvent::Status(if iteration == 0 {
            "🧠 Thinking...".to_string()
        } else {
            format!("🧠 Thinking... (iteration {})", iteration + 1)
        }));

        // Check for queued interjections before LLM call.
        while let Ok(ev) = ui_rx.try_recv() {
            match ev {
                ui_event::UiToAgentEvent::Interjection(text) => {
                    let trimmed = text.trim();
                    let is_confirm = trimmed == "c"
                        || trimmed.starts_with("/confirm")
                        || trimmed.starts_with("/fix")
                        || trimmed.contains("确认")
                        || trimmed.contains("开始实施");
                    if is_confirm {
                        // Set pre-ack so scope gate skips waiting
                        if let Some(wf) = &workflow_engine
                            && let Ok(engine) = wf.try_lock() {
                                engine.set_variable(
                                    crate::agent::business_gate::PRE_ACK_KEY,
                                    "1".to_string(),
                                );
                            }
                    }
                    push_interjection_message(&workflow_engine, &mut messages, &text, &ui_tx);
                }
                // ScopeConfirmed / BusinessAck sent by the UI when user presses "c".
                // These would be consumed by try_recv() and dropped — set pre-ack
                // so the scope gate can find it via engine variable instead.
                ui_event::UiToAgentEvent::ScopeConfirmed
                | ui_event::UiToAgentEvent::BusinessAck { .. } => {
                    if let Some(wf) = &workflow_engine
                        && let Ok(engine) = wf.try_lock() {
                            engine.set_variable(
                                crate::agent::business_gate::PRE_ACK_KEY,
                                "1".to_string(),
                            );
                        }
                }
                _ => {} // Other events ignored
            }
        }

        turn_memory.bump_iteration();
        persist_turn_memory(&workflow_engine, &turn_memory);

        // In-turn message growth is handled by the unified offload path: after
        // each LLM call, real prompt-token count is checked against the 80%
        // window budget and, if exceeded, the ReAct log is archived + old tool
        // messages placeholdered (memory_offload::offload_if_over_budget). The
        // former iteration-count-based compact_turn_messages was retired.

        // NOTE: fold_review_exploration removed — it replaced tool results with
        // placeholders pointing to [WORKSPACE]/STEP_MEMORY blocks that no longer
        // exist in the context, causing the LLM to see no actual code content.
        // File contents must stay visible so the LLM can edit them.

        // Sync turn memory from full message scan (survives compaction)
        let include_writes = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.is_task_step())
            .unwrap_or(true);
        turn_memory.sync_from_messages(&messages, include_writes);
        if let Some(wf) = &workflow_engine
            && let Ok(engine) = wf.try_lock()
                && let Some(ti) = user_round::get_turn_user_input(&engine) {
                    turn_memory.user_task = ti;
                }

        // Workflow: collapse repeated idle narration (keeps LLM context lean)
        if workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .is_some_and(|e| e.is_workflow_active())
        {
            crate::agent::idle_narrative::collapse_redundant_idle(&mut messages);
        }

        // ── Query SQLite react_log for ReAct timeline ──
        // This un-archived timeline (impacted=0) is the single source of recent
        // cross-round history; older work lives in [MEMORY_GRAPH] after offload.
        if let Some(ref ms) = tool_ctx.memory_store {
            if let Some(wf) = &workflow_engine {
                if let Ok(engine) = wf.try_lock() {
                    let sid = engine.session_id();
                    if let Ok(timeline) = ms.get_react_timeline(&sid, 50) {
                        if !timeline.is_empty() {
                            engine.set_variable("_react_timeline", timeline);
                        }
                    }
                }
            }
        }

        // ── Slim context injection ──
        // One compact [TURN_CONTEXT] block: task anchor + progress + next action.
        // Static content (routes, full workspace) lives in the system prompt, not here.
        let slim_in_impl_phase = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| crate::agent::workflow_session::is_implementation_phase(&e))
            .unwrap_or(false);
        inject_slim_context(
            &mut messages,
            user_task.as_deref().unwrap_or(""),
            iteration,
            &turn_memory,
            &workflow_engine,
            unified_tool_mode,
            &tool_ctx.memory_store,
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

        // 🚨 Sanitize tool pairs before EVERY LLM call within the agent turn.
        // This prevents OpenAI API errors like "ToolResult references non-existent tool call"
        // when a tool_call was skipped or only partially executed.
        crate::context::sanitize_tool_pairs(&mut messages);

        // Think/reasoning is display-only — strip before context assembly & LLM call.
        crate::agent::think_stream::prepare_messages_for_llm(&mut messages);

        // Single-step model: always show assistant output to the user.
        let pre_llm_step_idx = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.get_current_step_index())
            .unwrap_or(0);

        // Stream LLM response.
        let (llm_tx, mut llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

        let active_provider = if let Some(ref engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;
            let picked = role_providers.pick(&provider, &engine);
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
        let msgs = messages.clone();

        // Filter tool schemas based on current workflow step
        let workflow_blocks_planning = if let Some(ref engine_arc) = workflow_engine {
            engine_arc.lock().await.is_workflow_active()
        } else {
            false
        };

        let schemas: Vec<_> = if unified_tool_mode {
            if planning_mode && iteration == 0 && !workflow_blocks_planning {
                vec![]
            } else if let Some(ref engine_arc) = workflow_engine {
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
        } else if let Some(ref engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;
            if !engine.allows_tool_execution() {
                Vec::new()
            } else if engine.is_single_step() {
                let allowed = crate::agent::tool_graph::allowed_tool_names(&engine);
                crate::agent::tool_graph::filter_tool_schemas(&tool_schemas, &allowed)
            } else {
                tool_schemas.clone()
            }
        } else {
            tool_schemas.clone()
        };

        // 📝 LOG REQUEST CONTEXT (debug level — expensive, iterates all messages)
        tracing::debug!("\n{}", "=".repeat(80));
        tracing::debug!("🤖 LLM REQUEST CONTEXT (Iteration {})", iteration + 1);
        tracing::debug!("{}", "=".repeat(80));
        tracing::debug!("Total messages: {}", msgs.len());

        // Show system prompt preview (debug level)
        if let Some(first_msg) = msgs.first()
            && let Message::System { content } = first_msg {
                tracing::debug!("📋 SYSTEM PROMPT LENGTH: {} characters", content.chars().count());
            }
        tracing::debug!("Enabled tools: {}",
            schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
        );
        tracing::debug!("{}", "=".repeat(80));

        let mut llm_opts = crate::llm::StreamOptions::default();
        if unified_tool_mode && !schemas.is_empty() {
            // Unified mode always exposes exactly ONE tool (`complete_and_check`).
            // We force it by NAME rather than the generic "required": glm-5.1 (via
            // the aigw gateway) does NOT honor "required" — it writes the intended
            // action into reasoning and returns no tool_call, stalling the loop.
            // The named form `{"type":"function","function":{"name":...}}` is the
            // stronger contract. (Some GPT-compatible endpoints reject the named
            // form with `Missing required parameter: 'tool_choice.name'`; if this
            // endpoint does, fall back to ToolChoice::Required.)
            llm_opts.tool_choice = Some(crate::llm::ToolChoice::Function(
                crate::agent::unified_action::TOOL_NAME.to_string(),
            ));
            llm_opts.parallel_tool_calls = Some(true);
        }
        let cancel_clone = cancel_token.clone();
        let llm_tx_err = llm_tx.clone();
        let mut stream_handle = tokio::spawn(async move {
            tokio::select! {
                result = provider_clone.stream_chat(&msgs, &schemas, llm_tx, llm_opts) => {
                    if let Err(e) = result {
                        tracing::error!("LLM stream error: {e}");
                        // Propagate the error so the agent loop can handle it.
                        let _ = llm_tx_err.send(LlmStreamEvent::Error(format!("Stream failed: {e}")));
                    }
                }
                _ = cancel_clone.cancelled() => {}
            }
        });

        // Collect the full response (text + tool calls).
        let mut full_text = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut current_tool_args: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let use_findings_stream = review_stream_filter(&workflow_engine);
        let mut findings_stream =
            use_findings_stream.then(crate::agent::perception::FindingsStreamFilter::new);
        let mut think_stream = crate::agent::think_stream::ThinkTagStreamFilter::new();
        let mut last_stream_completion_tokens = 0u32;
        let mut last_prompt_tokens = 0u32;

        // Timeout for the entire LLM response (stream first token → stream done).
        // Separate from the per-tool 300s timeout; prevents the agent hanging
        // for 15+ minutes when the API silently drops the connection.
        const LLM_RESPONSE_TIMEOUT: std::time::Duration =
            std::time::Duration::from_secs(180);

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
                // Abort the stream task so it stops waiting on the API
                stream_handle.abort();
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "⏱️ LLM 响应超时 (180s) — 请重试或简化请求".to_string(),
                ));
                // Add interrupt boundary so the next round knows what happened
                let boundary = crate::agent::user_round::format_interrupt_boundary_message(
                    &user_task.clone().unwrap_or_default(),
                );
                new_messages.push(crate::message::Message::system(&boundary));
                messages.push(crate::message::Message::system(&boundary));
                emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                return;
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
                    // Strip <tool_call> XML tags from visible text so raw
                    // tool syntax doesn't appear in the UI.
                    let clean_visible = strip_tool_call_xml(&visible_piece);
                    // Detect XML tool calls mid-stream and update status bar
                    if clean_visible.len() < visible_piece.len() {
                        // Something was stripped — a tool call was detected in the stream
                        if let Some(action) = extract_action_from_xml(&visible_piece) {
                            let _ = ui_tx.send(AgentToUiEvent::Status(
                                format!("🔄 {} ...", action)
                            ));
                        }
                    }
                    if let Some(ref mut filter) = findings_stream {
                        if let Some(visible) = filter.push(&clean_visible)
                            && !visible.is_empty() {
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
                    // Show tool intent in status bar immediately
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
                        // Once we have at least the action field, show it in status bar
                        if was_empty && unified_tool_mode
                            && let Ok(action) = serde_json::from_str::<crate::agent::unified_action::UnifiedActionRequest>(args) {
                                let _ = ui_tx.send(AgentToUiEvent::Status(format!("🔄 {} ...", action.action)));
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
                    // Log the error to file.
                    tracing::error!("LLM error: {}", err);
                    // Abort the stream task if still running, don't block on it.
                    stream_handle.abort();

                    // Bounded self-heal for client-side API errors (HTTP 4xx):
                    // ARK returns `400 InvalidParameter` when the request body is
                    // malformed or oversized. Aborting the turn just resends the
                    // same body next time, so instead we trim the context and
                    // retry the same iteration a bounded number of times.
                    let is_client_api_error = err.contains("API error 400")
                        || err.contains("API error 413")
                        || err.contains("API error 422");
                    if is_client_api_error
                        && api_error_recovery_streak < MAX_API_ERROR_RECOVERY
                    {
                        api_error_recovery_streak += 1;
                        tracing::warn!(
                            "[AGENT] API error recovery {}/{}: trimming context and retrying",
                            api_error_recovery_streak,
                            MAX_API_ERROR_RECOVERY
                        );
                        let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                            "⚠️ API 拒绝请求（{}/{}）— 正在裁剪上下文后重试…",
                            api_error_recovery_streak, MAX_API_ERROR_RECOVERY
                        )));
                        crate::context::sanitize_tool_pairs(&mut messages);
                        crate::context::filter_noisy_messages(&mut messages);
                        memory_offload::hard_trim_public(&mut messages);
                        continue;
                    }

                    // Give up: surface the error and end the turn.
                    let _ = ui_tx.send(AgentToUiEvent::Error(err));
                    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                    return;
                }
            }
        }

        // Wait for the stream task to finish, but don't block forever.
        // If cancelled, abort the stream task immediately.
        tokio::select! {
            _ = &mut stream_handle => {}
            _ = cancel_token.cancelled() => {
                stream_handle.abort();
            }
        }

        if let Some(ref mut filter) = findings_stream
            && let Some(tail) = filter.flush_tail() {
                let _ = ui_tx.send(AgentToUiEvent::TextChunk(tail));
            }

        // ── Unified budget offload ──
        // When the API's real prompt-token count crosses 80% of the window,
        // cluster the un-archived ReAct log into memory-graph nodes, persist
        // them, and placeholder the old ReAct messages (one action = archive +
        // compaction). Runs here, after the stream fully ends, so `messages`
        // isn't borrowed mid-loop.
        if last_prompt_tokens > 0
            && let Some(ref ms) = tool_ctx.memory_store {
                let session_id = workflow_engine
                    .as_ref()
                    .and_then(|wf| wf.try_lock().ok())
                    .map(|e| e.session_id())
                    .unwrap_or_else(|| "default".to_string());
                let fail_streak = workflow_engine
                    .as_ref()
                    .and_then(|wf| wf.try_lock().ok())
                    .and_then(|e| e.get_variable(memory_offload::OFFLOAD_FAIL_VAR))
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let context_window = active_provider.context_window_size();
                let summarizer = tool_ctx.summarizer.clone();
                let ui_tx_offload = ui_tx.clone();
                let (outcome, new_streak) = memory_offload::offload_if_over_budget(
                    last_prompt_tokens,
                    context_window,
                    &mut messages,
                    summarizer,
                    &active_provider,
                    ms,
                    &session_id,
                    fail_streak,
                    |s| {
                        let _ = ui_tx_offload.send(AgentToUiEvent::Status(s));
                    },
                )
                .await;
                if let Some(wf) = &workflow_engine
                    && let Ok(engine) = wf.try_lock() {
                        engine.set_variable(
                            memory_offload::OFFLOAD_FAIL_VAR,
                            new_streak.to_string(),
                        );
                        // Refresh the top-of-context graph block after any archival.
                        if matches!(outcome, memory_offload::OffloadOutcome::Archived { .. }) {
                            let block = memory_offload::build_memory_graph_block(ms, &session_id);
                            engine.set_variable(memory_offload::MEMORY_GRAPH_VAR, block);
                        }
                    }
            }

        // Repair malformed / empty tool arguments (GLM empty JSON, XML <tool_call> hallucinations).
        // If arguments are XML, return error immediately — don't silently repair.
        let fallback_blob = format!("{full_text}\n{reasoning_content}");
        let fallbacks = [fallback_blob.as_str()];
        for tc in &mut tool_calls {
            // XML-style args (`<tool_call>` / `<arg_key>`) are auto-repaired to JSON
            // inside `recover_tool_call_arguments`. Repair is reliable, so we do it
            // silently — no scolding, no extra system noise for the model.
            tc.arguments = crate::agent::tool_args_repair::recover_tool_call_arguments(
                &tc.name,
                &tc.arguments,
                &fallbacks,
            );
        }

        // 🚨 GLM models output `<tool_call>` XML as text CONTENT instead of
        // using the OpenAI function-calling protocol. The SSE parser sees these
        // as regular text, so tool_calls stays empty. Extract them from the text.
        if tool_calls.is_empty() {
            let extracted = crate::agent::tool_args_repair::extract_xml_tool_calls(&full_text);
            if !extracted.is_empty() {
                tracing::info!(
                    "[XML_EXTRACT] Extracted {} tool call(s) from <tool_call> XML in text content",
                    extracted.len()
                );
                tool_calls = extracted;
            }
        }

        // Onboarding: ## Done when both project skill files exist (no workflow).
        let onboarding_turn =
            workflow_engine.is_none() && onboarding::is_onboarding_turn(&messages);
        if onboarding_turn && crate::agent::engine::WorkflowEngine::text_signals_done(&full_text) {
            let root = tool_ctx
                .runtime
                .project_root
                .clone()
                .unwrap_or_else(|| tool_ctx.working_dir.clone());
            if onboarding::onboarding_files_complete(&root) {
                let msg = Message::Assistant {
                    content: crate::agent::think_stream::visible_only(&full_text),
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                };
                new_messages.push(msg.clone());
                messages.push(msg);
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "✅ 项目规范与业务指导 Skill 已创建".to_string(),
                ));
                emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                return;
            } else {
                let missing = onboarding::missing_onboarding_files(&root).join("、");
                messages.push(Message::system(format!(
                    "还不能 ## Done：还缺 {missing}。请分别 file_write 后再结束。"
                )));
                persist_turn_memory(&workflow_engine, &turn_memory);
                iteration += 1;
                continue;
            }
        }

        if !unified_tool_mode && try_capture_review_findings(&workflow_engine, &full_text, &ui_tx) {
            let visible = crate::agent::think_stream::visible_only(&full_text);
            let content_for_session =
                execute_user_display(&workflow_engine, pre_llm_step_idx, &visible);
            let msg = Message::Assistant {
                content: content_for_session,
                tool_calls: Vec::new(),
                reasoning_content: None,
            };
            upsert_review_report_assistant(&mut messages, &msg);
            upsert_review_report_assistant(&mut new_messages, &msg);

            match business_gate::await_findings_scope_gate(
                &mut ui_rx,
                &cancel_token,
                &workflow_engine,
                &mut messages,
                &ui_tx,
                push_interjection_message,
            )
            .await
            {
                business_gate::BusinessGateResume::Cancelled => break,
                business_gate::BusinessGateResume::Acknowledged => {
                    refresh_turn_memory_for_implement(&workflow_engine, &mut turn_memory);
                    tools_used_this_turn.clear();
                    idle_streak = 0;
                    persist_turn_memory(&workflow_engine, &turn_memory);
                    iteration += 1;
                    continue;
                }
                business_gate::BusinessGateResume::Discuss => {
                    messages.push(Message::system(
                        "📋 用户提供了反馈。请根据反馈更新 findings/计划，重新提交。禁止直接进入实施。",
                    ));
                    persist_turn_memory(&workflow_engine, &turn_memory);
                    iteration += 1;
                    continue;
                }
            }
        }

        // If no tool calls, the turn is complete.
        if tool_calls.is_empty() {
            if unified_tool_mode {
                // We force `complete_and_check` on every step (tool_choice=Function),
                // so a response with NO tool call is the model NOT complying — it
                // returned prose only. There is no legitimate "plain-text step": the
                // Thought must ride in the content field ALONGSIDE the call, and the
                // only way to end the turn is an explicit `finish`. This branch is
                // anomaly recovery: preserve whatever reasoning the model wrote (so it
                // isn't lost), then make it emit a proper call. Cap so a persistently
                // non-complying model can't burn the whole turn.
                let visible = crate::agent::think_stream::visible_only(&full_text);
                if !visible.trim().is_empty() {
                    content_only_streak += 1;
                    let msg = Message::Assistant {
                        content: visible.clone(),
                        tool_calls: Vec::new(),
                        reasoning_content: None,
                    };
                    new_messages.push(msg.clone());
                    messages.push(msg);

                    const CONTENT_ONLY_HARD_CAP: u32 = 8;
                    if content_only_streak >= CONTENT_ONLY_HARD_CAP {
                        let _ = ui_tx.send(AgentToUiEvent::Status(
                            "⏹️ 多次未发出 complete_and_check — 结束本轮，等用户输入".to_string(),
                        ));
                        persist_turn_memory(&workflow_engine, &turn_memory);
                        emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                        return;
                    }

                    messages.push(Message::system(
                        "⚠️ 你这步没有调用 complete_and_check（本系统每步都必须通过它行动）。\
                         把上面的思考作为依据，立即发出一个 complete_and_check 调用：\
                         继续就用 file_read/find_symbol/edit_file… 行动；确已完成就 finish(params.content=…) 收尾。",
                    ));
                    persist_turn_memory(&workflow_engine, &turn_memory);
                    iteration += 1;
                    continue;
                }
                // Empty visible output. If the model produced ONLY reasoning
                // (content all inside <think>), we must NOT silently drop it:
                // doing so leaves the message history unchanged, so the next LLM
                // call sees identical input and regenerates the identical thought
                // — a reasoning-only infinite loop. Persist a digest of the
                // reasoning into the visible history so context advances, and
                // count it toward the hard cap so the loop can terminate.
                let reasoning_digest = crate::agent::think_stream::visible_only(&reasoning_content);
                let reasoning_digest = if reasoning_digest.is_empty() {
                    reasoning_content.trim().to_string()
                } else {
                    reasoning_digest
                };
                if !reasoning_digest.is_empty() {
                    content_only_streak += 1;
                    // Keep head + tail: the conclusion (what to do next) usually
                    // lives at the END of the reasoning, so prioritize the tail.
                    let digest = digest_reasoning(&reasoning_digest, 1400);
                    let msg = Message::Assistant {
                        content: format!("(内部思考摘要)\n{digest}"),
                        tool_calls: Vec::new(),
                        reasoning_content: None,
                    };
                    new_messages.push(msg.clone());
                    messages.push(msg);

                    const CONTENT_ONLY_HARD_CAP: u32 = 8;
                    if content_only_streak >= CONTENT_ONLY_HARD_CAP {
                        let _ = ui_tx.send(AgentToUiEvent::Status(
                            "⏹️ 多次只思考未发出动作 — 结束本轮，等用户输入".to_string(),
                        ));
                        persist_turn_memory(&workflow_engine, &turn_memory);
                        emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                        return;
                    }

                    messages.push(Message::system(
                        "⚠️ 你上一步只输出了思考，没有发出 complete_and_check。\
                         不要重复同样的思考。基于上面的思考摘要，立即发出一个具体动作：\
                         file_read 一个**还没读过**的文件 / edit_file / finish 收尾。",
                    ));
                    persist_turn_memory(&workflow_engine, &turn_memory);
                    iteration += 1;
                    continue;
                }
                // Truly empty response (no visible, no reasoning) — nudge to act.
                content_only_streak += 1;
                const EMPTY_HARD_CAP: u32 = 8;
                if content_only_streak >= EMPTY_HARD_CAP {
                    let _ = ui_tx.send(AgentToUiEvent::Status(
                        "⏹️ 多次空响应 — 结束本轮，等用户输入".to_string(),
                    ));
                    persist_turn_memory(&workflow_engine, &turn_memory);
                    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                    return;
                }
                messages.push(Message::system(
                    "【ALL-TOOLING】请勿空输出。必须调用 complete_and_check（行动 或 finish 收尾）。",
                ));
                persist_turn_memory(&workflow_engine, &turn_memory);
                iteration += 1;
                continue;
            }
            // Cross-step idle detection — break prose↔gate loops before stacking messages.
            if let Some(ref engine_arc) = workflow_engine
                && let Ok(engine) = engine_arc.try_lock()
                    && engine.is_workflow_active() && pre_llm_step_idx <= 3 {
                        let ctx = crate::agent::idle_narrative::IdleContext {
                            step_idx: pre_llm_step_idx,
                            engine: Some(&*engine),
                        };
                        let visible_for_idle = crate::agent::think_stream::visible_only(&full_text);
                        if !crate::agent::idle_narrative::is_step_deliverable(
                            &ctx,
                            &visible_for_idle,
                        ) && crate::agent::idle_narrative::is_idle_narrative(&visible_for_idle)
                        {
                            match crate::agent::idle_narrative::handle_empty_response(
                                &ctx,
                                &visible_for_idle,
                                &mut idle_streak,
                                false,
                                Some(last_stream_completion_tokens),
                                unified_tool_mode,
                            ) {
                                crate::agent::idle_narrative::IdleAction::EndTurn {
                                    user_status,
                                } => {
                                    tracing::warn!(
                                        "[IDLE] step {} streak {} — ending turn",
                                        pre_llm_step_idx,
                                        idle_streak
                                    );
                                    let _ = ui_tx.send(AgentToUiEvent::Status(user_status));
                                    persist_turn_memory(&workflow_engine, &turn_memory);
                                    drop(engine);
                                    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                                    return;
                                }
                                crate::agent::idle_narrative::IdleAction::Continue {
                                    directive,
                                } => {
                                    let msg = Message::Assistant {
                                        content: crate::agent::think_stream::visible_only(
                                            &full_text,
                                        ),
                                        tool_calls: Vec::new(),
                                        reasoning_content: None,
                                    };
                                    crate::agent::idle_narrative::upsert_idle_assistant(
                                        &mut messages,
                                        &msg,
                                    );
                                    crate::agent::idle_narrative::upsert_idle_assistant(
                                        &mut new_messages,
                                        &msg,
                                    );
                                    if let Some(d) = directive {
                                        crate::agent::idle_narrative::upsert_idle_hint(
                                            &mut messages,
                                            &d,
                                        );
                                    }
                                    persist_turn_memory(&workflow_engine, &turn_memory);
                                    drop(engine);
                                    iteration += 1;
                                    continue;
                                }
                            }
                        }
                    }

            // Single-step model: always show the assistant's text to the user
            // (perception filter strips machine-only findings JSON when present).
            let content_for_session = execute_user_display(
                &workflow_engine,
                pre_llm_step_idx,
                &crate::agent::think_stream::visible_only(&full_text),
            );

            let msg = Message::Assistant {
                content: content_for_session.clone(),
                tool_calls: Vec::new(),
                reasoning_content: None,
            };
            let workflow_active = workflow_engine.as_ref().is_some_and(|wf| {
                wf.try_lock()
                    .map(|e| e.is_workflow_active())
                    .unwrap_or(false)
            });
            if crate::agent::engine::WorkflowEngine::looks_like_review_report(&content_for_session)
            {
                upsert_review_report_assistant(&mut messages, &msg);
                upsert_review_report_assistant(&mut new_messages, &msg);
                if let Some(ref engine_arc) = workflow_engine
                    && let Ok(engine) = engine_arc.try_lock()
                        && engine.is_single_step() {
                            let phase = crate::agent::phase::get(&engine);
                            if matches!(
                                phase,
                                crate::agent::phase::SingleFlowPhase::Receive
                                    | crate::agent::phase::SingleFlowPhase::Review
                            ) {
                                let result = crate::agent::phase::transition(
                                    &engine,
                                    crate::agent::phase::PhaseEvent::ReviewReportDelivered,
                                );
                                notify_workspace_state_if_changed(&ui_tx, &engine, &result);
                            }
                        }
            } else if workflow_active
                && crate::agent::idle_narrative::is_idle_narrative(&content_for_session)
            {
                crate::agent::idle_narrative::upsert_idle_assistant(&mut messages, &msg);
                crate::agent::idle_narrative::upsert_idle_assistant(&mut new_messages, &msg);
            } else {
                new_messages.push(msg.clone());
                messages.push(msg);
            }

            // ── Implement: block re-emitting review findings instead of editing ──
            if let Some(ref engine_arc) = workflow_engine
                && let Ok(engine) = engine_arc.try_lock()
                    && crate::agent::phase::get(&engine)
                        == crate::agent::phase::SingleFlowPhase::Implement
                        && !crate::agent::engine::WorkflowEngine::text_signals_done(&full_text)
                        && (crate::agent::engine::WorkflowEngine::looks_like_review_report(
                            &full_text,
                        ) || crate::agent::perception::extract_from_text(&full_text).is_some())
                    {
                        messages.push(Message::system(
                            "【实施轮】禁止重新输出 findings / 审查报告。\
                             读 [TURN_CONTEXT]「下一步」，直接 file_read → edit_file。",
                        ));
                        persist_turn_memory(&workflow_engine, &turn_memory);
                        iteration += 1;
                        continue;
                    }

            // ── ## Done → gatekeeper pipeline (single-step model) ──
            if crate::agent::engine::WorkflowEngine::text_signals_done(&full_text)
                && let Some(ref engine_arc) = workflow_engine {
                    let mut engine = engine_arc.lock().await;
                    if engine.is_workflow_active() && !engine.is_workflow_complete() {
                        let had_code = turn_memory.had_code_changes();
                        match engine.run_done_gates(&full_text, had_code) {
                            crate::agent::gatekeeper::GateReport::Pass => {
                                engine.set_previous_output(&full_text);
                                let had_receipt =
                                    crate::agent::completion::extract_from_text(&full_text)
                                        .is_some();
                                if let Some(receipt) =
                                    crate::agent::completion::extract_from_text(&full_text)
                                    && let Some(mut store) =
                                        crate::agent::findings::load_or_migrate(&engine)
                                    {
                                        crate::agent::completion::apply_receipt(
                                            &mut store, &receipt,
                                        );
                                        crate::agent::findings::save(&engine, &store);
                                    }
                                let result = crate::agent::phase::transition(
                                    &engine,
                                    crate::agent::phase::PhaseEvent::DoneGatePassed {
                                        had_completion_receipt: had_receipt,
                                    },
                                );
                                notify_workspace_state_if_changed(&ui_tx, &engine, &result);
                                if result.phase == crate::agent::phase::SingleFlowPhase::Complete {
                                    let _ = engine.complete_workflow();
                                    emit_workflow_completed(
                                        &ui_tx,
                                        user_task.as_ref(),
                                        &engine,
                                        &full_text,
                                    );
                                    let _ =
                                        ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                                } else if result.phase
                                    == crate::agent::phase::SingleFlowPhase::AwaitUser
                                {
                                    let _ = ui_tx.send(AgentToUiEvent::Status(
                                        "✅ 审查完成 — 门禁暂停，待用户在面板确认范围（c /confirm）"
                                            .to_string(),
                                    ));
                                } else {
                                    let _ =
                                        ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                                }
                                drop(engine);
                                emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                                return;
                            }
                            crate::agent::gatekeeper::GateReport::Fail { gate, feedback } => {
                                let recovery = gate_recovery_hint(&gate);
                                messages.push(Message::system(format!(
                                    "【门禁·{gate}】{feedback}\n\n\
                                     👉 **恢复：** 按 [TURN_CONTEXT]「下一步」执行；{recovery}"
                                )));
                                persist_turn_memory(&workflow_engine, &turn_memory);
                                drop(engine);
                                iteration += 1;
                                continue;
                            }
                            crate::agent::gatekeeper::GateReport::NeedsUser { gate, prompt } => {
                                let status = format!("【门禁·{gate}】{prompt}");
                                let _ = ui_tx.send(AgentToUiEvent::Status(status.clone()));
                                messages.push(Message::system(&status));
                                persist_turn_memory(&workflow_engine, &turn_memory);
                                drop(engine);
                                emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                                return;
                            }
                        }
                    }
                }

            emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
            return;
        }

        // Sanitize tool_call arguments: if the LLM response was truncated
        // (e.g. finish_reason="length"), arguments may be incomplete JSON.
        // Mark truncated tool calls so we skip execution and return an error
        // to the LLM, letting it retry.
        let mut truncated_ids = std::collections::HashSet::new();
        for tc in &mut tool_calls {
            if !tc.arguments.trim().is_empty() {
                match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    Ok(_) => {} // Valid JSON, no issue
                    Err(e) => {
                        // Check if this looks like truncation vs other JSON errors
                        let is_likely_truncated = is_likely_json_truncation(&tc.arguments, &e);

                        if is_likely_truncated {
                            tracing::warn!(
                                "Truncated tool arguments for '{}' (len {}, error: {}), will return error to LLM",
                                tc.name,
                                tc.arguments.len(),
                                e
                            );
                            truncated_ids.insert(tc.id.clone());
                            tc.arguments = "{}".to_string();
                        } else {
                            // Not truncation, let it pass through to normal error handling
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

        // ✅ CRITICAL FIX: Filter out truncated tool_calls from the Assistant message.
        // Truncated tool calls have already been handled (error ToolResult added),
        // so they should NOT appear in the Assistant message to avoid confusing
        // the compression logic and causing "tool call result does not follow tool call" errors.

        // 🚨 Also filter out tool calls that exceeded the infinite loop limit
        let mut exceeded_loop_limit_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut temp_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        let mut tool_loop_keys: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        let execute_step = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.is_task_step())
            .unwrap_or(false);

        for tc in &tool_calls {
            let loop_key = tool_loop_key(&tc.name, &tc.arguments);
            tool_loop_keys.insert(tc.id.clone(), loop_key.clone());
            let count = temp_counts.entry(loop_key).or_insert(0);
            *count += 1;
            let limit = MAX_SAME_TOOL_CALLS;
            if *count > limit {
                exceeded_loop_limit_ids.insert(tc.id.clone());
            }
        }

        // Single-step model: always show the assistant's text to the user
        // (perception filter strips machine-only findings JSON when present).
        let display = execute_user_display(
            &workflow_engine,
            pre_llm_step_idx,
            &crate::agent::think_stream::visible_only(&full_text),
        );

        // Persist a digest of this turn's reasoning alongside the action. glm-style
        // models put nearly all their analysis inside <think> and emit only a
        // tool_call as visible output; dropping the reasoning every turn means the
        // model can't see WHY it did what it did last turn, driving re-exploration.
        // We fold a short head+tail digest into the content so it survives into the
        // next turn's context (and the message history) without bloating tokens.
        let reasoning_digest_for_action = {
            let r = crate::agent::think_stream::visible_only(&reasoning_content);
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
        let content_with_reasoning = if reasoning_digest_for_action.is_empty() {
            display
        } else if display.trim().is_empty() {
            format!("(本轮思考) {reasoning_digest_for_action}")
        } else {
            format!("{display}\n(本轮思考) {reasoning_digest_for_action}")
        };

        // 🪞 Reflect-FIRST guard (fires BEFORE this turn's tools execute).
        //
        // Two separate loops we catch here:
        //  • Exploration: read-after-read without ever acting (threshold 10).
        //  • Implementation: plan confirmed, but drifting into no-edit turns
        //    instead of editing (threshold 3, implementation phase only).
        //
        // When a threshold trips we DISCARD this turn's chosen tool batch,
        // record the reasoning as a tool-call-free assistant message (so no
        // ToolResult is orphaned), inject the reflection, and loop — forcing the
        // model to re-decide with the reflection in view. A `finish` batch is
        // treated as progress and never skipped.
        {
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
            let user_task_str = user_task.as_deref().unwrap_or("");
            let in_impl_phase = workflow_engine
                .as_ref()
                .and_then(|wf| wf.try_lock().ok())
                .map(|e| crate::agent::workflow_session::is_implementation_phase(&e))
                .unwrap_or(false);

            // 🔍 Information-gain signal: does this turn's read-only batch surface
            // anything NEW (unread file, further slice, fresh query, structural
            // listing)? Evaluated against engine state, which already records what
            // prior turns read. A discovering turn is real progress and resets the
            // exploration streak — so reading many *different* files in a large
            // project never trips the budget; only repeated low-gain reads do.
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
                                    serde_json::from_str(&tc.arguments)
                                        .unwrap_or(serde_json::json!({})),
                                ),
                            }
                        } else {
                            (
                                tc.name.clone(),
                                serde_json::from_str(&tc.arguments)
                                    .unwrap_or(serde_json::json!({})),
                            )
                        };
                        crate::agent::read_guard::is_discovery_call(
                            &engine,
                            &inner_name,
                            &inner_args,
                        )
                    })
                })
                .unwrap_or(true); // No engine → don't penalize.

            // Implementation phase → impl guard (no-edit streak); otherwise the
            // exploration guard (low-gain read streak). Never both in one turn.
            let action = if in_impl_phase {
                explore_reflect::evaluate_impl(
                    &mut impl_streak,
                    &mut impl_reflected,
                    &turn_tool_names,
                    had_finish,
                    user_task_str,
                )
            } else {
                // Convergence action depends on task intent: review submits a plan
                // (writes locked); fix/general edits directly (writes unlocked);
                // Q&A answers. Default SubmitPlan when no engine — never nudge an
                // edit unless we can confirm writes are unlocked.
                let converge = workflow_engine
                    .as_ref()
                    .and_then(|wf| wf.try_lock().ok())
                    .map(|e| explore_reflect::ConvergeMode::from_intent(e.get_task_intent()))
                    .unwrap_or(explore_reflect::ConvergeMode::SubmitPlan);
                explore_reflect::evaluate(
                    &mut explore_streak,
                    &mut explore_reflected,
                    &mut total_explore,
                    &turn_tool_names,
                    had_finish,
                    made_discovery,
                    user_task_str,
                    converge,
                )
            };

            let reflect_prompt = match action {
                explore_reflect::ReflectAction::Continue => None,
                explore_reflect::ReflectAction::Reflect(prompt) => {
                    let label = if in_impl_phase {
                        "🛠️ 实施反思检查点 — 提示模型停止泛读、立即动手。"
                    } else {
                        "🪞 探索反思检查点 — 提示模型盘点已知信息后动手。"
                    };
                    tracing::info!(
                        "[REFLECT] Pre-exec reflect (impl_phase={in_impl_phase}, explore_streak={explore_streak}, impl_streak={impl_streak}) — skipping this tool batch"
                    );
                    let _ = ui_tx.send(AgentToUiEvent::Status(label.to_string()));
                    Some(prompt)
                }
                explore_reflect::ReflectAction::Stop(handoff) => {
                    // Exploration hit a stop threshold — either the low-gain streak
                    // ran past reflection, or the cumulative ceiling tripped. The
                    // handoff message already states which; keep the c/其他 gate so
                    // the user can wave it on.
                    let gate_msg = format!(
                        "{handoff}\n\n\
                         **c** 继续探索\n\
                         **其他** 结束本轮"
                    );
                    let _ = ui_tx.send(AgentToUiEvent::Status(
                        "⏸️ 探索预算耗尽 — c 继续 · 其他结束".to_string(),
                    ));
                    explore_streak = 0;
                    total_explore = 0;
                    Some(gate_msg)
                }
            };

            if let Some(prompt) = reflect_prompt {
                // Record the reasoning without the (discarded) tool_calls so the
                // model retains WHY it was about to act, then inject the reflection.
                let reasoning_only = Message::Assistant {
                    content: content_with_reasoning.clone(),
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                };
                new_messages.push(reasoning_only.clone());
                messages.push(reasoning_only);
                messages.push(Message::system(&prompt));
                new_messages.push(Message::system(&prompt));
                persist_turn_memory(&workflow_engine, &turn_memory);
                iteration += 1;
                continue;
            }
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

        let visible_summary = crate::agent::think_stream::visible_only(&full_text)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ");
        let mut visible_summary: String = visible_summary.chars().take(260).collect();
        // The model usually puts its reasoning inside <think>, leaving the visible
        // text empty. Fall back to a reasoning digest so the decision record
        // captures WHY this action was chosen — not just that it was chosen.
        if visible_summary.trim().is_empty() && !reasoning_content.trim().is_empty() {
            let r = crate::agent::think_stream::visible_only(&reasoning_content);
            let r = if r.is_empty() { reasoning_content.clone() } else { r };
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

        // 🧠 Record this turn as L0 WorkingMemory with the LLM's raw response
        let user_text = user_task.as_deref().unwrap_or("");
        let assistant_preview: String = full_text.chars().take(400).collect();
        let assistant_truncated = if assistant_preview.len() < full_text.len() {
            "..."
        } else {
            ""
        };
        let l0_content = format!(
            "User: {}\n\nAssistant: {}{}",
            user_text.chars().take(300).collect::<String>(),
            assistant_preview,
            assistant_truncated
        );
        {
            if let Some(knowledge) = tool_ctx.knowledge.clone() {
                tokio::task::spawn(async move {
                    if let Ok(mut engine) = knowledge.try_write() {
                        let _ =
                            engine.record_turn("current", &l0_content, None, None, vec![], true);
                    }
                });
            }
        }

        // ── Context Offloader: created once and reused across all tools in this iteration ──
        let mut offloader = context_offloader::ContextOffloader::new(
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

            if unified_tool_mode
                && tc.name == crate::agent::unified_action::TOOL_NAME
                && tc.arguments.trim().is_empty()
            {
                let error_msg = "❌ complete_and_check 参数为空。\n\n\
                     必须发送合法 JSON，例如：\n\
                     {\"action\":\"file_read\",\"params\":{\"path\":\"src/main.rs\"}}\n\n\
                     禁止空 arguments；每轮必须包含 action 与 params。";
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
                unified_parse_error_streak += 1;
                if unified_parse_error_streak >= 3 {
                    messages.push(Message::system(
                        "⚠️ 已连续 3 次空/无效 complete_and_check 参数。\
                         必须发送合法 JSON：{\"action\":\"…\",\"params\":{…}}\n\
                         例如 action=file_read, action=edit_file, action=finish",
                    ));
                }
                if unified_parse_error_streak >= 5 {
                    // Hard stop — LLM is stuck in an empty-arg loop, force turn end
                    let _ = ui_tx.send(AgentToUiEvent::Status(
                        "⏹️ 连续 5 次空 complete_and_check — 强制结束本轮".to_string(),
                    ));
                    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                    return;
                }
                continue;
            }

            if unified_tool_mode && tc.name == crate::agent::unified_action::TOOL_NAME {
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
                        &tool_registry,
                        &tool_ctx,
                        &trust_manager,
                        &workflow_engine,
                        &mut messages,
                        &ui_tx,
                        &mut ui_rx,
                        &cancel_token,
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
                        // 增强超时日志：记录更多上下文信息
                        let action_hint = tc.arguments.chars().take(100).collect::<String>();
                        tracing::error!(
                            "[UNIFIED_CALL] TIMEOUT after 300s — aborting | iteration={} | tool_calls_in_turn={} | action_hint={}",
                            iteration,
                            tool_calls.len(),
                            action_hint
                        );
                        let _ = ui_tx.send(AgentToUiEvent::Status(
                            format!(
                                "⏱️ 操作超时 (300s) — 强制结束 | 已重试 {} 次",
                                iteration
                            )
                        ));
                        emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                        return;
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
                            if content.contains("empty arguments")
                                || content.contains("invalid JSON")
                            {
                                unified_parse_error_streak += 1;
                                if unified_parse_error_streak >= 3 {
                                    messages.push(Message::system(
                                        "⚠️ 已连续 3 次空/无效 complete_and_check 参数。\
                                         必须发送合法 JSON：{\"action\":\"…\",\"params\":{…}}",
                                    ));
                                }
                                if unified_parse_error_streak >= 5 {
                                    let _ = ui_tx.send(AgentToUiEvent::Status(
                                        "⏹️ 连续 5 次无效 complete_and_check — 强制结束本轮"
                                            .to_string(),
                                    ));
                                    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                                    return;
                                }
                            }
                        } else {
                            unified_parse_error_streak = 0;
                        }
                        // Track findings format errors to break retry loops
                        if is_error && tc.arguments.contains("\"finding") {
                            findings_deliver_error_streak += 1;
                            if findings_deliver_error_streak >= 3 {
                                messages.push(Message::system(
                                    "⚠️ 连续 3 次 finding_json 格式错误。改用 finish(params.content=...) 先汇报分析。",
                                ));
                                findings_deliver_error_streak = 0;
                            }
                        }
                        deferred_tool_system.extend(deferred_system);

                        // Log full unified handler result. NOTE: truncate by CHARS,
                        // not bytes — `&s[..n]` panics when byte `n` lands inside a
                        // multibyte UTF-8 char (e.g. Chinese), which silently killed
                        // the agent task and froze the UI (no TurnDone ever emitted).
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
                            record_tool_live_update(
                                &tool_ctx,
                                &workflow_engine,
                                &user_task,
                                &meta.inner_tool,
                                &meta.inner_args,
                                &meta.live_output,
                                is_error,
                            )
                            .await;
                            // ── Persist the ReAct triple to react_log (unified path) ──
                            // This is the L0 memory ground truth: [user, assistant,
                            // tool_result]. Previously only the legacy tool path wrote
                            // here, so in unified mode react_log stayed empty.
                            if let Some(ref ms) = tool_ctx.memory_store {
                                let session_id = workflow_engine
                                    .as_ref()
                                    .and_then(|wf| wf.try_lock().ok())
                                    .map(|e| e.session_id())
                                    .unwrap_or_else(|| "default".to_string());
                                let react_task = user_task.clone().unwrap_or_default();
                                let decision = turn_memory
                                    .decisions
                                    .last()
                                    .map(|d| d.chars().take(200).collect::<String>())
                                    .unwrap_or_default();
                                let assistant_text =
                                    crate::agent::think_stream::visible_only(&full_text);
                                let outcome = if is_error { "error" } else { "ok" };
                                let _ = ms.record_react(
                                    &session_id,
                                    &react_task,
                                    &meta.inner_tool,
                                    &target,
                                    outcome,
                                    &decision,
                                    &assistant_text,
                                    &meta.live_output,
                                );
                            }
                        } else {
                            turn_memory.record_tool(&tc.name, &tc.arguments, is_error);
                        }
                        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                            name: tc.name.clone(),
                            output: content,
                            is_error,
                        });
                    }
                    crate::agent::unified_handler::UnifiedHandleOutcome::TurnDone { summary } => {
                        // Persist the agent's final free-text summary so it lives
                        // in the session transcript (it was previously only
                        // previewed in the UI via DeliverPreview and lost on
                        // reload). Prefer attaching it to the finishing assistant
                        // message (which holds the finish tool call) so we don't
                        // create back-to-back assistant messages.
                        if let Some(summary) = summary {
                            let summary = summary.trim();
                            if !summary.is_empty() {
                                match new_messages
                                    .iter_mut()
                                    .rev()
                                    .find(|m| matches!(m, Message::Assistant { .. }))
                                {
                                    Some(Message::Assistant { content, .. })
                                        if content.trim().is_empty() =>
                                    {
                                        *content = summary.to_string();
                                    }
                                    _ => new_messages.push(Message::assistant(summary)),
                                }
                            }
                        }
                        if let Some(wf) = &workflow_engine
                            && let Ok(engine) = wf.try_lock() {
                                crate::agent::round_memory::append_round(
                                    &engine,
                                    crate::agent::round_memory::RoundRecord {
                                        round_id: iteration,
                                        user_intent: user_task.clone().unwrap_or_default(),
                                        actions_summary: turn_memory.tool_names_summary(),
                                        deliverables_summary: "finish confirmed".into(),
                                        gate_outcomes: vec!["finish:user_finished".into()],
                                    },
                                );
                            }
                        emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                        return;
                    }
                    crate::agent::unified_handler::UnifiedHandleOutcome::Aborted => {
                        emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                        return;
                    }
                }
                persist_turn_memory(&workflow_engine, &turn_memory);
                continue;
            }

            // 🚨 Detect infinite loop: same tool called too many times
            // Note: We already calculated exceeded_loop_limit_ids above, so just check if this ID is in the set
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
                let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                    name: tc.name.clone(),
                    output: error_msg,
                    is_error: true,
                });
                continue;
            }

            // Skip truncated tool calls — return error so LLM can retry.
            if truncated_ids.contains(&tc.id) {
                // Special handling for different tools
                let is_file_write = tc.name == "file_write";
                let is_edit_file = tc.name == "edit_file";
                let content_length = tc.arguments.len();

                let error_msg = if is_file_write && content_length > 10000 {
                    // Likely large file write that was truncated
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
                    // Likely edit_file with long search/replace that was truncated
                    // Try to extract partial info for better error message
                    let partial_info = if let Ok(args_val) =
                        serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    {
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
                    // General truncation error
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
                        tc.name
                    )
                };

                tracing::warn!(
                    "Tool '{}' (id={}) had truncated arguments ({} bytes). Sending error to LLM.",
                    tc.name,
                    tc.id,
                    content_length
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
                continue;
            }

            let _ = ui_tx.send(AgentToUiEvent::Status(format!("Running tool: {}", tc.name)));

            // ── Workflow validation before execution ──
            if let Some(ref engine_arc) = workflow_engine {
                let engine = engine_arc.lock().await;

                // Parse tool arguments for validation
                let args_value = if !tc.arguments.trim().is_empty() {
                    serde_json::from_str::<serde_json::Value>(&tc.arguments)
                        .unwrap_or(serde_json::json!({}))
                } else {
                    serde_json::json!({})
                };

                // Read guard: duplicate file_read / shell-as-read
                if let Err(e) = crate::agent::read_guard::check(&tc.name, &args_value, &engine) {
                    if tc.name == "file_read"
                        && let Some(path) = args_value.get("path").and_then(|p| p.as_str())
                            && let Some(cached) =
                                crate::agent::read_guard::cached_file_read_response(&engine, path)
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
                                continue;
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
                    continue;
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
                    continue; // Skip this tool call
                }
            }

            // Send detailed ToolStart for UI display
            let tool_detail = tool_executor::extract_tool_detail(&tc.name, &tc.arguments);
            // Always send ToolStart to UI (detail is optional)
            let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                name: tc.name.clone(),
                id: tc.id.clone(),
                detail: tool_detail,
            });

            tracing::info!("[AGENT] About to get tool object for: {}", tc.name);
            let tool = match tool_registry.get(&tc.name) {
                Some(t) => {
                    tracing::info!("[AGENT] Tool object retrieved for: {}", tc.name);
                    t
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
                    continue;
                }
            };

            // ── Safety check before execution ──
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

            // 🆕 Workflow step validation before execution
            // In pipeline mode, Steps 0-2 handle planning/review. Rule enforcement
            // (plan_before_edit, read_before_edit) is bypassed for Step 3 (Execute).
            let skip_plan_rules = matches!(&workflow_engine, Some(wf) if {
                wf.try_lock().is_ok_and(|e| {
                    e.is_single_step()
                        || (e.is_workflow_active() && e.get_current_step_index() >= 3)
                })
            });

            if !skip_plan_rules
                && let Err(violation_msg) = crate::agent::enforcer::RuleEnforcer::validate(
                    &tool_ctx.config.enforcement_rules,
                    tc,
                    &messages,
                ) {
                    tracing::warn!(
                        "🚫 Rule Enforcer blocked tool '{}': {}",
                        tc.name,
                        violation_msg
                    );

                    let error_result = Message::ToolResult {
                        tool_call_id: tc.id.clone(),
                        content: violation_msg.clone(),
                    };
                    new_messages.push(error_result.clone());
                    messages.push(error_result);

                    let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                        name: tc.name.clone(),
                        output: violation_msg,
                        is_error: true,
                    });

                    continue;
                }

            let mut blacklist_warning: Option<String> = None;
            if tc.name == "shell_exec"
                && let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    && let Some(cmd) = args_val.get("command").and_then(|v| v.as_str()) {
                        blacklist_warning =
                            safety_gate::shell_blacklist_warning(&trust_manager, cmd);
                    }

            let should_confirm = safety_gate::needs_confirmation(
                &trust_manager,
                &tc.name,
                safety_level,
                path_outside,
                blacklist_warning.is_some(),
            );

            if should_confirm {
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
                safety_gate::emit_request(&ui_tx, &req);

                let decision = match safety_gate::await_decision(
                    &mut ui_rx,
                    &cancel_token,
                    &tc.id,
                    &workflow_engine,
                    &mut messages,
                    &ui_tx,
                    push_interjection_message,
                )
                .await
                {
                    Ok(d) => d,
                    Err(safety_gate::SafetyGateCancelled) => {
                        let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
                        emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                        return;
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
                        let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                            name: tc.name.clone(),
                            output: error_msg,
                            is_error: true,
                        });
                        continue;
                    }
                    ui_event::ConfirmationDecision::TrustAlways => {
                        tracing::info!("[AGENT] User trusted all tools");
                        safety_gate::apply_trust_all(&trust_manager);
                    }
                    ui_event::ConfirmationDecision::Allow => {
                        tracing::info!("[AGENT] User allowed tool: {}", tc.name);
                    }
                }
            }

            let args: serde_json::Value = if tc.arguments.trim().is_empty() {
                // LLM sent no arguments — treat as empty object (common for no-param tools).
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                // Clean think tags from arguments before parsing
                let cleaned_args = clean_think_tags(&tc.arguments);

                match serde_json::from_str(&cleaned_args) {
                    Ok(v) => v,
                    Err(parse_err) => {
                        // Provide helpful guidance with examples
                        let example = match tc.name.as_str() {
                            "file_read" => "{\"path\": \"src/main.rs\", \"limit\": 100}",
                            "file_write" => {
                                "{\"path\": \"output.txt\", \"content\": \"Hello World\"}"
                            }
                            "edit_file" => {
                                "{\"path\": \"src/lib.rs\", \"old_string\": \"...\", \"new_string\": \"...\"}"
                            }
                            "shell_exec" => "{\"command\": \"ls -la\", \"timeout_ms\": 5000}",
                            "file_search" => "{\"pattern\": \"*.rs\", \"path\": \"src/\"}",
                            "code_search" => "{\"query\": \"fn main\", \"path\": \"src/\"}",
                            _ => "{ /* check tool documentation */ }",
                        };

                        let error_msg = format!(
                            "❌ JSON Parse Error for tool '{}':\n{}\n\n\
                             💡 How to fix:\n\
                             • Ensure valid JSON syntax (no trailing commas)\n\
                             • Quote all keys and string values with double quotes\n\
                             • Escape special characters in strings\n\
                             • Check for missing brackets or braces\n\n\
                             📝 Correct format example:\n\
                             {}\n\n\
                             Please retry with corrected arguments.",
                            tc.name, parse_err, example
                        );

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
                        continue;
                    }
                }
            };

            // Check for queued interjections before tool execution.
            while let Ok(ev) = ui_rx.try_recv() {
                match ev {
                    ui_event::UiToAgentEvent::Interjection(text) => {
                        push_interjection_message(&workflow_engine, &mut messages, &text, &ui_tx);
                    }
                    ui_event::UiToAgentEvent::ScopeConfirmed
                    | ui_event::UiToAgentEvent::BusinessAck { .. } => {
                        if let Some(wf) = &workflow_engine
                            && let Ok(engine) = wf.try_lock() {
                                engine.set_variable(
                                    crate::agent::business_gate::PRE_ACK_KEY,
                                    "1".to_string(),
                                );
                            }
                    }
                    _ => {}
                }
            }

            // ── Pre-execution validation for file_write tool ──
            if tc.name == "file_write" {
                let has_path = args.get("path").is_some();
                let has_filename = args.get("filename").is_some();
                let has_file_id = args.get("file_id").is_some();

                if !has_path && !has_filename && !has_file_id {
                    // Return error to LLM before executing
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
                    continue;
                }
            }

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
            let tool_ctx_with_progress =
                Arc::new(crate::tools::ToolContext::with_progress_callback(
                    tool_ctx.runtime.clone(),
                    tool_ctx.working_dir.clone(),
                    tool_ctx.config.clone(),
                    tool_ctx.knowledge.clone(),
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
            if result.is_error
                && matches!(tc.name.as_str(), "file_write" | "shell_exec" | "web_fetch")
            {
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
            // ── Full tool I/O logging for debugging ──
            let args_preview: String =
                serde_json::to_string_pretty(&args).unwrap_or_else(|_| format!("{:?}", args));
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

            record_tool_live_update(
                &tool_ctx,
                &workflow_engine,
                &user_task,
                &tc.name,
                &tc.arguments,
                &result.content,
                result.is_error,
            )
            .await;

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
            if let Some(new_dir) = result.new_working_dir.clone() {
                tool_ctx = Arc::new(ToolContext::new(
                    tool_ctx.runtime.clone(),
                    new_dir.clone(),
                    tool_ctx.config.clone(),
                    tool_ctx.knowledge.clone(),
                ));
                let _ = ui_tx.send(AgentToUiEvent::WorkingDirChanged(new_dir));
            }

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

            // ── Context Offloading: only offload shell_exec (build logs can be huge) ──
            // file_read results are essential context — never offload
            let offload_threshold: usize = if tc.name == "shell_exec" {
                4000
            } else {
                usize::MAX // Never offload non-shell_exec results
            };
            let offloaded = offloader.process_result(
                &tc.name,
                &tc.arguments,
                &sanitized_content,
                iteration as usize,
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

            // Record to SQLite react_log for cross-round memory
            if let Some(ref ms) = tool_ctx.memory_store {
                let session_id = workflow_engine.as_ref()
                    .and_then(|wf| wf.try_lock().ok())
                    .map(|e| e.session_id())
                    .unwrap_or_else(|| "default".to_string());
                let task = workflow_engine.as_ref()
                    .and_then(|wf| wf.try_lock().ok())
                    .and_then(|e| e.get_variable("_current_user_request"))
                    .unwrap_or_default();
                let target = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    .ok()
                    .and_then(|v| {
                        v.get("params")
                            .or_else(|| v.get("path"))
                            .or_else(|| v.get("name"))
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();
                let outcome = if result.is_error { "error" } else { "ok" };
                // Attach the latest in-turn decision so the summarizer has "why".
                let decision = turn_memory
                    .decisions
                    .last()
                    .map(|d| d.chars().take(200).collect::<String>())
                    .unwrap_or_default();
                // ReAct triple: assistant reasoning (visible text this turn) + tool result.
                let assistant_text = crate::agent::think_stream::visible_only(&full_text);
                let _ = ms.record_react(
                    &session_id,
                    &task,
                    &tc.name,
                    &target,
                    outcome,
                    &decision,
                    &assistant_text,
                    &result.content,
                );
            }

            let mut result_content = format!(
                "── DATA ({}) ──\n{}\n── END DATA ──",
                tc.name,
                offloaded.to_context_message()
            );

            if tc.name == "file_read"
                && !result.is_error
                && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
            {
                if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                    let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as u32;
                    if let Some(ref engine_arc) = workflow_engine
                        && let Ok(engine) = engine_arc.try_lock() {
                            crate::agent::read_guard::record_file_read(&engine, path);
                            crate::agent::tool_digest::record_read(
                                &engine,
                                path,
                                &result.content,
                                offset,
                                None,
                            );
                            // Digest wrapping removed — LLM needs full file content.
                            // Compaction at iteration 3 handles context bloat.
                        }
                }
            } else if matches!(tc.name.as_str(), "find_symbol" | "code_search")
                && !result.is_error
                && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                && let Some(ref engine_arc) = workflow_engine
                && let Ok(engine) = engine_arc.try_lock() {
                    crate::agent::read_guard::record_symbol_query(&engine, &tc.name, &args);
                }

            // Snapshot tool results for Plan / Execute step iteration memory
            if !result.is_error
                && let Some(ref engine_arc) = workflow_engine
                    && let Ok(engine) = engine_arc.try_lock() {
                        let step = engine.get_current_step_index();
                        if crate::agent::exploration_snapshot::should_snapshot_for_step(
                            step, &tc.name,
                        ) {
                            let target = crate::agent::exploration_snapshot::target_from_tool_args(
                                &tc.name,
                                &tc.arguments,
                            );
                            engine.record_exploration_result(
                                &tool_ctx.working_dir,
                                &tc.name,
                                &target,
                                &result_content,
                            );
                        }
                    }

            turn_memory.record_tool_with_result(
                &tc.name,
                &tc.arguments,
                !result.is_error,
                Some(&result_content),
            );
            let target =
                crate::agent::exploration_snapshot::target_from_tool_args(&tc.name, &tc.arguments);
            let observation: String =
                crate::agent::exploration_snapshot::extract_data_content(&result_content)
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
            persist_turn_memory(&workflow_engine, &turn_memory);

            if tc.name == "shell_exec"
                && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    && let Some(cmd) = args.get("command").and_then(|c| c.as_str()) {
                        let succeeded =
                            post_edit_verification::shell_result_success(&sanitized_content);
                        if let Some(ref engine_arc) = workflow_engine
                            && let Ok(engine) = engine_arc.try_lock() {
                                post_edit_verification::note_shell_verify_result(
                                    &engine, cmd, succeeded,
                                );
                                if succeeded
                                    && let Some(idx) = engine.get_plan_tracker().and_then(|t| {
                                        t.steps
                                            .iter()
                                            .find(|s| !s.verify.is_empty() && s.awaiting_verify)
                                            .map(|s| s.index)
                                    }) {
                                        crate::agent::verifier::after_verify_pass(&engine, idx);
                                    }
                            }
                    }

            if result.is_error && tc.name == "edit_file"
                && let Some(ref engine_arc) = workflow_engine
                    && let Ok(engine) = engine_arc.try_lock()
                        && crate::agent::workflow_session::is_implementation_phase(&engine)
                            && let Ok(args) =
                                serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                && let Some(path) = args.get("path").and_then(|p| p.as_str()) {
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

            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: result_content.clone(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);

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
                        .map(|p| format!(" → {}", p))
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
                    "📋 ✅ {tool_name}{file_info} — {done_label}",
                    tool_name = tool_name,
                    file_info = file_info,
                    done_label = done_label
                ));
                tools_used_this_turn.insert(tool_name.clone());

                // Track explored paths during Plan only (Execute may re-read files)
                if matches!(tool_name.as_str(), "file_list" | "file_read")
                    && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                        if let Some(ref engine_arc) = workflow_engine
                            && let Ok(engine) = engine_arc.try_lock() {
                                if crate::agent::phase::get(&engine)
                                    == crate::agent::phase::SingleFlowPhase::Review
                                {
                                    engine.record_explored_path(&tool_name, path);
                                } else if engine.is_task_step() && tool_name == "file_list" {
                                    engine.record_explored_path(&tool_name, path);
                                }
                            }
                    }

                // Execute: update plan tracker for completing tools
                if let Some(ref engine_arc) = workflow_engine
                    && let Ok(engine) = engine_arc.try_lock() {
                        if engine.is_task_step() {
                            if tool_name == "file_read"
                                && crate::agent::workflow_session::is_implementation_phase(&engine)
                                && let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                    && let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                                        engine.record_impl_file_read(path, &tc.arguments);
                                        if let Some(nudge) =
                                            engine.impl_edit_nudge_after_read(path, &result_content)
                                        {
                                            deferred_tool_system.push(nudge);
                                        }
                                    }
                            let (plan_changed, plan_hint) = engine.record_execute_tool_success(
                                &tool_name,
                                &tc.arguments,
                                &result_content,
                            );
                            if let Some(hint) = plan_hint {
                                deferred_tool_system.push(hint);
                            }
                            if plan_changed
                                && let Some(msg) =
                                    engine.plan_progress_message_after_tool(&tool_name)
                                {
                                    deferred_tool_system.push(msg);
                                }
                            if matches!(
                                tool_name.as_str(),
                                "edit_file" | "file_write" | "delete_range"
                            ) && crate::agent::workflow_session::is_implementation_phase(&engine)
                                && let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                    && let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                                        engine.record_impl_file_edited(path);
                                        let idx = engine
                                            .get_plan_tracker()
                                            .and_then(|t| t.current_step().map(|s| s.index))
                                            .unwrap_or(1);
                                        if let Some(note) = crate::agent::verifier::after_edit_note(
                                            &engine,
                                            idx,
                                            path,
                                            &result_content,
                                        ) {
                                            deferred_tool_system.push(note);
                                        }
                                    }
                        }
                        if matches!(
                            tool_name.as_str(),
                            "file_write" | "edit_file" | "delete_range"
                        )
                            && let Ok(args) =
                                serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                && let Some(path) = args.get("path").and_then(|p| p.as_str())
                                    && let Some(verify) = engine.verify_hint_for_path(path) {
                                        deferred_tool_system.push(format!(
                                            "📋 计划验证: `{verify}` — 请用 shell_exec 执行（需用户确认），验证通过后再继续下一项。"
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
                let onboarding_skill = workflow_engine.is_none()
                    && onboarding::is_onboarding_turn(&messages)
                    && is_skill;

                // Execute step skill creation: tell LLM to output ## Done
                let is_execute_step = workflow_engine.as_ref().is_some_and(|wf| {
                    wf.try_lock().is_ok_and(|e| e.is_task_step())
                });

                if is_execute_step && is_skill {
                    deferred_tool_system.push(if unified_tool_mode {
                        "✅ 文件已写入。若全部完成，调用 complete_and_check(action=finish, params={summary:\"...\"})。".to_string()
                    } else {
                        "✅ 文件已写入。如果所有需要的文件都已完成，输出 `## Done` 结束。".to_string()
                    });
                } else if onboarding_skill {
                    let root = tool_ctx
                        .runtime
                        .project_root
                        .clone()
                        .unwrap_or_else(|| tool_ctx.working_dir.clone());
                    if onboarding::onboarding_files_complete(&root) {
                        deferred_tool_system.push(if unified_tool_mode {
                            "✅ 两个 Skill 都已写入。调用 action=finish 结束，不要再改文件。".to_string()
                        } else {
                            "✅ 两个 Skill 都已写入（项目规范 + 业务指导）。输出 `## Done` 结束，不要再改文件。"
                                .to_string()
                        });
                    } else {
                        let missing = onboarding::missing_onboarding_files(&root).join("、");
                        deferred_tool_system.push(format!(
                            "✅ 已写入一个 Skill。还缺：{missing}。请继续 file_write 缺失文件。"
                        ));
                    }
                }
            } // verify-after-edit
        } // end for tc

        for note in deferred_tool_system {
            messages.push(Message::system(&note));
        }

        // ── Post-hoc fix: remove tool_calls from LATEST Assistant msg that have no ToolResult ──
        // The Assistant message was pushed before execution. Tools rejected by
        // validation/safety/truncation/loop-limit were skipped. Fix the LATEST
        // Assistant message only — NOT previous iterations' messages (which were
        // already clean). Previous messages MUST be preserved for context continuity.
        {
            // Collect tool call IDs that have ToolResults in THIS iteration's batch.
            // Also include FULL message list IDs to protect previous iterations.
            let all_result_ids: std::collections::HashSet<String> = messages
                .iter()
                .filter_map(|m| {
                    if let Message::ToolResult { tool_call_id, .. } = m {
                        Some(tool_call_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            // Only fix the LAST Assistant message in each list (the one just pushed).
            // Previous iterations' Assistant messages are already correctly paired.
            for msgs in [&mut messages, &mut new_messages] {
                if let Some(last_assistant_pos) = msgs
                    .iter()
                    .rposition(|m| matches!(m, Message::Assistant { .. }))
                    && let Message::Assistant { tool_calls, .. } = &mut msgs[last_assistant_pos] {
                        let before = tool_calls.len();
                        tool_calls.retain(|tc| all_result_ids.contains(&tc.id));
                        if tool_calls.len() != before {
                            tracing::info!(
                                "[POST-FILTER] Removed {} orphaned tool_calls from latest Assistant msg ({} → {})",
                                before - tool_calls.len(),
                                before,
                                tool_calls.len()
                            );
                        }
                    }
                // Remove Assistant at that position if it became empty
                if let Some(pos) = msgs.iter().rposition(|m| matches!(m, Message::Assistant { content, tool_calls, .. } if content.is_empty() && tool_calls.is_empty())) {
                    msgs.remove(pos);
                }
            }
        }

        // 🗺️ Inject task canvas if any results were offloaded
        if let Some(canvas_ctx) = offloader.get_canvas_context() {
            messages.push(Message::system(&canvas_ctx));
        }

        // 🚨 Done reminder + AST recovery + verify hints
        if !tool_calls.is_empty() {
            let has_write = tool_calls.iter().any(|tc| {
                matches!(
                    tc.name.as_str(),
                    "file_write" | "edit_file" | "delete_range"
                )
            });
            let has_ast =
                post_edit_verification::tool_batch_has_ast_issues(&new_messages, &tool_calls);

            post_edit_verification::check_ast_and_recover(
                &mut messages,
                &new_messages,
                &tool_calls,
            );

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
                if let Some(ref engine_arc) = workflow_engine
                    && let Ok(engine) = engine_arc.try_lock() {
                        post_edit_verification::track_edits_and_verify_plan(
                            &engine,
                            &project_root,
                            &tool_calls,
                            &new_messages,
                            true,
                        );
                        if !has_ast
                            && let Some(hint) = post_edit_verification::verify_hint_message(&engine)
                            {
                                messages.push(Message::system(&hint));
                            }
                    }
            }

            if has_write && !onboarding::is_onboarding_turn(&messages) && !has_ast {
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

            // 🔄 Auto-fix: if build/test failed, inject error for self-repair
            // Also pass gitnexus for impact analysis when available
            error_recovery::check_and_recover(
                &mut messages,
                &new_messages,
                &tool_calls,
                tool_ctx.gitnexus.as_ref(),
            );

            // 🛑 Repeated-failure hand-off: if the same verify has failed N times
            // in a row, stop auto-retrying and give control back to the user
            // instead of spinning. Mirrors the `## Done` gatekeeper stop path.
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
                    "🛑 连续 {streak} 次验证失败 — 暂停本轮，等待你的指示。"
                )));
                messages.push(Message::system(&handoff));
                new_messages.push(Message::system(&handoff));
                if let Some(wf) = &workflow_engine
                    && let Ok(engine) = wf.try_lock() {
                        post_edit_verification::reset_verify_failures(&engine);
                    }
                persist_turn_memory(&workflow_engine, &turn_memory);
                emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                return;
            }
        }

        // Clean up old offloaded refs, keeping at most the 50 most recent ones.
        if let Err(e) = offloader.cleanup_old_refs(50) {
            tracing::warn!("Failed to clean up old refs: {}", e);
        }

        // 🔁 Repeated-output guard: catch the degenerate loop where the model
        // emits near-identical reasoning turn after turn without progressing.
        match repeat_guard.observe(&crate::agent::think_stream::visible_only(&full_text)) {
            repeat_guard::RepeatAction::Continue => {}
            repeat_guard::RepeatAction::Nudge(nudge) => {
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "🔁 检测到重复思考 — 提示模型发出具体动作。".to_string(),
                ));
                messages.push(Message::system(&nudge));
            }
            repeat_guard::RepeatAction::Stop(handoff) => {
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "🛑 连续重复思考无法推进 — 暂停本轮，等待你的指示。".to_string(),
                ));
                messages.push(Message::system(&handoff));
                new_messages.push(Message::system(&handoff));
                persist_turn_memory(&workflow_engine, &turn_memory);
                emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
                return;
            }
        }

        // Loop back to call LLM again with tool results.
        persist_turn_memory(&workflow_engine, &turn_memory);
        iteration += 1;
        if !tool_calls.is_empty() {
            idle_streak = 0;
            content_only_streak = 0;
        }
    }

    persist_turn_memory(&workflow_engine, &turn_memory);
    // Loop exited via break (cancellation or user declined to continue).
    emit_turn_done(&ui_tx, turn_id, new_messages, total_usage);
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
    crate::agent::idle_narrative::strip_idle_hints(messages);
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
        .map(|e| !crate::agent::workflow_session::is_implementation_phase(&e))
        .unwrap_or(false);
    if filter {
        crate::agent::perception::format_for_user_display(text)
    } else {
        text.to_string()
    }
}

/// Emit WorkflowCompleted so the CLI can trigger auto-reflection.
fn emit_workflow_completed(
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    user_task: Option<&String>,
    engine: &crate::agent::engine::WorkflowEngine,
    fallback_summary: &str,
) {
    let task_description = user_task
        .cloned()
        .unwrap_or_else(|| "Unknown task".to_string());
    let summary = engine.get_all_step_outputs_summary();
    let execution_summary = if summary == "（无上一步输出）" {
        fallback_summary.chars().take(1000).collect()
    } else {
        summary
    };
    let _ = ui_tx.send(AgentToUiEvent::WorkflowCompleted {
        task_description,
        execution_summary,
    });
}

fn gate_recovery_hint(gate: &str) -> &'static str {
    match gate {
        "verify" | "syntax" => "运行验证命令或修正语法后再 ## Done。",
        "citation" | "provenance" => "先 file_read 相关文件再断言。",
        "plan" => "补全 ## Plan 勾选或调整 findings。",
        "scope" => "只处理 in-scope findings。",
        _ => "避免重复探索，聚焦当前任务。",
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

/// Push L0 working-memory + symbol relations into the knowledge graph after each tool call.
async fn record_tool_live_update(
    tool_ctx: &Arc<ToolContext>,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    user_task: &Option<String>,
    tool_name: &str,
    tool_args: &str,
    tool_result: &str,
    is_error: bool,
) {
    let session_id = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| e.session_id())
        .unwrap_or_else(|| "default".to_string());
    let ctx = crate::knowledge::live_update::ToolExecutionContext {
        session_id,
        user_message: user_task.clone().unwrap_or_default(),
        tool_name: tool_name.to_string(),
        tool_args: tool_args.to_string(),
        tool_result: tool_result.chars().take(4000).collect(),
        is_error,
        project_root: tool_ctx.working_dir.to_string_lossy().to_string(),
    };
    if let Some(knowledge) = &tool_ctx.knowledge
        && let Ok(mut engine) = knowledge.try_write()
            && let Err(e) = engine.process_tool_execution(&ctx) {
                tracing::warn!("[LIVE_UPDATE] apply failed: {e}");
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
