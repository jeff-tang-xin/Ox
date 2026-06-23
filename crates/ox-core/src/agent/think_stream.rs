//! Split streaming assistant text into **think** vs **visible** channels.
//!
//! Models often embed reasoning in `think` / `redacted_thinking` XML blocks inside
//! `delta.content` instead of a separate `reasoning_content` field.

const OPEN_TAGS: &[&str] = &[concat!("<", "think"), concat!("<", "redacted_thinking")];
const CLOSE_TAGS: &[&str] = &[concat!("<", "/think", ">"), concat!("<", "/redacted_thinking", ">")];

/// Incremental filter: think-tag bodies → reasoning UI; outside text → chat.
#[derive(Debug, Default)]
pub struct ThinkTagStreamFilter {
    buffer: String,
    emitted_think: usize,
    emitted_visible: usize,
}

impl ThinkTagStreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `(reasoning_delta, visible_delta)` — either may be `None` if unchanged.
    pub fn push(&mut self, chunk: &str) -> (Option<String>, Option<String>) {
        if !chunk.is_empty() {
            self.buffer.push_str(chunk);
        }
        let (think, visible) = partition_think_stream(&self.buffer);
        let mut reasoning = None;
        let mut vis = None;
        if think.len() > self.emitted_think {
            reasoning = Some(think[self.emitted_think..].to_string());
            self.emitted_think = think.len();
        }
        if visible.len() > self.emitted_visible {
            vis = Some(visible[self.emitted_visible..].to_string());
            self.emitted_visible = visible.len();
        }
        (reasoning, vis)
    }
}

struct CloseTagPos {
    start: usize,
    end: usize,
}

/// Partition full streamed text into (think_body, visible_text).
pub fn partition_think_stream(text: &str) -> (String, String) {
    let mut think = String::new();
    let mut visible = String::new();
    let mut i = 0usize;

    while i < text.len() {
        let Some(tag_start) = find_open_tag_start_in(text, i) else {
            visible.push_str(&text[i..]);
            break;
        };
        visible.push_str(&text[i..tag_start]);

        let after_tag = &text[tag_start..];
        let Some(gt) = after_tag.find('>') else {
            think.push_str(after_tag);
            break;
        };
        let content_start = tag_start + gt + 1;

        if let Some(close) = find_close_tag(text, content_start) {
            think.push_str(&text[content_start..close.start]);
            i = close.end;
        } else {
            think.push_str(&text[content_start..]);
            break;
        }
    }

    (think, visible)
}

/// User/chat/context-visible text only — think bodies are excluded.
pub fn visible_only(text: &str) -> String {
    partition_think_stream(text).1.trim().to_string()
}

/// Strip think channels before an LLM call (think is UI-only, not context).
pub fn prepare_messages_for_llm(messages: &mut [crate::message::Message]) {
    use crate::message::Message;
    for msg in messages.iter_mut() {
        if let Message::Assistant {
            content,
            reasoning_content,
            ..
        } = msg
        {
            *reasoning_content = None;
            *content = visible_only(content);
        }
    }
}

fn find_open_tag_start_in(text: &str, from: usize) -> Option<usize> {
    let slice = &text[from..];
    let lower = slice.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for needle in OPEN_TAGS {
        if let Some(pos) = lower.find(needle) {
            best = Some(best.map_or(pos, |b| b.min(pos)));
        }
    }
    best.map(|p| from + p)
}

fn find_close_tag(text: &str, from: usize) -> Option<CloseTagPos> {
    let slice = &text[from..];
    let lower = slice.to_ascii_lowercase();
    let mut best: Option<(usize, usize)> = None;
    for needle in CLOSE_TAGS {
        if let Some(pos) = lower.find(needle) {
            let end = pos + needle.len();
            if best.is_none_or(|(bp, _)| pos < bp) {
                best = Some((pos, end));
            }
        }
    }
    best.map(|(start, end)| CloseTagPos {
        start: from + start,
        end: from + end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_complete_think_block() {
        let open = concat!("<", "think", ">");
        let close = concat!("<", "/think", ">");
        let input = format!("Hello {open}secret{close} world");
        let (t, v) = partition_think_stream(&input);
        assert_eq!(t, "secret");
        assert_eq!(v, "Hello  world");
    }

    #[test]
    fn unclosed_think_goes_to_reasoning() {
        let open = concat!("<", "think", ">");
        let input = format!("Hi {open}still going");
        let (t, v) = partition_think_stream(&input);
        assert_eq!(t, "still going");
        assert_eq!(v, "Hi ");
    }

    #[test]
    fn incremental_filter_emits_deltas() {
        let open = concat!("<", "think", ">");
        let close = concat!("<", "/think", ">");
        let mut f = ThinkTagStreamFilter::new();
        let hi_open = format!("Hi {open}");
        let _ = f.push(&hi_open);
        let (r, v) = f.push("ab");
        assert_eq!(r.as_deref(), Some("ab"));
        assert!(v.is_none());
        let close_vis = format!("{close}visible");
        let (r, v) = f.push(&close_vis);
        assert!(r.as_deref().is_none() || r.as_deref() == Some(""));
        assert_eq!(v.as_deref(), Some("visible"));
    }

    #[test]
    fn redacted_thinking_tag() {
        let (t, v) = partition_think_stream(
            "x<think>inner</think>y",
        );
        assert_eq!(t, "inner");
        assert_eq!(v, "xy");
    }

    #[test]
    fn plain_text_is_visible() {
        let (t, v) = partition_think_stream("just answer");
        assert!(t.is_empty());
        assert_eq!(v, "just answer");
    }

    #[test]
    fn visible_only_strips_think_block() {
        let open = concat!("<", "think", ">");
        let close = concat!("<", "/think", ">");
        let input = format!("ok {open}secret\n{close}more");
        assert_eq!(visible_only(&input), "ok more");
    }

    #[test]
    fn prepare_messages_clears_reasoning() {
        use crate::message::Message;
        let open = concat!("<", "think", ">");
        let close = concat!("<", "/think", ">");
        let content = format!("hi {open}secret{close}x");
        let mut msgs = vec![Message::Assistant {
            content: content.into(),
            tool_calls: vec![],
            reasoning_content: Some("long reasoning".into()),
        }];
        prepare_messages_for_llm(&mut msgs);
        if let Message::Assistant {
            content,
            reasoning_content,
            ..
        } = &msgs[0]
        {
            assert!(reasoning_content.is_none());
            assert!(!content.contains("secret"));
            assert!(content.contains('x'));
        }
    }
}
