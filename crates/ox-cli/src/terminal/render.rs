use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::input_pane::InputPane;
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

    // Adaptive layout based on terminal height
    let is_tiny = area.height < 15;
    let indexing_bar = if app.indexing && app.index_total_files > 0 { 1u16 } else { 0 };
    let has_workflow = app.workflow_display.is_some() as u16;
    let header_height = if is_tiny { 0 } else { (2u16 + has_workflow + indexing_bar).min(5) };

    let input_height = if app.pending_confirmation.is_some() { 3 } else { 2 };

    let chunks = Layout::vertical([
        Constraint::Length(header_height), // Header (hidden on tiny screens)
        Constraint::Min(3),                // Main output
        Constraint::Length(1),             // Status bar
        Constraint::Length(input_height),  // Input pane
    ])
    .split(area);

    render_header(frame, app, chunks[0]);
    render_main(frame, app, chunks[1]);
    render_status_bar(frame, app, chunks[2], tick_count);
    render_input_pane(frame, app, chunks[3], tick_count);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let max_lines = area.height as usize;

    // Line 1: Title (all owned to satisfy 'static)
    let title_line = format!(" ◆ Ox  {} ", app.model_name);
    lines.push(Line::from(vec![
        Span::styled(title_line, Style::default().fg(HEADING_FG).add_modifier(Modifier::BOLD)),
    ]));

    // Line 2: Workflow step status
    if let Some(ref wf_info) = app.workflow_display && lines.len() < max_lines {
        lines.push(Line::from(vec![
            Span::styled(" ● ".to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(
                wf_info.step_name.clone(),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Additional header info (within remaining lines)
    let max_lines = area.height as usize;

    // Indexing progress bar
    if app.indexing && app.index_total_files > 0 && lines.len() < max_lines {
        let pct = (app.index_files_done * 100) / app.index_total_files.max(1);
        let bar_width = 20u64;
        let filled = (app.index_files_done as u64 * bar_width) / app.index_total_files.max(1) as u64;
        let empty = bar_width - filled;
        let progress_bar = "█".repeat(filled as usize) + &"░".repeat(empty as usize);
        lines.push(Line::from(vec![
            Span::styled("  ⏳ ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("[{progress_bar}] {pct}%"), Style::default().fg(Color::Green)),
            Span::styled(format!(" {}/{} files", app.index_files_done, app.index_total_files), Style::default().fg(TEXT)),
        ]));
    }

    for info in app.header_info.iter().take(max_lines.saturating_sub(lines.len())) {
        let text = info.clone();
        lines.push(Line::from(vec![
            Span::styled(" ● ".to_string(), Style::default().fg(SYS_COLOR)),
            Span::styled(text, Style::default().fg(TEXT_DIM)),
        ]));
    }

    let bottom_border = if lines.len() < max_lines { Borders::BOTTOM } else { Borders::NONE };
    let block = Block::default()
        .borders(bottom_border)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(BG));

    let para = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(BG));
    frame.render_widget(para, area);
}

fn render_main(frame: &mut Frame, app: &mut App, area: Rect) {
    let has_sidebar_content = !app.sessions.is_empty() || !app.plan_items.is_empty();
    let sidebar_w = if has_sidebar_content {
        (area.width / 5).clamp(18, 35) // Adaptive sidebar width
    } else {
        0
    };

    let main_chunks =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(sidebar_w)]).split(area);

    render_chat(frame, app, main_chunks[0]);

    if sidebar_w > 0 {
        render_sidebar(frame, app, main_chunks[1]);
    }
}

fn render_chat(frame: &mut Frame, app: &mut App, area: Rect) {
    let spinner_frame = app.spinner_frame;
    let scroll_offset = app.scroll_offset;

    // Enhanced title with better scroll indication
    let title = if app.indexing {
        let s = SPINNER[(spinner_frame as usize) % SPINNER.len()];
        format!(" {s} Indexing… ")
    } else if app.agent_running {
        let s = SPINNER[(spinner_frame as usize) % SPINNER.len()];
        format!(" {s} Ox ")
    } else if app.user_scrolled && scroll_offset > 0 {
        format!(" Ox ↓ {} lines up ", scroll_offset)
    } else if app.user_scrolled {
        " Ox ↓ Scrolled ".to_string()
    } else {
        " Ox ".to_string()
    };

    let block = Block::default()
        .borders(Borders::NONE)
        .title(title.as_str())
        .title_style(if app.indexing {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else if app.agent_running {
            Style::default().fg(STREAMING).add_modifier(Modifier::BOLD)
        } else if app.user_scrolled {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
        })
        .style(Style::default().bg(BG));

    // Use block's inner area to account for title line.
    let inner = block.inner(area);
    let output_width = inner.width as usize;
    let inner_height = inner.height as usize;

    let md = &app.md_renderer;
    let out = &mut app.output;
    let (lines, _total) =
        out.get_visible_lines(output_width, inner_height, scroll_offset, |ol, w| {
            render_single_line(ol, w, md)
        });

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

fn render_single_line(
    ol: &OutputLine,
    width: usize,
    md_renderer: &super::markdown::MarkdownRenderer,
) -> Vec<Line<'static>> {
    match ol {
        OutputLine::User(s) => {
            // User messages with distinct background
            vec![Line::from(vec![
                Span::styled(
                    " ▸ ".to_string(),
                    Style::default().fg(USER_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(s.clone(), Style::default().fg(TEXT_BRIGHT)),
            ])]
        }
        OutputLine::Assistant(s) => {
            if s.is_empty() {
                return vec![Line::from("")];
            }
            let rendered = md_renderer.render_lines(s, width);
            if rendered.is_empty() {
                vec![Line::from(Span::styled(
                    s.clone(),
                    Style::default().fg(TEXT),
                ))]
            } else {
                rendered
            }
        }
        OutputLine::Tool { name, detail } => {
            // Tool calls with subtle background indicator
            let mut spans = vec![
                Span::styled(
                    " ⚙ ".to_string(),
                    Style::default().fg(TOOL_COLOR).bg(Color::Rgb(40, 40, 30)),
                ),
                Span::styled(
                    name.clone(),
                    Style::default().fg(TOOL_COLOR).add_modifier(Modifier::BOLD),
                ),
            ];
            if let Some(cmd) = detail {
                spans.push(Span::styled(
                    format!(" → {}", cmd),
                    Style::default().fg(TEXT_DIM),
                ));
            }
            vec![Line::from(spans)]
        }
        OutputLine::ToolResult {
            name: _,
            summary,
            is_error,
        } => {
            // Tool results with clear success/error indicators
            let (icon, color, bg) = if *is_error {
                (" ✗ ", TOOL_ERR, Color::Rgb(50, 20, 20))
            } else {
                (" ✓ ", TOOL_OK, Color::Rgb(20, 40, 20))
            };
            vec![Line::from(vec![
                Span::styled(
                    icon,
                    Style::default()
                        .fg(color)
                        .add_modifier(Modifier::BOLD)
                        .bg(bg),
                ),
                Span::styled(summary.clone(), Style::default().fg(color)),
            ])]
        }
        OutputLine::System(s) => {
            // System messages with subtle indicator
            vec![Line::from(vec![
                Span::styled(" ● ".to_string(), Style::default().fg(SYS_COLOR)),
                Span::styled(s.clone(), Style::default().fg(TEXT_DIM)),
            ])]
        }
        OutputLine::Error(s) => {
            // Error messages with prominent styling
            vec![Line::from(vec![
                Span::styled(
                    " ✗ ".to_string(),
                    Style::default()
                        .fg(ERR_COLOR)
                        .add_modifier(Modifier::BOLD)
                        .bg(Color::Rgb(60, 10, 10)),
                ),
                Span::styled(
                    s.clone(),
                    Style::default().fg(ERR_COLOR).add_modifier(Modifier::BOLD),
                ),
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
                    new_spans.push(Span::styled(
                        "▌".to_string(),
                        Style::default().fg(STREAMING),
                    ));
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
        OutputLine::ToolLog {
            tool_call_id: _,
            message,
            timestamp: _,
        } => {
            // Tool execution logs in small dim font below tool card
            vec![Line::from(vec![
                Span::styled("   └─ ", Style::default().fg(TEXT_DIM)),
                Span::styled(
                    message.clone(),
                    Style::default().fg(TEXT_DIM).add_modifier(Modifier::DIM),
                ),
            ])]
        }
    }
}

fn render_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Tasks Section ──
    if !app.plan_items.is_empty() {
        lines.push(Line::from(Span::styled(
            " Tasks ".to_string(),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
        for item in &app.plan_items {
            let (icon, style) = match item.status {
                super::app::PlanItemStatus::Done => ("✅", Style::default().fg(TOOL_OK)),
                super::app::PlanItemStatus::Pending => ("⏳", Style::default().fg(Color::Yellow)),
                super::app::PlanItemStatus::Cancelled => ("❌", Style::default().fg(TEXT_DIM)),
            };
            let display: String = item.file.chars().take(area.width.saturating_sub(6) as usize).collect();
            lines.push(Line::from(Span::styled(
                format!(" {icon} {display}"),
                style,
            )));
        }
        lines.push(Line::from("")); // separator
    }

    // ── Sessions Section ──
    if !app.sessions.is_empty() {
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
            let display_short: String = entry.display_name().chars().take(area.width as usize - 4).collect();
            lines.push(Line::from(Span::styled(
                format!(" {icon} {display_short}"),
                style,
            )));
        }
    }

    if lines.is_empty() {
        return;
    }

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(BG));

    let para = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(BG));
    frame.render_widget(para, area);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    // When indexing or agent running, show progress status prominently
    let (status_style, status_bg) = if app.indexing {
        (Style::default().fg(TEXT_BRIGHT).bg(Color::Rgb(80, 60, 0)), Color::Rgb(80, 60, 0))
    } else {
        (Style::default().fg(TEXT_BRIGHT).bg(BLUE), BLUE)
    };

    // Left side: Model and working directory (essential info)
    let mut left_parts: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", app.model_name),
            status_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", status_style),
        Span::styled(format!(" {} ", app.working_dir), status_style),
    ];

    // Show app.status (indexing progress, thinking, etc.)
    if !app.status.is_empty() {
        let available = (area.width as usize).saturating_sub(80);
        let status_text = if app.status.chars().count() > available && available > 10 {
            format!("{}…", app.status.chars().take(available.saturating_sub(1)).collect::<String>())
        } else {
            app.status.clone()
        };
        // Pad with spaces to ensure old text is fully overwritten on re-render
        let padded = format!(" {:<width$} ", status_text, width = available.min(80));
        left_parts.push(Span::styled(" │ ", status_style));
        left_parts.push(Span::styled(padded, status_style.add_modifier(Modifier::BOLD)));
    }

    // Right side: Message count and cost (compact format)
    let running_indicator = if app.agent_running { "⏳ " } else { "" };
    let right_text = format!(
        "{}{} msgs | {}",
        running_indicator, app.message_count, app.cost_summary
    );
    let right_width = UnicodeWidthStr::width(right_text.as_str()) as u16;

    let left_line = Line::from(left_parts);
    let right_span = Span::styled(right_text, status_style);

    let left_area = Rect {
        width: area.width.saturating_sub(right_width),
        ..area
    };
    frame.render_widget(
        Paragraph::new(left_line).style(Style::default().bg(status_bg)),
        left_area,
    );

    let right_area = Rect {
        x: area.x + area.width.saturating_sub(right_width),
        width: right_width,
        ..area
    };
    frame.render_widget(
        Paragraph::new(Line::from(right_span)).style(Style::default().bg(status_bg)),
        right_area,
    );
}

fn render_input_pane(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    let indexing_prompt: String;
    let (prompt, prompt_style) = if app.pending_confirmation.is_some() {
        (
            "❯ Y/N/T > ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
                .bg(BG_INPUT),
        )
    } else if app.indexing {
        let s = SPINNER[app.spinner_frame as usize % SPINNER.len()];
        indexing_prompt = format!("{} indexing > ", s);
        (&*indexing_prompt as &str, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD).bg(BG_INPUT))
    } else if app.agent_running {
        (
            "▸ ox > ",
            Style::default()
                .fg(STREAMING)
                .add_modifier(Modifier::BOLD)
                .bg(BG_INPUT),
        )
    } else {
        (
            "ox > ",
            Style::default()
                .fg(BLUE)
                .add_modifier(Modifier::BOLD)
                .bg(BG_INPUT),
        )
    };
    let prompt_len = prompt.len();

    let border_color = if app.indexing {
        Color::Yellow
    } else if app.agent_running {
        STREAMING
    } else {
        BLUE
    };

    // Add visual indicator for confirmation mode
    let block = if app.pending_confirmation.is_some() {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(BG_INPUT))
            .title(" Confirmation Mode ")
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(BG_INPUT))
    };

    // Calculate available width for input text (excluding borders and padding)
    let input_width = area.width.saturating_sub(2) as usize; // -2 for left/right borders

    // Get visible content based on available width
    let (visible_text, scroll_offset) = app.input.get_visible_content(input_width);

    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(prompt.to_string(), prompt_style),
        Span::styled(visible_text, Style::default().fg(TEXT_BRIGHT).bg(BG_INPUT)),
    ]))
    .block(block);

    frame.render_widget(paragraph, area);

    // Calculate cursor position using visual width of visible portion before cursor
    let visible_before_cursor = if scroll_offset <= app.input.cursor {
        app.input.buffer.get(scroll_offset..app.input.cursor).unwrap_or("")
    } else {
        ""
    };
    let cursor_visual_offset = InputPane::visual_width(visible_before_cursor);

    let cursor_x = area.x + 1 + prompt_len as u16 + cursor_visual_offset as u16;
    let cursor_y = area.y + 1;

    // Only set cursor if it's within the visible area
    if cursor_x < area.x + area.width - 1 && cursor_x >= area.x {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
