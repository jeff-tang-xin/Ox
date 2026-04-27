use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::output_pane::OutputLine;

// ── VS Code-inspired dark theme ────────────────────────────────────

#[allow(dead_code)]
struct OxTheme {
    bg_base: Color,
    bg_input: Color,
    border: Color,
    border_active: Color,
    text_primary: Color,
    text_secondary: Color,
    text_bright: Color,
    user_msg: Color,
    ai_msg: Color,
    tool_msg: Color,
    system_msg: Color,
    error_msg: Color,
    accent: Color,
    accent_purple: Color,
    streaming_cursor: Color,
    status_bg: Color,
    status_text: Color,
}

const THEME: OxTheme = OxTheme {
    bg_base: Color::Rgb(30, 30, 30),
    bg_input: Color::Rgb(45, 45, 45),
    border: Color::Rgb(64, 64, 64),
    border_active: Color::Rgb(0, 122, 204),
    text_primary: Color::Rgb(212, 212, 212),
    text_secondary: Color::Rgb(128, 128, 128),
    text_bright: Color::Rgb(255, 255, 255),
    user_msg: Color::Rgb(78, 201, 176),
    ai_msg: Color::Rgb(212, 212, 212),
    tool_msg: Color::Rgb(220, 220, 170),
    system_msg: Color::Rgb(106, 153, 85),
    error_msg: Color::Rgb(244, 71, 71),
    accent: Color::Rgb(0, 122, 204),
    accent_purple: Color::Rgb(197, 134, 199),
    streaming_cursor: Color::Rgb(78, 201, 176),
    status_bg: Color::Rgb(0, 122, 204),
    status_text: Color::Rgb(255, 255, 255),
};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const ICON_USER: &str = "▸";
const ICON_TOOL: &str = "⚙";
const ICON_SYSTEM: &str = "●";

/// Render the full layout.
pub fn render(frame: &mut Frame, app: &mut App, tick_count: u64) {
    let area = frame.area();

    if area.width < 10 || area.height < 5 {
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .split(area);

    render_output_pane(frame, app, chunks[0]);
    render_status_bar(frame, app, chunks[1], tick_count);
    render_input_pane(frame, app, chunks[2], tick_count);
}

fn render_output_pane(frame: &mut Frame, app: &mut App, area: Rect) {
    let agent_running = app.agent_running;
    let spinner_frame = app.spinner_frame;
    let scroll_offset = app.scroll_offset;
    let user_scrolled = app.user_scrolled;

    let title = if agent_running {
        let spinner = SPINNER_FRAMES[(spinner_frame as usize) % SPINNER_FRAMES.len()];
        format!(" {spinner} Ox ")
    } else if user_scrolled {
        " Ox ↓ scrolling (PgDn/Shift↓ to return) ".to_string()
    } else {
        " Ox ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title.as_str())
        .title_style(if app.agent_running {
            Style::default().fg(THEME.streaming_cursor).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(THEME.accent).add_modifier(Modifier::BOLD)
        })
        .border_style(Style::default().fg(THEME.border))
        .style(Style::default().bg(THEME.bg_base));

    let output_width = area.width.saturating_sub(4) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;

    // Get only visible lines — clone only what we display, not the entire buffer.
    let md = &app.md_renderer;
    let out = &mut app.output;
    let (lines, _total) = out.get_visible_lines(output_width, inner_height, scroll_offset, |ol, w| {
        render_single_line(ol, w, md)
    });

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render a single OutputLine into ratatui Lines (may produce multiple lines for Markdown).
fn render_single_line(ol: &OutputLine, width: usize, md_renderer: &super::markdown::MarkdownRenderer) -> Vec<Line<'static>> {
    match ol {
        OutputLine::Plain(s) => {
            if s.is_empty() {
                vec![Line::from("")]
            } else {
                vec![Line::from(Span::styled(
                    s.clone(),
                    Style::default().fg(THEME.text_primary).bg(THEME.bg_base),
                ))]
            }
        }
        OutputLine::Styled { prefix, content } => {
            let (icon, prefix_color, content_style) = if prefix == "You" || prefix == "ox>" {
                (ICON_USER, THEME.user_msg, Style::default().fg(THEME.text_bright).bg(THEME.bg_base))
            } else if prefix == "Tool" {
                (ICON_TOOL, THEME.tool_msg, Style::default().fg(THEME.text_primary).bg(THEME.bg_base))
            } else if prefix == "[system]" {
                (ICON_SYSTEM, THEME.system_msg, Style::default().fg(THEME.text_secondary).bg(THEME.bg_base))
            } else {
                ("", THEME.accent, Style::default().fg(THEME.text_primary).bg(THEME.bg_base))
            };
            let prefix_text = if icon.is_empty() {
                format!("{prefix} ")
            } else {
                format!("{icon} {prefix} ")
            };
            vec![Line::from(vec![
                Span::styled(
                    prefix_text,
                    Style::default().fg(prefix_color).add_modifier(Modifier::BOLD).bg(THEME.bg_base),
                ),
                Span::styled(content.clone(), content_style),
            ])]
        }
        OutputLine::StreamingPartial(s) => vec![Line::from(vec![
            Span::styled(s.clone(), Style::default().fg(THEME.ai_msg).bg(THEME.bg_base)),
            Span::styled(
                "▌".to_string(),
                Style::default().fg(THEME.streaming_cursor).bg(THEME.bg_base),
            ),
        ])],
        OutputLine::Markdown(s) => md_renderer.render_lines(s, width),
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    let left_parts: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", app.model_name),
            Style::default().fg(THEME.status_text).bg(THEME.status_bg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " │ ".to_string(),
            Style::default().fg(THEME.status_text).bg(THEME.status_bg),
        ),
        Span::styled(
            format!(" {} ", app.working_dir),
            Style::default().fg(THEME.status_text).bg(THEME.status_bg),
        ),
    ];

    let right_text = format!("{} msgs │ {} ", app.message_count, app.cost_summary);
    let right_len = right_text.len() as u16;

    let left_line = Line::from(left_parts);
    let right_span = Span::styled(
        right_text,
        Style::default().fg(THEME.status_text).bg(THEME.status_bg),
    );

    let left_area = Rect {
        width: area.width.saturating_sub(right_len),
        ..area
    };
    let left_para = Paragraph::new(left_line)
        .style(Style::default().bg(THEME.status_bg));
    frame.render_widget(left_para, left_area);

    let right_area = Rect {
        x: area.x + area.width.saturating_sub(right_len),
        width: right_len,
        ..area
    };
    let right_para = Paragraph::new(Line::from(right_span))
        .style(Style::default().bg(THEME.status_bg));
    frame.render_widget(right_para, right_area);
}

fn render_input_pane(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    let (title, title_style) = if app.agent_running {
        (" Working… ".to_string(), Style::default().fg(THEME.streaming_cursor))
    } else {
        (" Input ".to_string(), Style::default().fg(THEME.border_active))
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title.as_str())
        .title_style(title_style)
        .border_style(Style::default().fg(if app.agent_running {
            THEME.streaming_cursor
        } else {
            THEME.border_active
        }))
        .style(Style::default().bg(THEME.bg_input));

    let prompt = if app.pending_confirmation.is_some() {
        "confirm [Y/N/T] ❯ "
    } else {
        "ox❯ "
    };
    let prompt_len = prompt.len();

    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            prompt.to_string(),
            Style::default().fg(THEME.accent).add_modifier(Modifier::BOLD).bg(THEME.bg_input),
        ),
        Span::styled(
            app.input.buffer.clone(),
            Style::default().fg(THEME.text_bright).bg(THEME.bg_input),
        ),
    ]))
    .block(block);

    frame.render_widget(paragraph, area);

    let cursor_x = area.x + 1 + prompt_len as u16 + app.input.cursor_char_pos() as u16;
    let cursor_y = area.y + 1;
    if cursor_x < area.x + area.width - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
