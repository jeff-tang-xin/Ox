pub mod interjection;
pub mod interrupt;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::llm::{LlmProvider, LlmStreamEvent};
use crate::message::{Message, ToolCall, TokenUsage};
use crate::tools::{ToolContext, ToolRegistry};

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
    cancel_token: CancellationToken,
) {
    let tool_schemas = tool_registry.schemas();
    let max_iterations = 25; // Safety limit to prevent infinite loops.

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

        // Stream LLM response.
        let (llm_tx, mut llm_rx) = mpsc::unbounded_channel::<LlmStreamEvent>();

        let provider_clone = Arc::clone(&provider);
        let msgs = messages.clone();
        let schemas = tool_schemas.clone();
        let stream_handle = tokio::spawn(async move {
            if let Err(e) = provider_clone.stream_chat(&msgs, &schemas, llm_tx).await {
                tracing::error!("LLM stream error: {e}");
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
                    let _ = stream_handle.await;
                    return;
                }
            }
        }

        let _ = stream_handle.await;

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
                    let error_msg = format!("Unknown tool: {}", tc.name);
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

            let args: serde_json::Value = serde_json::from_str(&tc.arguments)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

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
