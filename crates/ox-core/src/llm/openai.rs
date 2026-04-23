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

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines.
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer = buffer[pos + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        continue;
                    }

                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        process_openai_chunk(&json, &tx, &mut tool_call_index_to_id);
                    }
                }
            }
        }

        let _ = tx.send(LlmStreamEvent::Done {
            usage: TokenUsage::default(),
        });

        Ok(())
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn context_window_size(&self) -> u32 {
        context_window_for_model(&self.model)
    }
}

fn process_openai_chunk(
    json: &serde_json::Value,
    tx: &mpsc::UnboundedSender<LlmStreamEvent>,
    index_to_id: &mut std::collections::HashMap<u64, String>,
) {
    // Check for usage in the final chunk.
    if let Some(usage) = json.get("usage").filter(|u| !u.is_null()) {
        let _ = tx.send(LlmStreamEvent::Done {
            usage: TokenUsage {
                prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: usage["total_tokens"].as_u64().unwrap_or(0) as u32,
            },
        });
        return;
    }

    let Some(choices) = json.get("choices").and_then(|c| c.as_array()) else {
        return;
    };

    for choice in choices {
        let Some(delta) = choice.get("delta") else {
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

                // First chunk for this tool call includes "id" and "function.name".
                if let Some(id) = tc.get("id").and_then(|i| i.as_str())
                    && !id.is_empty() {
                        index_to_id.insert(index, id.to_string());
                    }

                // Resolve the tool call id from the index map.
                let id = match index_to_id.get(&index) {
                    Some(id) => id.clone(),
                    None => {
                        // No id mapped for this index yet — skip.
                        tracing::warn!("Tool call delta with unknown index {index}, skipping");
                        continue;
                    }
                };

                if let Some(func) = tc.get("function") {
                    // If name is present, it's a new tool call start.
                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                        let _ = tx.send(LlmStreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.to_string(),
                        });
                    }
                    // Arguments delta.
                    if let Some(args) = func.get("arguments").and_then(|a| a.as_str())
                        && !args.is_empty() {
                            let _ = tx.send(LlmStreamEvent::ToolCallArgumentsDelta {
                                id: id.clone(),
                                delta: args.to_string(),
                            });
                        }
                }

                // finish_reason == "tool_calls" signals end (checked at choice level).
                if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str())
                    && finish == "tool_calls" {
                        let _ = tx.send(LlmStreamEvent::ToolCallEnd { id });
                    }
            }
        }
    }
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
