use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::llm::{context_window_for_model, LlmProvider, LlmStreamEvent, ToolSchema};
use crate::message::{Message, TokenUsage};

pub struct OpenAiProvider {
    model: String,
    api_key: String,
    base_url: String,
    max_tokens: Option<u32>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(model: String, api_key: String, base_url: String, max_tokens: Option<u32>) -> Self {
        Self {
            model,
            api_key,
            base_url: if base_url.is_empty() {
                "https://api.openai.com/v1".into()
            } else {
                base_url
            },
            max_tokens,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    async fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tx: mpsc::UnboundedSender<LlmStreamEvent>,
    ) -> Result<()> {
        let api_messages = messages
            .iter()
            .map(message_to_openai)
            .collect::<Vec<_>>();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": api_messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if !tools.is_empty() {
            let tool_defs: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tool_defs);
        }

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            let _ = tx.send(LlmStreamEvent::Error(format!(
                "OpenAI API error {status}: {body_text}"
            )));
            return Ok(());
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut tool_call_index_to_id: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        let mut done_sent = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines using drain to avoid reallocating the buffer.
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        continue;
                    }

                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        if process_openai_chunk(&json, &tx, &mut tool_call_index_to_id) {
                            done_sent = true;
                        }
                    }
                }
            }
        }

        // Only send Done if process_openai_chunk didn't already send one
        // (e.g. API didn't include usage in stream, or non-OpenAI compatible API).
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

/// Returns `true` if a `LlmStreamEvent::Done` was sent (i.e. usage chunk detected).
fn process_openai_chunk(
    json: &serde_json::Value,
    tx: &mpsc::UnboundedSender<LlmStreamEvent>,
    index_to_id: &mut std::collections::HashMap<u64, String>,
) -> bool {
    // Check for usage in the final chunk.
    if let Some(usage) = json.get("usage").filter(|u| !u.is_null()) {
        let _ = tx.send(LlmStreamEvent::Done {
            usage: TokenUsage {
                prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: usage["total_tokens"].as_u64().unwrap_or(0) as u32,
            },
        });
        return true;
    }

    let Some(choices) = json.get("choices").and_then(|c| c.as_array()) else {
        return false;
    };

    for choice in choices {
        // Some compatible APIs use "message" instead of "delta" (e.g. non-streaming mixed in).
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let Some(delta) = delta else {
            continue;
        };

        // Text content.
        if let Some(content) = delta.get("content").and_then(|c| c.as_str())
            && !content.is_empty() {
                let _ = tx.send(LlmStreamEvent::TextDelta(content.to_string()));
            }

        // Tool calls.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                tracing::debug!("Tool call chunk index={index}: {}", serde_json::to_string(tc).unwrap_or_default());

                // First chunk for this tool call includes "id" and "function.name".
                if let Some(id) = tc.get("id").and_then(|i| i.as_str())
                    && !id.is_empty() {
                        index_to_id.insert(index, id.to_string());
                    }

                // Ensure an id exists for this index (fallback to generated id).
                let id = if let Some(id) = index_to_id.get(&index) {
                    id.clone()
                } else {
                    let fallback_id = format!("tc_{index}");
                    tracing::debug!("No tool call id for index {index}, using fallback: {fallback_id}");
                    index_to_id.insert(index, fallback_id.clone());
                    fallback_id
                };

                // Check for tool call name/arguments.
                // Standard OpenAI format: tc.function.name / tc.function.arguments
                // Some compatible APIs: tc.name / tc.arguments (flat)
                let func = tc.get("function");
                let name_src = func.and_then(|f| f.get("name")).or_else(|| tc.get("name"));
                let args_src = func.and_then(|f| f.get("arguments")).or_else(|| tc.get("arguments"));

                if let Some(name_val) = name_src.and_then(|n| n.as_str()) {
                    if !name_val.is_empty() {
                        let _ = tx.send(LlmStreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name_val.to_string(),
                        });
                    } else {
                        tracing::warn!("Tool call with empty name at index {index}, skipping");
                    }
                }
                // Arguments delta.
                if let Some(args) = args_src.and_then(|a| a.as_str())
                    && !args.is_empty() {
                        let _ = tx.send(LlmStreamEvent::ToolCallArgumentsDelta {
                            id: id.clone(),
                            delta: args.to_string(),
                        });
                    }

                // finish_reason == "tool_calls" signals end (checked at choice level).
                if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str())
                    && finish == "tool_calls" {
                        let _ = tx.send(LlmStreamEvent::ToolCallEnd { id });
                    }
            }
        }
    }
    false
}

fn message_to_openai(msg: &Message) -> serde_json::Value {
    match msg {
        Message::System { content } => serde_json::json!({
            "role": "system",
            "content": content,
        }),
        Message::User { content } => serde_json::json!({
            "role": "user",
            "content": content,
        }),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut obj = serde_json::json!({
                "role": "assistant",
                "content": content,
            });
            if !tool_calls.is_empty() {
                let tcs: Vec<serde_json::Value> = tool_calls
                    .iter()
                    .map(|tc| {
                        serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }
                        })
                    })
                    .collect();
                obj["tool_calls"] = serde_json::Value::Array(tcs);
            }
            obj
        }
        Message::ToolResult {
            tool_call_id,
            content,
        } => serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }),
    }
}
