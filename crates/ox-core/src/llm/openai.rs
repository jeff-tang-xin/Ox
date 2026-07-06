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
    temperature: Option<f32>,
    top_p: Option<f32>,
    disable_tools: bool,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(
        model: String,
        api_key: String,
        base_url: String,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        top_p: Option<f32>,
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
            temperature,
            top_p,
            disable_tools,
            client,
        }
    }
}

/// Determine if a network error is retryable
fn is_network_retryable_error(error: &reqwest::Error) -> bool {
    // Retryable errors:
    // - Connection reset
    // - Timeout
    // - Network unreachable
    // - DNS resolution temporary failure

    error.is_timeout()
        || error.is_connect()
        || error.to_string().contains("connection reset")
        || error.to_string().contains("broken pipe")
        || error.to_string().contains("network unreachable")
        || error.to_string().contains("temporary failure")
}

/// Calculate exponential backoff delay for retries
fn calculate_retry_delay(consecutive_errors: u32) -> u64 {
    // Exponential backoff: 100ms, 200ms, 400ms, 800ms, 1600ms...
    // Cap at 5 seconds to avoid excessive waiting
    let base_delay = 100u64;
    let max_delay = 5000u64;

    let delay = base_delay * (2_u64.pow(consecutive_errors.min(6))); // Cap exponent at 6
    delay.min(max_delay)
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    async fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tx: mpsc::UnboundedSender<LlmStreamEvent>,
        opts: crate::llm::StreamOptions,
    ) -> Result<()> {
        // 🛡️ Auto-fix orphaned tool_call/ToolResult pairs before sending to API.
        // Instead of aborting on errors, we sanitize and proceed.
        let mut messages: Vec<Message> = messages.to_vec();
        {
            let mut assistant_call_ids = std::collections::HashSet::new();
            let mut result_call_ids = std::collections::HashSet::new();
            for msg in messages.iter() {
                match msg {
                    Message::Assistant { tool_calls, .. } => {
                        for tc in tool_calls {
                            assistant_call_ids.insert(tc.id.clone());
                        }
                    }
                    Message::ToolResult { tool_call_id, .. } => {
                        result_call_ids.insert(tool_call_id.clone());
                    }
                    _ => {}
                }
            }

            // Remove orphaned ToolResults
            let orphaned_results: Vec<_> = result_call_ids
                .iter()
                .filter(|id| !assistant_call_ids.contains(*id))
                .cloned()
                .collect();
            if !orphaned_results.is_empty() {
                tracing::warn!(
                    "[OPENAI_API] Auto-fixing {} orphaned ToolResult(s): {:?}",
                    orphaned_results.len(),
                    orphaned_results
                );
                messages.retain(|m| {
                    if let Message::ToolResult { tool_call_id, .. } = m {
                        assistant_call_ids.contains(tool_call_id)
                    } else {
                        true
                    }
                });
            }

            // Re-collect result IDs after removing orphaned results
            let mut updated_result_ids = std::collections::HashSet::new();
            for msg in messages.iter() {
                if let Message::ToolResult { tool_call_id, .. } = msg {
                    updated_result_ids.insert(tool_call_id.clone());
                }
            }

            // Remove orphaned tool_calls from Assistant messages
            for msg in messages.iter_mut() {
                if let Message::Assistant { tool_calls, .. } = msg {
                    tool_calls.retain(|tc| updated_result_ids.contains(&tc.id));
                }
            }

            // Remove empty Assistant messages (no content + no tool_calls)
            messages.retain(|m| {
                if let Message::Assistant {
                    content,
                    tool_calls,
                    ..
                } = m
                {
                    !(content.is_empty() && tool_calls.is_empty())
                } else {
                    true
                }
            });

            // Fix ordering: ensure tool_calls are immediately followed by ToolResults
            let mut i = 0;
            while i < messages.len() {
                if let Message::Assistant { tool_calls, .. } = &messages[i]
                    && !tool_calls.is_empty() {
                        let expected = tool_calls.len();
                        let expected_ids: Vec<_> =
                            tool_calls.iter().map(|tc| tc.id.clone()).collect();
                        let mut valid = true;
                        let mut found_ids = Vec::new();
                        for j in 1..=expected {
                            if i + j >= messages.len() {
                                valid = false;
                                break;
                            }
                            if let Message::ToolResult { tool_call_id, .. } = &messages[i + j] {
                                found_ids.push(tool_call_id.clone());
                            } else {
                                valid = false;
                                break;
                            }
                        }
                        if valid && found_ids == expected_ids {
                            i += expected + 1;
                            continue;
                        }
                        // Invalid sequence: strip tool_calls and remove dangling ToolResults
                        tracing::warn!(
                            "[OPENAI_API] Auto-fixing tool_call ordering at index {}",
                            i
                        );
                        if let Message::Assistant { tool_calls, .. } = &mut messages[i] {
                            tool_calls.clear();
                        }
                        // Remove dangling ToolResults that followed
                        let mut remove_count = 0;
                        for j in 1..=expected {
                            if i + j < messages.len()
                                && matches!(&messages[i + j], Message::ToolResult { .. })
                            {
                                remove_count += 1;
                            }
                        }
                        for _ in 0..remove_count {
                            if i + 1 < messages.len() {
                                messages.remove(i + 1);
                            }
                        }
                    }
                i += 1;
            }
        }

        let api_messages = messages.iter().map(message_to_openai).collect::<Vec<_>>();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": api_messages,
            "stream": true,
        });

        // Always request usage in streaming mode (supported by OpenAI + most compatible APIs)
        body["stream_options"] = serde_json::json!({ "include_usage": true });

        if let Some(max_tokens) = opts.max_tokens.or(self.max_tokens) {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        // Temperature is optional. When unset, the API uses its own default
        // (typically 0.7 for OpenAI). Only explicitly set when user configured.
        if let Some(t) = self.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        body["top_p"] = serde_json::json!(self.top_p.unwrap_or(0.8));

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
            if let Some(tc) = &opts.tool_choice {
                body["tool_choice"] = match tc {
                    crate::llm::ToolChoice::Auto => serde_json::json!("auto"),
                    crate::llm::ToolChoice::None => serde_json::json!("none"),
                    crate::llm::ToolChoice::Required => serde_json::json!("required"),
                    crate::llm::ToolChoice::Function(name) => serde_json::json!({
                        "type": "function",
                        "function": { "name": name }
                    }),
                };
            }
            if let Some(par) = opts.parallel_tool_calls {
                body["parallel_tool_calls"] = serde_json::json!(par);
            }
        }

        // Debug: Log the request body for troubleshooting
        tracing::debug!(
            "OpenAI request body: {}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );

        // Retry loop with exponential backoff for transient failures
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0u32;

        // Normalize base_url: strip trailing slash to avoid double-slash in path
        let base = self.base_url.trim_end_matches('/');
        let request_url = format!("{}/chat/completions", base);

        let resp = loop {
            attempt += 1;
            let result = self
                .client
                .post(&request_url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            match result {
                Ok(r) if r.status().is_server_error() && attempt < MAX_RETRIES => {
                    let delay = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
                    tracing::warn!(
                        "[API RETRY] 5xx, retry {}/{} in {:?}",
                        attempt,
                        MAX_RETRIES,
                        delay
                    );
                    tokio::time::sleep(delay).await;
                }
                Ok(r) => break r,
                Err(e) if attempt < MAX_RETRIES && (e.is_timeout() || e.is_connect()) => {
                    let delay = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
                    tracing::warn!(
                        "[API RETRY] Network error, retry {}/{} in {:?}: {}",
                        attempt,
                        MAX_RETRIES,
                        delay,
                        e
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(e) => {
                    let err_msg = format!("API request failed to {}: {}", request_url, e);
                    tracing::error!("{}", err_msg);
                    let _ = tx.send(LlmStreamEvent::Error(err_msg));
                    return Ok(());
                }
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            let err_msg = format!("API error {status}: {body_text}");
            tracing::error!(
                "[LLM ERROR] {} | URL: {} | Model: {}",
                err_msg,
                request_url,
                self.model
            );
            let _ = tx.send(LlmStreamEvent::Error(err_msg));
            return Ok(());
        }

        tracing::info!(
            "[LLM STREAM] Starting stream to {} (model: {})",
            self.base_url,
            self.model
        );

        let mut stream = resp.bytes_stream();
        let mut parser = OpenAiSseParser::new();
        let mut done_sent = false;
        let mut consecutive_errors = 0u32;
        let mut total_chunks_received = 0u32;
        let mut total_errors = 0u32;
        const MAX_CONSECUTIVE_ERRORS: u32 = 10; // Increased from 3 to 10 for better network tolerance
        const MAX_TOTAL_ERRORS: u32 = 20; // Total error limit to prevent infinite loops

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    consecutive_errors = 0;
                    total_chunks_received += 1;

                    if total_chunks_received % 10 == 1 {
                        // Log every 10 chunks to avoid spam
                        tracing::debug!(
                            "[LLM STREAM] Received chunk #{}: {} bytes",
                            total_chunks_received,
                            chunk.len()
                        );
                    }

                    let chunk_str = String::from_utf8_lossy(&chunk);

                    let events = parser.parse_chunk(&chunk_str);
                    for event in events {
                        if let LlmStreamEvent::Done { .. } = &event { done_sent = true }
                        let _ = tx.send(event);
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    total_errors += 1;

                    // Determine if error is retryable
                    let is_retryable = is_network_retryable_error(&e);

                    tracing::warn!(
                        "[LLM STREAM] ⚠️ Chunk error (consecutive: {}/{}, total: {}): {} - Retryable: {}",
                        consecutive_errors,
                        MAX_CONSECUTIVE_ERRORS,
                        total_errors,
                        e,
                        is_retryable
                    );

                    // Check if we should abort
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS
                        || total_errors >= MAX_TOTAL_ERRORS
                    {
                        tracing::error!(
                            "Too many stream errors (consecutive: {}, total: {}), aborting. Received {} chunks before failure.",
                            consecutive_errors,
                            total_errors,
                            total_chunks_received
                        );

                        let error_msg = if total_chunks_received == 0 {
                            "无法连接到 LLM API，请检查网络连接".to_string()
                        } else if total_chunks_received < 10 {
                            format!(
                                "流式响应过早中断（仅接收 {} 个数据块）。\n\n可能原因：\n• 网络连接不稳定\n• API 服务器超时\n• 防火墙/代理阻止\n\n建议：检查网络后重试",
                                total_chunks_received
                            )
                        } else {
                            format!(
                                "网络不稳定，流式响应中断（已接收 {} 个数据块，失败 {} 次）。\n\n已接收的内容可能不完整，建议：\n• 检查网络连接\n• 稍后重试\n• 尝试使用更小的请求",
                                total_chunks_received, total_errors
                            )
                        };

                        let _ = tx.send(LlmStreamEvent::Error(error_msg));
                        return Ok(());
                    }

                    // For retryable errors, add delay before continuing
                    if is_retryable {
                        let delay_ms = calculate_retry_delay(consecutive_errors);
                        tracing::info!(
                            "[LLM STREAM] Waiting {}ms before continuing (retryable error)...",
                            delay_ms
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }

                    tracing::debug!("Skipping failed chunk, continuing to receive data...");
                    continue;
                }
            }
        }

        tracing::info!(
            "[LLM STREAM] Stream ended: total_chunks={}, consecutive_errors={}, done_sent={}",
            total_chunks_received,
            consecutive_errors,
            done_sent
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
            reasoning_content,
        } => {
            let mut obj = serde_json::json!({
                "role": "assistant",
                "content": content,
            });
            // DeepSeek thinking mode: reasoning_content MUST be passed back to the API
            if let Some(reasoning) = reasoning_content
                && !reasoning.is_empty() {
                    obj["reasoning_content"] = serde_json::Value::String(reasoning.clone());
                }
            if !tool_calls.is_empty() {
                let tcs: Vec<serde_json::Value> = tool_calls
                    .iter()
                    .map(|tc| {
                        let args_str = if tc.arguments.trim().is_empty() {
                            "{}".to_string()
                        } else if tc.name == crate::agent::unified_action::TOOL_NAME {
                            crate::agent::tool_args_repair::repair_unified_arguments(&tc.arguments)
                                .unwrap_or_else(|| {
                                    match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                                        Ok(_) => tc.arguments.clone(),
                                        Err(e) => {
                                            tracing::warn!(
                                                "Invalid tool arguments JSON for '{}', sending empty object: {e}",
                                                tc.name
                                            );
                                            "{}".to_string()
                                        }
                                    }
                                })
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
