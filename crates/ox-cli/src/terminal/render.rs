use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::markdown::MarkdownRenderer;
use super::output_pane::OutputLine;

/// Render the Split-View layout: output pane (top) + input pane (bottom).
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Split: 85% output, rest input (minimum 3 lines for input).
    let chunks = Layout::vertical([
        Constraint::Percentage(85),
        Constraint::Min(3),
    ])
    .split(area);

    render_output_pane(frame, app, chunks[0]);
    render_input_pane(frame, app, chunks[1]);
}

fn render_output_pane(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.agent_running {
        " Agent Output [working...] "
    } else {
        " Agent Output "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));

    let md_renderer = MarkdownRenderer::new();

    // Convert output lines to ratatui Lines.
    let lines: Vec<Line> = app
        .output
        .lines
        .iter()
        .flat_map(|ol| match ol {
            OutputLine::Plain(s) => vec![Line::from(s.as_str().to_string())],
            OutputLine::Styled { prefix, content } => vec![Line::from(vec![
                Span::styled(
                    format!("{} ", prefix),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(content.clone()),
            ])],
            OutputLine::StreamingPartial(s) => vec![Line::from(vec![
                Span::raw(s.clone()),
                Span::styled("█".to_string(), Style::default().fg(Color::Green)),
            ])],
            OutputLine::Markdown(s) => md_renderer.render_lines(s),
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

fn render_input_pane(frame: &mut Frame, app: &App, area: Rect) {
    let status_hint = if app.agent_running {
        " [Tab: focus] [Ctrl+C: interrupt] "
    } else {
        " Input "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(status_hint)
        .border_style(Style::default().fg(Color::Blue));

    let prompt = "ox> ";
    let display_text = format!("{}{}", prompt, &app.input.buffer);

    let paragraph = Paragraph::new(display_text.as_str()).block(block);

    frame.render_widget(paragraph, area);

    // Position the cursor inside the input pane.
    // +1 for left border, +prompt length, +cursor char position.
    let cursor_x = area.x + 1 + prompt.len() as u16 + app.input.cursor_char_pos() as u16;
    let cursor_y = area.y + 1; // +1 for top border
    if cursor_x < area.x + area.width - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
