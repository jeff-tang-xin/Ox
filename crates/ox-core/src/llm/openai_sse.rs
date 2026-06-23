//! OpenAI-compatible SSE parser.

use super::LlmStreamEvent;
use super::sse::{SseEventBuffer, parse_json_values};
use crate::message::TokenUsage;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct OpenAiSseParser {
    buffer: SseEventBuffer,
    tool_call_ids: HashMap<u64, String>,
    tool_call_names: HashMap<u64, String>,
    active_tool_calls: HashSet<u64>,
    seen_indices: HashSet<u64>,
    argument_buffers: HashMap<u64, String>,
    /// Reverse map: tool_call_id → index (for O(1) lookup)
    id_to_index: HashMap<String, u64>,
}

impl Default for OpenAiSseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiSseParser {
    pub fn new() -> Self {
        Self {
            buffer: SseEventBuffer::new(),
            tool_call_ids: HashMap::new(),
            tool_call_names: HashMap::new(),
            active_tool_calls: HashSet::new(),
            seen_indices: HashSet::new(),
            argument_buffers: HashMap::new(),
            id_to_index: HashMap::new(),
        }
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<LlmStreamEvent> {
        let line = line.trim_end_matches('\r');
        if self.buffer.push_line(line) {
            let data = self.buffer.take_data();
            self.process_raw_data(&data)
        } else {
            Vec::new()
        }
    }

    pub fn parse_chunk(&mut self, chunk: &str) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();
        for line in chunk.lines() {
            events.extend(self.parse_line(line));
        }
        events
    }

    fn process_raw_data(&mut self, data: &str) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return events;
        }
        for result in parse_json_values(data) {
            match result {
                Ok(json) => events.extend(self.process_json_value(&json)),
                Err(e) => tracing::warn!("Failed to parse SSE data: {}", e),
            }
        }
        events
    }

    fn process_json_value(&mut self, json: &serde_json::Value) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();
        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                let index = choice.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                self.seen_indices.insert(index);

                if let Some(finish_reason) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    if finish_reason == "tool_calls" || finish_reason == "stop" {
                        if self.active_tool_calls.contains(&index) {
                            if let Some(id) = self.tool_call_ids.get(&index) {
                                events.push(LlmStreamEvent::ToolCallEnd { id: id.clone() });
                            }
                            self.active_tool_calls.remove(&index);
                        }
                    }
                }

                if let Some(delta) = choice.get("delta") {
                    events.extend(self.process_delta(delta, index));
                }
            }
        }

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

    fn process_delta(&mut self, delta: &serde_json::Value, index: u64) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();

        if let Some(content) = delta
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            events.push(LlmStreamEvent::TextDelta(content.to_string()));
        }

        // DeepSeek / compatible reasoning fields
        for key in ["reasoning_content", "reasoning", "thinking"] {
            if let Some(reasoning) = delta
                .get(key)
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
            {
                events.push(LlmStreamEvent::ReasoningDelta(reasoning.to_string()));
                break;
            }
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(index);

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
                    let buffer = self
                        .argument_buffers
                        .entry(tc_index)
                        .or_insert_with(String::new);
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

    pub fn finalize(self) -> Vec<LlmStreamEvent> {
        let mut events = Vec::new();
        for index in self.active_tool_calls {
            if let Some(id) = self.tool_call_ids.get(&index) {
                events.push(LlmStreamEvent::ToolCallEnd { id: id.clone() });
            }
        }
        events
    }

    pub fn reset(&mut self) {
        self.buffer.reset();
        self.tool_call_ids.clear();
        self.tool_call_names.clear();
        self.active_tool_calls.clear();
        self.seen_indices.clear();
        self.argument_buffers.clear();
        self.id_to_index.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_events(sse_data: &str) -> Vec<LlmStreamEvent> {
        let mut parser = OpenAiSseParser::new();
        let mut events = Vec::new();
        for line in sse_data.lines() {
            events.extend(parser.parse_line(line));
        }
        // Add empty line to trigger event boundary (SSE protocol requirement)
        events.extend(parser.parse_line(""));
        events.extend(parser.finalize());
        events
    }

    #[test]
    fn test_simple_text_response() {
        let data = r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}"#;
        let events = parse_events(data);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::TextDelta(s) if s == "Hello"))
        );
    }

    #[test]
    fn test_tool_call_with_finish_reason() {
        // Two separate SSE events (note the empty line between them)
        let data = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_abc","function":{"name":"test","arguments":"{}"}}]}}]}

data: {"choices":[{"index":0,"finish_reason":"tool_calls"}]}"#;
        let events = parse_events(data);
        eprintln!("Events received: {:?}", events);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmStreamEvent::ToolCallEnd { id } if id == "call_abc"))
        );
    }

    #[test]
    fn test_json_array_response() {
        let data = r#"data: [{"choices":[{"index":0,"delta":{"content":"A"}}]},{"choices":[{"index":0,"delta":{"content":"B"}}]}]"#;
        let events = parse_events(data);
        let text_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                LlmStreamEvent::TextDelta(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(text_events.contains(&"A".to_string()));
        assert!(text_events.contains(&"B".to_string()));
    }
}
