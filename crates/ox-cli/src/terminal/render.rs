use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::output_pane::OutputLine;

const BG: Color = Color::Rgb(0, 0, 0);
const BG_INPUT: Color = Color::Rgb(30, 30, 30);
const BORDER: Color = Color::Rgb(64, 64, 64);
const BLUE: Color = Color::Rgb(0, 122, 204);
const TEXT: Color = Color::Rgb(212, 212, 212);
const TEXT_DIM: Color = Color::Rgb(128, 128, 128);
const TEXT_BRIGHT: Color = Color::Rgb(255, 255, 255);
const USER_COLOR: Color = Color::Rgb(78, 201, 176);
const TOOL_COLOR: Color = Color::Rgb(220, 220, 170);
const TOOL_OK: Color = Color::Rgb(106, 153, 85);
const TOOL_ERR: Color = Color::Rgb(244, 71, 71);
const SYS_COLOR: Color = Color::Rgb(106, 153, 85);
const ERR_COLOR: Color = Color::Rgb(244, 71, 71);
const HEADING_FG: Color = Color::Rgb(0, 122, 204);
const STREAMING: Color = Color::Rgb(78, 201, 176);

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render(frame: &mut Frame, app: &mut App, tick_count: u64) {
    let area = frame.area();
    if area.width < 20 || area.height < 8 {
        return;
    }

    let header_height = app.header_info.len().min(4) as u16 + 1;

    let chunks = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .split(area);

    render_header(frame, app, chunks[0]);
    render_main(frame, app, chunks[1]);
    render_status_bar(frame, app, chunks[2], tick_count);
    render_input_pane(frame, app, chunks[3], tick_count);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(" ◆ ".to_string(), Style::default().fg(HEADING_FG).add_modifier(Modifier::BOLD)),
        Span::styled("Ox".to_string(), Style::default().fg(HEADING_FG).add_modifier(Modifier::BOLD)),
        Span::styled(" v0.1.0".to_string(), Style::default().fg(TEXT_DIM)),
        Span::styled(" — AI Programming Assistant".to_string(), Style::default().fg(TEXT)),
    ]));

    for info in &app.header_info {
        if lines.len() < area.height as usize - 1 {
            lines.push(Line::from(vec![
                Span::styled(" ● ".to_string(), Style::default().fg(SYS_COLOR)),
                Span::styled(info.clone(), Style::default().fg(TEXT_DIM)),
            ]));
        }
    }

    let bottom_border = if lines.len() < area.height as usize - 1 { Borders::BOTTOM } else { Borders::NONE };
    let block = Block::default()
        .borders(bottom_border)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(BG));

    let para = Paragraph::new(lines).block(block).style(Style::default().bg(BG));
    frame.render_widget(para, area);
}

fn render_main(frame: &mut Frame, app: &mut App, area: Rect) {
    let sidebar_w = if app.sessions.is_empty() { 0 } else { app.sidebar_width.min(area.width / 4) };

    let main_chunks = Layout::horizontal([
        Constraint::Min(1),
        Constraint::Length(sidebar_w),
    ])
    .split(area);

    render_chat(frame, app, main_chunks[0]);

    if sidebar_w > 0 {
        render_sidebar(frame, app, main_chunks[1]);
    }
}

fn render_chat(frame: &mut Frame, app: &mut App, area: Rect) {
    let spinner_frame = app.spinner_frame;
    let scroll_offset = app.scroll_offset;

    let title = if app.agent_running {
        let s = SPINNER[(spinner_frame as usize) % SPINNER.len()];
        format!(" {s} Ox ")
    } else if app.user_scrolled {
        " Ox ↓ PgDn ".to_string()
    } else {
        " Ox ".to_string()
    };

    let block = Block::default()
        .borders(Borders::NONE)
        .title(title.as_str())
        .title_style(if app.agent_running {
            Style::default().fg(STREAMING).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
        })
        .style(Style::default().bg(BG));

    // No borders, so use full width/height
    let output_width = area.width as usize;
    let inner_height = area.height as usize;

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

fn render_single_line(ol: &OutputLine, width: usize, md_renderer: &super::markdown::MarkdownRenderer) -> Vec<Line<'static>> {
    match ol {
        OutputLine::User(s) => {
            vec![Line::from(vec![
                Span::styled(" ▸ ".to_string(), Style::default().fg(USER_COLOR).add_modifier(Modifier::BOLD)),
                Span::styled(s.clone(), Style::default().fg(TEXT_BRIGHT)),
            ])]
        }
        OutputLine::Assistant(s) => {
            if s.is_empty() {
                return vec![Line::from("")];
            }
            let rendered = md_renderer.render_lines(s, width);
            if rendered.is_empty() {
                vec![Line::from(Span::styled(s.clone(), Style::default().fg(TEXT)))]
            } else {
                rendered
            }
        }
        OutputLine::Tool { name } => {
            vec![Line::from(vec![
                Span::styled(" ⚙ ".to_string(), Style::default().fg(TOOL_COLOR)),
                Span::styled(name.clone(), Style::default().fg(TOOL_COLOR).add_modifier(Modifier::BOLD)),
            ])]
        }
        OutputLine::ToolResult { name: _, summary, is_error } => {
            let (icon, color) = if *is_error { (" ✗", TOOL_ERR) } else { (" ✓", TOOL_OK) };
            vec![Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(summary.clone(), Style::default().fg(color)),
            ])]
        }
        OutputLine::System(s) => {
            vec![Line::from(vec![
                Span::styled(" ● ".to_string(), Style::default().fg(SYS_COLOR)),
                Span::styled(s.clone(), Style::default().fg(TEXT_DIM)),
            ])]
        }
        OutputLine::Error(s) => {
            vec![Line::from(vec![
                Span::styled(" ✗ ".to_string(), Style::default().fg(ERR_COLOR).add_modifier(Modifier::BOLD)),
                Span::styled(s.clone(), Style::default().fg(ERR_COLOR)),
            ])]
        }
        OutputLine::StreamingPartial(s) => {
            if s.is_empty() {
                return vec![Line::from(Span::styled(
                    " ▌".to_string(),
                    Style::default().fg(STREAMING),
                ))];
            }
            let rendered = md_renderer.render_lines(s, width);
            if rendered.len() == 1 && rendered[0].spans.len() <= 2 {
                vec![Line::from(vec![
                    Span::styled(" ".to_string(), Style::default()),
                    Span::styled(s.clone(), Style::default().fg(TEXT)),
                    Span::styled("▌".to_string(), Style::default().fg(STREAMING)),
                ])]
            } else {
                let mut lines = rendered;
                if let Some(last) = lines.last_mut() {
                    let mut new_spans: Vec<Span<'static>> = last.spans.drain(..).collect();
                    new_spans.push(Span::styled("▌".to_string(), Style::default().fg(STREAMING)));
                    *last = Line::from(new_spans);
                }
                lines
            }
        }
        OutputLine::Markdown(s) => {
            if s.is_empty() {
                return vec![Line::from("")];
            }
            md_renderer.render_lines(s, width)
        }
    }
}

fn render_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " Sessions ".to_string(),
        Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
    )));

    for entry in &app.sessions {
        let style = if entry.is_active {
            Style::default().fg(USER_COLOR).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        let icon = if entry.is_active { "▸" } else { " " };
        let info_short: String = entry.info.chars().take(area.width as usize - 4).collect();
        lines.push(Line::from(Span::styled(format!(" {icon} {info_short}"), style)));
    }

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(BG));

    let para = Paragraph::new(lines).block(block).style(Style::default().bg(BG));
    frame.render_widget(para, area);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    let status_style = Style::default().fg(TEXT_BRIGHT).bg(BLUE);

    let left_parts: Vec<Span> = vec![
        Span::styled(format!(" {} ", app.model_name), status_style.add_modifier(Modifier::BOLD)),
        Span::styled(" │ ", status_style),
        Span::styled(format!(" {} ", app.working_dir), status_style),
    ];

    let running = if app.agent_running { "⏳" } else { "" };
    let right_text = format!("{}{} msgs │ {} ", running, app.message_count, app.cost_summary);
    let right_len = right_text.len() as u16;

    let left_line = Line::from(left_parts);
    let right_span = Span::styled(right_text, status_style);

    let left_area = Rect {
        width: area.width.saturating_sub(right_len),
        ..area
    };
    frame.render_widget(
        Paragraph::new(left_line).style(Style::default().bg(BLUE)),
        left_area,
    );

    let right_area = Rect {
        x: area.x + area.width.saturating_sub(right_len),
        width: right_len,
        ..area
    };
    frame.render_widget(
        Paragraph::new(Line::from(right_span)).style(Style::default().bg(BLUE)),
        right_area,
    );
}

fn render_input_pane(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    let prompt = if app.pending_confirmation.is_some() {
        "Y/N/T > "
    } else {
        "ox > "
    };
    let prompt_len = prompt.len();

    let border_color = if app.agent_running { STREAMING } else { BLUE };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(BG_INPUT));

    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            prompt.to_string(),
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD).bg(BG_INPUT),
        ),
        Span::styled(
            app.input.buffer.clone(),
            Style::default().fg(TEXT_BRIGHT).bg(BG_INPUT),
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
