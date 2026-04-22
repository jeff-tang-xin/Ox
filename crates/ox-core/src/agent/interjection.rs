/// Priority level for interjection messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterjectionPriority {
    /// Normal message — will be delivered when the current turn completes.
    Normal,
    /// Urgent message — may interrupt the current tool execution.
    Urgent,
}

/// Buffers user messages typed while the agent is running.
///
/// Messages are queued and can be drained when the agent turn completes,
/// or urgent messages can be checked mid-turn.
pub struct InterjectionBuffer {
    messages: Vec<(String, InterjectionPriority)>,
}

impl Default for InterjectionBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl InterjectionBuffer {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Push a new interjection message.
    pub fn push(&mut self, msg: String, priority: InterjectionPriority) {
        self.messages.push((msg, priority));
    }

    /// Check if there are any urgent messages waiting.
    pub fn has_urgent(&self) -> bool {
        self.messages
            .iter()
            .any(|(_, p)| *p == InterjectionPriority::Urgent)
    }

    /// Drain all messages (consuming them), returning texts in order.
    pub fn drain(&mut self) -> Vec<String> {
        self.messages.drain(..).map(|(msg, _)| msg).collect()
    }

    /// Return the number of pending messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer() {
        let buf = InterjectionBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(!buf.has_urgent());
    }

    #[test]
    fn push_and_drain() {
        let mut buf = InterjectionBuffer::new();
        buf.push("hello".into(), InterjectionPriority::Normal);
        buf.push("STOP".into(), InterjectionPriority::Urgent);

        assert_eq!(buf.len(), 2);
        assert!(buf.has_urgent());

        let msgs = buf.drain();
        assert_eq!(msgs, vec!["hello", "STOP"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn normal_only_no_urgent() {
        let mut buf = InterjectionBuffer::new();
        buf.push("continue".into(), InterjectionPriority::Normal);
        assert!(!buf.has_urgent());
        assert_eq!(buf.len(), 1);
    }
}
