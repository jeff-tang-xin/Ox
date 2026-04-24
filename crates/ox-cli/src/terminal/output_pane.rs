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
#[derive(Debug)]
pub struct OutputPane {
    pub lines: Vec<OutputLine>,
}

impl OutputPane {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }

    /// Push a complete line.
    pub fn push_line(&mut self, line: OutputLine) {
        self.lines.push(line);
    }

    /// Push a system-style message.
    pub fn push_system(&mut self, msg: &str) {
        self.lines.push(OutputLine::Styled {
            prefix: "[system]".to_string(),
            content: msg.to_string(),
        });
    }

    /// Append a streaming text chunk to the current partial line.
    /// If there's no active streaming line, start one.
    /// When a `\n` is encountered, finalize the current line and start a new one.
    /// Optimized: splits by `\n` in bulk instead of iterating char-by-char.
    pub fn push_streaming_chunk(&mut self, chunk: &str) {
        // Fast path: no newline in chunk — just append to current streaming line.
        if !chunk.contains('\n') {
            match self.lines.last_mut() {
                Some(OutputLine::StreamingPartial(s)) => {
                    s.push_str(chunk);
                }
                _ => {
                    self.lines.push(OutputLine::StreamingPartial(chunk.to_string()));
                }
            }
            return;
        }

        // Slow path: split by newlines and finalize each complete line.
        let mut remaining = chunk;
        while let Some(pos) = remaining.find('\n') {
            let before = &remaining[..pos];
            // Append before-newline text to current streaming line.
            match self.lines.last_mut() {
                Some(OutputLine::StreamingPartial(s)) => {
                    s.push_str(before);
                }
                _ => {
                    if !before.is_empty() {
                        self.lines.push(OutputLine::StreamingPartial(before.to_string()));
                    }
                }
            }
            // Finalize the line at the newline.
            self.finalize_streaming();
            remaining = &remaining[pos + 1..];
        }
        // Handle trailing text after the last newline.
        if !remaining.is_empty() {
            match self.lines.last_mut() {
                Some(OutputLine::StreamingPartial(s)) => {
                    s.push_str(remaining);
                }
                _ => {
                    self.lines.push(OutputLine::StreamingPartial(remaining.to_string()));
                }
            }
        }
    }

    /// Convert any trailing StreamingPartial to a Markdown line.
    pub fn finalize_streaming(&mut self) {
        if let Some(OutputLine::StreamingPartial(s)) = self.lines.last() {
            let finalized = OutputLine::Markdown(s.clone());
            *self.lines.last_mut().unwrap() = finalized;
        }
        // Always ensure next chunk starts a fresh StreamingPartial.
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}
