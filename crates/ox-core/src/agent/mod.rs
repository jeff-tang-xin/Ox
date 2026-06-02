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



pub use engine::StepDisplayInfo;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::llm::{LlmProvider, LlmStreamEvent};
use crate::message::{Message, TokenUsage, ToolCall};
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

    let mut iteration = 0u32;
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

        if !workflow_active && iteration > 0 && iteration >= max_iterations {
            let _ = ui_tx.send(AgentToUiEvent::IterationLimitReached { iteration });

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
                }
            };

            if !should_continue {
                break;
            }
            // User chose to continue — reset counter so we get another full batch.
            iteration = 0;
        }

        let _ = ui_tx.send(AgentToUiEvent::Status(if iteration == 0 {
            "Thinking...".to_string()
        } else {
            format!("Thinking... (iteration {})", iteration + 1)
        }));

        // Check for queued interjections before LLM call.
        while let Ok(ev) = ui_rx.try_recv() {
            if let ui_event::UiToAgentEvent::Interjection(text) = ev {
                messages.push(Message::user(&text));
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
                    "⏸️ Waiting for your confirmation... Use /Y, /N, or /O".to_string(),
                ));

                // Return early without calling LLM
                let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                    new_messages,
                    usage: total_usage,
                });
                return;
            }
        }

        // Stream LLM response.
        let (llm_tx, mut llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

        let provider_clone = Arc::clone(&provider);
        let msgs = messages.clone();

        // Filter tool schemas based on current workflow step
        let schemas = if planning_mode && iteration == 0 {
            vec![] // Planning mode: no tools in first iteration
        } else if let Some(ref engine_arc) = workflow_engine {
            let engine = engine_arc.lock().await;
            let allowed_tools = engine.get_allowed_tools();

            if allowed_tools.is_empty() {
                // Empty list means all tools allowed
                tool_schemas.clone()
            } else {
                // Filter to only include allowed tools
                tool_schemas
                    .iter()
                    .filter(|schema| allowed_tools.contains(&schema.name))
                    .cloned()
                    .collect()
            }
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
                Message::Assistant { content, tool_calls } => {
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
                    // Simple approach: just pass through all text including think tags
                    // The UI can decide how to display them (or we can strip them later)
                    let _ = ui_tx.send(AgentToUiEvent::TextChunk(text.clone()));
                    full_text.push_str(&text);
                }
                LlmStreamEvent::ToolCallStart { id, name } => {
                    let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                        name: name.clone(),
                        id: id.clone(),
                        detail: None,
                    });
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

        // If no tool calls, the turn is complete.
        if tool_calls.is_empty() {
            let msg = Message::Assistant {
                content: full_text.clone(), // Clone for workflow check
                tool_calls: Vec::new(),
            };
            new_messages.push(msg.clone());
            messages.push(msg);

            // ── Workflow Step Advancement Logic (before returning) ──
            // Check if we should advance to the next workflow step
            if let Some(ref engine_arc) = workflow_engine {
                let mut engine = engine_arc.lock().await;

                // Check if AI signaled step completion via [STEP_COMPLETE] marker
                let ai_signaled_complete = full_text.contains("[STEP_COMPLETE]");
                
                // Also check for Phase completion messages
                let phase_complete = full_text.contains("✅ Phase") && 
                    (full_text.contains("Complete") || full_text.contains("complete"));

                // Check if key operations were completed (e.g., file creation)
                let key_operation_completed = new_messages.iter().any(|msg| {
                    matches!(msg, Message::ToolResult { content, .. } if {
                        // Detect successful file_write or file_patch
                        content.contains("✅ Successfully written") ||
                        content.contains("✅ File patched") ||
                        content.contains("Successfully created")
                    })
                });
                
                // Check if there were tool errors that need attention
                let has_tool_errors = new_messages.iter().any(|msg| {
                    matches!(msg, Message::ToolResult { content, .. } if {
                        content.contains("Missing Required Parameter") ||
                        content.contains("JSON Parse Error") ||
                        content.contains("❌")
                    })
                });
                
                // If there are tool errors, don't advance - let LLM retry
                if has_tool_errors {
                    tracing::warn!("⚠️ Tool execution failed, not advancing step. Letting LLM retry...");
                    
                    // Inject guidance message if it's a parameter error
                    let has_param_error = new_messages.iter().any(|msg| {
                        matches!(msg, Message::ToolResult { content, .. } if {
                            content.contains("Missing Required Parameter")
                        })
                    });
                    
                    if has_param_error {
                        messages.push(Message::user(
                            "⚠️ IMPORTANT: Your tool call failed due to missing parameters.\n\n\
                             When calling file_write, you MUST provide:\n\
                             - 'path': The file path (e.g., '.ox/order-optimization/spec.md')\n\
                             - 'content': The file content as a string\n\n\
                             Example:\n\
                             {{\"path\": \".ox/order-optimization/spec.md\", \"content\": \"# Title\\n\\nContent here\"}}\n\n\
                             Please retry with COMPLETE parameters."
                        ));
                    }
                    
                    // Return without advancing
                    let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                        new_messages,
                        usage: total_usage,
                    });
                    return;
                }

                // Advance step if:
                // 1. AI explicitly signaled completion with [STEP_COMPLETE], OR
                // 2. AI outputted Phase completion message (e.g., "✅ Phase 1 Complete!"), OR
                // 3. Key operations were completed (file creation) AND step requires confirmation
                // 
                // NOTE: We rely on AI to signal completion, not on tool execution results.
                // This allows AI to make multiple tool calls within a single phase.
                let should_advance = ai_signaled_complete || phase_complete || key_operation_completed;
                
                // If AI completed work but forgot to signal, and step requires confirmation,
                // we still need to wait for user's explicit command (/Y, /N, /O).
                // The confirmation flag will be set when user inputs the command.

                if should_advance && !engine.is_workflow_complete() {
                    tracing::info!(
                        "Advancing workflow step (AI signaled: {}, Key op completed: {})",
                        ai_signaled_complete,
                        key_operation_completed
                    );
                    
                    // 🚨 PATH VALIDATION: Verify file paths before advancing (Spec/Council Mode only)
                    let current_step = engine.current_step();
                    let needs_path_validation = current_step.map(|step| {
                        step.name == "phase_1_documentation" || step.name == "topic_definition"
                    }).unwrap_or(false);
                    
                    if needs_path_validation {
                        // Check if files were created in correct location (.ox/{name}/ not .ox/)
                        let has_wrong_path = new_messages.iter().any(|msg| {
                            matches!(msg, Message::ToolResult { content, .. } if {
                                // Detect file_write to .ox/ directly (without subdirectory)
                                content.contains(".ox/") && (
                                    content.contains(".ox/spec.md") ||
                                    content.contains(".ox/task.md") ||
                                    content.contains(".ox/council_record.md")
                                )
                            })
                        });
                        
                        if has_wrong_path {
                            tracing::warn!("❌ Path validation failed: Files created in wrong location!");
                            
                            // Inject error message and force retry
                            messages.push(Message::user(
                                "❌ CRITICAL ERROR: Files were created in the WRONG location!\n\n\
                                 Expected format: `.ox/{requirement_name}/spec.md`\n\
                                 Your format: `.ox/spec.md` (MISSING requirement name!)\n\n\
                                 💡 How to fix:\n\
                                 1. Generate a requirement name (e.g., 'order-optimization')\n\
                                 2. Create files in `.ox/order-optimization/` directory\n\
                                 3. Example paths:\n\
                                    - `.ox/order-optimization/spec.md`\n\
                                    - `.ox/order-optimization/task.md`\n\n\
                                 Please REDO Phase 1 with CORRECT paths now."
                            ));
                            
                            // Don't advance - force LLM to retry
                            let _ = ui_tx.send(AgentToUiEvent::Status(
                                "❌ Path validation failed. Retrying Phase 1...".to_string()
                            ));
                            
                            // Return early without advancing
                            let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                                new_messages,
                                usage: total_usage,
                            });
                            return;
                        }
                    }
                    
                    // 🚨 CONFIRMATION CHECK: Check if CURRENT step requires confirmation BEFORE advancing
                    let current_step_requires_confirmation = engine.requires_user_confirmation();
                    
                    if current_step_requires_confirmation {
                        tracing::info!("Current step requires user confirmation - setting flag and blocking next LLM call");
                        
                        // Set confirmation flag to block next LLM call
                        engine.set_confirmation_flag();
                        
                        // Notify UI about waiting for confirmation
                        if let Some(step_name) = get_current_step_name(&engine) {
                            let status_msg = format!(
                                "✅ {} completed. ⏸️ Waiting for your confirmation (/Y, /N, /O)",
                                step_name
                            );
                            let _ = ui_tx.send(AgentToUiEvent::Status(status_msg));
                        }
                        
                        // DON'T advance yet - wait for user confirmation
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
                                // Check if the NEW step requires confirmation
                                let needs_confirmation = engine.requires_user_confirmation();

                                if needs_confirmation {
                                    // Set confirmation flag to block next LLM call
                                    engine.set_confirmation_flag();

                                    tracing::info!(
                                        "New step requires user confirmation - setting flag"
                                    );
                                }

                                // Notify UI about step transition
                                if let Some(step_name) = get_current_step_name(&engine) {
                                    let status_msg = if needs_confirmation {
                                        format!(
                                            "✅ {} completed. ⏸️ Waiting for your confirmation (/Y, /N, /O)",
                                            step_name
                                        )
                                    } else {
                                        format!(
                                            "✅ Step completed. Moving to: {}",
                                            step_name
                                        )
                                    };

                                    let _ = ui_tx.send(AgentToUiEvent::Status(status_msg));
                                }
                            } else {
                                // Workflow complete
                                let _ = ui_tx.send(AgentToUiEvent::Status(
                                    "🎉 Workflow completed!".to_string(),
                                ));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to advance workflow step: {}", e);
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
        
        for tc in &tool_calls {
            let count = temp_counts.entry(tc.name.clone()).or_insert(0);
            *count += 1;
            if *count > MAX_SAME_TOOL_CALLS {
                exceeded_loop_limit_ids.insert(tc.id.clone());
            }
        }
        
        let valid_tool_calls: Vec<_> = tool_calls
            .iter()
            .filter(|tc| !truncated_ids.contains(&tc.id) && !exceeded_loop_limit_ids.contains(&tc.id))
            .cloned()
            .collect();

        // Push assistant message with ONLY valid (non-truncated) tool calls.
        let assistant_msg = Message::Assistant {
            content: full_text.clone(), // Clone to keep full_text for workflow advancement check
            tool_calls: valid_tool_calls,
        };
        new_messages.push(assistant_msg.clone());
        messages.push(assistant_msg);

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
                let call_count = temp_counts.get(&tc.name).copied().unwrap_or(0);
                tracing::error!(
                    "🚨 INFINITE LOOP DETECTED: Tool '{}' called {} times in one turn. Stopping.",
                    tc.name,
                    call_count
                );
                
                let error_msg = format!(
                    "❌ Infinite Loop Detected:\n\
                     The tool '{}' has been called {} times in this conversation turn.\n\
                     This suggests the AI is stuck in a loop.\n\n\
                     💡 Solutions:\n\
                     1. Try a different approach to solve the problem\n\
                     2. Break the task into smaller steps\n\
                     3. Provide more specific instructions\n\
                     4. Use /clear to start fresh if needed",
                    tc.name, call_count
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

            // Skip truncated tool calls — return error so LLM can retry.
            if truncated_ids.contains(&tc.id) {
                // Special handling for different tools
                let is_file_write = tc.name == "file_write";
                let is_file_patch = tc.name == "file_patch";
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
                            - Use file_patch to append/modify remaining parts\n\n\
                         3️⃣ Use file_patch for modifications:\n\
                            If modifying existing file, use search/replace instead of rewriting entire file\n\n\
                         📝 Note: Files >1 MB are automatically written in 512 KB chunks",
                        content_length as f64 / 1024.0
                    )
                } else if is_file_patch && content_length > 500 {
                    // Likely file_patch with long search/replace that was truncated
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
                        "❌ Arguments Truncated - file_patch parameters incomplete:\n\
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
                            - Instead of one large patch, make 2-3 smaller file_patch calls\n\
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

                    // Return error to LLM
                    let result_msg = Message::ToolResult {
                        tool_call_id: tc.id.clone(),
                        content: format!(
                            "❌ Workflow Restriction: {}\n\n💡 Please follow the current workflow step requirements.",
                            e
                        ),
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

            // For shell_exec, send an updated ToolStart with the command detail.
            if tc.name == "shell_exec" {
                if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    if let Some(cmd) = args_val.get("command").and_then(|v| v.as_str()) {
                        let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                            name: tc.name.clone(),
                            id: tc.id.clone(),
                            detail: Some(cmd.to_string()),
                        });
                    }
                }
            } else if tc.name == "file_write" {
                // For file_write, analyze arguments to show file size and write strategy
                if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    if let Some(content) = args_val.get("content").and_then(|v| v.as_str()) {
                        let content_len = content.len();
                        let path = args_val
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<unknown>");

                        // Determine write strategy based on content size
                        let strategy_detail = if content_len > 1024 * 1024 {
                            // > 1MB
                            let chunk_count = (content_len + 512 * 1024 - 1) / (512 * 1024); // 512KB chunks
                            format!(
                                "Large file ({} bytes) - will use chunked write ({} chunks of 512KB)",
                                content_len, chunk_count
                            )
                        } else {
                            format!("Small file ({} bytes) - will use atomic write", content_len)
                        };

                        let detail = format!("{} | {}", path, strategy_detail);

                        let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                            name: tc.name.clone(),
                            id: tc.id.clone(),
                            detail: Some(detail),
                        });
                    }
                }
            } else if tc.name == "file_patch" {
                tracing::info!("[AGENT] Sending ToolStart for file_patch");
                // For file_patch, show file path and search context
                if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    let path = args_val
                        .get("path")
                        .and_then(|v| v.as_str())
                        .or_else(|| args_val.get("filename").and_then(|v| v.as_str()))
                        .unwrap_or("<unknown>");

                    let search_preview = args_val
                        .get("search")
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            if s.len() > 60 {
                                format!("{}...", s.get(..60).unwrap_or(s))
                            } else {
                                s.to_string()
                            }
                        })
                        .unwrap_or("<missing>".to_string());

                    let detail = format!("{} | search: {}", path, search_preview);

                    let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                        name: tc.name.clone(),
                        id: tc.id.clone(),
                        detail: Some(detail),
                    });
                    tracing::info!("[AGENT] ToolStart event sent for file_patch, proceeding to get tool object");
                }
            }

            tracing::info!("[AGENT] About to get tool object for: {}", tc.name);
            let tool = match tool_registry.get(&tc.name) {
                Some(t) => {
                    tracing::info!("[AGENT] Tool object retrieved for: {}", tc.name);
                    t
                }
                None => {
                    let available = tool_registry.names().join(", ");
                    // Find similar tool names (simple string matching)
                    let mut suggestions: Vec<&str> = Vec::new();
                    for name in tool_registry.names() {
                        let tc_name_prefix = tc.name.get(..tc.name.len().min(3)).unwrap_or(&tc.name);
                        let name_prefix = name.get(..name.len().min(3)).unwrap_or(name);
                        if name.starts_with(tc_name_prefix)
                            || tc.name.starts_with(name_prefix)
                        {
                            suggestions.push(name);
                        }
                    }

                    let suggestion_text = if !suggestions.is_empty() {
                        format!("\n\n💡 Did you mean: {}?", suggestions.join(", "))
                    } else {
                        String::new()
                    };

                    let error_msg = format!(
                        "❌ Unknown tool: '{}'\n\n\
                         Available tools: {}{}\n\n\
                         💡 Tips:\n\
                         • Check the tool name spelling\n\
                         • Use /help to see all available tools\n\
                         • Tool names are case-sensitive",
                        tc.name, available, suggestion_text
                    );

                    tracing::warn!(
                        "Unknown tool requested: '{}'. Available: {}",
                        tc.name,
                        available
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

            // 🆕 NEW: Enforce mandatory rules (e.g., plan_before_edit)
            if let Err(violation_msg) = crate::agent::enforcer::RuleEnforcer::validate(
                &tool_ctx.config.enforcement_rules,
                &tc,
                &messages,
            ) {
                tracing::warn!("🚫 Rule Enforcer blocked tool '{}': {}", tc.name, violation_msg);

                // Send error to LLM as a tool result
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

                continue; // Skip this tool call and move to the next iteration
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
                            if crate::safety::is_high_risk_command(cmd) {
                                Some("HIGH RISK COMMAND".to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
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
                            "file_patch" => {
                                "{\"path\": \"src/lib.rs\", \"edits\": [{\"old\": \"...\", \"new\": \"...\"}]}"
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
                "file_patch" => "Patching file...",
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
            let tool_call_id_clone = tc.id.clone();
            let tool_name_clone = tc.name.clone();
            let tool_ctx_with_progress = Arc::new(crate::tools::ToolContext::with_progress_callback(
                tool_ctx.runtime.clone(),
                tool_ctx.working_dir.clone(),
                tool_ctx.config.clone(),
                Arc::clone(&tool_ctx.memory),
                Arc::clone(&tool_ctx.file_index),
                tc.id.clone(), // Pass tool_call_id
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
            let result = tool.execute(args, &tool_ctx_with_progress).await;
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
                    Arc::clone(&tool_ctx.memory),
                    Arc::clone(&tool_ctx.file_index),
                ));
                let _ = ui_tx.send(AgentToUiEvent::WorkingDirChanged(new_dir));
            }

            // ── Context Offloading: Save verbose results to external files ──
            let offloader = context_offloader::ContextOffloader::new(
                &tool_ctx.working_dir,
                &format!("session_{}", iteration),
            );
            
            let offloaded = offloader.process_result(
                &tc.name,
                &result.content,
                iteration as usize,
                2000, // threshold: 2000 chars
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

            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: offloaded.to_context_message(),
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
        }

        // ── Workflow Step Advancement Logic (after tool execution) ──
        // Check if we should advance to the next workflow step
        // This handles cases where tools were executed in this iteration
        if let Some(ref engine_arc) = workflow_engine {
            let mut engine = engine_arc.lock().await;

            // Check if AI signaled step completion via [STEP_COMPLETE] marker
            let ai_signaled_complete = full_text.contains("[STEP_COMPLETE]");
            
            // Also check for Phase completion messages
            let phase_complete = full_text.contains("✅ Phase") && 
                (full_text.contains("Complete") || full_text.contains("complete"));
            
            // Advance step if:
            // 1. AI explicitly signaled completion with [STEP_COMPLETE], OR
            // 2. AI outputted Phase completion message (e.g., "✅ Phase 1 Complete!")
            // 
            // NOTE: We do NOT advance based on tool execution results (file_write, etc.)
            // because the AI may perform multiple operations within a single phase.
            // We wait for the AI to explicitly signal completion.
            let should_advance = ai_signaled_complete || phase_complete;
            
            if should_advance && !engine.is_workflow_complete() {
                tracing::info!(
                    "Advancing workflow step after tool execution (AI signaled: {}, Phase complete: {})",
                    ai_signaled_complete,
                    phase_complete
                );
                
                // 🚨 PATH VALIDATION: Verify file paths before advancing (Spec/Council Mode only)
                let current_step = engine.current_step();
                let needs_path_validation = current_step.map(|step| {
                    step.name == "phase_1_documentation" || step.name == "topic_definition"
                }).unwrap_or(false);
                
                if needs_path_validation {
                    // Check if files were created in correct location (.ox/{name}/ not .ox/)
                    let has_wrong_path = new_messages.iter().any(|msg| {
                        matches!(msg, Message::ToolResult { content, .. } if {
                            content.contains(".ox/") && (
                                content.contains(".ox/spec.md") ||
                                content.contains(".ox/task.md") ||
                                content.contains(".ox/council_record.md")
                            )
                        })
                    });
                    
                    if has_wrong_path {
                        tracing::warn!("❌ Path validation failed: Files created in wrong location!");
                        
                        messages.push(Message::user(
                            "❌ CRITICAL ERROR: Files were created in the WRONG location!\n\n\
                             Expected format: `.ox/{requirement_name}/spec.md`\n\
                             Your format: `.ox/spec.md` (MISSING requirement name!)\n\n\
                             💡 How to fix:\n\
                             1. Generate a requirement name (e.g., 'order-optimization')\n\
                             2. Create files in `.ox/order-optimization/` directory\n\
                             3. Example paths:\n\
                                - `.ox/order-optimization/spec.md`\n\
                                - `.ox/order-optimization/task.md`\n\n\
                             Please REDO Phase 1 with CORRECT paths now."
                        ));
                        
                        let _ = ui_tx.send(AgentToUiEvent::Status(
                            "❌ Path validation failed. Retrying Phase 1...".to_string()
                        ));
                        
                        let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                            new_messages,
                            usage: total_usage,
                        });
                        return;
                    }
                }
                
                // 🚨 CONFIRMATION CHECK: Check if CURRENT step requires confirmation BEFORE advancing
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
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to advance workflow step: {}", e);
                    }
                }
            }
        }

        // Loop back to call LLM again with tool results.
        iteration += 1;
    }

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
    let has_unclosed_structure = (
        // Count opening/closing braces
        (trimmed.matches('{').count() > trimmed.matches('}').count()) ||
        (trimmed.matches('[').count() > trimmed.matches(']').count()) ||
        // Ends with incomplete syntax
        trimmed.ends_with(',') ||
        trimmed.ends_with(':') ||
        // Has unclosed quote
        (trimmed.matches('"').count() % 2 != 0)
    );

    is_eof_error || has_unclosed_structure
}

/// Helper function to get current step name from workflow engine.
fn get_current_step_name(engine: &crate::agent::engine::WorkflowEngine) -> Option<String> {
    engine.current_step().map(|step| step.name.clone())
}

/// Remove think tags (<think>...</think>) from text.
/// LLMs sometimes include thinking content in tool arguments, which breaks JSON parsing.
fn clean_think_tags(text: &str) -> String {
    use regex::Regex;

    // Pattern to match <think>...</think> tags (case-insensitive)
    static THINK_PATTERN: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?s)<think[^>]*>.*?</think>").unwrap());

    // Also handle unclosed think tags
    static UNCLOSED_THINK: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?s)<think[^>]*>.*$").unwrap());

    let result = THINK_PATTERN.replace_all(text, "");
    let result = UNCLOSED_THINK.replace_all(&result, "");

    result.to_string()
}
