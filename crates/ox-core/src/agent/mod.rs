pub mod interjection;
pub mod interrupt;
pub mod ui_event;
pub mod workflow;
pub mod session;
pub mod intervention;
pub mod engine;

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
    ToolStart { name: String, id: String, detail: Option<String> },
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
    /// Council debate completed.
    CouncilDone {
        session: crate::council::CouncilSession,
    },
    /// Agent detected a working directory change (e.g. shell cd).
    WorkingDirChanged(std::path::PathBuf),
    /// Agent reached the iteration limit and is asking user to continue.
    IterationLimitReached { iteration: u32 },
    /// Compression completed — carries compressed messages to persist into SQLite.
    CompressionComplete {
        compressed_messages: Vec<Message>,
        /// Number of original session messages that were compressed.
        source_msg_count: usize,
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

    let mut iteration = 0u32;
    loop {
        // Check cancellation before each LLM call.
        if cancel_token.is_cancelled() {
            let _ = ui_tx.send(AgentToUiEvent::Status("Interrupted.".to_string()));
            break;
        }

        // When iteration limit is reached, ask user whether to continue.
        if iteration > 0 && iteration >= max_iterations {
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

        let _ = ui_tx.send(AgentToUiEvent::Status(
            if iteration == 0 {
                "Thinking...".to_string()
            } else {
                format!("Thinking... (iteration {})", iteration + 1)
            }
        ));

        // Check for queued interjections before LLM call.
        while let Ok(ev) = ui_rx.try_recv() {
            if let ui_event::UiToAgentEvent::Interjection(text) = ev {
                messages.push(Message::user(&text));
                let _ = ui_tx.send(AgentToUiEvent::Status(
                    format!("💬 User: {}", text.trim())
                ));
            }
        }

        // Stream LLM response.
        let (llm_tx, mut llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

        let provider_clone = Arc::clone(&provider);
        let msgs = messages.clone();
        // In planning mode, first iteration omits tool schemas so the LLM
        // can only respond with text (the plan). Subsequent iterations use real schemas.
        let schemas = if planning_mode && iteration == 0 {
            vec![]
        } else {
            tool_schemas.clone()
        };
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
                    total_usage.prompt_tokens += usage.prompt_tokens;
                    total_usage.completion_tokens += usage.completion_tokens;
                    total_usage.total_tokens += usage.total_tokens;
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

        // Push assistant message with tool calls.
        let assistant_msg = Message::Assistant {
            content: full_text.clone(), // Clone to keep full_text for workflow advancement check
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

            // Skip truncated tool calls — return error so LLM can retry.
            if truncated_ids.contains(&tc.id) {
                // Special handling for file_write with large content
                let is_file_write = tc.name == "file_write";
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

            let _ = ui_tx.send(AgentToUiEvent::Status(format!(
                "Running tool: {}",
                tc.name
            )));

            // ── Workflow validation before execution ──
            if let Some(ref engine_arc) = workflow_engine {
                let engine = engine_arc.lock().await;
                
                // Parse tool arguments for validation
                let args_value = if !tc.arguments.trim().is_empty() {
                    serde_json::from_str::<serde_json::Value>(&tc.arguments).unwrap_or(serde_json::json!({}))
                } else {
                    serde_json::json!({})
                };
                
                // Validate tool call against current workflow step
                if let Err(e) = engine.validate_tool_call(&tc.name, &args_value) {
                    tracing::warn!("Workflow validation failed for tool '{}': {}", tc.name, e);
                    
                    // Return error to LLM
                    let result_msg = Message::ToolResult {
                        tool_call_id: tc.id.clone(),
                        content: format!("❌ Workflow Restriction: {}\n\n💡 Please follow the current workflow step requirements.", e),
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
                        let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("<unknown>");
                        
                        // Determine write strategy based on content size
                        let strategy_detail = if content_len > 1024 * 1024 {  // > 1MB
                            let chunk_count = (content_len + 512 * 1024 - 1) / (512 * 1024); // 512KB chunks
                            format!("Large file ({} bytes) - will use chunked write ({} chunks of 512KB)", 
                                   content_len, chunk_count)
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
            }

            let tool = match tool_registry.get(&tc.name) {
                Some(t) => t,
                None => {
                    let available = tool_registry.names().join(", ");
                    // Find similar tool names (simple string matching)
                    let mut suggestions: Vec<&str> = Vec::new();
                    for name in tool_registry.names() {
                        if name.starts_with(&tc.name[..tc.name.len().min(3)]) ||
                           tc.name.starts_with(&name[..name.len().min(3)]) {
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
                    
                    tracing::warn!("Unknown tool requested: '{}'. Available: {}", tc.name, available);
                    
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
                    SafetyLevel::RequiresConfirmation | SafetyLevel::Dangerous => {
                        let tm = trust_manager.lock().unwrap();
                        !tm.can_skip_confirmation(&tc.name, safety_level)
                    }
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
                        // Provide helpful guidance with examples
                        let example = match tc.name.as_str() {
                            "file_read" => "{\"path\": \"src/main.rs\", \"limit\": 100}",
                            "file_write" => "{\"path\": \"output.txt\", \"content\": \"Hello World\"}",
                            "file_patch" => "{\"path\": \"src/lib.rs\", \"edits\": [{\"old\": \"...\", \"new\": \"...\"}]}",
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
                            tc.name,
                            parse_err,
                            example
                        );
                        
                        tracing::warn!(
                            "Tool argument parse error for '{}': {} | Raw: {}",
                            tc.name,
                            parse_err,
                            if tc.arguments.len() > 100 {
                                &tc.arguments[..100]
                            } else {
                                &tc.arguments
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
                    let _ = ui_tx.send(AgentToUiEvent::Status(
                        format!("💬 User (before tool): {}", text.trim())
                    ));
                }
            }

            // Send ToolProgress event to indicate execution starting
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

            let result = tool.execute(args, &tool_ctx).await;

            // Send completion progress event
            let _ = ui_tx.send(AgentToUiEvent::ToolProgress {
                tool_call_id: tc.id.clone(),
                tool_name: tc.name.clone(),
                message: "Completed".to_string(),
                progress_percent: Some(100),
            });

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

        // ── Workflow Step Advancement Logic (after each iteration) ──
        // Check if we should advance to the next workflow step
        if let Some(ref engine_arc) = workflow_engine {
            let mut engine = engine_arc.lock().await;
            
            // Check if AI signaled step completion via [STEP_COMPLETE] marker
            let ai_signaled_complete = full_text.contains("[STEP_COMPLETE]");
            
            // Check if key operations were completed (e.g., file creation)
            let key_operation_completed = new_messages.iter().any(|msg| {
                matches!(msg, Message::ToolResult { content, .. } if {
                    // Detect successful file_write or file_patch
                    content.contains("✅ File written") || 
                    content.contains("✅ File patched") ||
                    content.contains("Successfully created")
                })
            });
            
            // Advance step if:
            // 1. AI explicitly signaled completion, OR
            // 2. Key operation completed AND current step doesn't require user confirmation
            let should_advance = ai_signaled_complete || 
                (key_operation_completed && !engine.requires_user_confirmation());
            
            if should_advance && !engine.is_workflow_complete() {
                tracing::info!("Advancing workflow step (AI signaled: {}, Key op completed: {})", 
                    ai_signaled_complete, key_operation_completed);
                
                match engine.advance_step() {
                    Ok(has_next_step) => {
                        if has_next_step {
                            // Notify UI about step transition
                            if let Some(step_info) = get_current_step_info(&engine) {
                                let _ = ui_tx.send(AgentToUiEvent::Status(
                                    format!("✅ Step completed. Moving to: {}", step_info.step_name)
                                ));
                            }
                        } else {
                            // Workflow complete
                            let _ = ui_tx.send(AgentToUiEvent::Status(
                                "🎉 Workflow completed!".to_string()
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
        "EOF",                    // End of file unexpectedly
        "expected `,` or `}`",   // Missing closing brace
        "expected `,` or `]`",   // Missing closing bracket
        "expected `\"`",         // Unclosed string
        "control character",     // Cut off in middle of content
        "invalid escape",        // Truncated escape sequence
    ];
    
    // Check if error message matches truncation patterns
    let is_eof_error = truncation_patterns.iter().any(|pattern| error_msg.contains(pattern));
    
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

/// Helper function to get current step information from workflow engine
#[derive(Debug, Clone)]
struct StepInfo {
    workflow_name: String,
    step_num: usize,
    total_steps: usize,
    step_name: String,
}

fn get_current_step_info(engine: &crate::agent::engine::WorkflowEngine) -> Option<StepInfo> {
    if let Some(workflow) = engine.current_workflow() {
        if let Some((step_num, total_steps)) = engine.get_progress() {
            if let Some(step) = engine.current_step() {
                return Some(StepInfo {
                    workflow_name: workflow.name.clone(),
                    step_num,
                    total_steps,
                    step_name: step.name.clone(),
                });
            }
        }
    }
    None
}
