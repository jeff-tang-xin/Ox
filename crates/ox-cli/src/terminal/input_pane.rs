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
}

impl InputPane {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            saved_buffer: String::new(),
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            // Find the previous char boundary.
            let prev = self.buffer[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.buffer.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete the character at the cursor (delete key).
    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            let next = self.cursor
                + self.buffer[self.cursor..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(0);
            self.buffer.drain(self.cursor..next);
        }
    }

    /// Move cursor left by one character.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.buffer[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    /// Move cursor right by one character.
    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += self.buffer[self.cursor..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
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

    /// Submit the current buffer. Returns the text and clears the buffer.
    pub fn submit(&mut self) -> String {
        let text = self.buffer.clone();
        if !text.is_empty() {
            self.history.push(text.clone());
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
            None => return,
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
    pub fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Cursor position in character count (not byte offset).
    pub fn cursor_char_pos(&self) -> usize {
        self.buffer[..self.cursor].chars().count()
    }
}
