use ratatui::text::Line;

/// A single line in the output pane.
#[derive(Debug, Clone)]
pub enum OutputLine {
    /// Plain text line (user input echo, system messages).
    Plain(String),
    /// Styled line (prefix + content).
    Styled { prefix: String, content: String },
    /// Streaming partial — not yet finalized (LLM streaming).
    StreamingPartial(String),
    /// Markdown content — will be rendered with syntax highlighting.
    Markdown(String),
}

impl OutputLine {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Plain(s) => s,
            Self::Styled { content, .. } => content,
            Self::StreamingPartial(s) => s,
            Self::Markdown(s) => s,
        }
    }
}

/// The output pane: a scrollable buffer of lines displayed in the upper region.
///
/// Performance: maintains a `rendered_cache` of pre-rendered ratatui `Line`s.
/// Only new/changed lines are rendered; cached lines are reused across frames.
/// Only visible lines are passed to the Paragraph widget.
pub struct OutputPane {
    pub lines: Vec<OutputLine>,
    /// Pre-rendered ratatui Lines, indexed 1:1 with `lines`.
    /// `None` entries mean "not yet rendered" and will be rendered on demand.
    rendered_cache: Vec<Option<Vec<Line<'static>>>>,
    /// Whether the cache is fully in sync with `lines`.
    cache_valid: bool,
    /// Width used for last rendering — if changed, invalidate all cache.
    last_output_width: usize,
}

impl OutputPane {
    /// Maximum lines to keep in the output buffer to prevent unbounded memory growth.
    const MAX_LINES: usize = 2000;
    /// Maximum characters per line. Longer lines are truncated.
    const MAX_LINE_LEN: usize = 5000;

    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            rendered_cache: Vec::new(),
            cache_valid: false,
            last_output_width: 0,
        }
    }

    /// Push a complete line, trimming excess if necessary.
    pub fn push_line(&mut self, line: OutputLine) {
        let truncated = self.truncate_line(line);
        self.lines.push(truncated);
        self.rendered_cache.push(None); // new line = not yet rendered
        self.cache_valid = false;
        self.trim_excess();
    }

    /// Push a system-style message.
    pub fn push_system(&mut self, msg: &str) {
        let msg = if msg.len() > Self::MAX_LINE_LEN {
            let end = Self::safe_char_boundary(msg, Self::MAX_LINE_LEN);
            format!("{}…[truncated]", &msg[..end])
        } else {
            msg.to_string()
        };
        self.lines.push(OutputLine::Styled {
            prefix: "[system]".to_string(),
            content: msg,
        });
        self.rendered_cache.push(None);
        self.cache_valid = false;
        self.trim_excess();
    }

    /// Append a streaming text chunk.
    pub fn push_streaming_chunk(&mut self, chunk: &str) {
        if !chunk.contains('\n') {
            match self.lines.last_mut() {
                Some(OutputLine::StreamingPartial(s)) => {
                    s.push_str(chunk);
                    if s.len() > Self::MAX_LINE_LEN {
                        let end = Self::safe_char_boundary(s, Self::MAX_LINE_LEN);
                        s.truncate(end);
                        s.push_str("…[truncated]");
                    }
                    if let Some(c) = self.rendered_cache.last_mut() {
                        *c = None;
                    }
                }
                _ => {
                    self.lines.push(OutputLine::StreamingPartial(chunk.to_string()));
                    self.rendered_cache.push(None);
                }
            }
            self.cache_valid = false;
            return;
        }

        let mut remaining = chunk;
        while let Some(pos) = remaining.find('\n') {
            let before = &remaining[..pos];
            match self.lines.last_mut() {
                Some(OutputLine::StreamingPartial(s)) => {
                    s.push_str(before);
                }
                _ => {
                    if !before.is_empty() {
                        self.lines.push(OutputLine::StreamingPartial(before.to_string()));
                        self.rendered_cache.push(None);
                    }
                }
            }
            self.finalize_streaming();
            remaining = &remaining[pos + 1..];
        }
        if !remaining.is_empty() {
            match self.lines.last_mut() {
                Some(OutputLine::StreamingPartial(s)) => {
                    s.push_str(remaining);
                    if let Some(c) = self.rendered_cache.last_mut() {
                        *c = None;
                    }
                }
                _ => {
                    self.lines.push(OutputLine::StreamingPartial(remaining.to_string()));
                    self.rendered_cache.push(None);
                }
            }
        }
        self.cache_valid = false;
        self.trim_excess();
    }

    /// Convert any trailing StreamingPartial to a Markdown line.
    pub fn finalize_streaming(&mut self) {
        if let Some(OutputLine::StreamingPartial(_)) = self.lines.last() {
            // Swap the last line to Markdown in-place to avoid clone.
            if let Some(OutputLine::StreamingPartial(s)) = self.lines.pop() {
                self.lines.push(OutputLine::Markdown(s));
                // Invalidate the cache entry for this line.
                if let Some(c) = self.rendered_cache.last_mut() {
                    *c = None;
                }
            }
        }
        self.cache_valid = false;
    }

    /// Get the rendered Lines for the visible window only.
    /// Returns `(visible_lines, total_rendered_lines)`.
    ///
    /// Only lines within the visible window are cloned; the rest stay in cache.
    /// `inner_height` is the display area height in lines.
    /// `scroll_offset` is 0 = bottom (most recent), increasing = scroll up.
    pub fn get_visible_lines(
        &mut self,
        output_width: usize,
        inner_height: usize,
        scroll_offset: u16,
        mut render_fn: impl FnMut(&OutputLine, usize) -> Vec<Line<'static>>,
    ) -> (Vec<Line<'static>>, usize) {
        // Populate cache for all entries.
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

        // Calculate total rendered lines and visible window.
        let total: usize = self.rendered_cache
            .iter()
            .map(|e| e.as_ref().map_or(0, |v| v.len()))
            .sum();

        let max_scroll = total.saturating_sub(inner_height);
        let effective_scroll = max_scroll.saturating_sub(scroll_offset as usize);
        let visible_start = total.saturating_sub(inner_height + effective_scroll).min(total);
        let visible_count = inner_height.min(total);
        let visible_end = visible_start + visible_count;

        // Walk through cache, cloning only lines in [visible_start, visible_end).
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

    /// Prevents unbounded memory growth by trimming oldest lines.
    fn trim_excess(&mut self) {
        if self.lines.len() > Self::MAX_LINES {
            let drain_count = self.lines.len() - Self::MAX_LINES;
            self.lines.drain(..drain_count);
            self.rendered_cache.drain(..drain_count);
        }
    }

    /// Truncate a line if it exceeds MAX_LINE_LEN.
    fn truncate_line(&self, line: OutputLine) -> OutputLine {
        match line {
            OutputLine::Plain(s) => {
                if s.len() > Self::MAX_LINE_LEN {
                    let end = Self::safe_char_boundary(&s, Self::MAX_LINE_LEN);
                    OutputLine::Plain(format!("{}…[truncated]", &s[..end]))
                } else {
                    OutputLine::Plain(s)
                }
            }
            OutputLine::Styled { prefix, content } => {
                if content.len() > Self::MAX_LINE_LEN {
                    let end = Self::safe_char_boundary(&content, Self::MAX_LINE_LEN);
                    OutputLine::Styled {
                        prefix,
                        content: format!("{}…[truncated]", &content[..end]),
                    }
                } else {
                    OutputLine::Styled { prefix, content }
                }
            }
            OutputLine::StreamingPartial(s) => {
                if s.len() > Self::MAX_LINE_LEN {
                    let end = Self::safe_char_boundary(&s, Self::MAX_LINE_LEN);
                    OutputLine::StreamingPartial(format!("{}…[truncated]", &s[..end]))
                } else {
                    OutputLine::StreamingPartial(s)
                }
            }
            OutputLine::Markdown(s) => {
                if s.len() > Self::MAX_LINE_LEN {
                    let end = Self::safe_char_boundary(&s, Self::MAX_LINE_LEN);
                    OutputLine::Markdown(format!("{}…[truncated]", &s[..end]))
                } else {
                    OutputLine::Markdown(s)
                }
            }
        }
    }

    /// Find the safe char boundary at or before `max_byte` offset.
    fn safe_char_boundary(s: &str, max_byte: usize) -> usize {
        s.char_indices()
            .take_while(|(i, _)| *i < max_byte)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0)
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
