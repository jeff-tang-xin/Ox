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
    ) -> Result<()> {
        // 🚨 CRITICAL VALIDATION: Verify tool_call/tool_result pairs before sending to API
        // This catches issues that sanitize_tool_pairs might have missed
        
        // Step 1: Collect all IDs (for basic validation)
        let mut assistant_call_ids = std::collections::HashSet::new();
        let mut result_call_ids = std::collections::HashSet::new();
        
        for msg in messages {
            match msg {
                crate::message::Message::Assistant { tool_calls, .. } => {
                    for tc in tool_calls {
                        assistant_call_ids.insert(tc.id.clone());
                    }
                }
                crate::message::Message::ToolResult { tool_call_id, .. } => {
                    result_call_ids.insert(tool_call_id.clone());
                }
                _ => {}
            }
        }
        
        // Check for orphaned ToolResults (ToolResult without matching tool_call)
        for result_id in &result_call_ids {
            if !assistant_call_ids.contains(result_id) {
                tracing::error!(
                    "[OPENAI_API_VALIDATION] ⚠️ CRITICAL: ToolResult ID '{}' has no matching tool_call! This will cause API error 400.",
                    result_id
                );
                let _ = tx.send(LlmStreamEvent::Error(
                    format!("Internal error: ToolResult references non-existent tool call '{}'. Please report this bug.", result_id)
                ));
                return Ok(());
            }
        }
        
        // Check for orphaned tool_calls (tool_call without matching ToolResult)
        // OpenAI API requires: if Assistant has tool_calls, they MUST be followed by ToolResults
        for call_id in &assistant_call_ids {
            if !result_call_ids.contains(call_id) {
                tracing::error!(
                    "[OPENAI_API_VALIDATION] ⚠️ CRITICAL: tool_call ID '{}' has no matching ToolResult! This will cause API error 'tool call result does not follow tool call'.",
                    call_id
                );
                let _ = tx.send(LlmStreamEvent::Error(
                    format!("Internal error: tool_call '{}' has no corresponding ToolResult. This indicates a bug in context sanitization.", call_id)
                ));
                return Ok(());
            }
        }
        
        // 🔍 ENHANCED VALIDATION: Check message order and pairing structure
        // OpenAI API requires strict ordering: Assistant with tool_calls must be immediately followed by ToolResults
        let mut i = 0;
        while i < messages.len() {
            if let crate::message::Message::Assistant { tool_calls, .. } = &messages[i] {
                if !tool_calls.is_empty() {
                    // This Assistant has tool_calls - verify the following messages are ToolResults
                    let expected_tool_count = tool_calls.len();
                    let mut found_results = Vec::new();
                    
                    // Check the next N messages (should all be ToolResults for this Assistant)
                    for j in 1..=expected_tool_count {
                        if i + j >= messages.len() {
                            break;
                        }
                        
                        if let crate::message::Message::ToolResult { tool_call_id, .. } = &messages[i + j] {
                            found_results.push(tool_call_id.clone());
                        } else {
                            // Found a non-ToolResult message where we expected one
                            tracing::error!(
                                "[OPENAI_API_VALIDATION] ⚠️ ORDER VIOLATION: Assistant at index {} has {} tool_calls, but message at index {} is not a ToolResult!",
                                i, expected_tool_count, i + j
                            );
                            
                            // List what tool_calls we have
                            let call_ids: Vec<_> = tool_calls.iter().map(|tc| tc.id.as_str()).collect();
                            tracing::error!(
                                "[OPENAI_API_VALIDATION] Expected ToolResults for: {:?}",
                                call_ids
                            );
                            
                            let _ = tx.send(LlmStreamEvent::Error(
                                format!("Internal error: Message ordering violation. Assistant tool_calls are not followed by ToolResults. This is a critical bug in context management.")
                            ));
                            return Ok(());
                        }
                    }
                    
                    // Verify all tool_call IDs match
                    let call_ids: Vec<_> = tool_calls.iter().map(|tc| tc.id.clone()).collect();
                    if found_results != call_ids {
                        tracing::error!(
                            "[OPENAI_API_VALIDATION] ⚠️ MISMATCH: Assistant tool_call IDs don't match ToolResult order!"
                        );
                        tracing::error!(
                            "[OPENAI_API_VALIDATION] Assistant tool_calls: {:?}",
                            call_ids
                        );
                        tracing::error!(
                            "[OPENAI_API_VALIDATION] Following ToolResults: {:?}",
                            found_results
                        );
                        
                        let _ = tx.send(LlmStreamEvent::Error(
                            format!("Internal error: Tool call/result ID mismatch. The order of ToolResults doesn't match the order of tool_calls.")
                        ));
                        return Ok(());
                    }
                    
                    // Skip past the ToolResults we just validated
                    i += expected_tool_count + 1;
                    continue;
                }
            }
            i += 1;
        }
        
        tracing::info!(
            "[OPENAI_API_VALIDATION] ✅ Validation passed: {} tool_calls, {} tool_results (all paired correctly)",
            assistant_call_ids.len(),
            result_call_ids.len()
        );

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
        const MAX_CONSECUTIVE_ERRORS: u32 = 10;  // Increased from 3 to 10 for better network tolerance
        const MAX_TOTAL_ERRORS: u32 = 20;         // Total error limit to prevent infinite loops

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    consecutive_errors = 0;
                    total_chunks_received += 1;

                    if total_chunks_received % 10 == 1 {  // Log every 10 chunks to avoid spam
                        tracing::debug!("[LLM STREAM] Received chunk #{}: {} bytes", total_chunks_received, chunk.len());
                    }

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
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS || total_errors >= MAX_TOTAL_ERRORS {
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
                                total_chunks_received,
                                total_errors
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
