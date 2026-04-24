use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

// ── VS Code-inspired markdown colors ───────────────────────────────

const COLOR_HEADING: Color = Color::Rgb(197, 134, 199);   // #C586C7 purple
const COLOR_LIST_BULLET: Color = Color::Rgb(78, 201, 176); // #4EC9B0 teal
const COLOR_INLINE_CODE: Color = Color::Rgb(78, 201, 176); // #4EC9B0 teal
const COLOR_CODE_BORDER: Color = Color::Rgb(64, 64, 64);   // #404040
const COLOR_CODE_GUTTER: Color = Color::Rgb(64, 64, 64);   // #404040
const COLOR_CODE_BG: Color = Color::Rgb(30, 30, 30);       // #1E1E1E
const COLOR_LANG_LABEL: Color = Color::Rgb(0, 122, 204);   // #007ACC blue

/// Markdown-to-ratatui renderer.
///
/// Converts markdown text lines into styled ratatui `Line`s.
/// Supports: headings, code blocks (with syntect highlighting),
/// inline code, bold, and italic.
pub struct MarkdownRenderer {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_else(|| theme_set.themes.values().next().unwrap().clone());

        Self { syntax_set, theme }
    }

    /// Render a block of markdown text into ratatui Lines.
    /// `output_width` is the available width for the code block borders.
    pub fn render_lines(&self, text: &str, output_width: usize) -> Vec<Line<'static>> {
        let mut result: Vec<Line<'static>> = Vec::new();
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buffer: Vec<String> = Vec::new();

        for line in text.lines() {
            if line.starts_with("```") {
                if in_code_block {
                    let highlighted = self.highlight_code(&code_buffer.join("\n"), &code_lang, output_width);
                    result.extend(highlighted);
                    code_buffer.clear();
                    code_lang.clear();
                    in_code_block = false;
                } else {
                    code_lang = line.trim_start_matches('`').trim().to_string();
                    in_code_block = true;
                }
                continue;
            }

            if in_code_block {
                code_buffer.push(line.to_string());
                continue;
            }

            result.push(self.render_markdown_line(line));
        }

        if in_code_block && !code_buffer.is_empty() {
            let highlighted = self.highlight_code(&code_buffer.join("\n"), &code_lang, output_width);
            result.extend(highlighted);
        }

        result
    }

    fn render_markdown_line(&self, line: &str) -> Line<'static> {
        // Heading detection.
        if let Some(rest) = line.strip_prefix("### ") {
            return Line::from(Span::styled(
                format!("▎ {rest}"),
                Style::default().fg(COLOR_HEADING).add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(rest) = line.strip_prefix("## ") {
            return Line::from(Span::styled(
                format!("▎▎ {rest}"),
                Style::default().fg(COLOR_HEADING).add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(rest) = line.strip_prefix("# ") {
            return Line::from(Span::styled(
                format!("▎▎▎ {rest}"),
                Style::default().fg(COLOR_HEADING).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ));
        }

        // Bullet list items.
        if line.starts_with("- ") || line.starts_with("* ") {
            let rest = &line[2..];
            let mut spans = vec![Span::styled(
                "• ".to_string(),
                Style::default().fg(COLOR_LIST_BULLET),
            )];
            spans.extend(self.parse_inline_spans(rest));
            return Line::from(spans);
        }

        // Numbered list.
        if let Some(dot_pos) = line.find(". ") {
            let prefix = &line[..dot_pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                let rest = &line[dot_pos + 2..];
                let mut spans = vec![Span::styled(
                    format!("{}. ", prefix),
                    Style::default().fg(COLOR_LIST_BULLET),
                )];
                spans.extend(self.parse_inline_spans(rest));
                return Line::from(spans);
            }
        }

        let spans = self.parse_inline_spans(line);
        Line::from(spans)
    }

    /// Parse inline markdown: **bold**, *italic*, `code`.
    fn parse_inline_spans(&self, text: &str) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut chars = text.char_indices().peekable();
        let mut current = String::new();

        while let Some((i, ch)) = chars.next() {
            match ch {
                '`' => {
                    if !current.is_empty() {
                        spans.push(Span::raw(std::mem::take(&mut current)));
                    }
                    let mut code = String::new();
                    let mut closed = false;
                    for (_, c) in chars.by_ref() {
                        if c == '`' {
                            closed = true;
                            break;
                        }
                        code.push(c);
                    }
                    if closed {
                        spans.push(Span::styled(
                            code,
                            Style::default().fg(COLOR_INLINE_CODE),
                        ));
                    } else {
                        current.push('`');
                        current.push_str(&code);
                    }
                }
                '*' => {
                    let is_double = chars.peek().is_some_and(|(_, c)| *c == '*');
                    if is_double {
                        chars.next();
                        if !current.is_empty() {
                            spans.push(Span::raw(std::mem::take(&mut current)));
                        }
                        let mut bold_text = String::new();
                        let mut closed = false;
                        while let Some((_, c)) = chars.next() {
                            if c == '*' && chars.peek().is_some_and(|(_, c2)| *c2 == '*') {
                                chars.next();
                                closed = true;
                                break;
                            }
                            bold_text.push(c);
                        }
                        if closed {
                            spans.push(Span::styled(
                                bold_text,
                                Style::default().add_modifier(Modifier::BOLD),
                            ));
                        } else {
                            current.push_str("**");
                            current.push_str(&bold_text);
                        }
                    } else {
                        if !current.is_empty() {
                            spans.push(Span::raw(std::mem::take(&mut current)));
                        }
                        let mut italic_text = String::new();
                        let mut closed = false;
                        for (_, c) in chars.by_ref() {
                            if c == '*' {
                                closed = true;
                                break;
                            }
                            italic_text.push(c);
                        }
                        if closed {
                            spans.push(Span::styled(
                                italic_text,
                                Style::default().add_modifier(Modifier::ITALIC),
                            ));
                        } else {
                            current.push('*');
                            current.push_str(&italic_text);
                        }
                    }
                }
                _ => {
                    let _ = i;
                    current.push(ch);
                }
            }
        }

        if !current.is_empty() {
            spans.push(Span::raw(current));
        }

        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }

        spans
    }

    /// Syntax-highlight a code block using syntect.
    fn highlight_code(&self, code: &str, lang: &str, available_width: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Top border with language label.
        let lang_label = if lang.is_empty() {
            String::new()
        } else {
            format!(" {lang} ")
        };
        let border_content_len = 3 + lang_label.len();
        let dash_count = available_width.saturating_sub(border_content_len).max(3);
        lines.push(Line::from(vec![
            Span::styled(
                format!("┌──"),
                Style::default().fg(COLOR_CODE_BORDER),
            ),
            Span::styled(
                lang_label.clone(),
                Style::default().fg(COLOR_LANG_LABEL).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "─".repeat(dash_count),
                Style::default().fg(COLOR_CODE_BORDER),
            ),
        ]));

        let syntax = self
            .syntax_set
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let mut highlighter = syntect::easy::HighlightLines::new(syntax, &self.theme);

        for line in code.lines() {
            let ranges = highlighter
                .highlight_line(line, &self.syntax_set)
                .unwrap_or_default();

            let spans: Vec<Span<'static>> = std::iter::once(Span::styled(
                "│ ".to_string(),
                Style::default().fg(COLOR_CODE_GUTTER),
            ))
            .chain(ranges.into_iter().map(|(style, text)| {
                let fg = Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                );
                Span::styled(text.to_string(), Style::default().fg(fg).bg(COLOR_CODE_BG))
            }))
            .collect();

            lines.push(Line::from(spans));
        }

        // Bottom border.
        lines.push(Line::from(Span::styled(
            format!("└{}", "─".repeat(available_width.saturating_sub(1).max(3))),
            Style::default().fg(COLOR_CODE_BORDER),
        )));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_rendering() {
        let md = MarkdownRenderer::new();
        let lines = md.render_lines("# Title\n## Subtitle\nHello", 80);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn code_block_rendering() {
        let md = MarkdownRenderer::new();
        let input = "text\n```rust\nfn main() {}\n```\nend";
        let lines = md.render_lines(input, 80);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn inline_code_rendering() {
        let md = MarkdownRenderer::new();
        let lines = md.render_lines("Use `cargo build` to compile", 80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.len() >= 3);
    }

    #[test]
    fn bold_italic_rendering() {
        let md = MarkdownRenderer::new();
        let lines = md.render_lines("This is **bold** and *italic* text", 80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.len() >= 4);
    }
}
