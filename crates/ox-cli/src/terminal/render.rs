use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::output_pane::OutputLine;

// ── Ox semantic color palette ──────────────────────────────────────

/// Semantic colors for the Ox UI. All rendering goes through this palette.
struct OxTheme {
    // Borders
    border: Color,
    border_input: Color,
    // Message prefixes
    user_prefix: Color,
    tool_prefix: Color,
    system_prefix: Color,
    // Content
    ai_text: Color,
    streaming_cursor: Color,
    // Status
    accent: Color,
    dim: Color,
    error: Color,
}

const THEME: OxTheme = OxTheme {
    border: Color::DarkGray,
    border_input: Color::Rgb(88, 130, 210),       // muted blue
    user_prefix: Color::Rgb(110, 210, 160),        // soft green
    tool_prefix: Color::Rgb(180, 140, 100),        // warm amber
    system_prefix: Color::Rgb(140, 140, 160),      // cool gray
    ai_text: Color::Rgb(210, 210, 230),            // light lavender
    streaming_cursor: Color::Rgb(100, 220, 100),   // bright green
    accent: Color::Rgb(130, 180, 255),             // sky blue
    dim: Color::Rgb(100, 100, 120),                // dim gray
    error: Color::Rgb(230, 90, 90),                // soft red
};

// ── Spinner animation ──────────────────────────────────────────────

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Render the Split-View layout: output pane (top) + input pane (bottom).
pub fn render(frame: &mut Frame, app: &App, tick_count: u64) {
    let area = frame.area();

    // Split: 85% output, rest input (minimum 3 lines for input).
    let chunks = Layout::vertical([
        Constraint::Percentage(85),
        Constraint::Min(3),
    ])
    .split(area);

    render_output_pane(frame, app, chunks[0]);
    render_input_pane(frame, app, chunks[1], tick_count);
}

fn render_output_pane(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.agent_running {
        " ◉ Agent "
    } else {
        " Ox "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
        .title_style(Style::default().fg(THEME.accent).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(THEME.border));

    let output_width = area.width.saturating_sub(4) as usize; // -2 border, -2 padding

    // Convert output lines to ratatui Lines.
    let lines: Vec<Line> = app
        .output
        .lines
        .iter()
        .flat_map(|ol| match ol {
            OutputLine::Plain(s) => vec![Line::from(Span::styled(
                s.clone(),
                Style::default().fg(THEME.ai_text),
            ))],
            OutputLine::Styled { prefix, content } => {
                let prefix_color = if prefix == "You" || prefix == "ox>" {
                    THEME.user_prefix
                } else if prefix == "Tool" {
                    THEME.tool_prefix
                } else if prefix == "[system]" {
                    THEME.system_prefix
                } else {
                    THEME.accent
                };
                vec![Line::from(vec![
                    Span::styled(
                        format!("{prefix} "),
                        Style::default().fg(prefix_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(content.clone(), Style::default().fg(THEME.ai_text)),
                ])]
            }
            OutputLine::StreamingPartial(s) => vec![Line::from(vec![
                Span::styled(s.clone(), Style::default().fg(THEME.ai_text)),
                Span::styled(
                    "▌".to_string(),
                    Style::default().fg(THEME.streaming_cursor),
                ),
            ])],
            OutputLine::Markdown(s) => app.md_renderer.render_lines(s, output_width),
        })
        .collect();

    // Calculate scroll: we want "scroll_offset=0" to show the bottom.
    let inner_height = area.height.saturating_sub(2) as usize; // -2 for border
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(inner_height);
    let effective_scroll = max_scroll.saturating_sub(app.scroll_offset as usize);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll as u16, 0));

    frame.render_widget(paragraph, area);
}

fn render_input_pane(frame: &mut Frame, app: &App, area: Rect, tick_count: u64) {
    let (title, title_style) = if app.agent_running {
        let spinner = SPINNER_FRAMES[(tick_count as usize) % SPINNER_FRAMES.len()];
        (
            format!(" {spinner} Working… "),
            Style::default().fg(THEME.streaming_cursor),
        )
    } else {
        (" Input ".to_string(), Style::default().fg(THEME.border_input))
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title.as_str())
        .title_style(title_style)
        .border_style(Style::default().fg(THEME.border_input));

    let prompt = "ox❯ ";

    let prompt_len = prompt.len();
    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            prompt.to_string(),
            Style::default().fg(THEME.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.input.buffer.clone(),
            Style::default().fg(Color::White),
        ),
    ]))
    .block(block);

    frame.render_widget(paragraph, area);

    // Position the cursor inside the input pane.
    // +1 for left border, +prompt length, +cursor char position.
    let cursor_x = area.x + 1 + prompt_len as u16 + app.input.cursor_char_pos() as u16;
    let cursor_y = area.y + 1; // +1 for top border
    if cursor_x < area.x + area.width - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
