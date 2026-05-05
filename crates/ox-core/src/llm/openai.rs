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
    stream_usage: bool,
    disable_tools: bool,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(model: String, api_key: String, base_url: String, max_tokens: Option<u32>, stream_usage: bool, disable_tools: bool) -> Self {
        // Build client with timeout settings for better stability
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300)) // 5 minutes total timeout
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to build custom reqwest client: {}, using default", e);
                reqwest::Client::new()
            });
        
        Self {
            model,
            api_key,
            base_url: if base_url.is_empty() {
                "https://api.openai.com/v1".into()
            } else {
                base_url
            },
            max_tokens,
            stream_usage,
            disable_tools,
            client,
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
        });

        if self.stream_usage {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if !tools.is_empty() && !self.disable_tools {
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
            .await;
        
        // Handle network errors with detailed logging
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to send request to {}: {}", self.base_url, e);
                
                // Provide helpful error messages based on error type
                let error_msg = if e.is_timeout() {
                    format!("请求超时：无法连接到 {}\n\n可能原因：\n• 网络连接不稳定\n• API 服务器响应过慢\n• 防火墙阻止连接", self.base_url)
                } else if e.is_connect() {
                    format!("连接失败：无法连接到 {}\n\n可能原因：\n• 网络连接中断\n• DNS 解析失败\n• 防火墙/代理阻止\n• API 服务不可用", self.base_url)
                } else if e.is_request() {
                    format!("请求错误：{}\n\n请检查：\n• API 密钥是否正确\n• base_url 是否配置正确\n• 模型名称是否有效", e)
                } else {
                    format!("网络错误：{}\n\nURL: {}\n\n请检查网络连接或稍后重试。", e, self.base_url)
                };
                
                let _ = tx.send(LlmStreamEvent::Error(error_msg));
                return Ok(());
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            let err_msg = format!("OpenAI API error {status}: {body_text}");
            tracing::error!("{}", err_msg);
            let _ = tx.send(LlmStreamEvent::Error(err_msg));
            return Ok(());
        }

        let mut stream = resp.bytes_stream();
        let mut tool_call_index_to_id: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        let mut done_sent = false;
        let mut pending_data = String::new();
        let mut line_buf = String::new();
        let mut consecutive_errors = 0u32;
        let mut total_chunks_received = 0u32;
        const MAX_CONSECUTIVE_ERRORS: u32 = 3;

        // SSE parsing: process char by char to handle JSON containing newlines.
        // Event boundary is empty line, not newlines in JSON content.
        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    consecutive_errors = 0; // Reset error counter on success
                    total_chunks_received += 1;
                    
                    // Log raw chunk size for debugging
                    tracing::debug!("Received chunk: {} bytes", chunk.len());
                    
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    
                    // Log first 100 chars of chunk for debugging
                    if chunk_str.len() > 0 {
                        let preview: String = chunk_str.chars().take(100).collect();
                        tracing::debug!("Chunk preview: {}", preview);
                    }

                    for ch in chunk_str.chars() {
                        if ch == '\n' {
                            let line = line_buf.trim_end_matches('\r').to_string();
                            line_buf.clear();

                            if line.is_empty() {
                                // Empty line - process accumulated data
                                if !pending_data.is_empty() && pending_data.trim() != "[DONE]" {
                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&pending_data) {
                                        if process_openai_chunk(&json, &tx, &mut tool_call_index_to_id) {
                                            done_sent = true;
                                        }
                                    } else {
                                        tracing::debug!("Failed to parse SSE data: {}", pending_data);
                                    }
                                }
                                pending_data.clear();
                            } else if let Some(content) = line.strip_prefix("data: ") {
                                pending_data.push_str(content);
                            } else if let Some(content) = line.strip_prefix("data:") {
                                pending_data.push_str(content.trim_start());
                            }
                        } else {
                            line_buf.push(ch);
                        }
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(
                        "Stream chunk error (consecutive: {}/{}): {} - Error type: {:?}",
                        consecutive_errors,
                        MAX_CONSECUTIVE_ERRORS,
                        e,
                        e
                    );
                    
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::error!("Too many consecutive stream errors, aborting. Total chunks received before failure: {}", total_chunks_received);
                        let _ = tx.send(LlmStreamEvent::Error(
                            format!("网络不稳定，流式响应中断（已接收 {} 个数据块）。请检查网络连接或稍后重试。", total_chunks_received)
                        ));
                        return Ok(());
                    }
                    
                    // Log that we're skipping this chunk
                    tracing::debug!("Skipping failed chunk, continuing to receive data...");
                    
                    // Skip this chunk and continue
                    continue;
                }
            }
        }

        tracing::info!("Stream ended: total_chunks={}, consecutive_errors={}", total_chunks_received, consecutive_errors);

        // Process any remaining data at stream end
        if !pending_data.is_empty() && pending_data.trim() != "[DONE]" {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&pending_data) {
                if process_openai_chunk(&json, &tx, &mut tool_call_index_to_id) {
                    done_sent = true;
                }
            } else {
                tracing::debug!("Failed to parse remaining data: {}", pending_data);
            }
        }

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
        tracing::info!("Received usage chunk: {:?}", usage);
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
        tracing::debug!("No choices in chunk: {}", json);
        return false;
    };

    for choice in choices {
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let Some(delta) = delta else {
            tracing::debug!("No delta in choice");
            continue;
        };

        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                tracing::debug!("Sending TextDelta: {} chars", content.len());
                let _ = tx.send(LlmStreamEvent::TextDelta(content.to_string()));
            } else {
                tracing::debug!("Empty content in delta");
            }
        } else {
            tracing::debug!("No content field in delta");
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);

                if let Some(id) = tc.get("id").and_then(|i| i.as_str())
                    && !id.is_empty() {
                        index_to_id.insert(index, id.to_string());
                    }

                let id = if let Some(id) = index_to_id.get(&index) {
                    id.clone()
                } else {
                    let fallback_id = format!("tc_{index}");
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
                        let args_str = if tc.arguments.trim().is_empty() {
                            "{}".to_string()
                        } else {
                            match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                                Ok(_) => tc.arguments.clone(),
                                Err(e) => {
                                    tracing::warn!("Invalid tool arguments JSON for '{}', sending empty object: {e}", tc.name);
                                    "{}".to_string()
                                }
                            }
                        };
                        serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": args_str,
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
