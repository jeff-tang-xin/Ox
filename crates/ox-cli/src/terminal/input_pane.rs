/// The input pane: handles line editing with cursor and history.
#[derive(Debug)]
pub struct InputPane {
    /// Current input buffer.
    pub buffer: String,
    /// Cursor position (byte offset into buffer).
    pub cursor: usize,
    /// Command history.
    pub history: Vec<String>,
    /// Current position in history navigation (None = not navigating).
    pub history_index: Option<usize>,
    /// Saved buffer when navigating history.
    saved_buffer: String,
    /// Whether multiline mode is active.
    #[allow(dead_code)]
    pub multiline_mode: bool,
}

impl InputPane {
    /// Maximum history entries to keep.
    const MAX_HISTORY: usize = 1000;
    /// Maximum length per history entry.
    const MAX_ENTRY_LEN: usize = 10000;

    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            saved_buffer: String::new(),
            multiline_mode: false,
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Insert a string at the cursor position (used for paste).
    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            // Find the previous char boundary.
            let prev = self.buffer.get(..self.cursor)
                .and_then(|s| s.char_indices().next_back().map(|(i, _)| i))
                .unwrap_or(0);
            self.buffer.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete the character at the cursor (delete key).
    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            let next_char_len = self.buffer.get(self.cursor..)
                .and_then(|s| s.chars().next().map(|c| c.len_utf8()))
                .unwrap_or(0);
            let next = self.cursor + next_char_len;
            self.buffer.drain(self.cursor..next);
        }
    }

    /// Move cursor left by one character.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.buffer.get(..self.cursor)
                .and_then(|s| s.char_indices().next_back().map(|(i, _)| i))
                .unwrap_or(0);
        }
    }

    /// Move cursor right by one character.
    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            let next_char_len = self.buffer.get(self.cursor..)
                .and_then(|s| s.chars().next().map(|c| c.len_utf8()))
                .unwrap_or(0);
            self.cursor += next_char_len;
        }
    }

    /// Move cursor to beginning of line.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end of line.
    pub fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// Insert a newline character at the cursor position (for multiline mode).
    #[allow(dead_code)]
    pub fn insert_newline(&mut self) {
        self.buffer.insert(self.cursor, '\n');
        self.cursor += 1;
    }

    /// Submit the current buffer. Returns the text and clears the buffer.
    pub fn submit(&mut self) -> String {
        let text = self.buffer.clone();
        if !text.is_empty() {
            // Truncate if too long.
            let entry = if text.len() > Self::MAX_ENTRY_LEN {
                text.get(..Self::MAX_ENTRY_LEN).unwrap_or(&text).to_string()
            } else {
                text.clone()
            };
            self.history.push(entry);
            // Trim oldest if over limit.
            if self.history.len() > Self::MAX_HISTORY {
                self.history.drain(..self.history.len() - Self::MAX_HISTORY);
            }
        }
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        text
    }

    /// Navigate to the previous history entry (Up arrow).
    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.saved_buffer = self.buffer.clone();
                self.history_index = Some(self.history.len() - 1);
            }
            Some(0) => return, // already at oldest
            Some(ref mut idx) => {
                *idx -= 1;
            }
        }
        if let Some(idx) = self.history_index {
            self.buffer = self.history[idx].clone();
            self.cursor = self.buffer.len();
        }
    }

    /// Navigate to the next history entry (Down arrow).
    pub fn history_down(&mut self) {
        match self.history_index {
            None => (),
            Some(idx) => {
                if idx + 1 < self.history.len() {
                    self.history_index = Some(idx + 1);
                    self.buffer = self.history[idx + 1].clone();
                } else {
                    // Return to the saved buffer.
                    self.history_index = None;
                    self.buffer = self.saved_buffer.clone();
                }
                self.cursor = self.buffer.len();
            }
        }
    }

    /// Character count of buffer (for display purposes).
    #[allow(dead_code)]
    pub fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Calculate the visual width of a string for terminal display.
    /// This accounts for wide characters like CJK, emoji, etc.
    pub fn visual_width(s: &str) -> usize {
        use unicode_width::UnicodeWidthStr;
        s.width()
    }

    /// Get the visible portion of the buffer given a maximum width.
    /// Returns (visible_text, scroll_offset) where scroll_offset is the
    /// number of characters scrolled from the start.
    pub fn get_visible_content(&self, max_width: usize) -> (String, usize) {
        if max_width == 0 {
            return (String::new(), 0);
        }

        let total_width = Self::visual_width(&self.buffer);

        // If content fits, show everything
        if total_width <= max_width {
            return (self.buffer.clone(), 0);
        }

        // Content doesn't fit - need to scroll
        // Strategy: keep cursor visible with some padding
        let cursor_visual_pos = self.buffer.get(..self.cursor)
            .map(|s| Self::visual_width(s))
            .unwrap_or(0);

        // Calculate optimal scroll position to keep cursor visible
        // Try to center cursor with ~20% padding on each side
        let padding = max_width / 5; // 20% padding
        let mut scroll_chars = 0;
        let mut current_width = 0;

        // Find how many characters to skip from the start
        for (char_idx, ch) in self.buffer.char_indices() {
            let char_width = Self::visual_width(&ch.to_string());
            if current_width + char_width > cursor_visual_pos.saturating_sub(padding) {
                scroll_chars = char_idx;
                break;
            }
            current_width += char_width;
        }

        // Extract visible portion
        let remaining = self.buffer.get(scroll_chars..).unwrap_or("");
        let mut visible = String::new();
        let mut visible_width = 0;

        for ch in remaining.chars() {
            let char_width = Self::visual_width(&ch.to_string());
            if visible_width + char_width > max_width {
                break;
            }
            visible.push(ch);
            visible_width += char_width;
        }

        (visible, scroll_chars)
    }

    /// Clear from cursor to beginning of line (Ctrl+U style).
    pub fn clear_to_home(&mut self) {
        self.buffer.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Clear from cursor to end of line (Ctrl+K style).
    pub fn clear_to_end(&mut self) {
        self.buffer.drain(self.cursor..);
    }

    /// Delete the word before the cursor (Ctrl+W style).
    pub fn delete_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Find the start of the current word by scanning backwards.
        let before_cursor = self.buffer.get(..self.cursor).unwrap_or("");
        let chars: Vec<(usize, char)> = before_cursor.char_indices().collect();
        let mut idx = chars.len();

        // Skip trailing whitespace.
        while idx > 0 && chars[idx - 1].1.is_whitespace() {
            idx -= 1;
        }
        // Skip non-whitespace characters (the word itself).
        while idx > 0 && !chars[idx - 1].1.is_whitespace() {
            idx -= 1;
        }

        let delete_start = if idx < chars.len() { chars[idx].0 } else { 0 };
        self.buffer.drain(delete_start..self.cursor);
        self.cursor = delete_start;
    }
}
