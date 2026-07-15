//! Universal streaming response adapter for LLM providers.
//!
//! This module provides a unified interface for parsing streaming responses
//! from the two major LLM provider families:
//! - OpenAI-compatible (OpenAI, DeepSeek, Groq, Together AI, etc.)
//! - Anthropic (Claude series)
//!
//! All other providers are mapped to one of these two protocols.

use super::LlmStreamEvent;
use crate::message::TokenUsage;

/// A trait for adapting provider-specific streaming formats to universal events.
pub trait StreamAdapter: Send {
    /// Process a single JSON value from the stream and produce events.
    ///
    /// Returns a list of events extracted from this JSON chunk.
    fn process_chunk(&mut self, json: &serde_json::Value) -> Vec<LlmStreamEvent>;

    /// Finalize the adapter, emitting any pending events (e.g., unclosed tool calls).
    fn finalize(&mut self) -> Vec<LlmStreamEvent> {
        Vec::new() // Default: no finalization needed
    }

    /// Reset the adapter state for reuse.
    fn reset(&mut self);
}

/// OpenAI-compatible adapter (works with OpenAI, DeepSeek, Groq, Together AI, and other OpenAI-API compatible providers)
pub struct OpenAiAdapter {
    tool_call_ids: std::collections::HashMap<u64, String>,
    tool_call_names: std::collections::HashMap<u64, String>,
    active_tool_calls: std::collections::HashSet<u64>,
    argument_buffers: std::collections::HashMap<u64, String>,
    /// Reverse map: tool_call_id → index (for O(1) lookup)
    id_to_index: std::collections::HashMap<String, u64>,
}

impl OpenAiAdapter {
    pub fn new() -> Self {
        Self {
            tool_call_ids: std::collections::HashMap::new(),
            tool_call_names: std::collections::HashMap::new(),
            active_tool_calls: std::collections::HashSet::new(),
            argument_buffers: std::collections::HashMap::new(),
            id_to_index: std::collections::HashMap::new(),
        }
    }
}

impl Default for OpenAiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAdapter for OpenAiAdapter {
    fn process_chunk(&mut self, json: &serde_json::Value) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();

        // Process choices array
        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                let index = choice.get("index").and_then(|i| i.as_u64()).unwrap_or(0);

                // Check finish_reason
                if let Some(finish_reason) = choice.get("finish_reason").and_then(|f| f.as_str())
                    && (finish_reason == "tool_calls" || finish_reason == "stop")
                    && self.active_tool_calls.contains(&index)
                {
                    if let Some(id) = self.tool_call_ids.get(&index) {
                        events.push(LlmStreamEvent::ToolCallEnd { id: id.clone() });
                    }
                    self.active_tool_calls.remove(&index);
                }

                // Process delta
                if let Some(delta) = choice.get("delta") {
                    events.extend(self.process_delta(delta, index));
                }
            }
        }

        // Process usage (typically in the last chunk)
        if let Some(usage) = json.get("usage").filter(|u| !u.is_null()) {
            events.push(LlmStreamEvent::Done {
                usage: TokenUsage {
                    prompt_tokens: usage
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    completion_tokens: usage
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    total_tokens: usage
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                },
            });
        }

        events
    }

    fn reset(&mut self) {
        self.tool_call_ids.clear();
        self.tool_call_names.clear();
        self.active_tool_calls.clear();
        self.argument_buffers.clear();
        self.id_to_index.clear();
    }
}

impl OpenAiAdapter {
    fn process_delta(&mut self, delta: &serde_json::Value, index: u64) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();

        // Text content
        if let Some(content) = delta
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            events.push(LlmStreamEvent::TextDelta(content.to_string()));
        }

        // Tool calls
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(index);

                // Extract ID and name (support both flat and nested "function" structure)
                let id = tc
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("id"))
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string())
                    });

                let name = tc
                    .get("name")
                    .and_then(|n| n.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                    });

                if let (Some(id), Some(name)) = (&id, &name) {
                    // O(1) lookup using reverse map
                    let is_new = !self.id_to_index.contains_key(id.as_str());
                    if is_new {
                        self.tool_call_ids.insert(tc_index, id.clone());
                        self.tool_call_names.insert(tc_index, name.clone());
                        self.active_tool_calls.insert(tc_index);
                        self.id_to_index.insert(id.clone(), tc_index);
                        events.push(LlmStreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                    }
                }

                // Arguments (can be incremental deltas)
                let args = tc
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        tc.get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string())
                    });

                if let Some(args) = args {
                    let buffer = self.argument_buffers.entry(tc_index).or_default();
                    buffer.push_str(&args);
                    let tc_id = self
                        .tool_call_ids
                        .get(&tc_index)
                        .cloned()
                        .unwrap_or_else(|| format!("tool_call_{}", tc_index));
                    events.push(LlmStreamEvent::ToolCallArgumentsDelta {
                        id: tc_id,
                        delta: args,
                    });
                }
            }
        }

        events
    }
}

/// Anthropic adapter
pub struct AnthropicAdapter {
    block_index_to_id: std::collections::HashMap<u64, String>,
    prompt_tokens: u32,
}

impl AnthropicAdapter {
    pub fn new() -> Self {
        Self {
            block_index_to_id: std::collections::HashMap::new(),
            prompt_tokens: 0,
        }
    }
}

impl Default for AnthropicAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAdapter for AnthropicAdapter {
    fn process_chunk(&mut self, json: &serde_json::Value) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();

        // Anthropic uses event_type field or top-level type field
        let event_type = json
            .get("type")
            .or_else(|| json.get("event_type"))
            .and_then(|t| t.as_str())
            .unwrap_or("");

        match event_type {
            "message_start" => {
                if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
                    self.prompt_tokens = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                }
            }
            "content_block_start" => {
                let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
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
                            self.block_index_to_id.insert(index, id.clone());
                            events.push(LlmStreamEvent::ToolCallStart {
                                id,
                                name: name.to_string(),
                            });
                        }
                    }
                }
            }
            "content_block_delta" => {
                let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                if let Some(delta) = json.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str())
                                && !text.is_empty()
                            {
                                events.push(LlmStreamEvent::TextDelta(text.to_string()));
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|p| p.as_str())
                                && let Some(id) = self.block_index_to_id.get(&index).cloned()
                            {
                                events.push(LlmStreamEvent::ToolCallArgumentsDelta {
                                    id,
                                    delta: partial.to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                if let Some(id) = self.block_index_to_id.get(&index) {
                    events.push(LlmStreamEvent::ToolCallEnd { id: id.clone() });
                }
            }
            "message_delta" => {
                if let Some(usage) = json.get("usage") {
                    let completion_tokens = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    events.push(LlmStreamEvent::Done {
                        usage: TokenUsage {
                            prompt_tokens: self.prompt_tokens,
                            completion_tokens,
                            total_tokens: self.prompt_tokens + completion_tokens,
                        },
                    });
                }
            }
            "error" => {
                let error_type = json
                    .get("error")
                    .and_then(|e| e.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown");
                let error_msg = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error");
                events.push(LlmStreamEvent::Error(format!(
                    "[{}] {}",
                    error_type, error_msg
                )));
            }
            _ => {}
        }

        events
    }

    fn reset(&mut self) {
        self.block_index_to_id.clear();
        self.prompt_tokens = 0;
    }
}

/// Factory function to create the appropriate adapter based on provider name.
///
/// Only two protocols are supported:
/// - "anthropic" or "claude" → AnthropicAdapter
/// - Everything else → OpenAiAdapter (covers OpenAI, DeepSeek, Groq, etc.)
pub fn create_adapter(provider: &str) -> Box<dyn StreamAdapter> {
    match provider.to_lowercase().as_str() {
        "anthropic" | "claude" => Box::new(AnthropicAdapter::new()),
        _ => Box::new(OpenAiAdapter::new()), // Default to OpenAI format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_text_response() {
        let mut adapter = OpenAiAdapter::new();
        let json = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello"}
            }]
        });
        let events = adapter.process_chunk(&json);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "Hello"))
        );
    }

    #[test]
    fn test_anthropic_text_response() {
        let mut adapter = AnthropicAdapter::new();
        let json = serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "text_delta",
                "text": "Hello"
            }
        });
        let events = adapter.process_chunk(&json);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "Hello"))
        );
    }

    #[test]
    fn test_adapter_factory() {
        // OpenAI format (default for unknown providers)
        let mut openai_adapter = create_adapter("openai");
        let json = serde_json::json!({"choices": [{"delta": {"content": "test"}}]});
        let events = openai_adapter.process_chunk(&json);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "test"))
        );

        // DeepSeek uses OpenAI format
        let mut deepseek_adapter = create_adapter("deepseek");
        let json = serde_json::json!({"choices": [{"delta": {"content": "test"}}]});
        let events = deepseek_adapter.process_chunk(&json);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "test"))
        );

        // Anthropic format
        let mut anthropic_adapter = create_adapter("anthropic");
        let json = serde_json::json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "test"}});
        let events = anthropic_adapter.process_chunk(&json);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "test"))
        );

        // Claude alias also uses Anthropic format
        let mut claude_adapter = create_adapter("claude");
        let json = serde_json::json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "test"}});
        let events = claude_adapter.process_chunk(&json);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "test"))
        );
    }
}
