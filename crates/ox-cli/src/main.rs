mod terminal;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::event::{EnableMouseCapture, DisableMouseCapture};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use ox_core::agent::{self, AgentToUiEvent};
use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::interrupt::{InterruptAction, InterruptController};
use ox_core::agent::ui_event::{UiToAgentEvent, ConfirmationDecision};
use ox_core::config::OxConfig;
use ox_core::config::AgentConfig;
use ox_core::context::{self, ContextBuilder};
use ox_core::cost::CostTracker;
use ox_core::llm::{self, LlmProvider, ProviderResolveInfo};
use ox_core::memory::MemoryManager;
use ox_core::message::{Message, Session};
use ox_core::runtime;
use ox_core::safety::TrustManager;
use ox_core::slash::{self, SlashCommand};
use ox_core::tools::{ToolContext, ToolRegistry};
use terminal::app::{App, UserInput, PendingConfirmation};
use terminal::event::{Event, EventHandler};
use terminal::output_pane::OutputLine;
use terminal::render;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Detect runtime early to get home_dir for log file path.
    let early_home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let log_dir = early_home.join(".ox").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file_path = log_dir.join("ox.log");

    // Initialize logging: stderr (for TUI) + file (~/.ox/logs/ox.log).
    if let Ok(log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::Layer;
        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ox=warn"));
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_filter(env_filter.clone());
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .with_filter(env_filter);
        tracing_subscriber::registry()
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        // Fallback: stderr only.
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ox=warn")),
            )
            .with_writer(io::stderr)
            .init();
    }

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
    let (provider, resolve_info): (Option<Arc<dyn LlmProvider>>, Option<ProviderResolveInfo>) =
        match llm::create_provider_with_info(&config.models.default, &config.models) {
            Ok((p, info)) => {
                tracing::info!("LLM provider: {}", p.model_name());
                (Some(Arc::from(p)), Some(info))
            }
            Err(e) => {
                tracing::warn!("No LLM provider: {e}. Running in echo mode.");
                (None, None)
            }
        };

    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(EnableMouseCapture)?;  // Enable mouse scroll events.
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run the app; always restore terminal on exit.
    let result = run_app(&mut terminal, &config, rt_env, provider, resolve_info).await;

    // Restore terminal.
    disable_raw_mode()?;
    io::stdout().execute(DisableMouseCapture)?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &OxConfig,
    mut rt_env: runtime::RuntimeEnvironment,
    mut provider: Option<Arc<dyn LlmProvider>>,
    mut resolve_info: Option<ProviderResolveInfo>,
) -> anyhow::Result<()> {
    // Ensure system-level directory structure: ~/.ox/{sessions,db,logs,skills,memory}
    {
        let ox = &rt_env.ox_home_dir;
        let _ = std::fs::create_dir_all(ox.join("sessions"));
        let _ = std::fs::create_dir_all(ox.join("db"));
        let _ = std::fs::create_dir_all(ox.join("logs"));
        let _ = std::fs::create_dir_all(ox.join("skills"));
        let _ = std::fs::create_dir_all(ox.join("memory"));
    }
    // Ensure project-level directory structure: <project_root>/.ox/{skills,memory}
    if let Some(ref proj_ox) = rt_env.project_ox_dir {
        let _ = std::fs::create_dir_all(proj_ox.join("skills"));
        let _ = std::fs::create_dir_all(proj_ox.join("memory"));
    }

    let mut app = App::new();

    // Set status bar info.
    app.model_name = provider
        .as_ref()
        .map(|p| p.model_name().to_string())
        .unwrap_or_else(|| "echo".to_string());
    app.working_dir = rt_env
        .working_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rt_env.working_dir.display().to_string());
    app.message_count = 0;

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
    // System-level: ~/.ox/sessions/<project_id>/
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);
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
    let mut persona_vector = ox_core::persona::PersonaVector::for_language(
        &rt_env.project_root.as_ref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    );
    let system_prompt = context::build_system_prompt(&rt_env, &tool_registry, None, Some(&persona_vector), Some(&config.behavior_rules));

    // Context builder for assembling LLM messages within token budgets.
    let context_builder = ContextBuilder::default();
    let context_window = provider
        .as_ref()
        .map(|p| p.context_window_size())
        .unwrap_or(128_000);

    // Cost tracking — system-level: ~/.ox/db/
    let db_dir = rt_env.ox_home_dir.join("db");
    let mut cost_tracker = CostTracker::load_or_create(&db_dir).unwrap_or_else(|e| {
        tracing::warn!("Failed to load cost tracker: {e}");
        CostTracker::load_or_create(&std::env::temp_dir()).expect("temp dir fallback")
    });

    // Memory system — system-level: ~/.ox/db/memories_*.db
    let mut memory = MemoryManager::init(&rt_env.ox_home_dir, &rt_env.project_id, &config.memory)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to init memory system: {e}");
            // Fallback to temp dir.
            let temp = std::env::temp_dir();
            MemoryManager::init(&temp, &rt_env.project_id, &config.memory)
                .expect("memory init with temp dir")
        });

    // Model name for cost recording.
    let mut model_name = provider
        .as_ref()
        .map(|p| p.model_name().to_string())
        .unwrap_or_default();

    // Session-scoped trust manager for tool confirmation (shared between UI and agent).
    let trust_manager = Arc::new(std::sync::Mutex::new(TrustManager::new()));

    // Interrupt controller for Ctrl+C handling.
    let mut interrupt_ctrl = InterruptController::new();

    // Interjection buffer for user input during agent runs.
    let mut interjection_buf = InterjectionBuffer::new();

    // Crossterm event polling thread.
    let mut events = EventHandler::new(Duration::from_millis(33));

    // Agent event channels (bidirectional).
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentToUiEvent>();

    // Agent config (shared with spawned task).
    let agent_config = Arc::new(config.agent.clone());

    // Tick counter for spinner animation.
    let mut tick_count: u64 = 0;

    loop {
        // Only re-render when dirty or agent is running (for spinner animation).
        if app.dirty || app.agent_running {
            terminal.draw(|frame| render::render(frame, &mut app, tick_count))?;
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
                            &mut session,
                            &mut memory,
                            &mut persona_vector,
                            &tool_registry,
                            &tool_ctx,
                            &context_builder,
                            &system_prompt,
                            context_window,
                            &mut cost_tracker,
                            &trust_manager,
                            &model_name,
                            &mut rt_env,
                            &mut interrupt_ctrl,
                            &mut interjection_buf,
                            &resolve_info,
                            &config,
                            &agent_config,
                        );
                        if let Some(new_model_name) = app.pending_model_switch.take() {
                            match llm::create_provider_with_info(&new_model_name, &config.models) {
                                Ok((new_provider, new_info)) => {
                                    provider = Some(Arc::from(new_provider));
                                    resolve_info = Some(new_info);
                                    model_name = provider
                                        .as_ref()
                                        .map(|p| p.model_name().to_string())
                                        .unwrap_or_default();
                                    app.model_name = model_name.clone();
                                }
                                Err(e) => {
                                    app.output.push_system(&format!(
                                        "Failed to switch to '{}': {e}",
                                        new_model_name
                                    ));
                                }
                            }
                        }
                    }
                    Some(Event::Resize(_, _)) => {
                        app.dirty = true;
                    }
                    Some(Event::ScrollUp) => {
                        app.scroll_up(3);
                        app.user_scrolled = true;
                        app.dirty = true;
                    }
                    Some(Event::ScrollDown) => {
                        app.scroll_down(3);
                        // If scrolled back to bottom, resume auto-scroll.
                        if app.scroll_offset == 0 {
                            app.user_scrolled = false;
                        }
                        app.dirty = true;
                    }
                    Some(Event::Tick) | None => {
                        tick_count = tick_count.wrapping_add(1);
                        app.spinner_frame = tick_count;
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
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolStart { name, id } => {
                            app.output.push_line(OutputLine::Styled {
                                prefix: "Tool".to_string(),
                                content: format!("{name} [{id}]"),
                            });
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolResult { name, output, is_error } => {
                            let status = if is_error { "ERROR" } else { "OK" };
                            let display_output = if output.len() > 200 {
                                let end = output.char_indices().take_while(|(i, _)| *i < 200).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
                                format!("{}...(truncated)", &output[..end])
                            } else {
                                output
                            };
                            app.output.push_line(OutputLine::Plain(
                                format!("  [{name} {status}] {display_output}")
                            ));
                            if !app.user_scrolled { app.scroll_to_bottom(); }
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
                            // Record cost.
                            cost_tracker.record(&model_name, &usage);
                            // Extract memories from this turn.
                            memory.update_from_turn(&new_messages, &rt_env.project_id, "");
                            app.agent_running = false;
                            app.status = String::new();
                            // Clear any stale pending confirmation.
                            app.pending_confirmation = None;
                            // Update status bar info.
                            app.message_count = session.messages.len();
                            app.cost_summary = cost_tracker.summary_short();

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
                                    if let Err(e) = session.append_message(user_msg) {
                                        tracing::error!("Failed to persist interjection: {e}");
                                    }
                                }
                            }

                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::Error(err) => {
                            app.output.finalize_streaming();
                            app.output.push_system(&format!("Error: {err}"));
                            app.agent_running = false;
                            app.status = String::new();
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::Status(status) => {
                            app.status = status;
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolConfirmationRequest {
                            tool_call_id,
                            tool_name,
                            args_summary,
                            safety_level,
                            high_risk_warning,
                        } => {
                            // Display confirmation request in output.
                            let warning_str = high_risk_warning
                                .as_ref()
                                .map(|w| format!(" [{}]", w))
                                .unwrap_or_default();
                            app.output.push_line(OutputLine::Styled {
                                prefix: "Confirm".to_string(),
                                content: format!(
                                    "{} {:?}{}: {}",
                                    tool_name, safety_level, warning_str, args_summary
                                ),
                            });
                            app.output.push_line(OutputLine::Plain(
                                "  [Y] Allow / [N] Deny / [T] Trust always".to_string(),
                            ));
                            // Store pending confirmation for key handling.
                            app.pending_confirmation = Some(PendingConfirmation {
                                tool_call_id,
                                tool_name,
                            });
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolOutputChunk { tool_call_id: _, chunk } => {
                            app.output.push_streaming_chunk(&chunk);
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::BudgetExceeded { total_tokens, estimated_cost } => {
                            app.output.push_line(OutputLine::Styled {
                                prefix: "Budget".to_string(),
                                content: format!(
                                    "Token limit reached: {} tokens, est. cost: {}. Continue? [Y/N]",
                                    total_tokens, estimated_cost
                                ),
                            });
                            app.pending_confirmation = Some(PendingConfirmation {
                                tool_call_id: "__budget__".into(),
                                tool_name: "budget".into(),
                            });
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::CouncilDone { session: council_session } => {
                            let summary = council_session.format_summary();
                            for line in summary.lines() {
                                app.output.push_line(OutputLine::Plain(line.to_string()));
                            }
                            // Store council conclusion to memory
                            if let Some(ref arb) = council_session.arbitration {
                                let mem_node = ox_core::memory::MemoryNode::new(
                                    arb.final_recommendation.clone(),
                                    ox_core::memory::MemoryNodeType::Architectural,
                                    Some(rt_env.project_id.clone()),
                                    "multi".into(),
                                    ox_core::memory::MemorySource::CouncilConclusion,
                                );
                                memory.store(mem_node);
                            }
                            app.last_council_session = Some(council_session);
                            app.agent_running = false;
                            app.status = "Ox".to_string();
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                    }
                }
            }
        }

        if app.should_quit {
            memory.flush();
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
    session: &mut Session,
    memory: &mut MemoryManager,
    mut persona_vector: &mut ox_core::persona::PersonaVector,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
    system_prompt: &str,
    context_window: u32,
    cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    model_name: &str,
    rt_env: &mut runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    interjection_buf: &mut InterjectionBuffer,
    resolve_info: &Option<ProviderResolveInfo>,
    config: &OxConfig,
    agent_config: &Arc<AgentConfig>,
) {
    match (key.code, key.modifiers) {
        // ── Confirmation key handling (Y/N/T when pending) ──
        (KeyCode::Char('y'), KeyModifiers::NONE) | (KeyCode::Char('Y'), KeyModifiers::NONE) => {
            if app.pending_confirmation.is_some() {
                let pc = app.pending_confirmation.take().unwrap();
                if let Some(tx) = &app.ui_to_agent_tx {
                    let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                        tool_call_id: pc.tool_call_id,
                        decision: ConfirmationDecision::Allow,
                    });
                    app.output.push_line(OutputLine::Plain("  → Allowed".to_string()));
                } else {
                    app.output.push_line(OutputLine::Plain("  → Error: agent channel closed, cannot confirm".to_string()));
                }
                app.dirty = true;
                return;
            }
            app.input.insert_char('y');
            app.dirty = true;
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) | (KeyCode::Char('N'), KeyModifiers::NONE) => {
            if app.pending_confirmation.is_some() {
                let pc = app.pending_confirmation.take().unwrap();
                if let Some(tx) = &app.ui_to_agent_tx {
                    let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                        tool_call_id: pc.tool_call_id,
                        decision: ConfirmationDecision::Deny,
                    });
                    app.output.push_line(OutputLine::Plain("  → Denied".to_string()));
                } else {
                    app.output.push_line(OutputLine::Plain("  → Error: agent channel closed, cannot deny".to_string()));
                }
                app.dirty = true;
                return;
            }
            app.input.insert_char('n');
            app.dirty = true;
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) | (KeyCode::Char('T'), KeyModifiers::NONE) => {
            if app.pending_confirmation.is_some() {
                let pc = app.pending_confirmation.take().unwrap();
                if let Some(tx) = &app.ui_to_agent_tx {
                    let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                        tool_call_id: pc.tool_call_id,
                        decision: ConfirmationDecision::TrustAlways,
                    });
                    app.output.push_line(OutputLine::Plain(
                        format!("  → Trusted {} for this session", pc.tool_name),
                    ));
                } else {
                    app.output.push_line(OutputLine::Plain("  → Error: agent channel closed, cannot trust".to_string()));
                }
                app.dirty = true;
                return;
            }
            app.input.insert_char('t');
            app.dirty = true;
        }
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => { app.input.move_home(); app.dirty = true; }
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => { app.input.move_end(); app.dirty = true; }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => { app.input.clear_to_home(); app.dirty = true; }
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => { app.input.clear_to_end(); app.dirty = true; }
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => { app.input.delete_word(); app.dirty = true; }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            let action = interrupt_ctrl.on_ctrl_c(app.agent_running);
            match action {
                InterruptAction::Shutdown | InterruptAction::ForceQuit => {
                    app.should_quit = true;
                }
                InterruptAction::CancelAgent => {
                    app.agent_running = false;
                    app.output.push_system("Agent interrupted.");
                    app.status = "Ox".to_string();
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
                            session,
                            memory,
                            &mut persona_vector,
                            &resolve_info,
                            &config,
                        );
                        // Check if a council discuss was queued
                        if let Some((question, rounds, verbose)) = app.pending_discuss.take() {
                            let council_config = config.council.clone();
                            let models_config = config.models.clone();
                            let ctx_messages = session.messages.clone();
                            let agent_tx_council = agent_tx.clone();
                            tokio::spawn(async move {
                                use ox_core::council::orchestrator::CouncilOrchestrator;
                                let orch = CouncilOrchestrator::new(models_config, council_config);
                                match orch.convene(&question, &ctx_messages, rounds, verbose).await {
                                    Ok(council_session) => {
                                        let _ = agent_tx_council.send(AgentToUiEvent::CouncilDone {
                                            session: council_session,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = agent_tx_council.send(AgentToUiEvent::Error(
                                            format!("Council failed: {}", e)
                                        ));
                                    }
                                }
                            });
                        }
                    }
                    UserInput::Text(text) => {
                        if app.agent_running {
                            interjection_buf.push(text.clone(), InterjectionPriority::Normal);
                            app.output.push_line(OutputLine::Plain(format!(
                                "(queued while agent running) {}",
                                text.trim()
                            )));
                        } else if let Some(provider) = provider {
                            let user_msg = Message::user(&text);
                            if let Err(e) = session.append_message(user_msg) {
                                tracing::error!("Failed to persist user message: {e}");
                            }
                            let memory_nodes = memory.retrieve(&text, &Some(rt_env.project_id.as_str()), 5);
                            let memory_ctx = memory.format_memory_context(&memory_nodes);
                            let turn_messages = context_builder.build(
                                system_prompt,
                                &memory_ctx,
                                &session.messages,
                                context_window,
                            );
                            app.agent_running = true;
                            app.status = "Thinking...".to_string();
                            let provider = Arc::clone(provider);
                            let tx = agent_tx.clone();
                            let registry = Arc::clone(tool_registry);
                            let ctx = Arc::clone(tool_ctx);
                            let cancel_token = interrupt_ctrl.token();
                            let tm = Arc::clone(&trust_manager);
                            let ac = Arc::clone(&agent_config);
                            let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
                            app.ui_to_agent_tx = Some(ui_to_agent_tx);
                            tokio::spawn(async move {
                                agent::run_agent_turn(
                                    provider,
                                    turn_messages,
                                    registry,
                                    ctx,
                                    tx,
                                    ui_to_agent_rx,
                                    cancel_token,
                                    tm,
                                    ac,
                                )
                                .await;
                            });
                        } else {
                            app.output.push_line(OutputLine::Plain(format!(
                                "[echo] {}",
                                text.trim()
                            )));
                        }
                    }
                }
                app.scroll_to_bottom();
                app.user_scrolled = false;
            }
        }
        (KeyCode::Backspace, _) => { app.input.backspace(); app.dirty = true; }
        (KeyCode::Delete, _) => { app.input.delete(); app.dirty = true; }
        (KeyCode::Left, _) => { app.input.move_left(); app.dirty = true; }
        (KeyCode::Right, _) => { app.input.move_right(); app.dirty = true; }
        (KeyCode::Up, KeyModifiers::SHIFT) => {
            app.scroll_up(1); app.user_scrolled = true; app.dirty = true;
        }
        (KeyCode::Down, KeyModifiers::SHIFT) => {
            app.scroll_down(1);
            if app.scroll_offset == 0 { app.user_scrolled = false; }
            app.dirty = true;
        }
        (KeyCode::Up, _) => { app.input.history_up(); app.dirty = true; }
        (KeyCode::Down, _) => { app.input.history_down(); app.dirty = true; }
        (KeyCode::Home, _) => { app.input.move_home(); app.dirty = true; }
        (KeyCode::End, _) => { app.input.move_end(); app.dirty = true; }
        (KeyCode::PageUp, _) => {
            app.scroll_up(10); app.user_scrolled = true; app.dirty = true;
        }
        (KeyCode::PageDown, _) => {
            app.scroll_down(10);
            if app.scroll_offset == 0 { app.user_scrolled = false; }
            app.dirty = true;
        }
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
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    model_name: &str,
    rt_env: &mut runtime::RuntimeEnvironment,
    session: &mut Session,
    memory: &mut MemoryManager,
    persona_vector: &mut ox_core::persona::PersonaVector,
    resolve_info: &Option<ProviderResolveInfo>,
    config: &OxConfig,
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
            // Archive current session before starting new one.
            let session_dir = session.dir().to_path_buf();
            if let Err(e) = session.archive(&session_dir) {
                tracing::warn!("Failed to archive session: {e}");
            }
            let project_id = rt_env.project_id.clone();
            match Session::new(&session_dir, &project_id) {
                Ok(s) => {
                    *session = s;
                    app.output.push_system("New session started. (Previous session archived.)");
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
            let mut tm = trust_manager.lock().unwrap();
            if all {
                tm.trust_all();
                app.output
                    .push_system("Trusted all non-dangerous tools for this session.");
            } else if tools.is_empty() {
                // Show currently trusted tools.
                let list = tm.trusted_list();
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
                    tm.trust(tool);
                }
                app.output.push_system(&format!(
                    "Trusted for this session: {}",
                    tools.join(", ")
                ));
            }
        }
        SlashCommand::Untrust => {
            trust_manager.lock().unwrap().untrust_all();
            app.output
                .push_system("All tool trust revoked. Confirmations restored.");
        }
        SlashCommand::Model { name } => {
            if let Some(new_model) = name {
                app.pending_model_switch = Some(new_model.clone());
                app.output.push_line(OutputLine::Plain(format!(
                    "Switching to: {}", new_model
                )));
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
            // Provider resolution info
            if let Some(info) = resolve_info {
                app.output.push_line(OutputLine::Plain(format!(
                    "Provider: {}",
                    info.provider_name
                )));
                let key_src = match &info.api_key_source {
                    llm::ApiKeySource::EnvVar(name) => format!("env var {}", name),
                    llm::ApiKeySource::ConfigFile => "config file".to_string(),
                    llm::ApiKeySource::NotFound => "NOT FOUND".to_string(),
                };
                app.output.push_line(OutputLine::Plain(format!(
                    "API key source: {key_src}"
                )));
                let url_src = match &info.base_url_source {
                    llm::BaseUrlSource::ConfigFile => "config file",
                    llm::BaseUrlSource::Default => "provider default",
                };
                app.output.push_line(OutputLine::Plain(format!(
                    "Base URL source: {url_src}"
                )));
            } else {
                app.output.push_line(OutputLine::Plain(
                    "Provider: (none — echo mode)".to_string(),
                ));
            }
            // Config file path
            let config_path = OxConfig::default_config_path();
            app.output.push_line(OutputLine::Plain(format!(
                "Config file: {}",
                config_path.display()
            )));
            // All providers key status (never show values)
            app.output.push_line(OutputLine::Plain("Providers:".to_string()));
            for (name, pcfg) in &config.models.providers {
                let env_key = format!("OX_{}_API_KEY", name.to_uppercase());
                let has_env = std::env::var(&env_key)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .is_some();
                let has_config = !pcfg.api_key.is_empty();
                let status = if has_env {
                    "key set (env var)"
                } else if has_config {
                    "key set (config)"
                } else {
                    "no key"
                };
                app.output.push_line(OutputLine::Plain(format!(
                    "  {name}: {status}"
                )));
            }
            // Model→provider mapping
            if !config.models.model_providers.is_empty() {
                app.output.push_line(OutputLine::Plain(
                    "Model→Provider mappings:".to_string(),
                ));
                for (model, provider) in &config.models.model_providers {
                    app.output.push_line(OutputLine::Plain(format!(
                        "  {model} → {provider}"
                    )));
                }
            }
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
                session.messages.len()
            )));
            let trusted = {
                let tm = trust_manager.lock().unwrap();
                tm.trusted_list()
            };
            app.output.push_line(OutputLine::Plain(format!(
                "Trusted tools: {}",
                if trusted.is_empty() {
                    "(none)".to_string()
                } else {
                    trusted.join(", ")
                }
            )));
        }
        SlashCommand::Sessions => {
            let session_dir = session.dir().to_path_buf();
            let archived = Session::list_archived(&session_dir);
            if archived.is_empty() {
                app.output.push_system("No archived sessions found. Use /new to start and archive sessions.");
            } else {
                app.output.push_line(OutputLine::Plain("Archived sessions:".to_string()));
                for (i, (filename, info)) in archived.iter().enumerate() {
                    app.output.push_line(OutputLine::Plain(format!(
                        "  {}. {}  ({})",
                        i + 1,
                        info,
                        filename
                    )));
                }
                app.output.push_line(OutputLine::Plain(
                    "Use /resume <filename> to restore a session.".to_string(),
                ));
            }
        }
        SlashCommand::Resume { filename } => {
            if filename.is_empty() {
                app.output.push_system("Usage: /resume <filename>  (use /sessions to list)");
            } else {
                let session_dir = session.dir().to_path_buf();
                match Session::load_archived(&session_dir, &filename) {
                    Ok(Some(archived_session)) => {
                        // Archive current session first.
                        if let Err(e) = session.archive(&session_dir) {
                            tracing::warn!("Failed to archive current session: {e}");
                        }
                        // Restore archived session.
                        let msg_count = archived_session.messages.len();
                        *session = archived_session;
                        app.output.push_system(&format!(
                            "Session restored: {} messages from {}",
                            msg_count, filename
                        ));
                        app.message_count = session.messages.len();
                    }
                    Ok(None) => {
                        app.output.push_system(&format!(
                            "Session '{}' not found. Use /sessions to list.",
                            filename
                        ));
                    }
                    Err(e) => {
                        app.output.push_system(&format!("Failed to resume session: {e}"));
                    }
                }
            }
        }
        SlashCommand::Remember { content } => {
            if content.is_empty() {
                app.output.push_system("Usage: /remember <content>  (stores as Style memory)");
            } else {
                memory.store_explicit(&content, &rt_env.project_id, "");
                app.output.push_system(&format!("Remembered: {}", content.chars().take(100).collect::<String>()));
            }
        }
        SlashCommand::Forget { keyword } => {
            if keyword.is_empty() {
                app.output.push_system("Usage: /forget <keyword>  (deletes matching memories)");
            } else {
                let deleted = memory.forget(&keyword, &rt_env.project_id);
                app.output.push_system(&format!("Forgot {} memory(ies) matching '{}'", deleted, keyword));
            }
        }
        SlashCommand::Memory => {
            let (project_count, overall_count) = memory.stats(&rt_env.project_id);
            app.output.push_line(OutputLine::Plain(format!("Memory: {} project, {} long-term", project_count, overall_count)));
            let nodes = memory.retrieve("", &Some(rt_env.project_id.as_str()), 5);
            for node in &nodes {
                app.output.push_line(OutputLine::Plain(format!(
                    "  [{}] {} (depth: {})",
                    node.node_type,
                    node.content.chars().take(80).collect::<String>(),
                    node.depth
                )));
            }
        }
        SlashCommand::Feedback { category } => {
            match category.as_str() {
                "good" => {
                    app.output.push_system("Feedback noted: positive. Memory reinforced.");
                }
                "bad" => {
                    app.output.push_system("Feedback noted: negative. Will adjust approach.");
                }
                "unsafe" => {
                    app.output.push_system("Safety violation noted. Reviewing constraints.");
                }
                _ => {
                    app.output.push_system("Usage: /feedback <good|bad|unsafe>");
                }
            }
        }
        SlashCommand::Persona { action } => {
            if action.is_empty() || action == "show" {
                app.output.push_line(OutputLine::Plain(format!("Persona: {}", persona_vector)));
            } else if action == "freeze" {
                app.output.push_system("Persona frozen (evolution stopped). Use /persona unfreeze to resume.");
            } else if action == "unfreeze" {
                app.output.push_system("Persona unfrozen (evolution resumed).");
            } else {
                app.output.push_system("Usage: /persona [show|freeze|unfreeze]");
            }
        }
        SlashCommand::Discuss { question, rounds, verbose } => {
            let question_text = match question {
                Some(q) => q.clone(),
                None => {
                    app.output.push_system("Usage: /discuss <question> [--rounds N] [--verbose]");
                    return;
                }
            };
            app.output.push_system(&format!("Starting council debate on: {}", question_text));
            app.agent_running = true;
            app.status = "Council debating...".to_string();
            app.dirty = true;
            // Council will be run via tokio::spawn after this match
            // Store the discuss request for the main loop to pick up
            app.pending_discuss = Some((question_text, rounds, verbose));
        }
        SlashCommand::Council { action } => {
            if action == "last" {
                if let Some(ref session) = app.last_council_session {
                    let output = if session.phases.len() > 2 {
                        session.format_verbose()
                    } else {
                        session.format_summary()
                    };
                    for line in output.lines() {
                        app.output.push_line(OutputLine::Plain(line.to_string()));
                    }
                } else {
                    app.output.push_system("No previous council session.");
                }
            } else if action == "stats" {
                app.output.push_system("Council stats: (model capability tracking not yet persisted)");
            } else {
                app.output.push_system("Usage: /council <last|stats>");
            }
        }
        SlashCommand::Unknown { cmd } => {
            app.output
                .push_system(&format!("Unknown command: /{cmd}. Type /help for available commands."));
        }
    }
}
