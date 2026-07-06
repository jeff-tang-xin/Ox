use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::app::{App, ParkFollowUpTag};
use super::input_pane::InputPane;
use super::output_pane::{
    CHAT_THINK_HEIGHT_RATIO, OutputLine, SESSION_WIDTH_MAX, SESSION_WIDTH_MIN,
    SESSION_WIDTH_PERCENT, THINK_PANE_SLIM_HEIGHT, ThinkPaneMode,
};
use crate::helpers::formatting::short_display_path;

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

/// Clamp indexing percent to 0–100 (parsing and embedding share done/total fields per phase).
fn index_pct(done: usize, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    ((done.min(total)) * 100 / total).min(100)
}

/// Pad with spaces so fixed-width progress text fully overwrites prior frame (avoids ghost digits).
fn pad_to_width(text: String, width: u16) -> String {
    let w = UnicodeWidthStr::width(text.as_str());
    if w >= width as usize {
        text
    } else {
        format!("{text}{}", " ".repeat(width as usize - w))
    }
}

#[allow(dead_code)]
fn truncate_display(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}…",
            text.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub fn render(frame: &mut Frame, app: &mut App, tick_count: u64) {
    let area = frame.area();
    if area.width < 20 || area.height < 8 {
        return;
    }
    // Full-frame clear prevents ghost text when layout height changes (e.g. indexing bar).
    frame.render_widget(Clear, area);

    // Adaptive layout based on terminal height
    let is_tiny = area.height < 15;
    let header_height = if is_tiny {
        0
    } else {
        1u16 // compact: model + workflow step in one line
    };

    let input_height = {
        let base = if app.pending_confirmation.is_some() || app.workflow_awaiting_confirmation.is_some() {
            3
        } else {
            2
        };
        // Expand for multiline input (up to 8 lines)
        let lines = app.input.buffer.lines().count();
        (base + lines.saturating_sub(1)).min(8) as u16
    };

    let chunks = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(input_height),
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
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let max_lines = area.height as usize;

    // Line 1: LLM model + embedding model
    let mut title_spans = vec![
        Span::styled(
            " ◆ Ox  ".to_string(),
            Style::default().fg(HEADING_FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.model_name.clone(),
            Style::default().fg(HEADING_FG).add_modifier(Modifier::BOLD),
        ),
    ];
    if !app.embedding_model.is_empty() {
        title_spans.push(Span::styled(
            " │ embed: ".to_string(),
            Style::default().fg(TEXT_DIM),
        ));
        title_spans.push(Span::styled(
            app.embedding_model.clone(),
            Style::default().fg(Color::Rgb(140, 200, 255)),
        ));
    }
    title_spans.push(Span::raw(" "));
    lines.push(Line::from(title_spans));

    // Line 2: Workflow step status
    if let Some(ref wf_info) = app.workflow_display
        && lines.len() < max_lines
    {
        lines.push(Line::from(vec![
            Span::styled(" ● ".to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(
                wf_info.step_name.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Additional header info (within remaining lines)
    let max_lines = area.height as usize;

    // Indexing progress bar
    if app.indexing && app.index_has_progress() && lines.len() < max_lines {
        let (done, total) = app.index_progress_counts();
        let pct = index_pct(done, total);
        let bar_width = 20u64;
        let filled = (done as u64 * bar_width) / total.max(1) as u64;
        let empty = bar_width.saturating_sub(filled);
        let progress_bar = "█".repeat(filled as usize) + &"░".repeat(empty as usize);
        let phase_label = if app.index_phase == "embedding" {
            "Embed"
        } else {
            "AST"
        };
        let detail = if app.index_phase == "embedding" {
            format!("{:>5}/{:<5} entities", done, total)
        } else {
            format!(
                "{:>5}/{:<5} files, {:>6} sym",
                done, total, app.index_symbols
            )
        };
        let progress_text = pad_to_width(
            format!("  ⏳ {phase_label} [{progress_bar}] {pct:>3}% {detail}"),
            area.width,
        );
        lines.push(Line::from(vec![Span::styled(
            progress_text,
            Style::default().fg(TEXT).bg(BG),
        )]));
    }

    for info in app
        .header_info
        .iter()
        .take(max_lines.saturating_sub(lines.len()))
    {
        let text = info.clone();
        lines.push(Line::from(vec![
            Span::styled(" ● ".to_string(), Style::default().fg(SYS_COLOR)),
            Span::styled(text, Style::default().fg(TEXT_DIM)),
        ]));
    }

    let bottom_border = if lines.len() < max_lines {
        Borders::BOTTOM
    } else {
        Borders::NONE
    };
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
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);

    let has_sidebar_content = !app.sessions.is_empty() || !app.plan_items.is_empty();
    // Session column: smallest width slice.
    let sidebar_w = if has_sidebar_content {
        let pct = area.width.saturating_mul(SESSION_WIDTH_PERCENT) / 100;
        pct.clamp(SESSION_WIDTH_MIN, SESSION_WIDTH_MAX)
    } else {
        0
    };

    let row_chunks =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(sidebar_w)]).split(area);

    let tick = app.spinner_frame;
    let think_mode = app.output.think_pane_mode(app.agent_running);

    match think_mode {
        ThinkPaneMode::Hidden => {
            render_chat(frame, app, row_chunks[0]);
        }
        ThinkPaneMode::Slim => {
            let col_chunks = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(THINK_PANE_SLIM_HEIGHT),
            ])
            .split(row_chunks[0]);
            render_chat(frame, app, col_chunks[0]);
            render_think_pane(frame, app, col_chunks[1], tick, think_mode);
        }
        ThinkPaneMode::Expanded => {
            let (chat_ratio, think_ratio) = CHAT_THINK_HEIGHT_RATIO;
            let col_chunks = Layout::vertical([
                Constraint::Ratio(chat_ratio, chat_ratio + think_ratio),
                Constraint::Ratio(think_ratio, chat_ratio + think_ratio),
            ])
            .split(row_chunks[0]);
            render_chat(frame, app, col_chunks[0]);
            render_think_pane(frame, app, col_chunks[1], tick, think_mode);
        }
    }

    if sidebar_w > 0 {
        render_sidebar(frame, app, row_chunks[1]);
    }
}

fn render_chat(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);

    let spinner_frame = app.spinner_frame;
    let scroll_offset = app.scroll_offset;

    // Title: spinner only — indexing detail lives in header + status bar (avoid triple overlay).
    let title = if app.indexing {
        let s = SPINNER[(spinner_frame as usize) % SPINNER.len()];
        pad_to_width(format!(" {s} Indexing… "), area.width)
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
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
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

    let tick = spinner_frame;
    let md = &app.md_renderer;
    let out = &mut app.output;
    let (mut lines, _total) =
        out.get_visible_lines(output_width, inner_height, scroll_offset, |ol, w| {
            render_single_line(ol, w, md, tick)
        });

    // Pad to inner height so leftover rows don't show previous-frame ghosts.
    while lines.len() < inner_height {
        lines.push(Line::from(Span::styled(
            " ".repeat(output_width.max(1)),
            Style::default().bg(BG),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Dedicated think pane — expands only while reasoning / agent activity.
fn render_think_pane(frame: &mut Frame, app: &App, area: Rect, tick: u64, _mode: ThinkPaneMode) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);

    const THINK_COLOR: Color = Color::Rgb(160, 140, 220);
    const THINK_BG: Color = Color::Rgb(28, 24, 40);

    let active = app.output.has_live_thinking() || app.agent_running;
    let spinner = SPINNER[(tick as usize) % SPINNER.len()];
    let title = if active {
        format!(" {spinner} Think ")
    } else {
        " Think ".to_string()
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER))
        .title(title)
        .title_style(
            Style::default()
                .fg(THINK_COLOR)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(THINK_BG));

    let inner = block.inner(area);
    let width = inner.width as usize;
    let max_lines = inner.height as usize;

    let row_style = Style::default().fg(THINK_COLOR).bg(THINK_BG);
    let pad_bg = Style::default().bg(THINK_BG);

    let text_lines: Vec<String> = if let Some(dock) = &app.output.live_thinking {
        let mut lines = Vec::new();
        if let Some(hint) = dock.status_hint.as_deref().filter(|s| !s.trim().is_empty()) {
            let max_chars = width.saturating_sub(4).max(12);
            let h = if hint.chars().count() > max_chars {
                format!(
                    "{}…",
                    hint.chars()
                        .take(max_chars.saturating_sub(1))
                        .collect::<String>()
                )
            } else {
                hint.to_string()
            };
            lines.push(h);
        }
        let body_max = max_lines.saturating_sub(lines.len());
        if body_max > 0 && !dock.text.trim().is_empty() {
            let mut body =
                super::output_pane::thinking_pane_lines(&dock.text, None, width, body_max);
            lines.append(&mut body);
        } else if lines.len() == 1 && body_max > 0 {
            lines.push("等待模型推理输出…".to_string());
        }
        lines
    } else if app.agent_running {
        let hint = if app.status.trim().is_empty() {
            "等待模型推理…".to_string()
        } else {
            app.status.clone()
        };
        super::output_pane::thinking_pane_lines(&hint, None, width, max_lines)
    } else {
        return;
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, line) in text_lines.iter().enumerate() {
        let style = row_style;
        let prefix = if i == 0 {
            format!(" {spinner} ")
        } else {
            "    ".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(prefix, pad_bg),
            Span::styled(line.clone(), style),
        ]));
    }

    while lines.len() < max_lines {
        lines.push(Line::from(Span::styled(" ".repeat(width.max(1)), pad_bg)));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_single_line(
    ol: &OutputLine,
    width: usize,
    md_renderer: &super::markdown::MarkdownRenderer,
    _tick: u64,
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
            // Tool calls with action-specific icons
            let icon = match name.as_str() {
                "file_read" => " 📖 ",
                "file_write" | "edit_file" | "delete_range" => " ✏️ ",
                "find_symbol" | "code_search" | "file_search" => " 🔎 ",
                "shell_exec" => " 💻 ",
                "code_graph" => " 🕸 ",
                "finish" | "complete_and_check" => " ✅ ",
                _ => " ⚙ ",
            };
            let mut spans = vec![
                Span::styled(icon, Style::default().fg(TOOL_COLOR)),
                Span::styled(
                    name.clone(),
                    Style::default().fg(TOOL_COLOR).add_modifier(Modifier::BOLD),
                ),
            ];
            if let Some(cmd) = detail {
                let short = cmd.chars().take(80).collect::<String>();
                spans.push(Span::styled(
                    format!(" {}", short),
                    Style::default().fg(TEXT_DIM),
                ));
            }
            vec![Line::from(spans)]
        }
        OutputLine::Thinking { .. } => Vec::new(),
        OutputLine::ToolResult {
            name: _,
            summary,
            is_error,
        } => {
            let (icon, color) = if *is_error {
                (" ✗", TOOL_ERR)
            } else {
                (" ✓", TOOL_OK)
            };
            let short: String = summary.chars().take(120).collect();
            vec![Line::from(vec![
                Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(short, Style::default().fg(color)),
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
            // Apply color to tool-prefixed output lines (──, ✓, ✗)
            let first = s.lines().next().unwrap_or("").trim();
            if first.starts_with('✓') || first.starts_with("── DATA") {
                let rendered = md_renderer.render_lines(s, width);
                if rendered.is_empty() {
                    vec![Line::from(Span::styled(s.clone(), Style::default().fg(TOOL_OK)))]
                } else {
                    rendered
                }
            } else if first.starts_with('✗') || s.contains("Error:") || s.contains("错误") {
                let rendered = md_renderer.render_lines(s, width);
                if rendered.is_empty() {
                    vec![Line::from(Span::styled(s.clone(), Style::default().fg(TOOL_ERR)))]
                } else {
                    rendered
                }
            } else {
                md_renderer.render_lines(s, width)
            }
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
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Tasks Section ──
    if !app.plan_items.is_empty() {
        lines.push(Line::from(Span::styled(
            " Tasks ".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for item in &app.plan_items {
            let (icon, style) = match item.status {
                super::app::PlanItemStatus::Done => ("✅", Style::default().fg(TOOL_OK)),
                super::app::PlanItemStatus::Pending => ("⏳", Style::default().fg(Color::Yellow)),
                super::app::PlanItemStatus::Cancelled => ("❌", Style::default().fg(TEXT_DIM)),
            };
            let display: String = item
                .file
                .chars()
                .take(area.width.saturating_sub(6) as usize)
                .collect();
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
            let display_short: String = entry
                .display_name()
                .chars()
                .take(area.width as usize - 4)
                .collect();
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

#[allow(dead_code)]
fn render_findings_panel(frame: &mut Frame, app: &App, area: Rect) {
    let Some(panel) = &app.findings_panel else {
        return;
    };
    if area.width < 10 || area.height < 2 {
        return;
    }
    frame.render_widget(Clear, area);

    let inner_w = area.width.saturating_sub(4) as usize;
    let issue_budget = inner_w.saturating_sub(28).max(24);

    let mut lines: Vec<Line> = Vec::new();
    if !panel.summary.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("摘要 ", Style::default().fg(TEXT_DIM)),
            Span::styled(
                truncate_display(&panel.summary, inner_w.saturating_sub(6)),
                Style::default().fg(TEXT),
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        "1-9 范围 · c 确认 · d 讨论 · n 新任务",
        Style::default().fg(TEXT_DIM),
    )));
    for row in &panel.rows {
        let mark = if row.in_scope { "☑" } else { "☐" };
        let status_icon = match row.status {
            ox_core::agent::findings::FindingStatus::Done => "✅",
            ox_core::agent::findings::FindingStatus::InProgress
            | ox_core::agent::findings::FindingStatus::AwaitingVerify => "🔄",
            ox_core::agent::findings::FindingStatus::Skipped
            | ox_core::agent::findings::FindingStatus::WontFix => "⏭",
            ox_core::agent::findings::FindingStatus::Disputed => "⚠️",
            _ => "📌",
        };
        let loc = if row.file.is_empty() {
            truncate_display(&row.symbol, 32)
        } else {
            truncate_display(&short_display_path(&row.file, 36), 36)
        };
        let sev = truncate_display(&row.severity, 8);
        let issue = truncate_display(&row.issue, issue_budget);
        lines.push(Line::from(vec![
            Span::styled(format!("{mark} "), Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("#{} ", row.index),
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("[{sev}] "), Style::default().fg(HEADING_FG)),
            Span::styled(format!("{status_icon} "), Style::default().fg(TEXT_DIM)),
            Span::styled(format!("{loc} "), Style::default().fg(TEXT_DIM)),
            Span::styled(issue, Style::default().fg(TEXT)),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .style(Style::default().bg(BG))
        .title(" Findings — 选择修复范围 ")
        .title_style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    frame.render_widget(Clear, area);

    // When indexing or agent running, show progress status prominently
    let (status_style, status_bg) = if app.indexing {
        (
            Style::default().fg(TEXT_BRIGHT).bg(Color::Rgb(80, 60, 0)),
            Color::Rgb(80, 60, 0),
        )
    } else {
        (Style::default().fg(TEXT_BRIGHT).bg(BLUE), BLUE)
    };

    // Left side: Model and working directory (essential info)
    let dir_short = short_display_path(&app.working_dir, 28);
    let mut left_parts: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", app.model_name),
            status_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", status_style),
        Span::styled(format!(" {dir_short} "), status_style),
    ];

    // Show app.status (indexing progress, thinking, etc.)
    if !app.status.is_empty() {
        let available = (area.width as usize).saturating_sub(80);
        let status_text = if app.status.chars().count() > available && available > 10 {
            format!(
                "{}…",
                app.status
                    .chars()
                    .take(available.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            app.status.clone()
        };
        // Pad with spaces to ensure old text is fully overwritten on re-render
        let padded = format!(" {:<width$} ", status_text, width = available.min(80));
        left_parts.push(Span::styled(" │ ", status_style));
        left_parts.push(Span::styled(
            padded,
            status_style.add_modifier(Modifier::BOLD),
        ));
    }

    // Right side: phase + message count and cost (compact format)
    let running_indicator = if app.agent_running { "⏳ " } else { "" };
    let phase_part = if app.workflow_phase_line.is_empty() {
        String::new()
    } else {
        format!("{} │ ", app.workflow_phase_line)
    };
    let right_text = format!(
        "{}{}{} msgs | {}",
        running_indicator, phase_part, app.message_count, app.cost_summary
    );
    let right_width = UnicodeWidthStr::width(right_text.as_str()) as u16;

    // Single full-width line — avoids uncovered gap between left/right widgets.
    let right_text_padded = pad_to_width(right_text, right_width.max(1));
    left_parts.push(Span::raw(" "));
    left_parts.push(Span::styled(right_text_padded, status_style));

    frame.render_widget(
        Paragraph::new(Line::from(left_parts)).style(Style::default().bg(status_bg)),
        area,
    );
}

fn prompt_span(text: &str, fg: Color) -> (Vec<Span<'static>>, usize) {
    let owned = text.to_string();
    let len = owned.len();
    (
        vec![Span::styled(
            owned,
            Style::default()
                .fg(fg)
                .add_modifier(Modifier::BOLD)
                .bg(BG_INPUT),
        )],
        len,
    )
}

fn park_follow_up_prompt_spans(app: &App) -> (Vec<Span<'static>>, usize) {
    let base = Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
        .bg(BG_INPUT);
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
        .bg(BG_INPUT);
    let tag_style = Style::default()
        .fg(Color::Rgb(255, 200, 80))
        .add_modifier(Modifier::BOLD)
        .bg(BG_INPUT);
    let dim = Style::default().fg(TEXT_DIM).bg(BG_INPUT);

    if app.workflow_awaiting_confirmation == Some(4) && app.park_follow_up_tag.is_none() {
        let base = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
            .bg(BG_INPUT);
        let key_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
            .bg(BG_INPUT);
        let spans = vec![
            Span::styled("❯ ".to_string(), base),
            Span::styled("1-9".to_string(), key_style),
            Span::styled(" 切换  ".to_string(), base),
            Span::styled("c".to_string(), key_style),
            Span::styled(" 确认  ".to_string(), base),
            Span::styled("d".to_string(), key_style),
            Span::styled(" 讨论  ".to_string(), base),
            Span::styled("n".to_string(), key_style),
            Span::styled(" 新任务 > ".to_string(), base),
        ];
        let len = "❯ 1-9 切换  c 确认  d 讨论  n 新任务 > ".len();
        return (spans, len);
    }

    if let Some(tag) = app.park_follow_up_tag {
        let (label, hint) = match tag {
            ParkFollowUpTag::Continue => ("[继续]", "说明修复范围 > "),
            ParkFollowUpTag::Feedback => ("[意见·只读]", "只讨论不实施 > "),
            ParkFollowUpTag::NewTask => ("[新任务]", "描述任务 > "),
        };
        let spans = vec![
            Span::styled("❯ ".to_string(), base),
            Span::styled(label.to_string(), tag_style),
            Span::styled(format!(" {hint}"), dim),
        ];
        let len = format!("❯ {label} {hint}").len();
        return (spans, len);
    }

    let spans = vec![
        Span::styled("❯ 按 ".to_string(), base),
        Span::styled("1".to_string(), key_style),
        Span::styled("继续 ".to_string(), base),
        Span::styled("2".to_string(), key_style),
        Span::styled("意见 ".to_string(), base),
        Span::styled("3".to_string(), key_style),
        Span::styled("新任务 > ".to_string(), base),
    ];
    let len = "❯ 按 1继续 2意见 3新任务 > ".len();
    (spans, len)
}

fn park_follow_up_block_title(app: &App) -> Line<'static> {
    if app.workflow_awaiting_confirmation == Some(4) && app.park_follow_up_tag.is_none() {
        return Line::from(Span::styled(
            " 1-9 切换范围 · c 确认实施 · d 讨论 · n 新任务 ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.park_follow_up_tag.is_some() {
        return Line::from(Span::styled(
            " 已选择 — 在下方补充说明后按 Enter ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(vec![
        Span::styled(
            " 快捷键 ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "[1]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " 继续  ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "[2]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " 意见·只读  ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "[3]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " 新任务 ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn render_input_pane(frame: &mut Frame, app: &App, area: Rect, _tick_count: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);

    let indexing_prompt: String;
    let (prompt_spans, prompt_len) =
        if app.pending_confirmation.as_ref().is_some_and(|p| {
            p.tool_call_id == "__iteration_limit__" || p.tool_call_id == "__budget__"
        }) {
            prompt_span("❯ Y/N > ", Color::Yellow)
        } else if app.pending_confirmation.is_some() {
            prompt_span("❯ Y/N/T > ", Color::Yellow)
        } else if app.workflow_awaiting_confirmation == Some(0) {
            prompt_span("❯ 澄清 > ", Color::Magenta)
        } else if app.workflow_awaiting_confirmation == Some(4) {
            park_follow_up_prompt_spans(app)
        } else if app.workflow_awaiting_confirmation
            == Some(crate::terminal::app::UNIFIED_GATE_FINISH_STEP)
        {
            prompt_span("❯ f结束 / c继续 > ", Color::Yellow)
        } else if app.workflow_awaiting_confirmation
            == Some(crate::terminal::app::UNIFIED_GATE_DELIVER_STEP)
        {
            prompt_span("❯ c确认 > ", Color::Yellow)
        } else if app.workflow_awaiting_confirmation == Some(2) {
            prompt_span("❯ 确认/修改 > ", Color::Cyan)
        } else if app.workflow_awaiting_confirmation.is_some() {
            prompt_span("❯ 确认 > ", Color::Cyan)
        } else if app.indexing {
            let s = SPINNER[app.spinner_frame as usize % SPINNER.len()];
            indexing_prompt = format!("{s} > ");
            prompt_span(&indexing_prompt, Color::Yellow)
        } else if app.agent_running {
            prompt_span("▸ ox > ", STREAMING)
        } else {
            prompt_span("ox > ", BLUE)
        };

    let border_color = if app.indexing {
        Color::Yellow
    } else if app.agent_running {
        STREAMING
    } else {
        BLUE
    };

    // Add visual indicator for confirmation mode
    let block = if app
        .pending_confirmation
        .as_ref()
        .is_some_and(|p| p.tool_call_id == "__iteration_limit__")
    {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(BG_INPUT))
            .title(" 迭代续跑确认 ")
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else if app.pending_confirmation.is_some() {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(BG_INPUT))
            .title(" Tool Confirmation ")
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else if app.workflow_awaiting_confirmation == Some(0) {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Magenta))
            .style(Style::default().bg(BG_INPUT))
            .title(" 需求澄清 — 请直接回答问题 ")
            .title_style(
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )
    } else if app.workflow_awaiting_confirmation == Some(4) {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Magenta))
            .style(Style::default().bg(BG_INPUT))
            .title(park_follow_up_block_title(app))
            .title_style(Style::default().fg(Color::Magenta))
    } else if app.workflow_awaiting_confirmation
        == Some(crate::terminal::app::UNIFIED_GATE_FINISH_STEP)
    {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(BG_INPUT))
            .title(" Finish 门禁 — f 结束本轮 · c 继续 ")
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else if app.workflow_awaiting_confirmation
        == Some(crate::terminal::app::UNIFIED_GATE_DELIVER_STEP)
    {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(BG_INPUT))
            .title(" 交付门禁 — c 确认 · 可输入讨论 ")
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
    } else if app.workflow_awaiting_confirmation == Some(2) {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(BG_INPUT))
            .title(" 审阅完成 — ok/继续/确认 开始执行，或输入修改意见 ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
    } else if app.workflow_awaiting_confirmation.is_some() {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(BG_INPUT))
            .title(" Workflow Confirmation ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
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

    let mut line_spans = prompt_spans;
    line_spans.push(Span::styled(
        visible_text,
        Style::default().fg(TEXT_BRIGHT).bg(BG_INPUT),
    ));
    let paragraph = Paragraph::new(Line::from(line_spans)).block(block);

    frame.render_widget(paragraph, area);

    // Calculate cursor position using visual width of visible portion before cursor
    let visible_before_cursor = if scroll_offset <= app.input.cursor {
        app.input
            .buffer
            .get(scroll_offset..app.input.cursor)
            .unwrap_or("")
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
