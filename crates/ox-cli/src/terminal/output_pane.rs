use ratatui::text::Line;

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum OutputKind {
    User,
    Assistant,
    Tool,
    ToolResult,
    ToolLog,
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

    pub fn clear(&mut self) {
        self.lines.clear();
        self.rendered_cache.clear();
        self.cache_valid = true;
    }
}
