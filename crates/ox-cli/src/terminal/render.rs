use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::output_pane::OutputLine;

// ── VS Code-inspired dark theme ────────────────────────────────────
//
// Reference: the 4th screenshot — deep gray backgrounds, rich accent
// colors, clear visual hierarchy between user/AI/tool/system messages.

#[allow(dead_code)]
struct OxTheme {
    // Backgrounds
    bg_base: Color,          // #1E1E1E main background
    bg_surface: Color,       // #252526 slightly raised surface
    bg_input: Color,         // #2D2D2D input area background
    // Borders
    border: Color,           // #404040 subtle border
    border_active: Color,    // #007ACC active/focus border (VS Code blue)
    // Text
    text_primary: Color,     // #D4D4D4 primary text
    text_secondary: Color,   // #808080 secondary/dim text
    text_bright: Color,      // #FFFFFF bright white for emphasis
    // Semantic message colors
    user_msg: Color,         // #4EC9B0 teal-green (VS Code type color)
    ai_msg: Color,           // #D4D4D4 light gray
    tool_msg: Color,         // #DCDCAA yellow-gold (VS Code function color)
    system_msg: Color,       // #6A9955 olive green (VS Code comment color)
    error_msg: Color,        // #F44747 red
    // Accent
    accent: Color,           // #007ACC VS Code blue
    accent_purple: Color,    // #C586C7 purple (VS Code keyword color)
    streaming_cursor: Color, // #4EC9B0 teal cursor
    // Status bar
    status_bg: Color,        // #007ACC status bar background
    status_text: Color,      // #FFFFFF status bar text
}

const THEME: OxTheme = OxTheme {
    bg_base: Color::Rgb(30, 30, 30),
    bg_surface: Color::Rgb(37, 37, 38),
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

// ── Spinner animation ──────────────────────────────────────────────

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ── Message icons ──────────────────────────────────────────────────

const ICON_USER: &str = "▸";
const ICON_TOOL: &str = "⚙";
const ICON_SYSTEM: &str = "●";
#[allow(dead_code)]
const ICON_ERROR: &str = "✕";

/// Render the full layout: output pane (top) + status bar + input pane (bottom).
pub fn render(frame: &mut Frame, app: &App, tick_count: u64) {
    let area = frame.area();

    // Three-region layout: output (flex) + status bar (1 line) + input (min 3 lines).
    let chunks = Layout::vertical([
        Constraint::Min(10),       // output pane — takes all remaining space
        Constraint::Length(1),     // status bar — fixed 1 line
        Constraint::Length(3),     // input pane — fixed 3 lines
    ])
    .split(area);

    render_output_pane(frame, app, chunks[0]);
    render_status_bar(frame, app, chunks[1], tick_count);
    render_input_pane(frame, app, chunks[2], tick_count);
}

fn render_output_pane(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.agent_running {
        let spinner = SPINNER_FRAMES[(app.spinner_frame as usize) % SPINNER_FRAMES.len()];
        format!(" {spinner} Ox ")
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

    let output_width = area.width.saturating_sub(4) as usize; // -2 border, -2 padding

    // Convert output lines to ratatui Lines with semantic styling.
    let lines: Vec<Line> = app
        .output
        .lines
        .iter()
        .flat_map(|ol| match ol {
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

/// Bottom status bar — VS Code style: blue background, white text.
fn render_status_bar(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    // Left section: model + working dir
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

    // Right section: token cost + message count
    let right_text = format!("{} msgs │ {} ", app.message_count, app.cost_summary);
    let right_len = right_text.len() as u16;

    let left_line = Line::from(left_parts);
    let right_span = Span::styled(
        right_text,
        Style::default().fg(THEME.status_text).bg(THEME.status_bg),
    );

    // Render left part
    let left_area = Rect {
        width: area.width.saturating_sub(right_len),
        ..area
    };
    let left_para = Paragraph::new(left_line)
        .style(Style::default().bg(THEME.status_bg));
    frame.render_widget(left_para, left_area);

    // Render right part (right-aligned)
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

    let prompt = "ox❯ ";
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

    // Position the cursor inside the input pane.
    let cursor_x = area.x + 1 + prompt_len as u16 + app.input.cursor_char_pos() as u16;
    let cursor_y = area.y + 1; // +1 for top border
    if cursor_x < area.x + area.width - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
