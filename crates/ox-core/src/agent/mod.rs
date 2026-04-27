pub mod interjection;
pub mod interrupt;
pub mod ui_event;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::llm::{LlmProvider, LlmStreamEvent};
use crate::message::{Message, ToolCall, TokenUsage};
use crate::safety::TrustManager;
use crate::tools::{SafetyLevel, ToolContext, ToolRegistry};
use crate::config::AgentConfig;

/// Events sent from the agent to the UI.
#[derive(Debug, Clone)]
pub enum AgentToUiEvent {
    /// Streaming text from LLM.
    TextChunk(String),
    /// Agent is calling a tool.
    ToolStart { name: String, id: String },
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
    ToolOutputChunk {
        tool_call_id: String,
        chunk: String,
    },
    /// Budget exceeded — request user confirmation to continue.
    BudgetExceeded {
        total_tokens: u32,
        estimated_cost: String,
    },
    /// Council debate completed.
    CouncilDone {
        session: crate::council::CouncilSession,
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
) {
    let tool_schemas = tool_registry.schemas();
    let max_iterations = agent_config.max_iterations;

    // Track new messages produced during this turn for returning to the caller.
    let mut new_messages: Vec<Message> = Vec::new();
    let mut total_usage = TokenUsage::default();

    for iteration in 0..max_iterations {
        // Check cancellation before each LLM call.
        if cancel_token.is_cancelled() {
            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
            break;
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
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    format!("(interjection injected: {})", text.trim())
                ));
            }
        }

        // Stream LLM response.
        let (llm_tx, mut llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

        let provider_clone = Arc::clone(&provider);
        let msgs = messages.clone();
        let schemas = tool_schemas.clone();
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
                _ = cancel_clone.cancelled() => {
                    tracing::info!("LLM stream task cancelled");
                }
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
                None
            }
        } {
            match event {
                LlmStreamEvent::TextDelta(text) => {
                    let _ = ui_tx.send(AgentToUiEvent::TextChunk(text.clone()));
                    full_text.push_str(&text);
                }
                LlmStreamEvent::ToolCallStart { id, name } => {
                    let _ = ui_tx.send(AgentToUiEvent::ToolStart {
                        name: name.clone(),
                        id: id.clone(),
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
                    total_usage.prompt_tokens += usage.prompt_tokens;
                    total_usage.completion_tokens += usage.completion_tokens;
                    total_usage.total_tokens += usage.total_tokens;
                    break;
                }
                LlmStreamEvent::Error(err) => {
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
                content: full_text,
                tool_calls: Vec::new(),
            };
            new_messages.push(msg.clone());
            messages.push(msg);
            let _ = ui_tx.send(AgentToUiEvent::TurnDone {
                new_messages,
                usage: total_usage,
            });
            return;
        }

        // Push assistant message with tool calls.
        let assistant_msg = Message::Assistant {
            content: full_text,
            tool_calls: tool_calls.clone(),
        };
        new_messages.push(assistant_msg.clone());
        messages.push(assistant_msg);

        // Execute each tool call.
        for tc in &tool_calls {
            // Check cancellation before each tool execution.
            if cancel_token.is_cancelled() {
                let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted before tool execution.".to_string()));
                break;
            }

            let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                "Running tool: {}",
                tc.name
            )));

            let tool = match tool_registry.get(&tc.name) {
                Some(t) => t,
                None => {
                    let available = tool_registry.names().join(", ");
                    let error_msg = format!("Unknown tool: '{}'. Available tools: {}", tc.name, available);
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
            let safety_level = tool.safety_level();

            // Check if tool args reference a path outside working directory.
            let path_outside = if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                if let Some(path_str) = args_val.get("path").and_then(|v| v.as_str()) {
                    let resolved = tool_ctx.working_dir.join(path_str);
                    !crate::safety::is_path_within_workdir(&resolved, &tool_ctx.working_dir)
                } else {
                    false
                }
            } else {
                false
            };

            let should_confirm = if path_outside {
                true // Path outside workdir always requires confirmation.
            } else {
                match safety_level {
                    SafetyLevel::Safe => false,
                    SafetyLevel::RequiresConfirmation => {
                        let tm = trust_manager.lock().unwrap();
                        !tm.can_skip_confirmation(&tc.name, safety_level)
                    }
                    SafetyLevel::Dangerous => true,
                }
            };

            if should_confirm {
                // Build args_summary (truncated, sanitized).
                let args_summary = if tc.arguments.len() > 200 {
                    let end = tc.arguments.char_indices().take_while(|(i, _)| *i < 200).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
                    format!("{}...(truncated)", &tc.arguments[..end])
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
                        let mut tm = trust_manager.lock().unwrap();
                        tm.trust_all();
                    }
                    ui_event::ConfirmationDecision::Allow => {}
                }
            }

            let args: serde_json::Value = if tc.arguments.trim().is_empty() {
                // LLM sent no arguments — treat as empty object (common for no-param tools).
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                match serde_json::from_str(&tc.arguments) {
                    Ok(v) => v,
                    Err(parse_err) => {
                        let error_msg = format!(
                            "Invalid tool arguments JSON: {parse_err}. Raw: {}",
                            if tc.arguments.len() > 200 {
                                let end = tc.arguments.char_indices().take_while(|(i, _)| *i < 200).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
                                format!("{}...(truncated)", &tc.arguments[..end])
                            } else {
                                tc.arguments.clone()
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

            let result = tool.execute(args, &tool_ctx).await;

            let _ = ui_tx.send(AgentToUiEvent::ToolResult {
                name: tc.name.clone(),
                output: result.content.clone(),
                is_error: result.is_error,
            });

            let result_msg = Message::ToolResult {
                tool_call_id: tc.id.clone(),
                content: result.content,
            };
            new_messages.push(result_msg.clone());
            messages.push(result_msg);
        }

        // Loop back to call LLM again with tool results.
    }

    // If we exit the loop (max iterations or cancellation), still send TurnDone
    // so the UI can persist whatever messages were collected.
    if cancel_token.is_cancelled() {
        let _ = ui_tx.send(AgentToUiEvent::TurnDone {
            new_messages,
            usage: total_usage,
        });
    } else {
        let _ = ui_tx.send(AgentToUiEvent::Error(
            "Agent exceeded maximum iterations (25).".to_string(),
        ));
    }
}
