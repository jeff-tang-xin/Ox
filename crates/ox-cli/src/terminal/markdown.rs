use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

const COLOR_HEADING: Color = Color::Rgb(197, 134, 199);
const COLOR_LIST_BULLET: Color = Color::Rgb(78, 201, 176);
const COLOR_INLINE_CODE: Color = Color::Rgb(78, 201, 176);
const COLOR_CODE_BORDER: Color = Color::Rgb(64, 64, 64);
const COLOR_CODE_GUTTER: Color = Color::Rgb(64, 64, 64);
const COLOR_CODE_BG: Color = Color::Rgb(30, 30, 30);
const COLOR_LANG_LABEL: Color = Color::Rgb(0, 122, 204);
const COLOR_LINK: Color = Color::Rgb(86, 156, 214);

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

    pub fn render_lines(&self, text: &str, output_width: usize) -> Vec<Line<'static>> {
        let parser = Parser::new(text);
        let mut result: Vec<Line<'static>> = Vec::new();
        let mut current_spans: Vec<Span<'static>> = Vec::new();
        let mut style_stack: Vec<Style> = vec![Style::default()];
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buffer = String::new();

        for event in parser {
            match event {
                Event::Start(tag) => match tag {
                    Tag::Heading { level, .. } => {
                        let prefix = match level {
                            HeadingLevel::H1 => "▎▎▎ ",
                            HeadingLevel::H2 => "▎▎ ",
                            HeadingLevel::H3 => "▎ ",
                            _ => "",
                        };
                        current_spans.push(Span::styled(
                            prefix.to_string(),
                            Style::default().fg(COLOR_HEADING).add_modifier(Modifier::BOLD),
                        ));
                        style_stack.push(
                            Style::default()
                                .fg(COLOR_HEADING)
                                .add_modifier(Modifier::BOLD),
                        );
                    }
                    Tag::Paragraph => {
                        style_stack.push(Style::default());
                    }
                    Tag::CodeBlock(kind) => {
                        in_code_block = true;
                        code_lang = match kind {
                            CodeBlockKind::Fenced(lang) => lang.to_string(),
                            _ => String::new(),
                        };
                        code_buffer.clear();
                    }
                    Tag::List(_) => {
                        style_stack.push(Style::default());
                    }
                    Tag::Item => {
                        current_spans.push(Span::styled(
                            "• ".to_string(),
                            Style::default().fg(COLOR_LIST_BULLET),
                        ));
                        style_stack.push(Style::default());
                    }
                    Tag::Strong => {
                        let current = *style_stack.last().unwrap();
                        style_stack.push(current.add_modifier(Modifier::BOLD));
                    }
                    Tag::Emphasis => {
                        let current = *style_stack.last().unwrap();
                        style_stack.push(current.add_modifier(Modifier::ITALIC));
                    }
                    Tag::Link { .. } => {
                        let current = *style_stack.last().unwrap();
                        style_stack.push(
                            current
                                .fg(COLOR_LINK)
                                .add_modifier(Modifier::UNDERLINED),
                        );
                    }
                    _ => {
                        style_stack.push(*style_stack.last().unwrap());
                    }
                },

                Event::End(tag) => match tag {
                    TagEnd::Heading(_) => {
                        style_stack.pop();
                        if !current_spans.is_empty() {
                            result.push(Line::from(std::mem::take(&mut current_spans)));
                        }
                    }
                    TagEnd::Paragraph => {
                        style_stack.pop();
                        if !current_spans.is_empty() {
                            result.push(Line::from(std::mem::take(&mut current_spans)));
                        }
                    }
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        let highlighted =
                            self.highlight_code(&code_buffer, &code_lang, output_width);
                        result.extend(highlighted);
                        code_buffer.clear();
                        code_lang.clear();
                    }
                    TagEnd::Item => {
                        style_stack.pop();
                        if !current_spans.is_empty() {
                            result.push(Line::from(std::mem::take(&mut current_spans)));
                        }
                    }
                    TagEnd::List(_) => {
                        style_stack.pop();
                    }
                    TagEnd::Strong
                    | TagEnd::Emphasis
                    | TagEnd::Link => {
                        style_stack.pop();
                    }
                    _ => {
                        style_stack.pop();
                    }
                },

                Event::Text(s) => {
                    if in_code_block {
                        code_buffer.push_str(&s);
                    } else {
                        let style = *style_stack.last().unwrap();
                        current_spans.push(Span::styled(s.into_string(), style));
                    }
                }

                Event::Code(s) => {
                    let style = Style::default().fg(COLOR_INLINE_CODE);
                    current_spans.push(Span::styled(s.into_string(), style));
                }

                Event::SoftBreak => {
                    if !current_spans.is_empty() {
                        result.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                }

                Event::HardBreak => {
                    if !current_spans.is_empty() {
                        result.push(Line::from(std::mem::take(&mut current_spans)));
                    } else {
                        result.push(Line::from(""));
                    }
                }

                Event::FootnoteReference(s) => {
                    let style = *style_stack.last().unwrap();
                    current_spans.push(Span::styled(s.into_string(), style));
                }

                _ => {}
            }
        }

        if !current_spans.is_empty() {
            result.push(Line::from(current_spans));
        }

        if in_code_block && !code_buffer.is_empty() {
            let highlighted = self.highlight_code(&code_buffer, &code_lang, output_width);
            result.extend(highlighted);
        }

        result
    }

    fn highlight_code(&self, code: &str, lang: &str, available_width: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

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
