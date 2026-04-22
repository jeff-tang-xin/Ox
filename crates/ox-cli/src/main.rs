mod terminal;

use std::io;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use terminal::app::{App, UserInput};
use terminal::event::{Event, EventHandler};
use terminal::output_pane::OutputLine;
use terminal::render;

fn main() -> anyhow::Result<()> {
    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter("ox=debug")
        .with_writer(io::stderr) // stderr so it doesn't interfere with terminal UI
        .init();

    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run the app; always restore terminal on exit (even on panic).
    let result = run_app(&mut terminal);

    // Restore terminal.
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> anyhow::Result<()> {
    let mut app = App::new();

    // Show startup banner.
    app.output.push_line(OutputLine::Styled {
        prefix: "Ox".to_string(),
        content: "v0.1.0 — AI Programming Assistant".to_string(),
    });
    app.output
        .push_line(OutputLine::Plain("Type a message or /help for commands. /exit to quit.".to_string()));
    app.output.push_line(OutputLine::Plain(String::new()));

    // Spawn the crossterm event polling thread.
    // Tick rate = ~30fps for smooth rendering.
    let events = EventHandler::new(Duration::from_millis(33));

    loop {
        // Render.
        terminal.draw(|frame| render::render(frame, &app))?;

        // Process events (non-blocking drain).
        // Process all available events before next render to stay responsive.
        loop {
            match events.try_recv() {
                Some(Event::Key(key)) => {
                    handle_key_event(&mut app, key);
                }
                Some(Event::Resize(_, _)) => {
                    // ratatui handles resize automatically on next draw.
                }
                Some(Event::Tick) => {
                    // Nothing special on tick — just re-render.
                    break;
                }
                None => break,
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_key_event(app: &mut App, key: crossterm::event::KeyEvent) {
    match (key.code, key.modifiers) {
        // Ctrl+C → quit (when agent is not running; interrupt logic in M9).
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        // Ctrl+D → quit.
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        // Enter → submit input.
        (KeyCode::Enter, _) => {
            if let Some(input) = app.submit_input() {
                match input {
                    UserInput::Exit => {
                        app.output.push_system("Goodbye.");
                        app.should_quit = true;
                    }
                    UserInput::SlashCommand { cmd, args } => {
                        // Placeholder: echo the command back.
                        app.output.push_line(OutputLine::Plain(format!(
                            "[cmd] /{} {}",
                            cmd,
                            if args.is_empty() { "" } else { &args }
                        )));
                    }
                    UserInput::Text(text) => {
                        // Placeholder: echo text back (LLM integration in M3).
                        app.output.push_line(OutputLine::Plain(format!(
                            "[echo] {}",
                            text.trim()
                        )));
                    }
                }
                // Auto-scroll to bottom on new output.
                app.scroll_to_bottom();
            }
        }
        // Backspace.
        (KeyCode::Backspace, _) => {
            app.input.backspace();
        }
        // Delete.
        (KeyCode::Delete, _) => {
            app.input.delete();
        }
        // Arrow keys.
        (KeyCode::Left, _) => {
            app.input.move_left();
        }
        (KeyCode::Right, _) => {
            app.input.move_right();
        }
        (KeyCode::Up, _) => {
            app.input.history_up();
        }
        (KeyCode::Down, _) => {
            app.input.history_down();
        }
        // Home / End.
        (KeyCode::Home, _) => {
            app.input.move_home();
        }
        (KeyCode::End, _) => {
            app.input.move_end();
        }
        // Page Up / Page Down — scroll output.
        (KeyCode::PageUp, _) => {
            app.scroll_up(10);
        }
        (KeyCode::PageDown, _) => {
            app.scroll_down(10);
        }
        // Regular character input.
        (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.input.insert_char(ch);
        }
        _ => {}
    }
}
