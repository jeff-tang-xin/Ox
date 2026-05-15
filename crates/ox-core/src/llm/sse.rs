//! SSE (Server-Sent Events) parsing primitives.
//!
//! Handles the SSE protocol details: event boundaries, data accumulation,
//! and multi-line JSON support.

/// SSE event buffer that accumulates `data:` lines until an empty line
/// (event boundary) is encountered.
#[derive(Debug, Default)]
pub struct SseEventBuffer {
    /// Accumulated data content (everything after `data:`)
    data: String,
    /// Current event type (from `event:` field)
    event_type: String,
    /// Whether we've seen a non-empty data line
    has_content: bool,
}

impl SseEventBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a line from the SSE stream.
    /// Returns `true` if a complete event has been accumulated (empty line received).
    pub fn push_line(&mut self, line: &str) -> bool {
        let line = line.trim_end_matches('\r');

        if line.is_empty() {
            // Empty line = event boundary
            if self.has_content && !self.is_done() {
                self.has_content = false;
                return true;
            }
            self.data.clear();
            self.event_type.clear();
            return false;
        }

        // Parse event type: `event: message`
        if let Some(event) = line.strip_prefix("event:") {
            self.event_type = event.trim().to_string();
            return false;
        }

        // Parse data: `data: ...` (note: both `data:` and `data: ` are valid)
        if let Some(data) = line.strip_prefix("data:") {
            let data_content = data.strip_prefix(' ').unwrap_or(data);
            // Add newline between multiple data lines per SSE spec
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(data_content);
            self.has_content = true;
        }

        false
    }

    /// Check if this is a `[DONE]` sentinel.
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }

    /// Take and consume the accumulated data.
    pub fn take_data(&mut self) -> String {
        let data = self.data.clone();
        self.data.clear();
        self.has_content = false;
        data
    }

    /// Get a reference to the accumulated data without consuming it.
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Get the current event type.
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    /// Check if there's pending data that hasn't been consumed.
    pub fn has_pending_data(&self) -> bool {
        self.has_content && !self.data.is_empty()
    }

    /// Reset the buffer to initial state.
    pub fn reset(&mut self) {
        self.data.clear();
        self.event_type.clear();
        self.has_content = false;
    }
}

/// Parse multiple JSON values from a string that may contain:
/// - A single JSON object: `{"foo": "bar"}`
/// - A JSON array: `[{"foo": "bar"}, {"baz": "qux"}]`
/// - Multiple JSON values separated by newlines or other delimiters
pub fn parse_json_values(
    input: &str,
) -> impl Iterator<Item = serde_json::Result<serde_json::Value>> + '_ {
    let input = input.trim();

    // Try parsing as array first
    if input.starts_with('[') {
        // Try to parse as array
        if let Ok(arr) = serde_json::from_str::<serde_json::Value>(input) {
            if let Some(items) = arr.as_array() {
                return items
                    .iter()
                    .map(|v| Ok(v.clone()))
                    .collect::<Vec<_>>()
                    .into_iter();
            }
        }
        // Fallback: try to parse each element individually
    }

    // Try single value
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(input) {
        return vec![Ok(v)].into_iter();
    }

    // Try parsing line by line for multiple JSON objects
    #[derive(Clone, Copy)]
    enum State {
        Outside,
        InObject { depth: u32 },
        InArray { depth: u32 },
    }

    let mut state = State::Outside;
    let mut current = String::new();
    let mut results: Vec<serde_json::Result<serde_json::Value>> = Vec::new();

    for ch in input.chars() {
        match state {
            State::Outside => {
                if ch.is_ascii_whitespace() {
                    continue;
                } else if ch == '{' {
                    state = State::InObject { depth: 1 };
                    current.push(ch);
                } else if ch == '[' {
                    state = State::InArray { depth: 1 };
                    current.push(ch);
                }
            }
            State::InObject { depth } | State::InArray { depth } => {
                current.push(ch);
                if ch == '{' || ch == '[' {
                    let new_depth = if ch == '{' {
                        if matches!(state, State::InObject { .. }) {
                            depth + 1
                        } else {
                            depth
                        }
                    } else {
                        if matches!(state, State::InArray { .. }) {
                            depth + 1
                        } else {
                            depth
                        }
                    };
                    state = if ch == '{' {
                        State::InObject { depth: new_depth }
                    } else {
                        State::InArray { depth: new_depth }
                    };
                } else if ch == '}' || ch == ']' {
                    let new_depth = depth.saturating_sub(1);
                    if new_depth == 0 {
                        // Complete value
                        let value_str = current.trim();
                        if !value_str.is_empty() {
                            results.push(serde_json::from_str(value_str));
                        }
                        current.clear();
                        state = State::Outside;
                    } else {
                        state = if ch == '}' {
                            State::InObject { depth: new_depth }
                        } else {
                            State::InArray { depth: new_depth }
                        };
                    }
                }
            }
        }
    }

    // Handle any remaining content - ONLY if we're back to Outside state (complete JSON)
    // If still InObject/InArray, the JSON is incomplete (truncated stream) - skip it
    if !current.trim().is_empty() && matches!(state, State::Outside) {
        results.push(serde_json::from_str(current.trim()));
    } else if !current.trim().is_empty() {
        // Log truncated JSON for debugging but don't attempt to parse
        let preview = if current.chars().count() > 100 {
            current.chars().take(100).collect::<String>()
        } else {
            current.clone()
        };
        tracing::debug!(
            "Skipping incomplete JSON ({} chars): {}",
            current.len(),
            preview
        );
    }

    results.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_event_buffer_basic() {
        let mut buf = SseEventBuffer::new();

        assert!(!buf.push_line("event: message"));
        assert!(!buf.push_line("data: {\"foo\": \"bar\"}"));
        assert!(buf.push_line("")); // Event boundary
        assert_eq!(buf.data(), "{\"foo\": \"bar\"}");
    }

    #[test]
    fn test_sse_event_buffer_multiline_data() {
        let mut buf = SseEventBuffer::new();

        buf.push_line("data: {");
        buf.push_line("data:   \"foo\": \"bar\"");
        buf.push_line("data: }");
        assert!(buf.push_line(""));
        assert_eq!(buf.data().trim(), "{\n  \"foo\": \"bar\"\n}");
    }

    #[test]
    fn test_parse_json_array() {
        let input = r#"[{"a": 1}, {"b": 2}, {"c": 3}]"#;
        let values: Vec<_> = parse_json_values(input).collect();
        assert_eq!(values.len(), 3);
        assert_eq!(values[0].as_ref().unwrap()["a"], 1);
        assert_eq!(values[1].as_ref().unwrap()["b"], 2);
        assert_eq!(values[2].as_ref().unwrap()["c"], 3);
    }

    #[test]
    fn test_parse_json_object() {
        let input = r#"{"foo": "bar"}"#;
        let values: Vec<_> = parse_json_values(input).collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].as_ref().unwrap()["foo"], "bar");
    }

    #[test]
    fn test_truncated_json_is_skipped() {
        // Simulate a truncated JSON stream (common network issue)
        let input = r#"{"choices":[{"delta":{"content":"Hello world, this is a long string that gets cut off..."}}]"#;
        let values: Vec<_> = parse_json_values(input).collect();
        // Should not panic, should return empty or error (not crash with EOF)
        assert!(values.is_empty() || values.iter().all(|r| r.is_err()));
    }

    #[test]
    fn test_complete_and_truncated_mixed() {
        // First JSON complete, second truncated
        let input = r#"{"a": 1}
{"b": "incomplete""#;
        let values: Vec<_> = parse_json_values(input).collect();
        // Should successfully parse the first one, skip the incomplete second one
        assert_eq!(values.len(), 1); // Only the complete one is returned
        assert!(values[0].is_ok());
        assert_eq!(values[0].as_ref().unwrap()["a"], 1);
    }
}
