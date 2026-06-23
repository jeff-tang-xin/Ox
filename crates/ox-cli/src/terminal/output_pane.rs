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

/// Fixed-height thinking dock at the bottom of the chat pane (not in scroll history).
#[derive(Debug, Clone, Default)]
pub struct LiveThinking {
    pub text: String,
    pub status_hint: Option<String>,
}

/// Collapsed think strip (status only, no reasoning body).
pub const THINK_PANE_SLIM_HEIGHT: u16 = 3;
/// Compact terminals fall back to this many think lines inside chat.
pub const THINKING_DOCK_LINES: usize = 2;
/// Main column height split — chat largest, think medium.
pub const CHAT_THINK_HEIGHT_RATIO: (u32, u32) = (7, 3);
/// Session sidebar width — smallest slice of main row (~12%).
pub const SESSION_WIDTH_PERCENT: u16 = 12;
pub const SESSION_WIDTH_MIN: u16 = 12;
pub const SESSION_WIDTH_MAX: u16 = 20;

/// How much vertical space the think pane should take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkPaneMode {
    /// No thinking activity — pane hidden, chat uses full height.
    Hidden,
    /// Status / waiting only — slim strip.
    Slim,
    /// Reasoning stream — medium slice (ratio vs chat).
    Expanded,
}

pub struct OutputPane {
    pub lines: Vec<OutputLine>,
    /// Live LLM reasoning — pinned to chat bottom; cleared when streaming/tool output starts.
    pub live_thinking: Option<LiveThinking>,
    rendered_cache: Vec<Option<Vec<Line<'static>>>>,
    cache_valid: bool,
    last_output_width: usize,
}

impl OutputPane {
    const MAX_LINES: usize = 2000;

    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            live_thinking: None,
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
            if matches!(self.lines[i], OutputLine::Thinking { .. }) {
                continue;
            }
            if self.rendered_cache[i].is_none() {
                self.rendered_cache[i] = Some(render_fn(&self.lines[i], output_width));
            }
        }
        self.cache_valid = true;

        let total: usize = self
            .rendered_cache
            .iter()
            .zip(self.lines.iter())
            .filter(|(_, line)| !matches!(line, OutputLine::Thinking { .. }))
            .map(|(e, _)| e.as_ref().map_or(0, |v| v.len()))
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
        for (entry, line) in self.rendered_cache.iter().zip(self.lines.iter()) {
            if matches!(line, OutputLine::Thinking { .. }) {
                continue;
            }
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

    /// Append reasoning/thinking tokens to the bottom thinking dock.
    pub fn push_reasoning_chunk(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let dock = self
            .live_thinking
            .get_or_insert_with(LiveThinking::default);
        dock.text.push_str(chunk);
    }

    /// Record tool execution in the think pane while reasoning stream is idle.
    pub fn note_tool_activity(&mut self, tool_name: &str) {
        let dock = self
            .live_thinking
            .get_or_insert_with(LiveThinking::default);
        if dock.text.trim().is_empty() {
            dock.text = format!("调用工具: {tool_name}");
        }
    }

    /// Show status in the thinking dock before reasoning tokens arrive.
    pub fn touch_thinking_status(&mut self, status: &str) {
        let hint = status.trim();
        if hint.is_empty() {
            return;
        }
        let dock = self
            .live_thinking
            .get_or_insert_with(LiveThinking::default);
        dock.status_hint = Some(hint.to_string());
    }

    pub fn has_live_thinking(&self) -> bool {
        self.live_thinking.is_some()
    }

    /// Decide think pane size from live reasoning + agent activity.
    pub fn think_pane_mode(&self, agent_running: bool) -> ThinkPaneMode {
        if let Some(dock) = &self.live_thinking {
            if !dock.text.trim().is_empty() {
                return ThinkPaneMode::Expanded;
            }
            if dock
                .status_hint
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            {
                return ThinkPaneMode::Expanded;
            }
        }
        if agent_running {
            return ThinkPaneMode::Expanded;
        }
        ThinkPaneMode::Hidden
    }

    /// No-op — dock re-renders every frame while active.
    pub fn invalidate_thinking_cache(&mut self) {}

    /// Hide the bottom thinking dock (assistant text / tool output is starting).
    pub fn collapse_thinking(&mut self) {
        self.live_thinking = None;
        self.purge_thinking_from_timeline();
        self.cache_valid = false;
    }

    fn purge_thinking_from_timeline(&mut self) {
        let before = self.lines.len();
        self.lines
            .retain(|l| !matches!(l, OutputLine::Thinking { .. }));
        if self.lines.len() != before {
            self.rendered_cache.resize(self.lines.len(), None);
            self.cache_valid = false;
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.live_thinking = None;
        self.rendered_cache.clear();
        self.cache_valid = true;
    }
}

/// Last two visual lines for the fixed thinking dock (wrap long prose into segments first).
pub fn thinking_dock_two_lines(text: &str, status_hint: Option<&str>, width: usize) -> (String, String) {
    let max_chars = width.saturating_sub(6).max(16);
    let segs = thinking_carousel_segments(text, status_hint);
    if segs.is_empty() {
        return ("思考中…".to_string(), String::new());
    }
    let truncate = |s: &str| {
        if s.chars().count() > max_chars {
            format!("{}…", s.chars().take(max_chars.saturating_sub(1)).collect::<String>())
        } else {
            s.to_string()
        }
    };
    if segs.len() == 1 {
        return (truncate(&segs[0]), String::new());
    }
    let n = segs.len();
    (truncate(&segs[n - 2]), truncate(&segs[n - 1]))
}

/// Last `max_lines` wrapped segments for the dedicated think pane.
pub fn thinking_pane_lines(
    text: &str,
    status_hint: Option<&str>,
    width: usize,
    max_lines: usize,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let max_chars = width.saturating_sub(4).max(12);
    let truncate = |s: &str| {
        if s.chars().count() > max_chars {
            format!(
                "{}…",
                s.chars().take(max_chars.saturating_sub(1)).collect::<String>()
            )
        } else {
            s.to_string()
        }
    };
    let segs = thinking_carousel_segments(text, status_hint);
    if segs.is_empty() {
        return vec!["思考中…".to_string()];
    }
    let lines: Vec<String> = segs.iter().map(|s| truncate(s)).collect();
    if lines.len() <= max_lines {
        lines
    } else {
        lines[lines.len() - max_lines..].to_vec()
    }
}

/// Segments for live thinking (wrap long lines).
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
    fn carousel_splits_long_lines() {
        let long = "a".repeat(100);
        let segs = thinking_carousel_segments(&long, None);
        assert!(segs.len() >= 2);
    }

    #[test]
    fn think_pane_hidden_when_idle() {
        let pane = OutputPane::new();
        assert_eq!(pane.think_pane_mode(false), ThinkPaneMode::Hidden);
    }

    #[test]
    fn think_pane_expanded_when_agent_running() {
        let pane = OutputPane::new();
        assert_eq!(pane.think_pane_mode(true), ThinkPaneMode::Expanded);
    }

    #[test]
    fn think_pane_expanded_with_reasoning_text() {
        let mut pane = OutputPane::new();
        pane.push_reasoning_chunk("analyzing");
        assert_eq!(pane.think_pane_mode(true), ThinkPaneMode::Expanded);
    }

    #[test]
    fn pane_shows_last_n_segments() {
        let lines = thinking_pane_lines("one\n\ntwo\nthree\nfour", None, 80, 3);
        assert_eq!(lines, vec!["two", "three", "four"]);
    }

    #[test]
    fn dock_shows_last_two_segments() {
        let (a, b) = thinking_dock_two_lines("one\n\ntwo\nthree", None, 80);
        assert_eq!(a, "two");
        assert_eq!(b, "three");
    }

    #[test]
    fn live_thinking_not_in_scroll_timeline() {
        let mut pane = OutputPane::new();
        pane.push_reasoning_chunk("hello");
        assert!(pane.has_live_thinking());
        assert!(pane.lines.is_empty());
        pane.collapse_thinking();
        assert!(!pane.has_live_thinking());
    }
}
