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
pub mod exploration_snapshot; // Plan-step tool results for cross-step handoff
pub mod plan_tracker; // Execute-step plan progress
pub mod turn_memory; // In-turn tool log + message compaction
pub mod memory_bridge; // Cross-turn durable memory injection
pub mod user_round; // Per-user-message round segmentation
pub mod onboarding; // First-time project skill generation
pub mod error_recovery;    // 🆕 Build/test failure auto-fix
pub mod tool_executor;     // 🆕 Tool detail display + error formatting



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
    /// Formatted plan ready for user review (rendered as Markdown).
    PlanReviewReady { markdown: String },
    /// Workflow paused — waiting for user confirmation or feedback.
    WorkflowAwaitingConfirmation {
        step_idx: usize,
        message: String,
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
                                messages.push(Message::user(&text));
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
                // 🛡️ Scan interjection for prompt injection patterns
                if injection::is_suspicious(&text) {
                    let result = injection::detect(&text);
                    let categories: Vec<String> = result.matches.iter()
                        .map(|m| format!("{:?}", m.category))
                        .collect();
                    tracing::warn!(
                        "🛡️ Prompt injection detected in interjection: categories={:?}, text={:?}",
                        categories, &text[..text.len().min(100)]
                    );
                    let sanitized = injection::sanitize(&text);
                    messages.push(Message::system(
                        "⚠️ The following user input was sanitized for potential prompt injection:\n"
                    ));
                    messages.push(Message::user(&sanitized));
                } else {
                    messages.push(Message::user(&text));
                }
                let _ = ui_tx.send(AgentToUiEvent::Status(format!("💬 User: {}", text.trim())));
            }
        }

        // ── STRONG CONFIRMATION CHECK: Block LLM call if waiting for user confirmation ──
        if let Some(ref engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;

            // Check if we're waiting for user confirmation
            if engine.is_current_step_waiting_confirmation() {
                tracing::info!(
                    "Workflow engine is waiting for user confirmation - blocking LLM call"
                );

                // Send status to UI
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    "⏸️ 等待确认 — 输入 ok/继续/确认 执行，或输入修改意见".to_string(),
                ));

                // Return early without calling LLM
                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                    new_messages,
                    usage: total_usage,
                });
                return;
            }
        }

        turn_memory.bump_iteration();
        persist_turn_memory(&workflow_engine, &turn_memory);

        // Compress bloated in-turn history before LLM call
        if iteration >= COMPACT_MESSAGES_AFTER_ITER && messages.len() > COMPACT_KEEP_TAIL + 6 {
            turn_memory::compact_turn_messages(&mut messages, COMPACT_KEEP_TAIL);
        }

        // Sync turn memory from full message scan (survives compaction)
        let include_writes = workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.get_current_step_index() == 3)
            .unwrap_or(true);
        turn_memory.sync_from_messages(&messages, include_writes);

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
                let block = engine.durable_memory_block();
                memory_bridge::strip_durable_memory(&mut messages);
                memory_bridge::inject_durable_memory(&mut messages, &block);
            }
        }

        // 🚨 Sanitize tool pairs before EVERY LLM call within the agent turn.
        // This prevents OpenAI API errors like "ToolResult references non-existent tool call"
        // when a tool_call was skipped or only partially executed.
        crate::context::sanitize_tool_pairs(&mut messages);

        // ── Determine if current step is "internal" BEFORE LLM call ──
        // Steps 0,1,2 (Intent, Plan, Review): suppress raw JSON; Step 3 (Execute): show full output
        let (is_internal_step, pre_llm_step_idx) = if let Some(ref engine_arc) = workflow_engine {
            if let Ok(engine) = engine_arc.try_lock() {
                let idx = engine.get_current_step_index();
                let chat_reply = engine.is_chat_reply_pending();
                ((!chat_reply && idx != 3), idx)
            } else { (false, 5) }
        } else { (false, 5) };

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

            // Intent / Review: never expose tools in schema
            if matches!(step_idx, 0 | 2) {
                schemas.clear();
            }
            // Plan: remove project_detect after first use
            if step_idx == 1 {
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
            // Plan: JSON-only after exploration gate passes
            if step_idx == 1 && engine.plan_exploration_satisfied() {
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
        let cancel_clone = cancel_token.clone();
        let llm_tx_err = llm_tx.clone();
        let mut stream_handle = tokio::spawn(async move {
            tokio::select! {
                result = provider_clone.stream_chat(&msgs, &schemas, llm_tx) => {
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
                    // Suppress raw JSON output for internal workflow steps
                    if !is_internal_step {
                        let _ = ui_tx.send(AgentToUiEvent::TextChunk(text.clone()));
                    }
                    full_text.push_str(&text);
                }
                LlmStreamEvent::ReasoningDelta(text) => {
                    // DeepSeek reasoning_content (thinking mode) — accumulate for round-trip
                    reasoning_content.push_str(&text);
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

        // Execute: ## Done ends the turn immediately — do not run more tools or LLM iterations.
        if pre_llm_step_idx == 3 && crate::agent::engine::WorkflowEngine::text_signals_done(&full_text) {
            if let Some(ref engine_arc) = workflow_engine {
                let mut engine = engine_arc.lock().await;
                match engine.try_complete_execute_on_done(&full_text) {
                    Ok(true) => {
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
                        engine.set_previous_output(&full_text);
                        emit_workflow_completed(
                            &ui_tx,
                            user_task.as_ref(),
                            &engine,
                            &full_text,
                        );
                        let _ = ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                        let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                            new_messages,
                            usage: total_usage,
                        });
                        return;
                    }
                    Err(gate_msg) => {
                        let blocks = engine.bump_done_gate_block();
                        if blocks >= 2 {
                            tracing::warn!(
                                "[WORKFLOW] ## Done blocked {blocks} times — auto-completing execute"
                            );
                            engine.mark_plan_all_done();
                        }
                        match engine.try_complete_execute_on_done(&full_text) {
                            Ok(true) => {
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
                                engine.set_previous_output(&full_text);
                                emit_workflow_completed(
                                    &ui_tx,
                                    user_task.as_ref(),
                                    &engine,
                                    &full_text,
                                );
                                let _ = ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                    new_messages,
                                    usage: total_usage,
                                });
                                return;
                            }
                            _ => {
                                messages.push(Message::system(&gate_msg));
                                persist_turn_memory(&workflow_engine, &turn_memory);
                                iteration += 1;
                                continue;
                            }
                        }
                    }
                    Ok(false) => {}
                }
            }
        }

        // If no tool calls, the turn is complete.
        if tool_calls.is_empty() {
            let step_output_text = workflow_engine
                .as_ref()
                .and_then(|wf| wf.try_lock().ok())
                .map(|engine| {
                    if pre_llm_step_idx == 0 {
                        let user = engine
                            .get_variable("_current_user_request")
                            .unwrap_or_default();
                        crate::agent::engine::WorkflowEngine::correct_intent_json_for_user(&user, &full_text)
                    } else {
                        full_text.clone()
                    }
                })
                .unwrap_or_else(|| full_text.clone());

            // Format internal step outputs for session storage (same as tool-calls path)
            let content_for_session = if is_internal_step {
                let step_status = workflow_engine.as_ref().and_then(|wf| {
                    wf.try_lock().ok().and_then(|e| {
                        e.current_step().map(|s| {
                            if s.display_status.is_empty() {
                                s.name.clone()
                            } else {
                                s.display_status.clone()
                            }
                        })
                    })
                }).unwrap_or_else(|| String::new());
                format_step_output(pre_llm_step_idx, &step_output_text, &step_status)
            } else {
                step_output_text.clone()
            };

            let msg = Message::Assistant {
                content: content_for_session,
                tool_calls: Vec::new(),
                reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content.clone()) },
            };
            if is_internal_step {
                upsert_workflow_step_assistant(&mut messages, &msg);
                upsert_workflow_step_assistant(&mut new_messages, &msg);
            } else {
                new_messages.push(msg.clone());
                messages.push(msg);
            }

            // ── Workflow Step Advancement Logic (before returning) ──
            // Check if we should advance to the next workflow step
            // ── Workflow auto-advance ──
            if let Some(ref engine_arc) = workflow_engine {
                let mut engine = engine_arc.lock().await;

                // Chat reply turn: show natural language to user and end
                if engine.is_chat_reply_pending() && !is_internal_step {
                    engine.clear_chat_reply_pending();
                    engine.set_variable("_chat_reply", full_text.clone());
                    engine.set_previous_output(&full_text);
                    emit_workflow_completed(
                        &ui_tx,
                        user_task.as_ref(),
                        &engine,
                        &full_text,
                    );
                    let _ = ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                        new_messages,
                        usage: total_usage,
                    });
                    return;
                }

                if pre_llm_step_idx == 1 {
                    if let Some(draft) = extract_plan_draft_section(&full_text) {
                        engine.set_variable("_plan_draft", draft);
                    }
                }

                // If LLM called tools in a no-tool step, those tools were rejected.
                // Still check if the text contains valid JSON — LLM may have finished correctly.
                let had_successful_tool_calls = !tool_calls.is_empty() && new_messages.iter().any(|msg| {
                    matches!(msg, Message::ToolResult { content, .. } if !content.contains("❌") && !content.contains("not allowed"))
                });
                let (advance_result, validation_error) = engine.advance_on_output(&step_output_text, had_successful_tool_calls);

                // If validation failed, inject error so LLM can retry
                if let Some(ref err) = validation_error {
                    messages.push(Message::system(err));
                }

                // Review failed → rollback to Plan (第二步) and end turn so UI can auto-spawn Plan
                if pre_llm_step_idx == 2 {
                    if let Some(1) = advance_result {
                        let feedback = validation_error.clone().unwrap_or_else(|| full_text.clone());
                        if let Err(e) = engine.rollback_review_to_plan(&full_text, &feedback) {
                            tracing::warn!("Review rollback failed: {e}");
                        } else {
                            let _ = ui_tx.send(AgentToUiEvent::Status(
                                "⚠️ 审阅未通过 — 回到规划修正计划…".to_string(),
                            ));
                            engine.clear_turn_memory();
                            turn_memory = turn_memory::TurnMemory::new(
                                user_task.as_deref().unwrap_or(""),
                            );
                            persist_turn_memory(&workflow_engine, &turn_memory);
                            if let Some(new_prompt) = engine.get_step_system_prompt() {
                                let prompt_text =
                                    format!("【当前步骤 — 审阅回退】\n{new_prompt}");
                                let step_msg = Message::system(&prompt_text);
                                new_messages.push(step_msg.clone());
                                messages.push(step_msg);
                            }
                            let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                new_messages,
                                usage: total_usage,
                            });
                            return;
                        }
                    }
                }

                // Chat intent: finish workflow and re-prompt for natural language reply
                if engine.consume_chat_route() {
                    let _ = engine.complete_workflow();
                    engine.set_chat_reply_pending();
                    if matches!(messages.last(), Some(Message::Assistant { .. })) {
                        messages.pop();
                        if matches!(new_messages.last(), Some(Message::Assistant { .. })) {
                            new_messages.pop();
                        }
                    }
                    messages.push(Message::system(
                        "【意图】闲聊。请直接用自然语言回复用户的最后一个问题，不要 JSON，不要调用工具。"
                    ));
                    let _ = ui_tx.send(AgentToUiEvent::Status(
                        "💬 闲聊模式 — 生成回复...".to_string(),
                    ));
                    iteration += 1;
                    continue;
                }

                match advance_result {
                    Some(target_idx) => {
                        engine.set_previous_output(&step_output_text);

                        // Any transition into Execute requires human confirmation first.
                        let needs_human_before_execute = target_idx == 3;

                        if needs_human_before_execute {
                            let markdown = build_execute_confirmation_markdown(
                                &engine,
                                &full_text,
                                pre_llm_step_idx,
                            );
                            engine.arm_execute_confirmation();
                            let _ = ui_tx.send(AgentToUiEvent::PlanReviewReady {
                                markdown: markdown.clone(),
                            });
                            let _ = ui_tx.send(AgentToUiEvent::WorkflowAwaitingConfirmation {
                                step_idx: 2,
                                message: String::new(),
                            });
                            if let Some(last_assistant) = new_messages.iter_mut().rev()
                                .find(|m| matches!(m, Message::Assistant { .. }))
                            {
                                if let Message::Assistant { content, .. } = last_assistant {
                                    *content = markdown;
                                }
                            }
                            let status_msg = execute_confirmation_status(pre_llm_step_idx);
                            let _ = ui_tx.send(AgentToUiEvent::Status(status_msg));
                            let l0_entity = Entity::working_memory(
                                "current",
                                &format!(
                                    "[Review→Confirm] {}",
                                    full_text.chars().take(500).collect::<String>()
                                ),
                                None,
                                None,
                                vec![],
                                false,
                            );
                            {
                                let knowledge = Arc::clone(&tool_ctx.knowledge);
                                tokio::spawn(async move {
                                    if let Ok(mut eng) = knowledge.try_write() {
                                        eng.push_turn_buffer(l0_entity);
                                    }
                                });
                            }
                            let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                new_messages,
                                usage: total_usage,
                            });
                            return;
                        }

                        let step_name = engine.current_step()
                            .map(|s| if s.name.is_empty() { format!("step-{}", 0) } else { s.name.clone() })
                            .unwrap_or_else(|| "step-0".to_string());
                        let output_snippet: String = full_text.chars().take(500).collect();
                        let l0_entity = Entity::working_memory(
                            "current",
                            &format!("[{}] {}", step_name, output_snippet),
                            None, None, vec![], false,
                        );
                        {
                            let knowledge = Arc::clone(&tool_ctx.knowledge);
                            tokio::spawn(async move {
                                if let Ok(mut eng) = knowledge.try_write() {
                                    eng.push_turn_buffer(l0_entity);
                                }
                            });
                        }

                        // Advance to target step (supports skip Review for simple coding)
                        match engine.advance_to_step(Some(target_idx)) {
                            Ok(true) => {
                                engine.clear_turn_memory();
                                turn_memory = turn_memory::TurnMemory::new(
                                    user_task.as_deref().unwrap_or(""),
                                );
                                persist_turn_memory(&workflow_engine, &turn_memory);
                                let needs_confirmation = engine.requires_user_confirmation();
                                if needs_confirmation { engine.set_confirmation_flag(); }
                                let display = engine.current_step()
                                    .map(|s| if s.display_status.is_empty() { s.name.clone() } else { s.display_status.clone() })
                                    .unwrap_or_else(|| "处理中".to_string());
                                let status_msg = if needs_confirmation {
                                    format!("{} — ⏸️ 等待确认 (/Y)", display)
                                } else {
                                    display.clone()
                                };
                                let _ = ui_tx.send(AgentToUiEvent::Status(status_msg));
                                // Inject new step's prompt as a system message
                                if let Some(new_prompt) = engine.get_step_system_prompt() {
                                    let mut prompt_text = format!("【当前步骤】\n{}", new_prompt);
                                    if engine.get_current_step_index() == 1 {
                                        let skills_list = tool_registry.get_skills_list().iter()
                                            .map(|s| format!("- `{}` ({}) — {}", s.id, s.scope, s.description))
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        if skills_list.is_empty() {
                                            prompt_text.push_str("\n\n【可用 Skill】\n（无项目 Skill，使用内置默认）\n\n⚠️ skills 字段填写 `[\"coding-workflow\"]`。");
                                        } else {
                                            prompt_text.push_str(&format!(
                                                "\n\n【可用 Skill】\n{}\n\n⚠️ 必须从上面选择至少一个 skill。coding 任务优先选 project 级别的。",
                                                skills_list
                                            ));
                                        }
                                    }
                                    // Push to both messages (LLM context) and new_messages (session persistence)
                                    let step_msg = Message::system(&prompt_text);
                                    new_messages.push(step_msg.clone());
                                    messages.push(step_msg);
                                }

                                // Send formatted status so user sees step progress
                                let formatted = format_step_output(pre_llm_step_idx, &full_text, &display);
                                let _ = ui_tx.send(AgentToUiEvent::Status(formatted));
                                // Return TurnDone — main loop orchestrates next step
                                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                    new_messages,
                                    usage: total_usage,
                                });
                                return;
                            }
                            Ok(false) => {
                                let _ = ui_tx.send(AgentToUiEvent::Status("✅ 完成".to_string()));
                                emit_workflow_completed(
                                    &ui_tx,
                                    user_task.as_ref(),
                                    &engine,
                                    &full_text,
                                );
                                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                    new_messages,
                                    usage: total_usage,
                                });
                                return;
                            }
                            Err(e) => tracing::warn!("Failed to advance: {e}"),
                        }
                    }
                    None => {
                        // Validation error: retry unless ## Done already handled above
                        if validation_error.is_some() {
                            iteration += 1;
                            continue;
                        }
                    }
                }
            }
            // ── Workflow auto-advance END ──

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
            .map(|e| e.get_current_step_index() == 3)
            .unwrap_or(false);

        for tc in &tool_calls {
            let loop_key = tool_loop_key(&tc.name, &tc.arguments);
            tool_loop_keys.insert(tc.id.clone(), loop_key.clone());
            let count = temp_counts.entry(loop_key).or_insert(0);
            *count += 1;
            let limit = match tc.name.as_str() {
                "file_list" => 2,
                "file_read" if execute_step => 6,
                "file_read" => 2,
                _ => MAX_SAME_TOOL_CALLS,
            };
            if *count > limit {
                exceeded_loop_limit_ids.insert(tc.id.clone());
            }
        }
        
        // Push assistant message — format internal step JSON as user-readable summary
        // Step 0 (Intent), Step 1 (Plan), Step 2 (Review): formatted summary
        // Step 3 (Execute): show full output normally
        let (is_internal, step_idx, step_status) = if let Some(ref engine_arc) = workflow_engine {
            if let Ok(engine) = engine_arc.try_lock() {
                let idx = engine.get_current_step_index();
                let status = engine.current_step()
                    .map(|s| if s.display_status.is_empty() { s.name.clone() } else { s.display_status.clone() })
                    .unwrap_or_else(|| String::new());
                (idx != 3, idx, status)
            } else { (false, 5, String::new()) }
        } else { (false, 5, String::new()) };

        let display = if is_internal {
            // Try to parse JSON and format a human-readable summary
            let summary = format_step_output(step_idx, &full_text, &step_status);
            tracing::debug!("[STEP OUTPUT] step={}, formatted={}", step_idx, summary);
            summary
        } else { full_text.clone() };

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
                            .lookup_exploration_cache(tc.name.as_str(), path)
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
                if step_idx == 3 && tc.name == "file_list" {
                    let path = args_value
                        .get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or(".");
                    if engine.is_path_explored("file_list", path) {
                        let cached = engine
                            .lookup_exploration_cache("file_list", path)
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
                                    // Buffer interjection while waiting for confirmation.
                                    let _ = ui_tx.send(AgentToUiEvent::Status(
                                        format!("(interjection queued: {})", text.trim())
                                    ));
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
                    messages.push(Message::user(&text));
                    let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                        "💬 User (before tool): {}",
                        text.trim()
                    )));
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

            let result_content = format!(
                "── DATA ({}) ──\n{}\n── END DATA ──",
                tc.name,
                offloaded.to_context_message()
            );

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

            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: result_content,
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
            turn_memory.record_tool(&tc.name, &tc.arguments, !result.is_error);
            persist_turn_memory(&workflow_engine, &turn_memory);

            // 📋 Status log: tell LLM what it just accomplished (critical for multi-step awareness)
            if !result.is_error {
                let tool_name = tc.name.clone();
                let file_info = if matches!(tool_name.as_str(), "file_write" | "edit_file") {
                    serde_json::from_str::<serde_json::Value>(&tc.arguments).ok()
                        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
                        .map(|p| format!(" → {}", p))
                        .unwrap_or_default()
                } else { String::new() };
                messages.push(Message::system(&format!(
                    "📋 ✅ {tool_name}{file_info} — 已完成",
                    tool_name = tool_name, file_info = file_info
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
                                } else if engine.get_current_step_index() == 3
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
                        if engine.get_current_step_index() == 3
                            && engine.record_execute_tool_success(&tool_name, &tc.arguments)
                        {
                            if let Some(msg) =
                                engine.plan_progress_message_after_tool(&tool_name)
                            {
                                messages.push(Message::system(&msg));
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
                    wf.try_lock().map_or(false, |e| e.get_current_step_index() == 3)
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

        // 🚨 Done reminder + tool loop detection
        if !tool_calls.is_empty() {
            let has_write = tool_calls.iter().any(|tc| matches!(tc.name.as_str(), "file_write" | "edit_file" | "delete_range"));
            if has_write && !onboarding::is_onboarding_turn(&messages) {
                messages.push(Message::system(
                    "Files were modified. Output ## Done: list what was created/modified and the verify result. 3 lines max. No extra text."
                ));
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

/// Hint shown after Review before human confirms Execute.
const EXECUTE_CONFIRM_HINT: &str = "\
---\n\n\
> **审阅已完成 — 请确认后执行**\n\
> - 输入修改意见 → 回到规划重新生成\n\
> - 输入 `ok` / `继续` / `确认` → 开始执行";

/// Extract `## 计划草稿` section from Plan-step assistant text (parallel explore+draft).
fn extract_plan_draft_section(text: &str) -> Option<String> {
    let marker = "## 计划草稿";
    let start = text.find(marker)?;
    let rest = &text[start + marker.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    let draft = rest[..end].trim();
    if draft.len() < 8 {
        None
    } else {
        Some(format!("{marker}\n{draft}"))
    }
}

fn execute_confirmation_status(from_step: usize) -> String {
    match from_step {
        0 => "⏸️ 快速路径 — 已跳过规划/审阅，请确认后开始执行".to_string(),
        1 => "⏸️ 规划完成 — 请确认后开始执行".to_string(),
        2 => "⏸️ 审阅完成 — 请确认后开始执行".to_string(),
        _ => "⏸️ 请确认后开始执行".to_string(),
    }
}

fn build_execute_confirmation_markdown(
    engine: &crate::agent::engine::WorkflowEngine,
    review_text: &str,
    from_step: usize,
) -> String {
    let plan_raw = engine
        .get_variable("_step1_output")
        .unwrap_or_else(|| engine.get_previous_step_output().unwrap_or_default());
    let plan_md = format_step_output(1, &plan_raw, "📋 任务规划");
    let review_md = if from_step == 2 {
        format_step_output(2, review_text, "🛡️ 审阅计划")
    } else if from_step == 0 {
        let exploring = crate::agent::engine::WorkflowEngine::parse_intent_meta(
            engine.get_variable("_step0_output").as_deref(),
        )
        .map(|(intent, _)| intent == "exploring")
        .unwrap_or(false);
        if exploring {
            "✅ **只读检查快速路径** — 跳过规划/审阅，确认后直接探索并输出分析".to_string()
        } else {
            "✅ **快速路径** — 简单改动跳过规划/审阅，直接进入人工确认".to_string()
        }
    } else {
        "✅ **自动审阅已跳过**（只读检查或简单编码任务）".to_string()
    };
    format!("{plan_md}\n\n---\n\n{review_md}\n\n{EXECUTE_CONFIRM_HINT}")
}

/// Replace the latest internal-step assistant bubble (same step retries), keep tool-call assistants.
fn upsert_workflow_step_assistant(messages: &mut Vec<Message>, new_msg: &Message) {
    let Message::Assistant {
        content: _new_content,
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
    while let Some(Message::Assistant {
        content,
        tool_calls,
        ..
    }) = messages.last()
    {
        if !tool_calls.is_empty() {
            break;
        }
        if is_internal_formatted_assistant(content) {
            messages.pop();
        } else {
            break;
        }
    }
    messages.push(new_msg.clone());
}

fn is_internal_formatted_assistant(content: &str) -> bool {
    content.starts_with("## 执行计划")
        || content.starts_with('_')
        || content.contains("路径 `")
        || content.starts_with("🔍")
        || content.starts_with("💻")
        || content.starts_with("💬")
        || content.starts_with("🤔")
}

/// Format JSON output from internal workflow steps into user-readable Markdown.
fn format_step_output(step_idx: usize, text: &str, fallback: &str) -> String {
    let json_str = match crate::agent::engine::extract_json_block(text) {
        Some(s) => s,
        None => return format!("_{}_", fallback),
    };

    let parsed: Option<serde_json::Value> = serde_json::from_str(&json_str).ok();

    match step_idx {
        0 => {
            // Step 0: Intent Classification + routing
            if let Some(ref v) = parsed {
                let intent = v.get("intent").and_then(|s| s.as_str()).unwrap_or("?");
                let complexity = v.get("complexity").and_then(|s| s.as_str()).unwrap_or("");
                let topic = v.get("topic").and_then(|s| s.as_str()).unwrap_or("");
                let pipeline = v.get("pipeline").and_then(|s| s.as_str()).unwrap_or("?");
                let reason = v.get("routing_reason").and_then(|s| s.as_str()).unwrap_or("");
                let emoji = match intent {
                    "coding" => "💻", "exploring" => "🔍", "chat" => "💬", _ => "🤔",
                };
                let route = crate::agent::engine::WorkflowEngine::intent_routing_from_text(Some(text))
                    .map(|r| {
                        if r.requires_human_confirm {
                            format!("{}（待人工确认）", r.steps_summary)
                        } else {
                            r.steps_summary
                        }
                    })
                    .unwrap_or_else(|| pipeline.to_string());
                let head = if topic.is_empty() {
                    format!("{} {}({})", emoji, intent, complexity)
                } else {
                    format!("{} {}({}) — {}", emoji, intent, complexity, topic)
                };
                let mut lines = vec![head, format!("📍 路径 `{pipeline}`：{route}")];
                if !reason.is_empty() {
                    lines.push(format!("💡 {reason}"));
                }
                lines.join("\n")
            } else {
                format!("_🤔 分析意图_")
            }
        }
        1 => {
            // Step 1: Task Planning — structured Markdown
            if let Some(ref v) = parsed {
                let mut md = String::from("## 执行计划\n");
                let mut step_count = 0usize;
                if let Some(plan) = v.get("plan").and_then(|p| p.as_array()) {
                    for step in plan {
                        if let Some(obj) = step.as_object() {
                            step_count += 1;
                            let num = obj.get("step").and_then(|s| s.as_u64()).unwrap_or(step_count as u64);
                            let file = obj.get("file").and_then(|s| s.as_str()).unwrap_or("");
                            let action = obj.get("action").and_then(|s| s.as_str()).unwrap_or("");
                            let target = obj.get("target").and_then(|s| s.as_str()).unwrap_or("");
                            let desc = obj.get("desc").and_then(|s| s.as_str()).unwrap_or("");
                            let verify = obj.get("verify").and_then(|s| s.as_str()).unwrap_or("");

                            let action_label = match action {
                                "add" | "create" => "新增",
                                "modify" => "修改",
                                "delete" => "删除",
                                "explain" => "说明",
                                _ if action.is_empty() => "步骤",
                                _ => action,
                            };

                            md.push_str(&format!("\n### 步骤 {num} — {action_label}\n"));
                            if !file.is_empty() {
                                md.push_str(&format!("- **文件:** `{file}`\n"));
                            }
                            if !target.is_empty() {
                                md.push_str(&format!("- **目标:** `{target}`\n"));
                            }
                            if !desc.is_empty() {
                                md.push_str(&format!("- **说明:** {desc}\n"));
                            }
                            if !verify.is_empty() {
                                md.push_str(&format!("- **验证:** {verify}\n"));
                            }
                        } else if let Some(s) = step.as_str() {
                            step_count += 1;
                            md.push_str(&format!("\n- {s}\n"));
                        }
                    }
                }
                if step_count == 0 {
                    md.push_str("\n> ⚠️ 计划 JSON 已收到，但未解析出可展示的步骤。原始内容：\n\n");
                    md.push_str("```json\n");
                    md.push_str(&serde_json::to_string_pretty(v).unwrap_or_else(|_| json_str.clone()));
                    md.push_str("\n```\n");
                }
                if let Some(skills) = v.get("skills").and_then(|s| s.as_array()) {
                    let skill_names: Vec<&str> = skills
                        .iter()
                        .filter_map(|s| s.as_str())
                        .collect();
                    if !skill_names.is_empty() {
                        md.push_str("\n**Skills:** ");
                        md.push_str(&skill_names.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", "));
                        md.push('\n');
                    }
                }
                if let Some(files) = v.get("key_files").and_then(|f| f.as_array()) {
                    let file_names: Vec<&str> = files.iter().filter_map(|f| f.as_str()).collect();
                    if !file_names.is_empty() {
                        md.push_str("\n**关键文件:** ");
                        md.push_str(&file_names.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", "));
                        md.push('\n');
                    }
                }
                md
            } else {
                format!("_📋 任务规划_")
            }
        }
        2 => {
            // Step 2: Review — safety + completeness check on the plan
            if let Some(ref v) = parsed {
                let safe = v.get("safe").and_then(|s| s.as_bool()).unwrap_or(true);
                let complete = v.get("complete").and_then(|c| c.as_bool()).unwrap_or(true);
                let issues = v.get("issues").and_then(|i| i.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let warnings = v.get("warnings").and_then(|w| w.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();

                let mut lines = Vec::new();
                if safe && complete && issues.is_empty() {
                    lines.push("✅ 计划通过审阅".to_string());
                } else {
                    if !safe { lines.push("⚠️ 安全问题".to_string()); }
                    if !complete { lines.push("⚠️ 计划不完整".to_string()); }
                    for issue in &issues {
                        lines.push(format!("  ❌ {}", issue));
                    }
                }
                for warning in &warnings {
                    lines.push(format!("  💡 {}", warning));
                }
                lines.join("\n")
            } else {
                "🛡️ 审阅计划".to_string()
            }
        }
        _ => format!("_{}_", fallback),
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
