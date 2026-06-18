use ratatui::text::Line;

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum OutputKind {
    User,
    Assistant,
    Tool,
    ToolResult,
    ToolLog,
    Thinking,
    System,
    Error,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum OutputLine {
    User(String),
    #[allow(dead_code)]
    Assistant(String),
    Tool {
        name: String,
        detail: Option<String>,
    },
    ToolResult {
        name: String,
        summary: String,
        is_error: bool,
    },
    /// Real-time tool execution log (displayed below tool card in small font)
    ToolLog {
        tool_call_id: String,
        message: String,
        timestamp: std::time::Instant,
    },
    /// LLM reasoning stream — live carousel (one line); collapses to summary when done.
    Thinking {
        text: String,
        collapsed: bool,
        /// Shown when the model has no separate reasoning channel (status-only).
        status_hint: Option<String>,
    },
    System(String),
    Error(String),
    StreamingPartial(String),
    Markdown(String),
}

impl OutputLine {
    #[allow(dead_code)]
    pub fn kind(&self) -> OutputKind {
        match self {
            Self::User(_) => OutputKind::User,
            Self::Assistant(_) => OutputKind::Assistant,
            Self::Tool { .. } => OutputKind::Tool,
            Self::ToolResult { .. } => OutputKind::ToolResult,
            Self::ToolLog { .. } => OutputKind::ToolLog,
            Self::Thinking { .. } => OutputKind::Thinking,
            Self::System(_) => OutputKind::System,
            Self::Error(_) => OutputKind::Error,
            Self::StreamingPartial(_) => OutputKind::Assistant,
            Self::Markdown(_) => OutputKind::Assistant,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        match self {
            Self::User(s) => s,
            Self::Assistant(s) => s,
            Self::Tool { name, .. } => name,
            Self::ToolResult { summary, .. } => summary,
            Self::ToolLog { message, .. } => message,
            Self::Thinking { text, .. } => text,
            Self::System(s) => s,
            Self::Error(s) => s,
            Self::StreamingPartial(s) => s,
            Self::Markdown(s) => s,
        }
    }
}

pub struct OutputPane {
    pub lines: Vec<OutputLine>,
    rendered_cache: Vec<Option<Vec<Line<'static>>>>,
    cache_valid: bool,
    last_output_width: usize,
}

impl OutputPane {
    const MAX_LINES: usize = 2000;

    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            rendered_cache: Vec::new(),
            cache_valid: false,
            last_output_width: 0,
        }
    }

    /// Invalidate the rendered cache so lines are re-rendered on the next draw.
    pub fn invalidate_cache(&mut self) {
        self.cache_valid = false;
        for c in &mut self.rendered_cache {
            *c = None;
        }
    }

    pub fn push_line(&mut self, line: OutputLine) {
        let truncated = self.truncate_line(line);
        self.lines.push(truncated);
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    pub fn push_system(&mut self, msg: &str) {
        // 🚨 FIX: Do NOT truncate system messages
        self.lines.push(OutputLine::System(msg.to_string()));
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    pub fn push_error(&mut self, msg: &str) {
        // 🚨 FIX: Do NOT truncate error messages
        self.lines.push(OutputLine::Error(msg.to_string()));
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    /// Push a real-time tool execution log line
    pub fn push_tool_log(&mut self, tool_call_id: String, message: String) {
        self.lines.push(OutputLine::ToolLog {
            tool_call_id,
            message,
            timestamp: std::time::Instant::now(),
        });
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    pub fn push_streaming_chunk(&mut self, chunk: &str) {
        // Don't split on newlines during streaming - accumulate all chunks
        // into a single StreamingPartial to keep Markdown blocks continuous
        match self.lines.last_mut() {
            Some(OutputLine::StreamingPartial(s)) => {
                s.push_str(chunk);
                // 🚨 FIX: Do NOT truncate streaming content
                if let Some(c) = self.rendered_cache.last_mut() {
                    *c = None;
                }
            }
            _ => {
                self.lines
                    .push(OutputLine::StreamingPartial(chunk.to_string()));
                self.rendered_cache.push(None);
            }
        }
        self.cache_valid = false;
        self.trim_excess();
    }

    pub fn finalize_streaming(&mut self) {
        self.collapse_thinking();
        if let Some(OutputLine::StreamingPartial(_)) = self.lines.last() {
            if let Some(OutputLine::StreamingPartial(s)) = self.lines.pop() {
                self.lines.push(OutputLine::Markdown(s));
                if let Some(c) = self.rendered_cache.last_mut() {
                    *c = None;
                }
            }
        }
        self.cache_valid = false;
    }

    pub fn get_visible_lines(
        &mut self,
        output_width: usize,
        inner_height: usize,
        scroll_offset: u16,
        mut render_fn: impl FnMut(&OutputLine, usize) -> Vec<Line<'static>>,
    ) -> (Vec<Line<'static>>, usize) {
        if output_width != self.last_output_width {
            for entry in &mut self.rendered_cache {
                *entry = None;
            }
            self.last_output_width = output_width;
            self.cache_valid = false;
        }
        if self.rendered_cache.len() != self.lines.len() {
            self.rendered_cache.resize(self.lines.len(), None);
            self.cache_valid = false;
        }
        for i in 0..self.lines.len() {
            if self.rendered_cache[i].is_none() {
                self.rendered_cache[i] = Some(render_fn(&self.lines[i], output_width));
            }
        }
        self.cache_valid = true;

        let total: usize = self
            .rendered_cache
            .iter()
            .map(|e| e.as_ref().map_or(0, |v| v.len()))
            .sum();

        // scroll_offset = 0 means at bottom (newest), larger = scrolling up (older)
        let effective_offset = scroll_offset as usize;

        // Reserve 1 line for paragraph bottom padding to prevent last message from being cut off
        let usable_height = inner_height.saturating_sub(1);

        // visible_start: 0 = top (oldest), total-inner_height = bottom (newest)
        // We want to invert: scroll_offset=0 shows bottom, scroll_offset=max shows top
        let visible_start = if total <= usable_height {
            0
        } else {
            (total - usable_height).saturating_sub(effective_offset)
        };
        let visible_count = usable_height.min(total);
        let visible_end = (visible_start + visible_count).min(total);

        let mut result = Vec::with_capacity(visible_count);
        let mut line_idx = 0usize;
        for entry in &self.rendered_cache {
            if let Some(cached) = entry {
                let entry_start = line_idx;
                let entry_end = line_idx + cached.len();

                if entry_end > visible_start && entry_start < visible_end {
                    let local_start = visible_start.saturating_sub(entry_start);
                    let local_end = (visible_end - entry_start).min(cached.len());
                    for i in local_start..local_end {
                        result.push(cached[i].clone());
                    }
                }
                line_idx = entry_end;
            }
        }

        (result, total)
    }

    fn trim_excess(&mut self) {
        if self.lines.len() > Self::MAX_LINES {
            let drain_count = self.lines.len() - Self::MAX_LINES;
            self.lines.drain(..drain_count);
            self.rendered_cache.drain(..drain_count);
        }
    }

    fn truncate_line(&self, line: OutputLine) -> OutputLine {
        // 🚨 FIX: Do NOT truncate any content in UI.
        // Users need to see full output. Ratatui will handle wrapping and scrolling.
        // The only limit is MAX_LINES (2000 lines) to prevent memory issues.
        line
    }

    #[allow(dead_code)]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Index of the last thinking line in the timeline (if any).
    fn find_last_thinking_index(&self) -> Option<usize> {
        self.lines
            .iter()
            .rposition(|l| matches!(l, OutputLine::Thinking { .. }))
    }

    /// Append reasoning/thinking tokens to the live thinking line in the timeline.
    pub fn push_reasoning_chunk(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        if let Some(i) = self.find_last_thinking_index() {
            if let OutputLine::Thinking {
                text,
                collapsed,
                status_hint,
            } = &mut self.lines[i]
            {
                if *collapsed {
                    text.clear();
                    *collapsed = false;
                }
                *status_hint = None;
                text.push_str(chunk);
                if i < self.rendered_cache.len() {
                    self.rendered_cache[i] = None;
                }
                self.cache_valid = false;
                self.trim_excess();
                return;
            }
        }
        self.lines.push(OutputLine::Thinking {
            text: chunk.to_string(),
            collapsed: false,
            status_hint: None,
        });
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    /// Keep a live thinking row in sync with agent status when no reasoning channel exists.
    pub fn touch_thinking_status(&mut self, status: &str) {
        let hint = status.trim();
        if hint.is_empty() {
            return;
        }
        if let Some(i) = self.find_last_thinking_index() {
            if let OutputLine::Thinking {
                collapsed,
                status_hint,
                text,
            } = &mut self.lines[i]
            {
                *status_hint = Some(hint.to_string());
                if *collapsed && text.is_empty() {
                    text.push_str(hint);
                }
                if i < self.rendered_cache.len() {
                    self.rendered_cache[i] = None;
                }
                self.cache_valid = false;
                return;
            }
        }
        self.lines.push(OutputLine::Thinking {
            text: String::new(),
            collapsed: false,
            status_hint: Some(hint.to_string()),
        });
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    /// Keep at most one collapsed thinking line in the timeline.
    fn prune_extra_thinking_lines(&mut self) {
        let Some(keep) = self.find_last_thinking_index() else {
            return;
        };
        let mut new_lines = Vec::with_capacity(self.lines.len());
        let mut new_cache = Vec::with_capacity(self.rendered_cache.len());
        for (i, line) in self.lines.iter().enumerate() {
            if matches!(line, OutputLine::Thinking { .. }) && i != keep {
                continue;
            }
            new_lines.push(line.clone());
            if i < self.rendered_cache.len() {
                new_cache.push(self.rendered_cache[i].clone());
            }
        }
        if new_lines.len() != self.lines.len() {
            self.lines = new_lines;
            self.rendered_cache = new_cache;
            self.cache_valid = false;
        }
    }

    pub fn has_live_thinking(&self) -> bool {
        matches!(
            self.lines.last(),
            Some(OutputLine::Thinking {
                collapsed: false,
                ..
            })
        )
    }

    /// Force re-render of live thinking lines (carousel animation).
    pub fn invalidate_thinking_cache(&mut self) {
        for (i, line) in self.lines.iter().enumerate() {
            if matches!(
                line,
                OutputLine::Thinking {
                    collapsed: false,
                    ..
                }
            ) && i < self.rendered_cache.len()
            {
                self.rendered_cache[i] = None;
            }
        }
        self.cache_valid = false;
    }

    /// Collapse the active thinking line to a one-line summary in the timeline.
    pub fn collapse_thinking(&mut self) {
        let Some(OutputLine::Thinking {
            text,
            collapsed,
            status_hint,
        }) = self.lines.last_mut()
        else {
            return;
        };
        if *collapsed {
            return;
        }
        let summary = thinking_summary_line(text, status_hint.as_deref());
        if summary.is_empty() {
            self.lines.pop();
            if !self.rendered_cache.is_empty() {
                self.rendered_cache.pop();
            }
        } else {
            *text = summary;
            *collapsed = true;
            *status_hint = None;
            if let Some(c) = self.rendered_cache.last_mut() {
                *c = None;
            }
        }
        self.prune_extra_thinking_lines();
        self.cache_valid = false;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.rendered_cache.clear();
        self.cache_valid = true;
    }
}

fn thinking_summary_line(text: &str, status_hint: Option<&str>) -> String {
    let t = text.trim();
    if !t.is_empty() {
        if let Some(last) = t.lines().filter(|l| !l.trim().is_empty()).last() {
            let line = last.trim();
            if line.chars().count() > 120 {
                return format!("{}…", line.chars().take(119).collect::<String>());
            }
            return line.to_string();
        }
        if t.chars().count() > 120 {
            return format!("{}…", t.chars().take(119).collect::<String>());
        }
        return t.to_string();
    }
    status_hint
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Segments for live thinking carousel (one line shown at a time).
pub fn thinking_carousel_segments(text: &str, status_hint: Option<&str>) -> Vec<String> {
    let mut segments: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    if segments.is_empty() {
        if let Some(h) = status_hint.filter(|s| !s.trim().is_empty()) {
            segments.push(h.trim().to_string());
        }
        return segments;
    }
    let mut out = Vec::new();
    for seg in segments {
        if seg.chars().count() <= 72 {
            out.push(seg);
        } else {
            let chars: Vec<char> = seg.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let end = (i + 72).min(chars.len());
                out.push(chars[i..end].iter().collect());
                i = end;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_collapses_to_last_line() {
        let s = thinking_summary_line("line1\nline2\nfinal thought", None);
        assert_eq!(s, "final thought");
    }

    #[test]
    fn carousel_splits_long_lines() {
        let long = "a".repeat(100);
        let segs = thinking_carousel_segments(&long, None);
        assert!(segs.len() >= 2);
    }
}
