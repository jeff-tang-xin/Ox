pub mod engine;
pub mod enforcer;
pub mod interjection;
pub mod interrupt;
pub mod intervention;
pub mod progress;
pub mod session;
pub mod task_canvas;
pub mod ui_event;
pub mod workflow;
pub mod context_offloader;
pub mod auto_reflect;  // 🆕 Auto-reflection for skill generation
pub mod skill_reflect_buffer;
pub mod context_injector;  // 🆕 Task anchoring + knowledge re-injection
pub mod execute_handoff;
pub mod exploration_snapshot; // Plan-step tool results for cross-step handoff
pub mod plan_tracker; // Execute-step plan progress
pub mod preflight;
pub mod turn_memory; // In-turn tool log + message compaction
pub mod memory_bridge; // Cross-turn durable memory injection
pub mod user_round; // Per-user-message round segmentation
pub mod workflow_guidance; // Mid-workflow user corrections without restart
pub mod workflow_session; // Park / resume persistent task session
pub mod workflow_phases; // 感知 → 思考 → 执行 phase state machine
pub mod requirement_clarification; // Intent 后需求澄清门禁
pub mod perception; // Structured findings from perceive phase
pub mod findings; // Canonical findings store (review → park → implement)
pub mod presentation; // Executive summary formatting for findings
pub mod workspace; // Single [WORKSPACE] LLM context block
pub mod completion; // Machine-verifiable completion receipt
pub mod workflow_command; // /fix /pause /confirm slash commands
pub mod tool_digest; // Semantic file_read digests
pub mod verifier; // Post-edit read-only verifier pass
pub mod git_undo; // Git checkout undo per finding
pub mod onboarding; // First-time project skill generation
pub mod error_recovery;    // 🆕 Build/test failure auto-fix
pub mod post_edit_verification; // AST feedback + language verify gate
pub mod tool_executor;     // 🆕 Tool detail display + error formatting
pub mod idle_narrative; // Cross-step idle prose detection + output discipline
pub mod gatekeeper; // 🆕 Simplified single-step model: one LLM loop + gate pipeline



pub use engine::StepDisplayInfo;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::knowledge::entity::Entity;
use crate::llm::{LlmProvider, LlmStreamEvent};
use crate::message::{Message, TokenUsage, ToolCall};
use crate::safety::injection;
use crate::safety::TrustManager;
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
        new_messages: Vec<Message>,
        usage: TokenUsage,
    },
    /// Error during agent turn.
    Error(String),
    /// Status update (e.g. "Thinking...", "Running tool...").
    Status(String),
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
    WorkflowParked {
        message: String,
    },
    /// Formatted plan ready for user review (rendered as Markdown).
    PlanReviewReady { markdown: String },
    /// Workflow paused — waiting for user confirmation or feedback.
    WorkflowAwaitingConfirmation {
        step_idx: usize,
        message: String,
    },
    /// Findings list after review park — user selects scope via /fix or UI.
    FindingsPanel {
        summary: String,
        rows: Vec<crate::agent::findings::FindingProgressRow>,
    },
    /// Awaiting user to confirm implementation scope (/confirm).
    ScopeConfirmPrompt {
        summary: String,
    },
    /// Workspace mode changed (review / parked / impl / discuss / paused).
    WorkspaceModeChanged {
        mode: String,
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
}

/// Persist in-turn tool log to workflow session (survives TurnDone → next spawn).
fn persist_turn_memory(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    turn_memory: &turn_memory::TurnMemory,
) {
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            engine.save_turn_memory(turn_memory);
        }
    }
}

/// Deliver a user interjection into the live message list (workflow-aware).
fn push_interjection_message(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<crate::agent::engine::WorkflowEngine>>>,
    messages: &mut Vec<Message>,
    text: &str,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) {
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if !engine.allows_midflight_interjection() {
                if crate::agent::workflow_session::looks_like_fix_continuation(text) {
                    engine.append_workflow_guidance(text);
                    let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                        "💬 User (Act 修复介入): {}",
                        text.trim().chars().take(120).collect::<String>()
                    )));
                    return;
                }
                tracing::info!("[WORKFLOW] Blocked mid-flight interjection in Act phase");
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    crate::agent::workflow_phases::act_interjection_blocked_message().to_string(),
                ));
                return;
            }
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
            &text[..text.len().min(100)]
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
            if engine.workflow_preserves_on_user_input() {
                engine.append_workflow_guidance(&sanitized);
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

    messages.push(Message::user(&formatted));
    let _ = ui_tx.send(AgentToUiEvent::Status(format!(
        "💬 User (workflow 介入): {}",
        sanitized_for_user.trim().chars().take(120).collect::<String>()
    )));
}

/// Run a complete agent turn: LLM -> tool_calls -> execute -> loop -> text.
///
/// Takes owned data so it can be spawned into a `tokio::spawn` task.
/// New messages produced during the turn are returned via `TurnDone`.
pub async fn run_agent_turn(
    provider: Arc<dyn LlmProvider>,
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
) {
    let tool_schemas = tool_registry.schemas();
    let max_iterations = agent_config.max_iterations;
    let mut tool_ctx = tool_ctx; // Allow reassignment on cd

    // Track new messages produced during this turn for returning to the caller.
    let mut new_messages: Vec<Message> = Vec::new();
    let mut total_usage = TokenUsage::default();

    const MAX_SAME_TOOL_CALLS: u32 = 5; // Maximum times the same tool can be called in one turn
    /// Hard cap per agent turn even when workflow is active (prevents 39+ iteration runaway).
    const MAX_ITERATIONS_PER_TURN: u32 = 35;
    const COMPACT_MESSAGES_AFTER_ITER: u32 = 10;
    const COMPACT_KEEP_TAIL: usize = 36;

    // 🎯 Anchor to the **current** user round (not the first message in session history)
    let user_task: Option<String> = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .and_then(|e| e.get_variable("_current_user_request"))
        .filter(|s| !s.trim().is_empty())
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
        if let Ok(engine) = wf.try_lock() {
            crate::agent::gatekeeper::reset_failures(&engine);
            if let Some(saved) = engine.load_turn_memory() {
                turn_memory.merge_from(saved);
            }
            let block = engine.user_round_memory_block();
            if !block.is_empty() {
                user_round::inject_user_round(&mut messages, &block);
            }
            let block = engine.durable_memory_block();
            if !block.is_empty() {
                memory_bridge::inject_durable_memory(&mut messages, &block);
            }
        }
    }

    let mut iteration = 0u32;
    let mut idle_streak = 0u32;
    let mut tools_used_this_turn: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    loop {
        // Check cancellation before each LLM call.
        if cancel_token.is_cancelled() {
            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
            break;
        }

        // When iteration limit is reached, ask user whether to continue.
        // SKIP this check when workflow is active (workflow has its own confirmation mechanism)
        let workflow_active = if let Some(ref engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;
            engine.is_workflow_active()
        } else {
            false
        };

        // Iteration cap applies always (workflow previously had no cap → 39+ iteration runaway).
        let iter_cap = if workflow_active {
            MAX_ITERATIONS_PER_TURN.max(max_iterations)
        } else {
            max_iterations
        };

        if iteration > 0 && iteration >= iter_cap {
            let _ = ui_tx.send(AgentToUiEvent::IterationLimitReached { iteration });

            // 用户确认超时时间（秒）
            const CONFIRM_TIMEOUT_SECS: u64 = 60;
            let should_continue = loop {
                tokio::select! {
                    ev = ui_rx.recv() => {
                        match ev {
                            Some(ui_event::UiToAgentEvent::ToolConfirmation { tool_call_id, decision })
                                if tool_call_id == "__iteration_limit__" =>
                            {
                                break matches!(
                                    decision,
                                    ui_event::ConfirmationDecision::Allow
                                        | ui_event::ConfirmationDecision::TrustAlways
                                );
                            }
                            Some(ui_event::UiToAgentEvent::Interjection(text)) => {
                                push_interjection_message(
                                    &workflow_engine,
                                    &mut messages,
                                    &text,
                                    &ui_tx,
                                );
                            }
                            _ => continue,
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        break false;
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(CONFIRM_TIMEOUT_SECS)) => {
                        let _ = ui_tx.send(AgentToUiEvent::Status(
                            "⏰ 确认超时 — 已停止本轮。按 Y 可手动继续。".to_string()
                        ));
                        break false;
                    }
                }
            };

            if !should_continue {
                break;
            }
            // User chose to continue — reset counter so we get another full batch.
            iteration = 0;
        }

        let _ = ui_tx.send(AgentToUiEvent::Status(if iteration == 0 {
            "🧠 Thinking...".to_string()
        } else {
            format!("🧠 Thinking... (iteration {})", iteration + 1)
        }));

        // Check for queued interjections before LLM call.
        while let Ok(ev) = ui_rx.try_recv() {
            if let ui_event::UiToAgentEvent::Interjection(text) = ev {
                push_interjection_message(
                    &workflow_engine,
                    &mut messages,
                    &text,
                    &ui_tx,
                );
            }
        }

        turn_memory.bump_iteration();
        persist_turn_memory(&workflow_engine, &turn_memory);

        // Compress bloated in-turn history before LLM call
        let compact_after = workflow_engine.as_ref().is_some_and(|wf| {
            wf.try_lock()
                .map(|e| {
                    e.is_task_step()
                })
                .unwrap_or(false)
        });
        let compact_threshold = if compact_after {
            6
        } else {
            COMPACT_MESSAGES_AFTER_ITER
        };
        if iteration >= compact_threshold && messages.len() > COMPACT_KEEP_TAIL + 6 {
            turn_memory::compact_turn_messages(&mut messages, COMPACT_KEEP_TAIL);
        }

        // Sync turn memory from full message scan (survives compaction)
        let include_writes = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.is_task_step())
            .unwrap_or(true);
        turn_memory.sync_from_messages(&messages, include_writes);

        // Workflow: collapse repeated idle narration (keeps LLM context lean)
        if workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .is_some_and(|e| e.is_workflow_active())
        {
            crate::agent::idle_narrative::collapse_redundant_idle(&mut messages);
        }

        // 🎯 Task anchoring + exploration progress + multi-layer memory re-injection
        context_injector::inject_context(&mut messages, &user_task, iteration, &tool_ctx, &workflow_engine);

        // In-turn tool log (always — not only workflow steps)
        turn_memory::strip_turn_memory(&mut messages);
        messages.push(Message::system(&turn_memory.format_injection(iteration)));

        // Refresh user-round + durable memory every iteration (last = strongest attention)
        if let Some(wf) = &workflow_engine {
            if let Ok(engine) = wf.try_lock() {
                let ur = engine.user_round_memory_block();
                user_round::strip_user_round(&mut messages);
                user_round::inject_user_round(&mut messages, &ur);
                if !crate::agent::workspace::uses_workspace_memory(&engine) {
                    let block = engine.durable_memory_block();
                    memory_bridge::strip_durable_memory(&mut messages);
                    memory_bridge::inject_durable_memory(&mut messages, &block);
                } else {
                    memory_bridge::strip_durable_memory(&mut messages);
                    let block = engine.durable_memory_block();
                    if !block.is_empty() {
                        memory_bridge::inject_durable_memory(&mut messages, &block);
                    }
                }
            }
        }

        // 🚨 Sanitize tool pairs before EVERY LLM call within the agent turn.
        // This prevents OpenAI API errors like "ToolResult references non-existent tool call"
        // when a tool_call was skipped or only partially executed.
        crate::context::sanitize_tool_pairs(&mut messages);

        // Single-step model: always show assistant output to the user.
        let pre_llm_step_idx = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.get_current_step_index())
            .unwrap_or(0);

        // Stream LLM response.
        let (llm_tx, mut llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

        let provider_clone = Arc::clone(&provider);
        let msgs = messages.clone();

        // Filter tool schemas based on current workflow step
        let workflow_blocks_planning = if let Some(ref engine_arc) = workflow_engine {
            engine_arc.lock().await.is_workflow_active()
        } else {
            false
        };

        let schemas: Vec<_> = if planning_mode && iteration == 0 && !workflow_blocks_planning {
            vec![] // Legacy planning mode: no tools in first iteration (not used during workflow)
        } else if let Some(ref engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;
            let allowed_tools = engine.get_allowed_tools();
            let step_idx = engine.get_current_step_index();

            let mut schemas: Vec<_> = if !engine.allows_tool_execution() {
                Vec::new()
            } else if allowed_tools.is_empty() {
                tool_schemas.clone()
            } else {
                tool_schemas
                    .iter()
                    .filter(|schema| allowed_tools.contains(&schema.name))
                    .cloned()
                    .collect()
            };

            let single_step = engine
                .current_workflow()
                .is_some_and(|w| w.id == crate::agent::workflow::DEFAULT_WORKFLOW_ID);

            // Legacy 4-step only: Intent (0) / Review (2) are JSON-only — no tools in API.
            // Single-step workflow also uses index 0 but must expose tools (prompt lists them).
            if !single_step && matches!(step_idx, 0 | 2) {
                schemas.clear();
            }
            // Legacy Plan (1) only
            if !single_step && step_idx == 1 {
                let has_detect = engine.has_exploration_tool("project_detect")
                    || tools_used_this_turn.contains("project_detect");
                if has_detect {
                    let before = schemas.len();
                    schemas.retain(|s| s.name != "project_detect");
                    if schemas.len() < before {
                        tracing::info!("[STEP1] Removed project_detect from schema (already used, iter {})", iteration);
                    }
                }
            }
            // Legacy Plan: JSON-only after exploration gate passes
            if !single_step && step_idx == 1 && engine.plan_exploration_satisfied() {
                tracing::info!("[STEP1] Exploration gate passed — JSON-only mode");
                schemas.clear();
            }
            schemas
        } else {
            tool_schemas.clone()
        };

        // 📝 LOG REQUEST CONTEXT: Log the complete context sent to LLM for debugging
        tracing::info!("\n{}", "=".repeat(80));
        tracing::info!("🤖 LLM REQUEST CONTEXT (Iteration {})", iteration + 1);
        tracing::info!("{}", "=".repeat(80));
        tracing::info!("Total messages: {}", msgs.len());
        
        // Show system prompt preview
        if let Some(first_msg) = msgs.first() {
            if let Message::System { content } = first_msg {
                let sys_prompt_len = content.chars().count();
                tracing::info!("📋 SYSTEM PROMPT LENGTH: {} characters", sys_prompt_len);
                let preview = if sys_prompt_len > 1000 {
                    format!("{}...[truncated]", content.chars().take(1000).collect::<String>())
                } else {
                    content.clone()
                };
                tracing::info!("📋 SYSTEM PROMPT PREVIEW:\n{}", preview.replace('\n', "\\n"));
            }
        }
        
        // Log each message with role and preview
        for (i, msg) in msgs.iter().enumerate() {
            let (role, content_preview) = match msg {
                Message::System { .. } => continue,
                Message::User { content } => {
                    ("USER", if content.chars().count() > 150 {
                        format!("{}...", content.chars().take(150).collect::<String>())
                    } else { content.clone() })
                }
                Message::Assistant { content, tool_calls, .. } => {
                    let tc_info = if !tool_calls.is_empty() {
                        format!(" [tool_calls: {}]", tool_calls.len())
                    } else {
                        String::new()
                    };
                    let preview = if content.chars().count() > 150 {
                        format!("{}...", content.chars().take(150).collect::<String>())
                    } else { content.clone() };
                    ("ASSISTANT", format!("{}{}", preview, tc_info))
                }
                Message::ToolResult { tool_call_id, content } => {
                    let preview = if content.chars().count() > 100 {
                        format!("{}...", content.chars().take(100).collect::<String>())
                    } else { content.clone() };
                    ("TOOL_RESULT", format!("[{}] {}", tool_call_id, preview))
                }
            };
            tracing::info!("  [{}] {}: {}", i, role, content_preview.replace('\n', "\\n"));
        }
        tracing::info!("Enabled tools: {}", schemas.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
        tracing::info!("{}", "=".repeat(80));

        let llm_opts = crate::llm::StreamOptions::default();
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

        while let Some(event) = tokio::select! {
            ev = llm_rx.recv() => ev,
            _ = cancel_token.cancelled() => {
                // Cancellation requested — stop receiving LLM events.
                tracing::warn!("[AGENT] ⚠️ Cancellation token triggered, stopping LLM stream");
                None
            }
        } {
            match event {
                LlmStreamEvent::TextDelta(text) => {
                    let _ = ui_tx.send(AgentToUiEvent::TextChunk(text.clone()));
                    full_text.push_str(&text);
                }
                LlmStreamEvent::ReasoningDelta(text) => {
                    reasoning_content.push_str(&text);
                    let _ = ui_tx.send(AgentToUiEvent::ReasoningChunk(text));
                }
                LlmStreamEvent::ToolCallStart { id, name } => {
                    // Don't show ToolStart in UI yet — the tool may be rejected
                    // by workflow validation later. Only show when actually executing.
                    tracing::debug!("[AGENT] LLM requested tool: {} (id={})", name, id);
                    current_tool_args.insert(id.clone(), String::new());
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: String::new(),
                    });
                }
                LlmStreamEvent::ToolCallArgumentsDelta { id, delta } => {
                    if let Some(args) = current_tool_args.get_mut(&id) {
                        args.push_str(&delta);
                    }
                    if let Some(tc) = tool_calls.iter_mut().find(|tc| tc.id == id) {
                        tc.arguments.push_str(&delta);
                    }
                }
                LlmStreamEvent::ToolCallEnd { .. } => {}
                LlmStreamEvent::Done { usage } => {
                    tracing::info!(
                        "[AGENT] ✅ LLM stream completed (prompt: {}, completion: {}, total: {})",
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        usage.total_tokens
                    );
                    total_usage.prompt_tokens += usage.prompt_tokens;
                    total_usage.completion_tokens += usage.completion_tokens;
                    total_usage.total_tokens += usage.total_tokens;
                    
                    // 📝 LOG RESPONSE SUMMARY
                    tracing::info!("\n{}", "-".repeat(80));
                    tracing::info!("📤 LLM RESPONSE SUMMARY");
                    tracing::info!("{}", "-".repeat(80));
                    if !full_text.is_empty() {
                        // 🚨 FIX: Use char-based truncation
                        let preview = if full_text.chars().count() > 300 {
                            format!("{}...", full_text.chars().take(300).collect::<String>())
                        } else {
                            full_text.clone()
                        };
                        tracing::info!("Text response: {}", preview.replace('\n', "\\n"));
                    }
                    if !tool_calls.is_empty() {
                        tracing::info!("Tool calls: {}", tool_calls.iter().map(|tc| {
                            format!("{}({})", tc.name, tc.id)
                        }).collect::<Vec<_>>().join(", "));
                        
                        // Log each tool call's arguments (truncated)
                        for tc in &tool_calls {
                            // 🚨 FIX: Use char-based truncation
                            let args_preview = if tc.arguments.chars().count() > 200 {
                                format!("{}...", tc.arguments.chars().take(200).collect::<String>())
                            } else {
                                tc.arguments.clone()
                            };
                            tracing::info!("  - {} [{}]: {}", tc.name, tc.id, args_preview.replace('\n', "\\n"));
                        }
                    } else {
                        tracing::info!("No tool calls");
                    }
                    tracing::info!("{}", "-".repeat(80));
                    
                    break;
                }
                LlmStreamEvent::Error(err) => {
                    // Log the error to file.
                    tracing::error!("LLM error: {}", err);
                    let _ = ui_tx.send(AgentToUiEvent::Error(err));
                    // Abort the stream task if still running, don't block on it.
                    stream_handle.abort();
                    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                        new_messages,
                        usage: total_usage,
                    });
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
                    content: full_text.clone(),
                    tool_calls: Vec::new(),
                    reasoning_content: if reasoning_content.is_empty() {
                        None
                    } else {
                        Some(reasoning_content.clone())
                    },
                };
                new_messages.push(msg.clone());
                messages.push(msg);
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "✅ 项目规范与业务指导 Skill 已创建".to_string(),
                ));
                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                    new_messages,
                    usage: total_usage,
                });
                return;
            } else {
                let missing = onboarding::missing_onboarding_files(&root).join("、");
                messages.push(Message::system(&format!(
                    "还不能 ## Done：还缺 {missing}。请分别 file_write 后再结束。"
                )));
                persist_turn_memory(&workflow_engine, &turn_memory);
                iteration += 1;
                continue;
            }
        }

        // If no tool calls, the turn is complete.
        if tool_calls.is_empty() {
            // Cross-step idle detection — break prose↔gate loops before stacking messages.
            if let Some(ref engine_arc) = workflow_engine {
                if let Ok(engine) = engine_arc.try_lock() {
                    if engine.is_workflow_active() && pre_llm_step_idx <= 3 {
                        let ctx = crate::agent::idle_narrative::IdleContext {
                            step_idx: pre_llm_step_idx,
                            engine: Some(&*engine),
                        };
                        if !crate::agent::idle_narrative::is_step_deliverable(&ctx, &full_text)
                            && crate::agent::idle_narrative::is_idle_narrative(&full_text)
                        {
                            match crate::agent::idle_narrative::handle_empty_response(
                                &ctx,
                                &full_text,
                                &mut idle_streak,
                                false,
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
                                    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                        new_messages,
                                        usage: total_usage,
                                    });
                                    return;
                                }
                                crate::agent::idle_narrative::IdleAction::Continue { directive } => {
                                    let msg = Message::Assistant {
                                        content: full_text.clone(),
                                        tool_calls: Vec::new(),
                                        reasoning_content: if reasoning_content.is_empty() {
                                            None
                                        } else {
                                            Some(reasoning_content.clone())
                                        },
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
                }
            }

            // Single-step model: always show the assistant's text to the user
            // (perception filter strips machine-only findings JSON when present).
            let content_for_session =
                execute_user_display(&workflow_engine, pre_llm_step_idx, &full_text);

            let msg = Message::Assistant {
                content: content_for_session.clone(),
                tool_calls: Vec::new(),
                reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content.clone()) },
            };
            let workflow_active = workflow_engine.as_ref().is_some_and(|wf| {
                wf.try_lock()
                    .map(|e| e.is_workflow_active())
                    .unwrap_or(false)
            });
            if crate::agent::engine::WorkflowEngine::looks_like_review_report(&content_for_session) {
                upsert_review_report_assistant(&mut messages, &msg);
                upsert_review_report_assistant(&mut new_messages, &msg);
            } else if workflow_active
                && crate::agent::idle_narrative::is_idle_narrative(&content_for_session)
            {
                crate::agent::idle_narrative::upsert_idle_assistant(&mut messages, &msg);
                crate::agent::idle_narrative::upsert_idle_assistant(&mut new_messages, &msg);
            } else {
                new_messages.push(msg.clone());
                messages.push(msg);
            }

            // ── Findings: capture review findings JSON / prose so ScopeGate can enforce them.
            if let Some(ref engine_arc) = workflow_engine {
                if let Ok(engine) = engine_arc.try_lock() {
                    if crate::agent::engine::WorkflowEngine::looks_like_review_report(&full_text)
                        || crate::agent::perception::extract_from_text(&full_text).is_some()
                    {
                        crate::agent::findings::ensure_from_review_output(&engine, &full_text);
                    }
                }
            }

            // ── ## Done → gatekeeper pipeline (single-step model) ──
            if crate::agent::engine::WorkflowEngine::text_signals_done(&full_text) {
                if let Some(ref engine_arc) = workflow_engine {
                    let mut engine = engine_arc.lock().await;
                    if engine.is_workflow_active() && !engine.is_workflow_complete() {
                        let had_code = turn_memory.had_code_changes();
                        match engine.run_done_gates(&full_text, had_code) {
                            crate::agent::gatekeeper::GateReport::Pass => {
                                engine.set_previous_output(&full_text);
                                if let Some(receipt) =
                                    crate::agent::completion::extract_from_text(&full_text)
                                {
                                    if let Some(mut store) =
                                        crate::agent::findings::load_or_migrate(&engine)
                                    {
                                        crate::agent::completion::apply_receipt(
                                            &mut store, &receipt,
                                        );
                                        crate::agent::findings::save(&engine, &store);
                                    }
                                }
                                let _ = engine.complete_workflow();
                                emit_workflow_completed(
                                    &ui_tx,
                                    user_task.as_ref(),
                                    &engine,
                                    &full_text,
                                );
                                drop(engine);
                                let _ = ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                    new_messages,
                                    usage: total_usage,
                                });
                                return;
                            }
                            crate::agent::gatekeeper::GateReport::Fail { gate, feedback } => {
                                messages.push(Message::system(&format!(
                                    "【门禁·{gate}】{feedback}"
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
                                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                    new_messages,
                                    usage: total_usage,
                                });
                                return;
                            }
                        }
                    }
                }
            }

            let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                new_messages,
                usage: total_usage,
            });
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
        let mut exceeded_loop_limit_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut temp_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut tool_loop_keys: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        let execute_step = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.is_task_step())
            .unwrap_or(false);

        // Execute: after a full review report, refuse another exploration pass (read-only phase only).
        if execute_step && !tool_calls.is_empty() {
            if let Some(ref wf) = workflow_engine {
                if let Ok(engine) = wf.try_lock() {
                    if engine.should_block_execute_reexplore(&tool_calls, &full_text) {
                        let msg = Message::Assistant {
                            content: execute_user_display(&workflow_engine, 3, &full_text),
                            tool_calls: Vec::new(),
                            reasoning_content: if reasoning_content.is_empty() {
                                None
                            } else {
                                Some(reasoning_content.clone())
                            },
                        };
                        crate::agent::idle_narrative::upsert_idle_assistant(&mut messages, &msg);
                        crate::agent::idle_narrative::upsert_idle_hint(
                            &mut messages,
                            "❌ 审查报告已输出。请直接输出 `## Done` 或 `## 完成` 并等待用户跟进；\
                             禁止再用 file_read / code_search 做「补充探索」。用户说「执行/去改」后再动手。",
                        );
                        persist_turn_memory(&workflow_engine, &turn_memory);
                        iteration += 1;
                        continue;
                    }
                }
            }
        }

        for tc in &tool_calls {
            let loop_key = tool_loop_key(&tc.name, &tc.arguments);
            tool_loop_keys.insert(tc.id.clone(), loop_key.clone());
            let count = temp_counts.entry(loop_key).or_insert(0);
            *count += 1;
            let impl_phase = workflow_engine.as_ref().is_some_and(|wf| {
                wf.try_lock()
                    .map(|e| {
                        e.is_task_step()
                            && crate::agent::workflow_session::is_implementation_phase(&e)
                    })
                    .unwrap_or(false)
            });
            let limit = match tc.name.as_str() {
                "file_list" => 2,
                "file_read" if impl_phase => 24,
                "file_read" if execute_step => 6,
                "file_read" => 2,
                "shell_exec" if impl_phase => {
                    let looks_read = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                        .ok()
                        .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))
                        .is_some_and(|cmd| {
                            crate::agent::engine::WorkflowEngine::shell_looks_like_file_read(&cmd)
                        });
                    if looks_read {
                        1
                    } else {
                        MAX_SAME_TOOL_CALLS
                    }
                }
                _ => MAX_SAME_TOOL_CALLS,
            };
            if *count > limit {
                exceeded_loop_limit_ids.insert(tc.id.clone());
            }
        }
        
        // Single-step model: always show the assistant's text to the user
        // (perception filter strips machine-only findings JSON when present).
        let display = execute_user_display(&workflow_engine, pre_llm_step_idx, &full_text);

        // Keep ALL tool_calls on the assistant message so every ToolResult has a matching id.
        // (Filtering caused orphaned ToolResults → API auto-fix → context amnesia.)
        let assistant_msg = Message::Assistant {
            content: display,
            tool_calls: tool_calls.clone(),
            reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content.clone()) },
        };
        new_messages.push(assistant_msg.clone());
        messages.push(assistant_msg);

        // 🧠 Record this turn as L0 WorkingMemory with the LLM's raw response
        let user_text = user_task.as_deref().unwrap_or("");
        let assistant_preview: String = full_text.chars().take(400).collect();
        let assistant_truncated = if assistant_preview.len() < full_text.len() { "..." } else { "" };
        let l0_content = format!(
            "User: {}\n\nAssistant: {}{}",
            user_text.chars().take(300).collect::<String>(),
            assistant_preview,
            assistant_truncated
        );
        {
            let knowledge = Arc::clone(&tool_ctx.knowledge);
            tokio::task::spawn(async move {
                if let Ok(mut engine) = knowledge.try_write() {
                    let _ = engine.record_turn("current", &l0_content, None, None, vec![], true);
                }
            });
        }

        // ── Context Offloader: created once and reused across all tools in this iteration ──
        let mut offloader = context_offloader::ContextOffloader::new(
            &tool_ctx.working_dir,
            &format!("session_{}", iteration),
        );

        // Execute each tool call.
        for tc in &tool_calls {
            // Check cancellation before each tool execution.
            if cancel_token.is_cancelled() {
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "Interrupted before tool execution.".to_string(),
                ));
                break;
            }

            // 🚨 Detect infinite loop: same tool called too many times
            // Note: We already calculated exceeded_loop_limit_ids above, so just check if this ID is in the set
            if exceeded_loop_limit_ids.contains(&tc.id) {
                let loop_key = tool_loop_keys.get(&tc.id).cloned().unwrap_or_else(|| tc.name.clone());
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
                    let partial_info = if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("<not specified>");
                        let has_search = args_val.get("search").is_some();
                        let has_replace = args_val.get("replace").is_some();
                        format!(
                            "\n\n📋 Partial arguments received:\n\
                             • path: {}\n\
                             • search: {}\n\
                             • replace: {}",
                            path,
                            if has_search { "✅ present (may be truncated)" } else { "❌ missing" },
                            if has_replace { "✅ present (may be truncated)" } else { "❌ missing" }
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

                // Validate tool call against current workflow step
                if let Err(e) = engine.validate_tool_call(&tc.name, &args_value) {
                    tracing::warn!("Workflow validation failed for tool '{}': {}", tc.name, e);

                    // Directive message: tell LLM what to do instead
                    let step_idx = engine.get_current_step_index();
                    let directive = match step_idx {
                        0 => "\n\n⚡ You are in Step 1 (Intent). Tools are BLOCKED here. \nOutput ONLY the JSON: {\"intent\": \"...\", \"complexity\": \"...\", \"files\": [...], \"topic\": \"...\"}",
                        1 => "\n\n⚡ You are in Step 2 (Plan). Only read/search tools are allowed. \nOutput ONLY the JSON: {\"plan\": [...], \"skills\": [...], \"key_files\": [...]}",
                        2 => "\n\n⚡ You are in Step 3 (Review). No tools allowed. \nOutput ONLY the JSON: {\"safe\": true|false, \"complete\": true|false, \"issues\": [...], \"warnings\": [...]}",
                        3 => "\n\n⚡ You are in Step 4 (Execute). Follow the plan and use file/shell tools as needed. \nWhen finished, output ## Done with a brief summary.",
                        _ => "\n\n💡 Please follow the current step requirements.",
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

                // Plan: block duplicate exploration; return cache so LLM stops looping
                let step_idx = engine.get_current_step_index();
                if step_idx == 1
                    && matches!(tc.name.as_str(), "file_list" | "file_read")
                {
                    let path = args_value
                        .get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or(".");
                    if engine.is_path_explored(tc.name.as_str(), path) {
                        let cached = engine
                            .lookup_exploration_cache(
                                &tool_ctx.working_dir,
                                tc.name.as_str(),
                                path,
                            )
                            .unwrap_or_else(|| {
                                format!(
                                    "✅ 已探索过 `{path}`。勿重复 {}。见 [STEP_MEMORY] / [TURN_MEMORY]。\n{}",
                                    tc.name,
                                    engine.plan_exploration_hint()
                                )
                            });
                        tracing::info!(
                            "[WORKFLOW] Duplicate {} on {} — returning cache (step 1)",
                            tc.name,
                            path
                        );
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
                }
                if (step_idx == 0 || step_idx == 3) && tc.name == "file_list" {
                    let path = args_value
                        .get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or(".");
                    if engine.is_path_explored("file_list", path) {
                        let cached = engine
                            .lookup_exploration_cache(
                                &tool_ctx.working_dir,
                                "file_list",
                                path,
                            )
                            .unwrap_or_else(|| {
                                format!("✅ 已列出过 `{path}`。执行阶段勿重复 file_list，直接 file_read 或修改。")
                            });
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
                }
                if (step_idx == 0 || step_idx == 3)
                    && matches!(
                        tc.name.as_str(),
                        "file_read" | "code_search" | "find_symbol" | "file_search"
                    )
                    && !crate::agent::workflow_session::is_implementation_phase(&engine)
                {
                    let cached = engine
                        .lookup_execute_exploration_cache(
                            &tool_ctx.working_dir,
                            &tc.name,
                            &tc.arguments,
                        )
                        .or_else(|| {
                            if tc.name != "file_read" {
                                return None;
                            }
                            let path = args_value.get("path").and_then(|p| p.as_str())?;
                            if !engine.is_path_explored("file_read", path)
                                && !engine.has_file_read_snapshot(path)
                            {
                                return None;
                            }
                            Some(format!(
                                "✅ `{path}` 已在计划阶段探索。请使用【探索快照】/ 交接包写审查报告，勿重复 file_read。"
                            ))
                        });
                    if let Some(cached) = cached {
                        tracing::info!(
                            "[WORKFLOW] Execute duplicate {} — returning snapshot cache",
                            tc.name
                        );
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
                    let tool_names: Vec<String> = tool_registry.names().iter().map(|s| s.to_string()).collect();
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
                wf.try_lock().map_or(false, |e| e.is_workflow_active() && e.get_current_step_index() >= 3)
            });

            if !skip_plan_rules {
                if let Err(violation_msg) = crate::agent::enforcer::RuleEnforcer::validate(
                    &tool_ctx.config.enforcement_rules,
                    &tc,
                    &messages,
                ) {
                    tracing::warn!("🚫 Rule Enforcer blocked tool '{}': {}", tc.name, violation_msg);

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
            }

            let should_confirm = if path_outside {
                true // Path outside workdir always requires confirmation.
            } else {
                match safety_level {
                    SafetyLevel::Safe => false,
                    SafetyLevel::RequiresConfirmation | SafetyLevel::Dangerous => {
                        let tm = trust_manager.lock().unwrap();
                        !tm.can_skip_confirmation(&tc.name, safety_level)
                    }
                }
            };

            // Blacklist override: even if the tool is trusted, blacklisted
            // commands within shell_exec still require confirmation.
            let mut blacklist_warning: Option<String> = None;
            if !should_confirm && tc.name == "shell_exec" {
                if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    if let Some(cmd) = args_val.get("command").and_then(|v| v.as_str()) {
                        let tm = trust_manager.lock().unwrap();
                        if let Some(pattern) = tm.is_command_blacklisted(cmd) {
                            blacklist_warning = Some(format!("BLOCKED COMMAND (matches blacklist pattern: \"{}\")", pattern));
                            drop(tm);
                            // Force confirmation even though tool is trusted.
                        }
                    }
                }
            }
            let should_confirm = should_confirm || blacklist_warning.is_some();

            if should_confirm {
                tracing::info!("[AGENT] Tool {} requires confirmation", tc.name);
                // Build args_summary (truncated, sanitized).
                let args_summary = if tc.arguments.len() > 200 {
                    let end = tc
                        .arguments
                        .char_indices()
                        .take_while(|(i, _)| *i < 200)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(0);
                    format!("{}...(truncated)", tc.arguments.get(..end).unwrap_or(&tc.arguments))
                } else {
                    tc.arguments.clone()
                };

                // Check for high-risk command (shell_exec only).
                let high_risk_warning = if tc.name == "shell_exec" {
                    // Try to extract command from args JSON.
                    if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        if let Some(cmd) = args_val.get("command").and_then(|v| v.as_str()) {
                            let mut warning = None;
                            if crate::safety::is_high_risk_command(cmd) {
                                warning = Some("HIGH RISK COMMAND".to_string());
                            }
                            // Merge blacklist warning if present.
                            if let Some(ref bw) = blacklist_warning {
                                warning = Some(match warning {
                                    Some(mut w) => { w.push_str(" + "); w.push_str(bw); w }
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

                // Send confirmation request to UI.
                let _ = ui_tx.send(AgentToUiEvent::ToolConfirmationRequest {
                    tool_call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    args_summary,
                    safety_level,
                    high_risk_warning,
                });

                // Wait for user response.
                let decision = loop {
                    tokio::select! {
                        ev = ui_rx.recv() => {
                            match ev {
                                Some(ui_event::UiToAgentEvent::ToolConfirmation { tool_call_id, decision })
                                    if tool_call_id == tc.id => {
                                    break decision;
                                }
                                Some(ui_event::UiToAgentEvent::Interjection(text)) => {
                                    push_interjection_message(
                                        &workflow_engine,
                                        &mut messages,
                                        &text,
                                        &ui_tx,
                                    );
                                }
                                _ => continue,
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            // Cancelled while waiting for confirmation.
                            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
                            // Return early with what we have.
                            let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                new_messages,
                                usage: total_usage,
                            });
                            return;
                        }
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
                        let mut tm = trust_manager.lock().unwrap();
                        tm.trust_all();
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
                                let preview = if tc.arguments.chars().count() > 100 {
                                    tc.arguments.chars().take(100).collect::<String>()
                                } else {
                                    tc.arguments.clone()
                                };
                                preview
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
                if let ui_event::UiToAgentEvent::Interjection(text) = ev {
                    push_interjection_message(
                        &workflow_engine,
                        &mut messages,
                        &text,
                        &ui_tx,
                    );
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
            let tool_ctx_with_progress = Arc::new(crate::tools::ToolContext::with_progress_callback(
                tool_ctx.runtime.clone(),
                tool_ctx.working_dir.clone(),
                tool_ctx.config.clone(),
                Arc::clone(&tool_ctx.knowledge),
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
                tracing::warn!("[AGENT] Tool {} failed, retrying once: {}", tc.name, result.content);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                result = tool.execute(args.clone(), &tool_ctx_with_progress).await;
            }
            tracing::info!("[AGENT] Tool execution completed: {}, is_error: {}", tc.name, result.is_error);

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
                    Arc::clone(&tool_ctx.knowledge),
                ));
                let _ = ui_tx.send(AgentToUiEvent::WorkingDirChanged(new_dir));
            }

            // 🛡️ Scan web_fetch and file_read results for prompt injection before
            // they reach the LLM context. Untrusted external content may contain
            // injection attacks like "ignore previous instructions".
            let sanitized_content = if matches!(tc.name.as_str(), "web_fetch" | "file_read") && !result.is_error {
                let injection_result = injection::detect(&result.content);
                if injection_result.has_injection {
                    let categories: Vec<String> = injection_result.matches.iter()
                        .map(|m| format!("{:?}", m.category))
                        .collect();
                    tracing::warn!(
                        "🛡️ Prompt injection detected in {} output: categories={:?}",
                        tc.name, categories
                    );
                    injection::sanitize(&result.content)
                } else {
                    result.content.clone()
                }
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
                let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                    "📄 Result offloaded to: {}",
                    offloaded.ref_path.as_ref().unwrap().display()
                )));
            }

            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: offloaded.to_context_message(),
                is_error: result.is_error,
            });

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
                    let offset = args
                        .get("offset")
                        .and_then(|o| o.as_u64())
                        .unwrap_or(0) as u32;
                    if let Some(ref engine_arc) = workflow_engine {
                        if let Ok(engine) = engine_arc.try_lock() {
                            if engine.is_task_step()
                                && !engine.is_perceive_execute()
                            {
                                crate::agent::tool_digest::record_read(
                                    &engine,
                                    path,
                                    &sanitized_content,
                                    offset,
                                    None,
                                );
                                if let Some(digest) =
                                    crate::agent::tool_digest::get_digest(&engine, path)
                                {
                                    result_content =
                                        crate::agent::tool_digest::format_tool_result_for_history(
                                            path,
                                            &sanitized_content,
                                            &digest,
                                        );
                                }
                            }
                        }
                    }
                }
            }

            // Snapshot tool results for Plan / Execute step iteration memory
            if !result.is_error {
                if let Some(ref engine_arc) = workflow_engine {
                    if let Ok(engine) = engine_arc.try_lock() {
                        let step = engine.get_current_step_index();
                        if crate::agent::exploration_snapshot::should_snapshot_for_step(
                            step,
                            &tc.name,
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
                }
            }

            turn_memory.record_tool_with_result(
                &tc.name,
                &tc.arguments,
                !result.is_error,
                Some(&result_content),
            );
            persist_turn_memory(&workflow_engine, &turn_memory);

            if tc.name == "shell_exec" {
                if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    if let Some(cmd) = args.get("command").and_then(|c| c.as_str()) {
                        let succeeded = post_edit_verification::shell_result_success(&sanitized_content);
                        if let Some(ref engine_arc) = workflow_engine {
                            if let Ok(engine) = engine_arc.try_lock() {
                                post_edit_verification::note_shell_verify_result(
                                    &engine, cmd, succeeded,
                                );
                                if succeeded {
                                    if let Some(idx) = engine.get_plan_tracker().and_then(|t| {
                                        t.steps
                                            .iter()
                                            .find(|s| {
                                                !s.verify.is_empty()
                                                    && s.awaiting_verify
                                            })
                                            .map(|s| s.index)
                                    }) {
                                        crate::agent::verifier::after_verify_pass(&engine, idx);
                                    }
                                }
                            }
                        }
                    }
                }
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
                    serde_json::from_str::<serde_json::Value>(&tc.arguments).ok()
                        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
                        .map(|p| format!(" → {}", p))
                        .unwrap_or_default()
                } else { String::new() };
                let done_label = if matches!(tool_name.as_str(), "file_write" | "edit_file" | "delete_range") {
                    "工具执行成功（清单是否勾选见下方进度）"
                } else {
                    "已完成"
                };
                messages.push(Message::system(&format!(
                    "📋 ✅ {tool_name}{file_info} — {done_label}",
                    tool_name = tool_name, file_info = file_info, done_label = done_label
                )));
                tools_used_this_turn.insert(tool_name.clone());

                // Track explored paths during Plan only (Execute may re-read files)
                if matches!(tool_name.as_str(), "file_list" | "file_read") {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                        if let Some(ref engine_arc) = workflow_engine {
                            if let Ok(engine) = engine_arc.try_lock() {
                                if engine.get_current_step_index() == 1 {
                                    engine.record_explored_path(&tool_name, path);
                                } else if engine.is_task_step()
                                    && tool_name == "file_list"
                                {
                                    engine.record_explored_path(&tool_name, path);
                                }
                            }
                        }
                    }
                }

                // Execute: update plan tracker for completing tools
                if let Some(ref engine_arc) = workflow_engine {
                    if let Ok(engine) = engine_arc.try_lock() {
                        if engine.is_task_step() {
                            if tool_name == "file_read" {
                                if let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                {
                                    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                                        engine.record_impl_file_read(path, &tc.arguments);
                                        if let Some(nudge) = engine.impl_edit_nudge_after_read(
                                            path,
                                            &result_content,
                                        ) {
                                            messages.push(Message::system(&nudge));
                                        }
                                    }
                                }
                            }
                            let (plan_changed, plan_hint) = engine.record_execute_tool_success(
                                &tool_name,
                                &tc.arguments,
                                &result_content,
                            );
                            if let Some(hint) = plan_hint {
                                messages.push(Message::system(&hint));
                            }
                            if plan_changed {
                                if let Some(msg) =
                                    engine.plan_progress_message_after_tool(&tool_name)
                                {
                                    messages.push(Message::system(&msg));
                                }
                            }
                            if matches!(
                                tool_name.as_str(),
                                "edit_file" | "file_write" | "delete_range"
                            ) && crate::agent::workflow_session::is_implementation_phase(&engine)
                            {
                                if let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                {
                                    if let Some(path) = args.get("path").and_then(|p| p.as_str())
                                    {
                                        let idx = engine
                                            .get_plan_tracker()
                                            .and_then(|t| {
                                                t.current_step().map(|s| s.index)
                                            })
                                            .unwrap_or(1);
                                        if let Some(note) =
                                            crate::agent::verifier::after_edit_note(
                                                &engine,
                                                idx,
                                                path,
                                                &result_content,
                                            )
                                        {
                                            messages.push(Message::system(&note));
                                        }
                                    }
                                }
                            }
                        }
                        if matches!(tool_name.as_str(), "file_write" | "edit_file" | "delete_range")
                        {
                            if let Ok(args) =
                                serde_json::from_str::<serde_json::Value>(&tc.arguments)
                            {
                                if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                                    if let Some(verify) = engine.verify_hint_for_path(path) {
                                        messages.push(Message::system(&format!(
                                            "📋 计划验证: `{verify}` — 请用 shell_exec 执行（需用户确认），验证通过后再继续下一项。"
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 📖 Verify-after-edit: prompt LLM to verify changes
            if matches!(tc.name.as_str(), "edit_file" | "delete_range" | "file_write") && !result.is_error {
                let is_skill = tc.arguments.contains(".ox/skills/");
                let onboarding_skill = workflow_engine.is_none()
                    && onboarding::is_onboarding_turn(&messages)
                    && is_skill;

                // Execute step skill creation: tell LLM to output ## Done
                let is_execute_step = workflow_engine.as_ref().map_or(false, |wf| {
                    wf.try_lock().map_or(false, |e| e.is_task_step())
                });

                if is_execute_step && is_skill {
                    messages.push(Message::system(
                        "✅ 文件已写入。如果所有需要的文件都已完成，输出 `## Done` 结束。"
                    ));
                } else if onboarding_skill {
                    let root = tool_ctx
                        .runtime
                        .project_root
                        .clone()
                        .unwrap_or_else(|| tool_ctx.working_dir.clone());
                    if onboarding::onboarding_files_complete(&root) {
                        messages.push(Message::system(
                            "✅ 两个 Skill 都已写入（项目规范 + 业务指导）。输出 `## Done` 结束，不要再改文件。"
                        ));
                    } else {
                        let missing = onboarding::missing_onboarding_files(&root).join("、");
                        messages.push(Message::system(&format!(
                            "✅ 已写入一个 Skill。还缺：{missing}。请继续 file_write 缺失文件。"
                        )));
                    }
                }
            } // verify-after-edit
        } // end for tc

        // Plan step: remind not to repeat immutable exploration
        if pre_llm_step_idx == 1 && tools_used_this_turn.contains("project_detect") {
            messages.push(Message::system(
                "⚡ project_detect 已调用过。不要重复 file_list 相同目录 — 换子目录或 file_read 具体文件。"
            ));
        }

        // 🗺️ Inject task canvas if any results were offloaded
        if let Some(canvas_ctx) = offloader.get_canvas_context() {
            messages.push(Message::system(&canvas_ctx));
        }

        // 🚨 Done reminder + AST recovery + verify hints
        if !tool_calls.is_empty() {
            let has_write = tool_calls.iter().any(|tc| {
                matches!(tc.name.as_str(), "file_write" | "edit_file" | "delete_range")
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
                if let Some(ref engine_arc) = workflow_engine {
                    if let Ok(engine) = engine_arc.try_lock() {
                        post_edit_verification::track_edits_and_verify_plan(
                            &engine,
                            &project_root,
                            &tool_calls,
                            &new_messages,
                            true,
                        );
                        if !has_ast {
                            if let Some(hint) =
                                post_edit_verification::verify_hint_message(&engine)
                            {
                                messages.push(Message::system(&hint));
                            }
                        }
                    }
                }
            }

            if has_write
                && !onboarding::is_onboarding_turn(&messages)
                && !has_ast
            {
                let verify_blocking = workflow_engine.as_ref().and_then(|wf| {
                    wf.try_lock().ok().and_then(|e| {
                        post_edit_verification::check_execute_done_gate(&e)
                    })
                });
                let ast_pending = workflow_engine.as_ref().and_then(|wf| {
                    wf.try_lock()
                        .ok()
                        .and_then(|e| e.get_variable("_ast_pending"))
                        .filter(|s| !s.is_empty())
                });
                if verify_blocking.is_none() && ast_pending.is_none() {
                    messages.push(Message::system(
                        "Files were modified. Run project verify if not done yet, then output ## Done with what changed and verify result. 3 lines max."
                    ));
                }
            }

            // 🔄 Auto-fix: if build/test failed, inject error for self-repair
            error_recovery::check_and_recover(&mut messages, &new_messages, &tool_calls);
        }

        // ── Workflow Step Advancement Logic (after tool execution) ──
        // Check if we should advance to the next workflow step
        // This handles cases where tools were executed in this iteration
        if let Some(ref engine_arc) = workflow_engine {
            let mut engine = engine_arc.lock().await;

            // Check completion signals
            let ai_signaled_complete = full_text.contains("[STEP_COMPLETE]");
            let phase_complete = full_text.contains("✅ Phase") &&
                (full_text.contains("Complete") || full_text.contains("complete"));

            let should_advance = ai_signaled_complete || phase_complete;
            
            if should_advance && !engine.is_workflow_complete() {
                tracing::info!(
                    "Advancing workflow step after tool execution (AI signaled: {}, Phase complete: {})",
                    ai_signaled_complete,
                    phase_complete
                );

                // Check if CURRENT step requires confirmation BEFORE advancing
                let current_step_requires_confirmation = engine.requires_user_confirmation();
                
                if current_step_requires_confirmation {
                    tracing::info!("Current step requires user confirmation - setting flag and blocking next LLM call");
                    
                    engine.set_confirmation_flag();
                    
                    if let Some(step_name) = get_current_step_name(&engine) {
                        let status_msg = format!(
                            "✅ {} completed. ⏸️ Waiting for your confirmation (/Y, /N, /O)",
                            step_name
                        );
                        let _ = ui_tx.send(AgentToUiEvent::Status(status_msg));
                    }
                    
                    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                        new_messages,
                        usage: total_usage,
                    });
                    return;
                }

                // No confirmation needed, advance normally
                match engine.advance_step() {
                    Ok(has_next_step) => {
                        if has_next_step {
                            let needs_confirmation = engine.requires_user_confirmation();

                            if needs_confirmation {
                                engine.set_confirmation_flag();
                                tracing::info!("New step requires user confirmation - setting flag");
                            }

                            if let Some(step_name) = get_current_step_name(&engine) {
                                let status_msg = if needs_confirmation {
                                    format!(
                                        "✅ {} completed. ⏸️ Waiting for your confirmation (/Y, /N, /O)",
                                        step_name
                                    )
                                } else {
                                    format!("✅ Step completed. Moving to: {}", step_name)
                                };

                                let _ = ui_tx.send(AgentToUiEvent::Status(status_msg));
                            }
                        } else {
                            let _ = ui_tx.send(AgentToUiEvent::Status(
                                "🎉 Workflow completed!".to_string(),
                            ));
                            emit_workflow_completed(
                                &ui_tx,
                                user_task.as_ref(),
                                &engine,
                                &full_text,
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to advance workflow step: {}", e);
                    }
                }
            }
        }

        // Clean up old offloaded refs, keeping at most the 50 most recent ones.
        if let Err(e) = offloader.cleanup_old_refs(50) {
            tracing::warn!("Failed to clean up old refs: {}", e);
        }

        // Loop back to call LLM again with tool results.
        persist_turn_memory(&workflow_engine, &turn_memory);
        iteration += 1;
        if !tool_calls.is_empty() {
            idle_streak = 0;
        }
    }

    persist_turn_memory(&workflow_engine, &turn_memory);
    // Loop exited via break (cancellation or user declined to continue).
    // Send TurnDone so the UI can persist collected messages.
    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
        new_messages,
        usage: total_usage,
    });
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
        (trimmed.matches('"').count() % 2 != 0) ;

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
    {
        if prev_tc.is_empty()
            && crate::agent::engine::WorkflowEngine::looks_like_review_report(prev)
        {
            messages.pop();
        }
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

/// Helper function to get current step name from workflow engine.
fn get_current_step_name(engine: &crate::agent::engine::WorkflowEngine) -> Option<String> {
    engine.current_step().map(|step| step.name.clone())
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

/// Dedup key for same-tool loop detection (file_read includes offset/limit).
pub fn tool_loop_key(name: &str, arguments: &str) -> String {
    match name {
        "file_list" => {
            let path = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
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
        other => other.to_string(),
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
