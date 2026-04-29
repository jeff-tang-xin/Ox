use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::llm::{context_window_for_model, LlmProvider, LlmStreamEvent, ToolSchema};
use crate::message::{Message, TokenUsage};

pub struct AnthropicProvider {
    model: String,
    api_key: String,
    base_url: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(model: String, api_key: String, base_url: String, max_tokens: u32) -> Self {
        Self {
            model,
            api_key,
            base_url: if base_url.is_empty() {
                "https://api.anthropic.com/v1".into()
            } else {
                base_url
            },
            max_tokens,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    async fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tx: mpsc::UnboundedSender<LlmStreamEvent>,
    ) -> Result<()> {
        // Anthropic separates system message from the messages list.
        let mut system_prompt = String::new();
        let mut api_messages = Vec::new();

        for msg in messages {
            match msg {
                Message::System { content } => {
                    system_prompt.push_str(content);
                }
                other => {
                    api_messages.push(message_to_anthropic(other));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "stream": true,
            "messages": api_messages,
        });

        if !system_prompt.is_empty() {
            body["system"] = serde_json::Value::String(system_prompt);
        }

        if !tools.is_empty() {
            let tool_defs: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tool_defs);
        }

        let resp = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            let err_msg = format!("Anthropic API error {status}: {body_text}");
            tracing::error!("{}", err_msg);
            let _ = tx.send(LlmStreamEvent::Error(err_msg));
            return Ok(());
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut current_event_type = String::new();
        let mut block_index_to_id: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        let mut done_sent = false;
        let mut prompt_tokens: u32 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);

                if line.is_empty() {
                    // Empty line = end of event, reset event type.
                    current_event_type.clear();
                    continue;
                }

                if let Some(event_type) = line.strip_prefix("event: ") {
                    current_event_type = event_type.trim().to_string();
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        if process_anthropic_event(
                            &current_event_type,
                            &json,
                            &tx,
                            &mut block_index_to_id,
                            &mut prompt_tokens,
                        ) {
                            done_sent = true;
                        }
                    }
            }
        }

        // Only send Done if process_anthropic_event didn't already send one.
        if !done_sent {
            let _ = tx.send(LlmStreamEvent::Done {
                usage: TokenUsage::default(),
            });
        }

        Ok(())
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn context_window_size(&self) -> u32 {
        context_window_for_model(&self.model)
    }
}

/// Returns `true` if a `LlmStreamEvent::Done` was sent (i.e. message_delta with usage).
fn process_anthropic_event(
    event_type: &str,
    json: &serde_json::Value,
    tx: &mpsc::UnboundedSender<LlmStreamEvent>,
    block_index_to_id: &mut std::collections::HashMap<u64, String>,
    prompt_tokens: &mut u32,
) -> bool {
    match event_type {
        "message_start" => {
            if let Some(usage) = json
                .get("message")
                .and_then(|m| m.get("usage"))
            {
                *prompt_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            }
            false
        }
        "content_block_start" => {
            let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
            // Could be text or tool_use.
            if let Some(content_block) = json.get("content_block") {
                let block_type = content_block
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if block_type == "tool_use" {
                    let id = content_block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = content_block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    if !name.is_empty() {
                        block_index_to_id.insert(index, id.clone());
                        let _ = tx.send(LlmStreamEvent::ToolCallStart { id, name: name.to_string() });
                    } else {
                        tracing::warn!("Anthropic tool_use block with empty name at index {index}, skipping");
                    }
                }
            }
            false
        }
        "content_block_delta" => {
            let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
            if let Some(delta) = json.get("delta") {
                let delta_type = delta
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str())
                            && !text.is_empty() {
                                let _ = tx.send(LlmStreamEvent::TextDelta(text.to_string()));
                            }
                    }
                    "input_json_delta" => {
                        if let Some(partial) =
                            delta.get("partial_json").and_then(|p| p.as_str())
                        {
                            let id = block_index_to_id
                                .get(&index)
                                .cloned()
                                .unwrap_or_default();
                            let _ = tx.send(LlmStreamEvent::ToolCallArgumentsDelta {
                                id,
                                delta: partial.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            false
        }
        "content_block_stop" => {
            let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
            if let Some(id) = block_index_to_id.get(&index) {
                let _ = tx.send(LlmStreamEvent::ToolCallEnd { id: id.clone() });
            }
            false
        }
        "message_delta" => {
            if let Some(usage) = json.get("usage") {
                let completion_tokens = usage["output_tokens"].as_u64().unwrap_or(0) as u32;
                let _ = tx.send(LlmStreamEvent::Done {
                    usage: TokenUsage {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens,
                        total_tokens: *prompt_tokens + completion_tokens,
                    },
                });
                true
            } else {
                false
            }
        }
        "message_stop" => {
            // Streaming complete.
            false
        }
        "error" => {
            let error_msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown Anthropic error");
            let _ = tx.send(LlmStreamEvent::Error(error_msg.to_string()));
            false
        }
        _ => false
    }
}

fn message_to_anthropic(msg: &Message) -> serde_json::Value {
    match msg {
        Message::System { .. } => {
            // System messages are handled separately in stream_chat.
            serde_json::json!(null)
        }
        Message::User { content } => serde_json::json!({
            "role": "user",
            "content": content,
        }),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut blocks: Vec<serde_json::Value> = Vec::new();
            if !content.is_empty() {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": content,
                }));
            }
            for tc in tool_calls {
                let input: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": input,
                }));
            }
            serde_json::json!({
                "role": "assistant",
                "content": blocks,
            })
        }
        Message::ToolResult {
            tool_call_id,
            content,
        } => serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": content,
            }],
        }),
    }
}
