use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::llm::openai_sse::OpenAiSseParser;
use crate::llm::{LlmProvider, LlmStreamEvent, ToolSchema, context_window_for_model};
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
    pub fn new(
        model: String,
        api_key: String,
        base_url: String,
        max_tokens: Option<u32>,
        stream_usage: bool,
        disable_tools: bool,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "Failed to build custom reqwest client: {}, using default",
                    e
                );
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
        let api_messages = messages.iter().map(message_to_openai).collect::<Vec<_>>();

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

        // Debug: Log the request body for troubleshooting
        tracing::debug!("OpenAI request body: {}", serde_json::to_string_pretty(&body).unwrap_or_default());

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to send request to {}: {}", self.base_url, e);

                let error_msg = if e.is_timeout() {
                    format!(
                        "请求超时：无法连接到 {}\n\n可能原因：\n• 网络连接不稳定\n• API 服务器响应过慢\n• 防火墙阻止连接",
                        self.base_url
                    )
                } else if e.is_connect() {
                    format!(
                        "连接失败：无法连接到 {}\n\n可能原因：\n• 网络连接中断\n• DNS 解析失败\n• 防火墙/代理阻止\n• API 服务不可用",
                        self.base_url
                    )
                } else if e.is_request() {
                    format!(
                        "请求错误：{}\n\n请检查：\n• API 密钥是否正确\n• base_url 是否配置正确\n• 模型名称是否有效",
                        e
                    )
                } else {
                    format!(
                        "网络错误：{}\n\nURL: {}\n\n请检查网络连接或稍后重试。",
                        e, self.base_url
                    )
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
        let mut parser = OpenAiSseParser::new();
        let mut done_sent = false;
        let mut consecutive_errors = 0u32;
        let mut total_chunks_received = 0u32;
        const MAX_CONSECUTIVE_ERRORS: u32 = 3;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    consecutive_errors = 0;
                    total_chunks_received += 1;

                    tracing::debug!("Received chunk: {} bytes", chunk.len());

                    let chunk_str = String::from_utf8_lossy(&chunk);

                    let events = parser.parse_chunk(&chunk_str);
                    for event in events {
                        match &event {
                            LlmStreamEvent::Done { .. } => done_sent = true,
                            _ => {}
                        }
                        let _ = tx.send(event);
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
                        tracing::error!(
                            "Too many consecutive stream errors, aborting. Total chunks received before failure: {}",
                            total_chunks_received
                        );
                        let _ = tx.send(LlmStreamEvent::Error(
                            format!("网络不稳定，流式响应中断（已接收 {} 个数据块）。请检查网络连接或稍后重试。", total_chunks_received)
                        ));
                        return Ok(());
                    }

                    tracing::debug!("Skipping failed chunk, continuing to receive data...");
                    continue;
                }
            }
        }

        tracing::info!(
            "Stream ended: total_chunks={}, consecutive_errors={}",
            total_chunks_received,
            consecutive_errors
        );

        // Finalize any remaining tool calls
        for event in parser.finalize() {
            let _ = tx.send(event);
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
