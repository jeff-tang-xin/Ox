mod terminal;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use ox_core::agent::{self, AgentToUiEvent};
use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::interrupt::{InterruptAction, InterruptController};
use ox_core::config::OxConfig;
use ox_core::context::{self, ContextBuilder};
use ox_core::cost::CostTracker;
use ox_core::llm::{self, LlmProvider};
use ox_core::message::{Message, Session};
use ox_core::runtime;
use ox_core::safety::TrustManager;
use ox_core::slash::{self, SlashCommand};
use ox_core::tools::{ToolContext, ToolRegistry};
use terminal::app::{App, UserInput};
use terminal::event::{Event, EventHandler};
use terminal::output_pane::OutputLine;
use terminal::render;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging to stderr (doesn't interfere with TUI).
    tracing_subscriber::fmt()
        .with_env_filter("ox=debug")
        .with_writer(io::stderr)
        .init();

    // Install panic hook to restore terminal on panic.
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore.
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_panic(info);
    }));

    // Load config (defaults if file missing).
    let config = OxConfig::load(None)?;

    // Detect runtime environment.
    let rt_env = runtime::detect_runtime();
    tracing::info!("{}", rt_env.banner_summary());

    // Try to create LLM provider (may fail if no API key).
    let provider: Option<Arc<dyn LlmProvider>> =
        match llm::create_provider(&config.models.default, &config.models) {
            Ok(p) => {
                tracing::info!("LLM provider: {}", p.model_name());
                Some(Arc::from(p))
            }
            Err(e) => {
                tracing::warn!("No LLM provider: {e}. Running in echo mode.");
                None
            }
        };

    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run the app; always restore terminal on exit.
    let result = run_app(&mut terminal, &config, rt_env, provider).await;

    // Restore terminal.
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &OxConfig,
    mut rt_env: runtime::RuntimeEnvironment,
    provider: Option<Arc<dyn LlmProvider>>,
) -> anyhow::Result<()> {
    let mut app = App::new();

    // Show startup banner with runtime info.
    app.output.push_line(OutputLine::Styled {
        prefix: "Ox".to_string(),
        content: "v0.1.0 — AI Programming Assistant".to_string(),
    });
    app.output
        .push_line(OutputLine::Plain(rt_env.banner_summary()));

    // Startup check: warn if no config file exists.
    if !OxConfig::config_exists() {
        app.output.push_system(
            "No config file found. Run /init to create ~/.ox/config.toml with default settings.",
        );
    }

    if provider.is_some() {
        app.output.push_line(OutputLine::Plain(
            "Type a message or /help for commands. /exit to quit.".to_string(),
        ));
    } else {
        app.output.push_system(
            "No API key configured. Set env var or [models.providers.*] api_key in ~/.ox/config.toml. Running in echo mode.",
        );
    }

    // Session persistence: load or create.
    let session_dir = rt_env.working_dir.join(&config.session.session_dir);
    let mut session = if config.session.auto_restore {
        match Session::load(&session_dir)? {
            Some(s) => {
                app.output.push_line(OutputLine::Plain(format!(
                    "Session restored ({} messages)",
                    s.user_message_count()
                )));
                s
            }
            None => Session::new(&session_dir, &rt_env.project_id)?,
        }
    } else {
        Session::new(&session_dir, &rt_env.project_id)?
    };
    app.output.push_line(OutputLine::Plain(String::new()));

    // Create tool registry and context (shared via Arc for tokio::spawn).
    let tool_registry = Arc::new(ToolRegistry::new());
    let tool_ctx = Arc::new(ToolContext {
        runtime: rt_env.clone(),
        working_dir: rt_env.working_dir.clone(),
    });

    tracing::info!("Tools registered: {:?}", tool_registry.names());

    // Build system prompt using context module.
    let system_prompt = context::build_system_prompt(&rt_env, &tool_registry, None);

    // Context builder for assembling LLM messages within token budgets.
    let context_builder = ContextBuilder::default();
    let context_window = provider
        .as_ref()
        .map(|p| p.context_window_size())
        .unwrap_or(128_000);

    // Cost tracking.
    let ox_dir = rt_env.working_dir.join(".ox");
    let mut cost_tracker = CostTracker::load_or_create(&ox_dir).unwrap_or_else(|e| {
        tracing::warn!("Failed to load cost tracker: {e}");
        CostTracker::load_or_create(&std::env::temp_dir()).expect("temp dir fallback")
    });

    // Model name for cost recording.
    let model_name = provider
        .as_ref()
        .map(|p| p.model_name().to_string())
        .unwrap_or_default();

    // Session-scoped trust manager for tool confirmation.
    let mut trust_manager = TrustManager::new();

    // Interrupt controller for Ctrl+C handling.
    let mut interrupt_ctrl = InterruptController::new();

    // Interjection buffer for user input during agent runs.
    let mut interjection_buf = InterjectionBuffer::new();

    // Crossterm event polling thread.
    let mut events = EventHandler::new(Duration::from_millis(33));

    // Agent event channel.
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentToUiEvent>();

    // Tick counter for spinner animation.
    let mut tick_count: u64 = 0;

    // Conversation history (user + assistant messages, no system prompt — that's added by ContextBuilder).
    let mut history: Vec<Message> = Vec::new();
    for msg in &session.messages {
        history.push(msg.clone());
    }

    loop {
        // Only re-render when dirty or agent is running (for spinner animation).
        if app.dirty || app.agent_running {
            terminal.draw(|frame| render::render(frame, &app, tick_count))?;
            app.dirty = false;
        }

        // Async event loop: wait for crossterm event OR agent event.
        tokio::select! {
            ev = events.recv() => {
                match ev {
                    Some(Event::Key(key)) => {
                        handle_key_event(
                            &mut app,
                            key,
                            &provider,
                            &agent_tx,
                            &mut history,
                            &mut session,
                            &tool_registry,
                            &tool_ctx,
                            &context_builder,
                            &system_prompt,
                            context_window,
                            &mut cost_tracker,
                            &mut trust_manager,
                            &model_name,
                            &mut rt_env,
                            &mut interrupt_ctrl,
                            &mut interjection_buf,
                        );
                    }
                    Some(Event::Resize(_, _)) => {
                        app.dirty = true;
                    }
                    Some(Event::Tick) | None => {
                        tick_count = tick_count.wrapping_add(1);
                        // Agent running needs spinner animation updates.
                        if app.agent_running {
                            app.dirty = true;
                        }
                    }
                }
            }
            agent_ev = agent_rx.recv() => {
                if let Some(ev) = agent_ev {
                    match ev {
                        AgentToUiEvent::TextChunk(text) => {
                            app.output.push_streaming_chunk(&text);
                            app.scroll_to_bottom();
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolStart { name, id } => {
                            app.output.push_line(OutputLine::Styled {
                                prefix: "Tool".to_string(),
                                content: format!("{name} [{id}]"),
                            });
                            app.scroll_to_bottom();
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolResult { name, output, is_error } => {
                            let status = if is_error { "ERROR" } else { "OK" };
                            let display_output = if output.len() > 200 {
                                format!("{}...(truncated)", &output[..200])
                            } else {
                                output
                            };
                            app.output.push_line(OutputLine::Plain(
                                format!("  [{name} {status}] {display_output}")
                            ));
                            app.scroll_to_bottom();
                            app.dirty = true;
                        }
                        AgentToUiEvent::TurnDone { new_messages, usage } => {
                            app.output.finalize_streaming();
                            // Persist all new messages from this turn.
                            for msg in &new_messages {
                                if let Err(e) = session.append_message(msg.clone()) {
                                    tracing::error!("Failed to persist message: {e}");
                                }
                            }
                            // Append to local conversation history.
                            history.extend(new_messages);
                            // Record cost.
                            cost_tracker.record(&model_name, &usage);
                            app.agent_running = false;
                            app.status = String::new();

                            // Reset interrupt controller for next turn.
                            interrupt_ctrl.reset();

                            // Process any interjection messages queued during the turn.
                            let interjections = interjection_buf.drain();
                            if !interjections.is_empty() {
                                for inj_text in &interjections {
                                    app.output.push_line(OutputLine::Styled {
                                        prefix: "You".to_string(),
                                        content: format!("(queued) {inj_text}"),
                                    });
                                }
                                // Queue the last interjection as the next user message.
                                if let Some(last) = interjections.into_iter().last() {
                                    app.output.push_line(OutputLine::Plain(String::new()));
                                    let user_msg = Message::user(&last);
                                    history.push(user_msg.clone());
                                    if let Err(e) = session.append_message(user_msg) {
                                        tracing::error!("Failed to persist interjection: {e}");
                                    }
                                }
                            }

                            app.scroll_to_bottom();
                            app.dirty = true;
                        }
                        AgentToUiEvent::Error(err) => {
                            app.output.finalize_streaming();
                            app.output.push_system(&format!("Error: {err}"));
                            app.agent_running = false;
                            app.status = String::new();
                            app.scroll_to_bottom();
                            app.dirty = true;
                        }
                        AgentToUiEvent::Status(status) => {
                            app.status = status;
                            app.dirty = true;
                        }
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_key_event(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    history: &mut Vec<Message>,
    session: &mut Session,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
    system_prompt: &str,
    context_window: u32,
    cost_tracker: &mut CostTracker,
    trust_manager: &mut TrustManager,
    model_name: &str,
    rt_env: &mut runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    interjection_buf: &mut InterjectionBuffer,
) {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            let action = interrupt_ctrl.on_ctrl_c(app.agent_running);
            match action {
                InterruptAction::Shutdown | InterruptAction::ForceQuit => {
                    app.should_quit = true;
                }
                InterruptAction::CancelAgent => {
                    app.output.push_system("Interrupting agent...");
                    app.status = "Interrupting...".to_string();
                }
            }
            app.dirty = true;
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Enter, _) => {
            if let Some(input) = app.submit_input() {
                match input {
                    UserInput::Exit => {
                        app.output.push_system("Goodbye.");
                        app.should_quit = true;
                    }
                    UserInput::SlashCommand { cmd, args } => {
                        handle_slash_command(
                            app,
                            &cmd,
                            &args,
                            cost_tracker,
                            trust_manager,
                            model_name,
                            rt_env,
                            history,
                            session,
                        );
                    }
                    UserInput::Text(text) => {
                        if app.agent_running {
                            // Buffer the message as an interjection.
                            interjection_buf.push(text.clone(), InterjectionPriority::Normal);
                            app.output.push_line(OutputLine::Plain(format!(
                                "(queued while agent running) {}",
                                text.trim()
                            )));
                        } else if let Some(provider) = provider {
                            // Show user message in output.
                            app.output.push_line(OutputLine::Styled {
                                prefix: "You".to_string(),
                                content: text.clone(),
                            });
                            app.output.push_line(OutputLine::Plain(String::new()));

                            // Persist user message immediately.
                            let user_msg = Message::user(&text);
                            history.push(user_msg.clone());
                            if let Err(e) = session.append_message(user_msg) {
                                tracing::error!("Failed to persist user message: {e}");
                            }

                            // Build context-aware message list with token budgets.
                            let turn_messages = context_builder.build(
                                system_prompt,
                                history,
                                context_window,
                            );

                            // Start agent turn.
                            app.agent_running = true;
                            app.status = "Thinking...".to_string();

                            let provider = Arc::clone(provider);
                            let tx = agent_tx.clone();
                            let registry = Arc::clone(tool_registry);
                            let ctx = Arc::clone(tool_ctx);
                            let cancel_token = interrupt_ctrl.token();
                            tokio::spawn(async move {
                                agent::run_agent_turn(
                                    provider,
                                    turn_messages,
                                    registry,
                                    ctx,
                                    tx,
                                    cancel_token,
                                )
                                .await;
                            });
                        } else {
                            // No provider — echo mode.
                            app.output.push_line(OutputLine::Plain(format!(
                                "[echo] {}",
                                text.trim()
                            )));
                        }
                    }
                }
                app.scroll_to_bottom();
            }
        }
        (KeyCode::Backspace, _) => { app.input.backspace(); app.dirty = true; }
        (KeyCode::Delete, _) => { app.input.delete(); app.dirty = true; }
        (KeyCode::Left, _) => { app.input.move_left(); app.dirty = true; }
        (KeyCode::Right, _) => { app.input.move_right(); app.dirty = true; }
        (KeyCode::Up, _) => { app.input.history_up(); app.dirty = true; }
        (KeyCode::Down, _) => { app.input.history_down(); app.dirty = true; }
        (KeyCode::Home, _) => { app.input.move_home(); app.dirty = true; }
        (KeyCode::End, _) => { app.input.move_end(); app.dirty = true; }
        (KeyCode::PageUp, _) => { app.scroll_up(10); app.dirty = true; }
        (KeyCode::PageDown, _) => { app.scroll_down(10); app.dirty = true; }
        (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.input.insert_char(ch);
            app.dirty = true;
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    args: &str,
    cost_tracker: &mut CostTracker,
    trust_manager: &mut TrustManager,
    model_name: &str,
    rt_env: &mut runtime::RuntimeEnvironment,
    history: &mut Vec<Message>,
    session: &mut Session,
) {
    let parsed = slash::parse_slash_command(cmd, args);

    match parsed {
        SlashCommand::Help { topic } => {
            let text = slash::help_text(topic.as_deref());
            for line in text.lines() {
                app.output.push_line(OutputLine::Plain(line.to_string()));
            }
        }
        SlashCommand::Exit => {
            app.output.push_system("Goodbye.");
            app.should_quit = true;
        }
        SlashCommand::New => {
            history.clear();
            let session_dir = session.dir().to_path_buf();
            let project_id = rt_env.project_id.clone();
            match Session::new(&session_dir, &project_id) {
                Ok(s) => {
                    *session = s;
                    app.output.push_system("New session started.");
                }
                Err(e) => {
                    app.output
                        .push_system(&format!("Failed to create session: {e}"));
                }
            }
        }
        SlashCommand::Clear => {
            app.output.clear();
        }
        SlashCommand::Cost => {
            let summary = cost_tracker.summary();
            for line in summary.lines() {
                app.output.push_line(OutputLine::Plain(line.to_string()));
            }
        }
        SlashCommand::Plan => {
            app.output
                .push_system("Task plan: (not yet active — agent will create plans automatically)");
        }
        SlashCommand::Trust { tools, all } => {
            if all {
                trust_manager.trust_all();
                app.output
                    .push_system("Trusted all non-dangerous tools for this session.");
            } else if tools.is_empty() {
                // Show currently trusted tools.
                let list = trust_manager.trusted_list();
                if list.is_empty() {
                    app.output.push_system("No tools currently trusted. Use /trust <tool_name> or /trust --all.");
                } else {
                    app.output.push_system(&format!(
                        "Trusted tools: {}",
                        list.join(", ")
                    ));
                }
            } else {
                for tool in &tools {
                    trust_manager.trust(tool);
                }
                app.output.push_system(&format!(
                    "Trusted for this session: {}",
                    tools.join(", ")
                ));
            }
        }
        SlashCommand::Untrust => {
            trust_manager.untrust_all();
            app.output
                .push_system("All tool trust revoked. Confirmations restored.");
        }
        SlashCommand::Model { name } => {
            if let Some(_name) = name {
                app.output.push_system("Model switching will be available in a future update.");
            } else {
                app.output.push_line(OutputLine::Plain(format!(
                    "Current model: {}",
                    if model_name.is_empty() {
                        "(none)"
                    } else {
                        model_name
                    }
                )));
            }
        }
        SlashCommand::Cd { path } => {
            if let Some(target) = path {
                match runtime::change_directory(rt_env, &target) {
                    runtime::DirectoryChangeResult::Success { new_dir, project_changed } => {
                        app.output.push_line(OutputLine::Plain(format!(
                            "Changed to: {}",
                            new_dir.display()
                        )));
                        if project_changed {
                            let project_name = rt_env.project_root
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "(none)".into());
                            app.output.push_system(&format!(
                                "Project boundary changed → {project_name}"
                            ));
                        }
                    }
                    runtime::DirectoryChangeResult::NotFound(msg) => {
                        app.output.push_system(&msg);
                    }
                    runtime::DirectoryChangeResult::Error(msg) => {
                        app.output.push_system(&format!("Error: {msg}"));
                    }
                }
            } else {
                app.output.push_line(OutputLine::Plain(format!(
                    "Working directory: {}",
                    rt_env.working_dir.display()
                )));
            }
        }
        SlashCommand::Init => {
            match OxConfig::init_default_config() {
                Ok(path) => {
                    app.output.push_system(&format!(
                        "Config created at {}. Edit it to add your API keys.",
                        path.display()
                    ));
                }
                Err(e) => {
                    app.output.push_system(&format!("Init failed: {e}"));
                }
            }
        }
        SlashCommand::Debug => {
            app.output
                .push_line(OutputLine::Plain(format!("Model: {model_name}")));
            app.output
                .push_line(OutputLine::Plain(format!("OS: {} ({})", rt_env.os, rt_env.arch)));
            app.output
                .push_line(OutputLine::Plain(format!("Shell: {}", rt_env.shell.name)));
            app.output.push_line(OutputLine::Plain(format!(
                "Working dir: {}",
                rt_env.working_dir.display()
            )));
            app.output.push_line(OutputLine::Plain(format!(
                "Project root: {}",
                rt_env
                    .project_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none)".into())
            )));
            app.output.push_line(OutputLine::Plain(format!(
                "Project ID: {}",
                rt_env.project_id
            )));
            app.output.push_line(OutputLine::Plain(format!(
                "History: {} messages",
                history.len()
            )));
            let trusted = trust_manager.trusted_list();
            app.output.push_line(OutputLine::Plain(format!(
                "Trusted tools: {}",
                if trusted.is_empty() {
                    "(none)".to_string()
                } else {
                    trusted.join(", ")
                }
            )));
        }
        SlashCommand::Unknown { cmd } => {
            app.output
                .push_system(&format!("Unknown command: /{cmd}. Type /help for available commands."));
        }
    }
}
