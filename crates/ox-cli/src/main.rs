mod terminal;
mod spec_helpers;
pub mod slash_commands;
pub mod middleware;
pub mod helpers;
pub mod keyword_extraction;  // 🆕 Keyword extraction from LLM responses

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
#[cfg(not(target_os = "windows"))]
use crossterm::event::EnableMouseCapture;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use ox_core::agent::{self, AgentToUiEvent};
use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::interrupt::InterruptController;
use ox_core::agent::ui_event::UiToAgentEvent;
use ox_core::config::{AgentConfig, OxConfig};
use ox_core::context::{self, ContextBuilder};
use ox_core::cost::{self, CostTracker};
use ox_core::embedding::{CompressionManager, KadaneConfig};
use ox_core::llm::{self, LlmProvider, ProviderResolveInfo};
use ox_core::memory::MemoryManager;
use ox_core::message::{Message, Session};
use ox_core::runtime;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};
use terminal::app::{App, PendingConfirmation, SessionAction, UserInput, WorkflowState};
use terminal::event::{Event, EventHandler};
use terminal::output_pane::OutputLine;
use terminal::render;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    init_logging()?;

    // Install panic hook to restore terminal on panic
    install_panic_hook();

    // Load config (defaults if file missing)
    let config = OxConfig::load(None)?;

    // Detect runtime environment
    let rt_env = runtime::detect_runtime();

    // Try to create LLM provider (may fail if no API key)
    let (provider, resolve_info) = create_provider(&config)?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    #[cfg(not(target_os = "windows"))]
    stdout.execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run the application
    let result = run_app(&mut terminal, &config, rt_env, provider, resolve_info).await;

    // Restore terminal
    disable_raw_mode()?;
    #[cfg(not(target_os = "windows"))]
    io::stdout().execute(crossterm::event::DisableMouseCapture)?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Initialize logging to file (~/.ox/logs/ox.log)
fn init_logging() -> anyhow::Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let log_dir = home.join(".ox").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file_path = log_dir.join("ox.log");

    if let Ok(log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
    {
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ox=info"));
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .with_filter(filter);
        tracing_subscriber::registry().with(file_layer).init();
    } else {
        use tracing_subscriber::filter::LevelFilter;
        tracing_subscriber::fmt()
            .with_max_level(LevelFilter::OFF)
            .init();
    }
    Ok(())
}

/// Install panic hook to restore terminal on panic
fn install_panic_hook() {
    use std::io;
    use crossterm::ExecutableCommand;
    use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};

    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_panic(info);
    }));
}

/// Create LLM provider from config
fn create_provider(
    config: &OxConfig,
) -> anyhow::Result<(Option<Arc<dyn LlmProvider>>, Option<ProviderResolveInfo>)> {
    match llm::create_provider_with_info(&config.models.default, &config.models) {
        Ok((p, info)) => Ok((Some(Arc::from(p)), Some(info))),
        Err(e) => {
            tracing::warn!("No LLM provider: {e}. Running in echo mode.");
            Ok((None, None))
        }
    }
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

    // Set header info (fixed, non-scrolling).
    app.header_info.push(rt_env.banner_summary());
    if provider.is_some() {
        app.header_info
            .push("Type a message or /help for commands. /exit to quit.".to_string());
    } else {
        app.header_info
            .push("No API key. Set env var or config. Running in echo mode.".to_string());
    }

    // Startup check: warn if no config file exists.
    if !OxConfig::config_exists() {
        app.output.push_system(
            "No config file found. Run /init to create ~/.ox/config.toml with default settings.",
        );
    }

    // Session persistence: load or create.
    // System-level: ~/.ox/sessions/<project_id>/
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);
    let mut session = if config.session.auto_restore {
        match Session::load(&session_dir)? {
            Some(s) => {
                app.output.push_line(OutputLine::System(format!(
                    "Session restored ({} messages)",
                    s.user_message_count()
                )));
                // Replay session history into output pane.
                helpers::replay_session_history(&mut app, &s.messages, &rt_env, provider.is_some());
                s
            }
            None => Session::new(&session_dir, &rt_env.project_id)?,
        }
    } else {
        Session::new(&session_dir, &rt_env.project_id)?
    };
    // Truncate old ToolResult content loaded from disk (JSONL retains full content).
    // All loaded messages are from previous turns — use aggressive threshold.
    for msg in session.messages.iter_mut() {
        if let Message::ToolResult { content, .. } = msg {
            let char_len = content.chars().count();
            if char_len > 500 {
                let preview: String = content.chars().take(200).collect();
                *content = format!("{}...[truncated, {} chars total]", preview, char_len);
            }
        }
    }
    // Initialize compression debounce baseline so loaded sessions
    // don't immediately trigger compression on the first new message.
    app.last_compression_msg_count = session.messages.len();

    // 🚨 Display incomplete spec tasks on startup
    let project_root_for_display = rt_env.project_root.as_ref().map(|p| p.as_path());
    spec_helpers::display_incomplete_tasks(&mut app, project_root_for_display);

    // Populate sidebar with archived sessions.
    {
        let archived = Session::list_archived(&session_dir);
        for (filename, info) in archived {
            app.sessions.push(terminal::app::SessionEntry {
                filename,
                info,
                is_active: false,
            });
        }
    }
    app.sessions.insert(
        0,
        terminal::app::SessionEntry {
            filename: "current".to_string(),
            info: helpers::session_display_name(&session),
            is_active: true,
        },
    );

    // Create tool registry (tool context will be created after memory initialization)
    let tool_registry = Arc::new(ToolRegistry::new());

    // Initialize command registry
    let command_registry = slash_commands::CommandRegistry::new();

    // Load spec if auto_load enabled
    if config.spec.auto_load {
        if let Some(ref project_root) = rt_env.project_root {
            match context::load_spec(project_root, &config.spec.file_path) {
                Ok(content) if !content.is_empty() => {
                    app.activate_spec_mode(content);
                    tracing::info!("Spec mode activated from: {}", config.spec.file_path);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to load spec: {}", e);
                }
            }
        }
    }

    // Initial system prompt (not used for agent turns, built dynamically)
    let system_prompt = context::build_system_prompt(
        &rt_env,
        &tool_registry,
        None,
        Some(&config.behavior_rules),
        match &app.workflow_state {
            WorkflowState::Spec { spec_content, .. } if !spec_content.is_empty() => {
                Some(spec_content.as_str())
            }
            _ => None,
        },
    );

    // Context builder for assembling LLM messages within token budgets.
    // Uses ratios from config if available.
    let context_builder = ContextBuilder::from_config(&config.context);
    let context_window = provider
        .as_ref()
        .map(|p| p.context_window_size())
        .unwrap_or(128_000);

    // Cost tracking -- system-level: ~/.ox/db/
    let db_dir = rt_env.ox_home_dir.join("db");
    let mut cost_tracker = CostTracker::load_or_create(&db_dir).unwrap_or_else(|e| {
        tracing::warn!("Failed to load cost tracker: {e}");
        CostTracker::load_or_create(&std::env::temp_dir()).expect("temp dir fallback")
    });

    // Memory system -- system-level: ~/.ox/db/memories_*.db
    let memory = MemoryManager::init(&rt_env.ox_home_dir, &rt_env.project_id, &config.memory)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to init memory system: {e}");
            let temp = std::env::temp_dir();
            MemoryManager::init(&temp, &rt_env.project_id, &config.memory)
                .expect("memory init with temp dir")
        });

    // Wrap in Arc for shared access
    let memory_arc = Arc::new(memory);

    // Load EMA historical state from database for implicit feedback tracking
    if let Err(e) = app
        .ema_manager
        .load_from_store("code_accept_rate", memory_arc.overall_store())
    {
        tracing::warn!("Failed to load EMA history: {}", e);
    }

    // Baseline satisfaction for rollback evaluation
    let _baseline_satisfaction = 0.75; // Default baseline, can be made configurable

    // Probabilistic janitor run on startup (20% chance).
    if rand::random::<f64>() < config.memory.janitor_run_on_startup_prob {
        memory_arc.run_janitor(0.3, config.memory.max_nodes);
    }

    // Initialize file index registry (supports multiple directories)
    let file_index_db_dir = db_dir.join("file_indices");
    let mut file_index_registry = ox_core::file_index::FileIndexRegistry::new(file_index_db_dir);

    // Get or create index for current working directory
    let mut file_index_manager = file_index_registry
        .get_or_create(&rt_env.working_dir)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to initialize file index: {}. Using empty index.", e);
            // Fallback: create in-memory database
            Arc::new(
                ox_core::file_index::FileIndexManager::new(
                    &std::env::temp_dir().join("file_index.db"),
                )
                .expect("file index with temp dir"),
            )
        });

    // Start file system watcher for real-time updates
    if let Err(e) = file_index_manager.start_file_watcher(rt_env.working_dir.clone()) {
        tracing::warn!(
            "Failed to start file watcher: {}. Will rely on periodic refresh.",
            e
        );
    } else {
        tracing::info!("File watcher started for real-time index updates");
    }

    let mut tool_ctx = Arc::new(ToolContext::new(
        rt_env.clone(),
        rt_env.working_dir.clone(),
        Arc::new(config.clone()),
        Arc::clone(&memory_arc),
        Arc::clone(&file_index_manager),
    ));

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

    // Compressed context store -- SQLite: ~/.ox/db/compressed_context.db
    let compressed_ctx_store = Arc::new(
        ox_core::context::compressed_store::CompressedContextStore::open(
            &db_dir.join("compressed_context.db"),
        )
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to open compressed context store: {e}");
            ox_core::context::compressed_store::CompressedContextStore::open(
                &std::env::temp_dir().join("compressed_context.db"),
            )
            .expect("compressed context store with temp dir")
        }),
    );

    // Compression manager for context compression (KadaneDial).
    // Uses history_ratio from ContextBuilder for consistent configuration.
    let compression_manager: Option<CompressionManager> =
        if let Some(ref emb_config) = config.models.embedding {
            if emb_config.enabled {
                let model_path = emb_config
                    .model_path
                    .as_ref()
                    .map(|p| {
                        let p = p.replace(
                            '~',
                            &dirs::home_dir()
                                .unwrap_or_else(|| std::path::PathBuf::from("."))
                                .to_string_lossy(),
                        );
                        std::path::PathBuf::from(p)
                    })
                    .unwrap_or_else(|| {
                        dirs::home_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                            .join(".ox/models/bge-small-zh-v1.5")
                    });

                let kadane_config = KadaneConfig {
                    threshold: emb_config.threshold,
                    stop_threshold: emb_config.stop_threshold,
                    max_segments: emb_config.max_segments,
                    min_segment_len: emb_config.min_segment_len,
                    keep_recent: emb_config.keep_recent,
                    chunk_threshold_tokens: emb_config.chunk_threshold_tokens,
                    max_chunk_tokens: emb_config.max_chunk_tokens,
                };

                match ox_core::embedding::BgeEmbedder::load(&model_path) {
                    Ok(emb) => {
                        tracing::info!("Embedding model loaded: {:?}", model_path);
                        // Use history_ratio from ContextBuilder for consistent configuration
                        Some(CompressionManager::new(
                            emb,
                            kadane_config,
                            context_builder.history_ratio(),
                        ))
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load embedding model: {}. Compression disabled.",
                            e
                        );
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

    // Tick counter for spinner animation.
    let mut tick_count: u64 = 0;

    // Cached compressed context: (compressed_messages, source_msg_count).
    // source_msg_count = number of JSONL messages absorbed into the compressed context.
    let mut compressed_cache: Option<(Vec<Message>, usize)> =
        compressed_ctx_store.load(&session.meta.id).unwrap_or(None);

    // Session action signaled from slash commands.
    let mut session_action: SessionAction = SessionAction::None;

    // Holds the old session when switching during agent run.
    let mut background_session: Option<Session> = None;

    // Initialize Workflow Engine in App
    app.init_workflow_engine(&session.meta.id, &session.meta);

    loop {
        // === IMPLICIT FEEDBACK: Detect overrides before user input ===
        let override_signals = app.override_detector.detect_overrides();
        
        // Use middleware to process implicit feedback
        middleware::feedback::process_implicit_feedback(&mut app, &override_signals);
        
        // Update EMA metrics periodically
        middleware::feedback::update_feedback_metrics(&mut app, &memory_arc);
        // === END IMPLICIT FEEDBACK DETECTION ===

        // Only re-render when needed (dirty or spinner animation changed).
        if app.needs_render() {
            terminal.draw(|frame| render::render(frame, &mut app, tick_count))?;
            app.dirty = false;
            app.mark_spinner_rendered();
        }

        // Handle deferred compression using middleware
        if app.pending_compression.is_some() {
            middleware::compression::handle_pending_compression(
                &mut app,
                &mut session,
                &provider,
                &compression_manager,
                &compressed_cache,
                &system_prompt,
                &context_builder,
                context_window,
                &agent_tx,
                &tool_registry,
                &tool_ctx,
                &mut interrupt_ctrl,
                &trust_manager,
                &agent_config,
            ).await;
        }

        // Async event loop: wait for crossterm event OR agent event.
        // Use biased to prioritize user input over agent events.
        tokio::select! {
            biased;
            ev = events.recv() => {
                match ev {
                    Some(Event::Key(key)) => {
                        handle_key_event(
                            &mut app,
                            key,
                            &provider,
                            &agent_tx,
                            &mut session,
                            &memory_arc,
                            &tool_registry,
                            &tool_ctx,
                            &context_builder,
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
                            &compression_manager,
                            &compressed_cache,
                            &command_registry,
                        );
                        // Process session switch action from app.
                        match std::mem::replace(&mut app.session_action, SessionAction::None) {
                            SessionAction::New => {
                                // Archive current session (it stays in the old context).
                                let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);
                                if app.agent_running {
                                    // Move current session to background, create new one
                                    let project_id = rt_env.project_id.clone();
                                    let new_s = Session::new(&session_dir, &project_id)
                                        .expect("failed to create new session");
                                    background_session = Some(std::mem::replace(&mut session, new_s));
                                    // Clear UI→Agent channel for background session
                                    app.ui_to_agent_tx = None;

                                    // Reinitialize workflow engine for new session
                                    app.init_workflow_engine(&session.meta.id, &session.meta);
                                    
                                    // Load compressed context for new session
                                    compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                } else {
                                    let project_id = rt_env.project_id.clone();
                                    match Session::new(&session_dir, &project_id) {
                                        Ok(s) => {
                                            session = s;
                                            app.output.clear();
                                            app.output.push_system("New session started.");
                                            helpers::refresh_header_info(&mut app, &rt_env, provider.is_some());
                                            app.message_count = 0;

                                            // Reinitialize workflow engine for new session
                                            app.init_workflow_engine(&session.meta.id, &session.meta);
                                            
                                            // Load compressed context for new session
                                            compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                        }
                                        Err(e) => {
                                            app.output.push_system(&format!("Failed to create session: {e}"));
                                        }
                                    }
                                }
                            }
                            SessionAction::Resume { filename } => {
                                let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);
                                if app.agent_running {
                                    match Session::load_archived(&session_dir, &filename) {
                                        Ok(Some(archived)) => {
                                            background_session = Some(std::mem::replace(&mut session, archived));
                                            // Clear UI→Agent channel when switching to background
                                            app.ui_to_agent_tx = None;
                                            
                                            // Load compressed context for resumed session
                                            compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                        }
                                        _ => {}
                                    }
                                } else {
                                    match Session::load_archived(&session_dir, &filename) {
                                        Ok(Some(archived)) => {
                                            if let Err(e) = session.archive(&session_dir) {
                                                tracing::warn!("Failed to archive: {e}");
                                            }
                                            session = archived;
                                            helpers::replay_session_history(&mut app, &session.messages, &rt_env, provider.is_some());
                                            app.output.push_system(&format!(
                                                "Session restored: {} messages from {}",
                                                session.messages.len(), filename
                                            ));
                                            
                                            // Load compressed context for resumed session
                                            compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                        }
                                        Ok(None) => {
                                            app.output.push_system(&format!("Session '{}' not found.", filename));
                                        }
                                        Err(e) => {
                                            app.output.push_system(&format!("Failed to resume: {e}"));
                                        }
                                    }
                                }
                            }
                            SessionAction::None => {}
                        }

                        // Handle compressed cache clear when session is cleaned
                        if app.pending_compressed_cache_clear {
                            app.pending_compressed_cache_clear = false;
                            if let Err(e) = compressed_ctx_store.delete(&session.meta.id) {
                                tracing::warn!("Failed to delete compressed context: {}", e);
                            } else {
                                tracing::info!("Compressed context cleared for session {}", session.meta.id);
                            }
                            compressed_cache = None;
                        }

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
                    Some(Event::Tick) | None => {
                        tick_count = tick_count.wrapping_add(1);
                        app.spinner_frame = tick_count;

                        // Update workflow display cache (avoids locking in render)
                        app.update_workflow_display();

                        // Agent running needs spinner animation updates.
                        // Only mark dirty if spinner frame actually changed and agent is running
                        if app.agent_running && app.spinner_frame != app.last_spinner_frame {
                            app.dirty = true;
                        }
                    }
                }
            }
            agent_ev = agent_rx.recv() => {
                if let Some(ev) = agent_ev {
                    // When switching sessions during agent run, write to background session.
                    let target_session = background_session.as_mut().unwrap_or(&mut session);
                    match ev {
                        AgentToUiEvent::TextChunk(text) => {
                            app.output.push_streaming_chunk(&text);
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolStart { name, id: _, detail } => {
                            if detail.is_some() {
                                // Update the last matching Tool line with the detail.
                                let mut updated = false;
                                for line in app.output.lines.iter_mut().rev() {
                                    if let OutputLine::Tool { name: n, detail: d } = line {
                                        if *n == name {
                                            *d = detail.clone();
                                            updated = true;
                                            break;
                                        }
                                    }
                                }
                                if !updated {
                                    app.output.push_line(OutputLine::Tool { name: name.clone(), detail });
                                }
                                app.output.invalidate_cache();
                            } else {
                                app.output.push_line(OutputLine::Tool { name: name.clone(), detail: None });
                            }
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolResult { name, output, is_error } => {
                            let summary = helpers::summarize_tool_result(&name, &output);
                            app.output.push_line(OutputLine::ToolResult {
                                name: name.clone(),
                                summary,
                                is_error,
                            });

                            // Register file writes for implicit feedback tracking
                            if name == "file_write" && !is_error {
                                if let Some(path_str) = helpers::extract_file_path_from_output(&output) {
                                    if let Ok(path) = std::path::PathBuf::from(path_str).canonicalize() {
                                        if let Some(content) = helpers::extract_last_file_write_content(&target_session.messages) {
                                            app.override_detector.register_write(path.clone(), &content);
                                            app.total_file_writes += 1;

                                            tracing::debug!(
                                                "[IMPLICIT FEEDBACK] Registered write: {:?}, total: {}",
                                                path,
                                                app.total_file_writes
                                            );
                                        }
                                    }
                                }
                            }

                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::ToolProgress { tool_call_id, tool_name, message, progress_percent } => {
                            // Display real-time tool execution progress
                            let progress_display = if let Some(percent) = progress_percent {
                                format!("[{}] {} ({}%)", tool_name, message, percent)
                            } else {
                                format!("[{}] {}", tool_name, message)
                            };
                            app.output.push_tool_log(tool_call_id, progress_display);
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::TurnDone { new_messages, usage } => {
                            app.output.finalize_streaming();

                            // Display token usage summary for this turn
                            let total_tokens = usage.prompt_tokens + usage.completion_tokens;
                            let cost_this_turn = cost::estimate_cost(&model_name, &usage);

                            // Check if compression was used and display context info
                            let context_info = if let Some((ref compressed_msgs, source_count)) = compressed_cache {
                                let current_total = target_session.messages.len();
                                let recent_msgs = current_total.saturating_sub(source_count);
                                format!(" | Context: {} compressed + {} recent = {} total msgs",
                                    compressed_msgs.len(), recent_msgs, current_total)
                            } else {
                                let current_total = target_session.messages.len();
                                format!(" | Context: {} msgs (no compression)", current_total)
                            };

                            app.output.push_line(OutputLine::System(format!(
                                "\n💰 Token Usage: {} prompt + {} completion = {} total | Cost: ${:.4}{}",
                                usage.prompt_tokens,
                                usage.completion_tokens,
                                total_tokens,
                                cost_this_turn,
                                context_info
                            )));

                            // Two-tier ToolResult truncation (in-memory only; JSONL keeps full content).
                            let prev_count = target_session.messages.len();
                            let recent_boundary = {
                                let mut user_count = 0usize;
                                let mut boundary = prev_count;
                                for (i, m) in target_session.messages[..prev_count].iter().enumerate().rev() {
                                    if matches!(m, Message::User { .. }) {
                                        user_count += 1;
                                        if user_count >= 2 {
                                            boundary = i;
                                            break;
                                        }
                                    }
                                }
                                boundary
                            };
                            for (i, msg) in target_session.messages[..prev_count].iter_mut().enumerate() {
                                if let Message::ToolResult { content, .. } = msg {
                                    let char_len = content.chars().count();
                                    let (max_len, preview_len) = if i < recent_boundary {
                                        (500, 200)
                                    } else {
                                        (2000, 800)
                                    };
                                    if char_len > max_len {
                                        let preview: String = content.chars().take(preview_len).collect();
                                        *content = format!("{}...[truncated, {} chars total]", preview, char_len);
                                    }
                                }
                            }
                            for msg in &new_messages {
                                if let Err(e) = target_session.append_message(msg.clone()) {
                                    tracing::error!("Failed to persist message: {e}");
                                }
                            }
                            cost_tracker.record(&model_name, &usage);
                            memory_arc.update_from_turn(&new_messages, &rt_env.project_id, &rt_env.project_language);

                            // 🆕 Extract keywords from LLM response for semantic learning
                            for msg in &new_messages {
                                if let Message::Assistant { content, .. } = msg {
                                    // Try to extract keywords from the response
                                    if let Some(extracted) = keyword_extraction::extract_keywords_from_response(content) {
                                        // Get the last user query
                                        let last_user_query = target_session.messages.iter()
                                            .rev()
                                            .find_map(|m| match m {
                                                Message::User { content } => Some(content.as_str()),
                                                _ => None,
                                            })
                                            .unwrap_or("");
                                        
                                        // Record keywords (synchronous, fast operation)
                                        memory_arc.record_llm_keywords(last_user_query, extracted);
                                    }
                                }
                            }

                            // === IMPLICIT FEEDBACK: Evaluate satisfaction ===
                            // Calculate composite satisfaction score
                            let explicit_rate = if app.explicit_feedback_count > 0 {
                                app.good_feedback_count as f64 / app.explicit_feedback_count as f64
                            } else {
                                0.5 // Neutral if no explicit feedback
                            };

                            // Get tool success rate
                            let tool_success_rate = helpers::calculate_tool_success_rate(&target_session.messages);

                            // Get code accept rate from EMA
                            let code_accept_rate = app.ema_manager.get_value("code_accept_rate")
                                .unwrap_or(0.8); // Default if not tracked yet

                            let has_explicit = app.explicit_feedback_count >= 5;

                            let satisfaction_score = app.rollback_manager.calculate_satisfaction_score(
                                explicit_rate,
                                tool_success_rate,
                                code_accept_rate,
                                has_explicit,
                            );

                            tracing::info!(
                                "[SATISFACTION] explicit={:.2}, tool={:.2}, code_accept={:.2}, overall={:.2}",
                                explicit_rate,
                                tool_success_rate,
                                code_accept_rate,
                                satisfaction_score
                            );

                            // === END IMPLICIT FEEDBACK ===

                            // Post-turn asynchronous compression trigger
                            if let Some(ref cm) = compression_manager {
                                let session_id = target_session.meta.id.clone();
                                let msgs = target_session.messages.clone();
                                let store = Arc::clone(&compressed_ctx_store);
                                let tx_comp = agent_tx.clone();
                                let last_count = app.last_compression_msg_count;
                                let cw = context_window;
                                let cm_clone = cm.clone();

                                // Get memory context for better compression
                                let last_user_query = msgs.last()
                                    .and_then(|m| match m { Message::User { content } => Some(content.clone()), _ => None })
                                    .unwrap_or_default();
                                
                                // 🆕 Use retrieve_with_rerank if embedder is available
                                let memory_nodes = if let Some(cm) = &compression_manager {
                                    memory_arc.retrieve_with_rerank(
                                        &last_user_query,
                                        &Some(rt_env.project_id.as_str()),
                                        5,
                                        Some(cm.embedder()),
                                        None,
                                    )
                                } else {
                                    memory_arc.retrieve(&last_user_query, &Some(rt_env.project_id.as_str()), 5)
                                };
                                let memory_ctx = memory_arc.format_memory_context(&memory_nodes, false);

                                tokio::spawn(async move {
                                    let current_tokens = cm_clone.calculate_context_tokens(&msgs);

                                    // Use smart compression trigger
                                    let should_compress = cm_clone.should_compress_smart(&msgs, cw);

                                    if should_compress && msgs.len() > last_count {
                                        let query = msgs.last()
                                            .and_then(|m| match m { Message::User { content } => Some(content.clone()), _ => None })
                                            .unwrap_or_default();
                                        // Use enhanced compression with memory context
                                        let compressed_result = if !memory_ctx.is_empty() {
                                            cm_clone.compress_with_memory(&msgs, &query, Some(&memory_ctx))
                                        } else {
                                            cm_clone.compress(&msgs, &query)
                                        };

                                        if let Ok(Some(compressed)) = compressed_result {
                                            let source_count = msgs.len();
                                            let compressed_len = compressed.len();
                                            let _ = store.save(&session_id, &compressed, source_count);
                                            let _ = tx_comp.send(AgentToUiEvent::CompressionComplete {
                                                compressed_messages: compressed,
                                                source_msg_count: source_count,
                                            });
                                            tracing::info!("[ASYNC COMPRESS] Done: {} -> {} msgs (tokens: {})", source_count, compressed_len, current_tokens);
                                        }
                                    }
                                });
                            }

                            if background_session.is_some() {
                                background_session = None;
                                app.output.push_system("Background session completed and saved.");
                            } else {
                                app.agent_running = false;
                                app.status = String::new();
                                app.pending_confirmation = None;
                                app.message_count = session.messages.len();
                                app.cost_summary = cost_tracker.summary_short();
                                interrupt_ctrl.reset();
                                // Clear the UI→Agent channel after turn completes
                                app.ui_to_agent_tx = None;

                                // Process interjections after turn completion
                                let interjections_vec: Vec<String> = interjection_buf.drain();
                                if !interjections_vec.is_empty() {
                                    for inj_text in &interjections_vec {
                                        app.output.push_line(OutputLine::User(format!("(queued) {}", inj_text)));
                                    }
                                    if let Some(last) = interjections_vec.last() {
                                        app.output.push_line(OutputLine::System(String::new()));
                                        let user_msg = Message::user(last);
                                        if let Err(e) = session.append_message(user_msg) {
                                            tracing::error!("Failed to persist interjection: {e}");
                                        }
                                        // Trigger new run for interjection
                                        let text = last.clone();
                                        
                                        // 🆕 Use retrieve_with_rerank if embedder is available
                                        let memory_nodes = if let Some(cm) = &compression_manager {
                                            memory_arc.retrieve_with_rerank(
                                                &text,
                                                &Some(rt_env.project_id.as_str()),
                                                5,
                                                Some(cm.embedder()),
                                                None,
                                            )
                                        } else {
                                            memory_arc.retrieve(&text, &Some(rt_env.project_id.as_str()), 5)
                                        };
                                        let accessed_ids: Vec<&str> = memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                        memory_arc.reinforce_accessed(&accessed_ids);
                                        let memory_ctx = memory_arc.format_memory_context(&memory_nodes, false);
                                        let effective_messages = if let Some((ref cached, prev_count)) = compressed_cache {
                                            let new_msgs = &session.messages[prev_count.min(session.messages.len())..];
                                            let mut combined = cached.clone();
                                            combined.extend_from_slice(new_msgs);
                                            combined
                                        } else {
                                            session.messages.clone()
                                        };
                                        let turn_messages = context_builder.build(&system_prompt, &memory_ctx, &effective_messages, context_window);
                                        app.agent_running = true;
                                        app.status = "Thinking...".to_string();
                                        let provider = Arc::clone(provider.as_ref().unwrap());
                                        let tx = agent_tx.clone();
                                        let registry = Arc::clone(&tool_registry);
                                        let ctx = Arc::clone(&tool_ctx);
                                        let cancel_token = interrupt_ctrl.token();
                                        let tm = Arc::clone(&trust_manager);
                                        let ac = Arc::clone(&agent_config);
                                        let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
                                        app.ui_to_agent_tx = Some(ui_to_agent_tx);

                                        // Clone workflow engine for async task
                                        let workflow_engine_clone = app.workflow_engine.clone();

                                        tokio::spawn(async move {
                                            agent::run_agent_turn(provider, turn_messages, registry, ctx, tx, ui_to_agent_rx, cancel_token, tm, ac, false, workflow_engine_clone).await;
                                        });
                                        app.scroll_to_bottom();
                                        app.dirty = true;
                                        app.message_count = session.messages.len();
                                        app.cost_summary = cost_tracker.summary_short();
                                        continue;
                                    }
                                }
                            }

                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::Error(err) => {
                            app.output.finalize_streaming();
                            app.output.push_error(&format!("{err}"));
                            if background_session.is_some() {
                                background_session = None;
                            } else {
                                app.agent_running = false;
                                app.status = String::new();
                                // Clear the UI→Agent channel on error
                                app.ui_to_agent_tx = None;
                            }
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
                            app.output.push_line(OutputLine::Tool { name: format!("Confirm {} {:?}{}: {}", tool_name, safety_level, warning_str, args_summary), detail: None });
                            app.output.push_line(OutputLine::System(
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
                            app.output.push_line(OutputLine::System(format!(
                                "Token limit reached: {} tokens, est. cost: {}. Continue? [Y/N]",
                                total_tokens, estimated_cost
                            )));
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
                                app.output.push_line(OutputLine::System(line.to_string()));
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
                                memory_arc.store(mem_node);
                            }
                            app.last_council_session = Some(council_session);
                            if background_session.is_some() {
                                background_session = None;
                            } else {
                                app.agent_running = false;
                                app.status = "Ox".to_string();
                                // Clear the UI→Agent channel after council completes
                                app.ui_to_agent_tx = None;
                            }
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::WorkingDirChanged(new_dir) => {
                            let target = new_dir.display().to_string();
                            match runtime::change_directory(&mut rt_env, &target) {
                                runtime::DirectoryChangeResult::Success { new_dir, project_changed } => {
                                    app.output.push_line(OutputLine::System(format!(
                                        "Working directory: {}",
                                        new_dir.display()
                                    )));
                                    helpers::refresh_header_info(&mut app, &rt_env, provider.is_some());

                                    // Switch file index to new directory
                                    match file_index_registry.get_or_create(&new_dir) {
                                        Ok(new_file_index) => {
                                            file_index_manager = new_file_index;
                                            tracing::info!("Switched file index to: {:?}", new_dir);

                                            // Start file watcher for the new directory
                                            if let Err(e) = file_index_manager.start_file_watcher(new_dir.clone()) {
                                                tracing::warn!("Failed to start file watcher for new dir: {}", e);
                                            } else {
                                                tracing::info!("File watcher started for: {:?}", new_dir);
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to switch file index: {}. Keeping current index.", e);
                                        }
                                    }

                                    // Update tool_ctx for next agent turn.
                                    tool_ctx = Arc::new(ToolContext::new(
                                        rt_env.clone(),
                                        new_dir.clone(),
                                        Arc::new(config.clone()),
                                        Arc::clone(&memory_arc),
                                        Arc::clone(&file_index_manager),
                                    ));
                                    if project_changed {
                                        app.output.push_system(&format!(
                                            "Project boundary changed: {}",
                                            new_dir.display()
                                        ));
                                    }
                                }
                                _ => {} // Agent already resolved; unlikely to fail here.
                            }
                            app.dirty = true;
                        }
                        AgentToUiEvent::IterationLimitReached { iteration } => {
                            app.output.push_line(OutputLine::System(format!(
                                "Agent reached {} iterations. Continue? [Y] Yes / [N] Stop",
                                iteration
                            )));
                            app.pending_confirmation = Some(PendingConfirmation {
                                tool_call_id: "__iteration_limit__".into(),
                                tool_name: "iteration_limit".into(),
                            });
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::CompressionComplete { compressed_messages, source_msg_count } => {
                            let target_session = background_session.as_ref().unwrap_or(&session);
                            let sid = target_session.meta.id.clone();
                            if let Err(e) = compressed_ctx_store.save(&sid, &compressed_messages, source_msg_count) {
                                tracing::error!("Failed to save compressed context to SQLite: {e}");
                            } else {
                                tracing::info!(
                                    "[COMPRESSION] Saved to SQLite: source_msgs={}, compressed={}",
                                    source_msg_count,
                                    compressed_messages.len()
                                );

                                // Display compression notification to user
                                app.output.push_line(OutputLine::System(format!(
                                    "\n🗜️ Context Compressed: {} messages → {} messages (saved ~{} msgs)",
                                    source_msg_count,
                                    compressed_messages.len(),
                                    source_msg_count.saturating_sub(compressed_messages.len())
                                )));
                            }
                            compressed_cache = Some((compressed_messages, source_msg_count));
                            app.last_compression_msg_count = source_msg_count;
                            app.compression_in_progress = false;
                        }
                    }
                }
            }
        }

        if app.should_quit {
            memory_arc.flush();
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
    memory: &Arc<MemoryManager>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
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
    compression_manager: &Option<CompressionManager>,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    command_registry: &slash_commands::CommandRegistry,
) {
    match (key.code, key.modifiers) {
        // Confirmation key handling (Y/N/T when pending)
        (KeyCode::Char('y'), KeyModifiers::NONE) | (KeyCode::Char('Y'), KeyModifiers::NONE) => {
            if !helpers::handle_confirmation_key(app, &key) {
                app.input.insert_char('y');
                app.dirty = true;
            }
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) | (KeyCode::Char('N'), KeyModifiers::NONE) => {
            if !helpers::handle_confirmation_key(app, &key) {
                app.input.insert_char('n');
                app.dirty = true;
            }
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) | (KeyCode::Char('T'), KeyModifiers::NONE) => {
            if !helpers::handle_confirmation_key(app, &key) {
                app.input.insert_char('t');
                app.dirty = true;
            }
        }
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
            helpers::handle_control_key(app, &key);
        }
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
            helpers::handle_control_key(app, &key);
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            helpers::handle_control_key(app, &key);
        }
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
            helpers::handle_control_key(app, &key);
        }
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
            helpers::handle_control_key(app, &key);
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            helpers::handle_interrupt_key(app, &key, interrupt_ctrl);
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            helpers::handle_interrupt_key(app, &key, interrupt_ctrl);
        }
        (KeyCode::Enter, _) => {
            if let Some(input) = app.submit_input() {
                match input {
                    UserInput::Exit => {
                        app.output.push_system("Goodbye.");
                        app.should_quit = true;
                    }
                    UserInput::SlashCommand { cmd, args } => {
                        // Execute command via registry
                        if let Some(meta) = command_registry.get_command(&cmd) {
                            let result = (meta.handler)(
                                app,
                                &args,
                                session,
                                rt_env,
                                config,
                                memory,
                                cost_tracker,
                                trust_manager,
                            );
                            match result {
                                slash_commands::CommandResult::Error(msg) => {
                                    app.output.push_error(&msg);
                                }
                                slash_commands::CommandResult::Unknown(_) => {
                                    app.output.push_system(&format!(
                                        "Unknown command: /{}. Type /help for available commands.",
                                        cmd
                                    ));
                                }
                                _ => {}
                            }
                        } else {
                            app.output.push_system(&format!(
                                "Unknown command: /{}. Type /help for available commands.",
                                cmd
                            ));
                        }
                        // Mark dirty to trigger UI refresh after slash command processing
                        app.dirty = true;

                        // Check if spec planning was triggered by /spec command
                        if let Some(spec_content) = app.pending_spec_planning.take() {
                            if let Some(provider) = provider {
                                // Build system prompt with spec content
                                let mut current_system_prompt = context::build_system_prompt(
                                    &rt_env,
                                    &tool_registry,
                                    None,
                                    Some(&config.behavior_rules),
                                    Some(&spec_content),
                                );

                                // Add workflow step instructions
                                if let Some(ref wf_info) = app.workflow_display {
                                    if let Some(ref step_prompt) = wf_info.step_prompt {
                                        current_system_prompt
                                            .push_str("\n\n## Current Workflow Step\n\n");

                                        // Replace {project_ox_dir} placeholder with actual path
                                        let project_ox_dir = rt_env
                                            .project_ox_dir
                                            .as_ref()
                                            .map(|p| p.to_string_lossy().to_string())
                                            .unwrap_or_else(|| ".ox".to_string());
                                        let processed_prompt = step_prompt
                                            .replace("{project_ox_dir}", &project_ox_dir);

                                        current_system_prompt.push_str(&processed_prompt);

                                        if !wf_info.allows_code_modification {
                                            current_system_prompt.push_str("\n\n⚠️ IMPORTANT: You CAN use tools (file_read, file_search, etc.) but you CANNOT modify source code files (.rs, .py, .js, etc.) in this step. You can only create/modify documentation files (.md, .txt, etc.).");
                                        }
                                    }
                                }

                                // Create a user message to trigger planning
                                let planning_msg = Message::user(&format!(
                                    "Based on the following requirement, please analyze and create a detailed spec document (.ox/spec.md):\n\n{}",
                                    spec_content
                                ));

                                if let Err(e) = session.append_message(planning_msg) {
                                    tracing::error!("Failed to persist message: {e}");
                                }

                                let memory_nodes = if let Some(cm) = &compression_manager {
                                    memory.retrieve_with_rerank(
                                        &spec_content,
                                        &Some(rt_env.project_id.as_str()),
                                        5,
                                        Some(cm.embedder()),
                                        None,
                                    )
                                } else {
                                    memory.retrieve(
                                        &spec_content,
                                        &Some(rt_env.project_id.as_str()),
                                        5,
                                    )
                                };
                                let accessed_ids: Vec<&str> =
                                    memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                memory.reinforce_accessed(&accessed_ids);
                                let memory_ctx = memory.format_memory_context(&memory_nodes, false);

                                let effective_messages =
                                    if let Some((cached, prev_count)) = compressed_cache {
                                        let pc = *prev_count;
                                        let new_msgs =
                                            &session.messages[pc.min(session.messages.len())..];
                                        let mut combined = cached.clone();
                                        combined.extend_from_slice(new_msgs);
                                        combined
                                    } else {
                                        session.messages.clone()
                                    };

                                let turn_messages = context_builder.build(
                                    &current_system_prompt,
                                    &memory_ctx,
                                    &effective_messages,
                                    context_window,
                                );

                                app.agent_running = true;
                                app.status = "Planning...".to_string();
                                let effort = ox_core::context::estimate_effort(
                                    &spec_content,
                                    session.messages.len(),
                                );
                                let planning = effort == ox_core::context::EffortLevel::High;
                                let provider = Arc::clone(provider);
                                let tx = agent_tx.clone();
                                let registry = Arc::clone(tool_registry);
                                let ctx = Arc::clone(tool_ctx);
                                let cancel_token = interrupt_ctrl.token();
                                let tm = Arc::clone(&trust_manager);
                                let ac = Arc::clone(&agent_config);
                                let (ui_to_agent_tx, ui_to_agent_rx) =
                                    mpsc::unbounded_channel::<UiToAgentEvent>();
                                app.ui_to_agent_tx = Some(ui_to_agent_tx);

                                let workflow_engine_clone = app.workflow_engine.clone();

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
                                        planning,
                                        workflow_engine_clone,
                                    )
                                    .await;
                                });
                            }
                        }

                        // 🚨 Check if smart naming was requested
                        if let Some(pending) = app.pending_smart_naming.take() {
                            if let Some(provider) = provider {
                                let content = pending.content.clone();
                                
                                // Spawn async task to generate name
                                let provider_clone = Arc::clone(provider);
                                tokio::spawn(async move {
                                    match spec_helpers::generate_smart_name(&content, &provider_clone).await {
                                        Ok(name) => {
                                            tracing::info!("✅ Smart name generated: {}", name);
                                            // Store the result for next iteration
                                            // Note: This is a simplified approach - in production, you'd want to use a channel or shared state
                                        }
                                        Err(e) => {
                                            tracing::warn!("⚠️ Failed to generate smart name: {}, using auto-extraction", e);
                                        }
                                    }
                                });
                                
                                app.output.push_system("🧠 Generating smart name with LLM (timeout: 5s)...");
                                // For now, continue with auto-extraction as fallback
                                // The LLM-generated name will be used in future iterations
                            }
                        }

                        // 🚨 Check if workflow approval was triggered by /Y command
                        if app.pending_workflow_approval {
                            app.pending_workflow_approval = false;
                            
                            if let Some(provider) = provider {
                                // Build system prompt dynamically
                                let mut current_system_prompt = context::build_system_prompt(
                                    &rt_env,
                                    &tool_registry,
                                    None,
                                    Some(&config.behavior_rules),
                                    if app.spec_active && !app.spec_content.is_empty() {
                                        Some(&app.spec_content)
                                    } else {
                                        None
                                    },
                                );

                                // Add workflow step instructions
                                if let Some(ref wf_info) = app.workflow_display {
                                    if let Some(ref step_prompt) = wf_info.step_prompt {
                                        current_system_prompt
                                            .push_str("\n\n## Current Workflow Step\n\n");

                                        let project_ox_dir = rt_env
                                            .project_ox_dir
                                            .as_ref()
                                            .map(|p| p.to_string_lossy().to_string())
                                            .unwrap_or_else(|| ".ox".to_string());
                                        let mut processed_prompt = step_prompt
                                            .replace("{project_ox_dir}", &project_ox_dir);
                                        
                                        // 🚨 Replace {REQUIREMENT_NAME} placeholder with actual requirement name
                                        if let Some(ref req_name) = wf_info.requirement_name {
                                            processed_prompt = processed_prompt.replace("{REQUIREMENT_NAME}", req_name);
                                            processed_prompt = processed_prompt.replace("{YOUR_NAME}", req_name);
                                            processed_prompt = processed_prompt.replace("{IDENTIFIED_NAME}", req_name);
                                            processed_prompt = processed_prompt.replace("{requirement_name}", req_name);
                                        }

                                        current_system_prompt.push_str(&processed_prompt);

                                        if !wf_info.allows_code_modification {
                                            current_system_prompt.push_str("\n\n⚠️ IMPORTANT: You CAN use tools (file_read, file_search, etc.) but you CANNOT modify source code files (.rs, .py, .js, etc.) in this step. You can only create/modify documentation files (.md, .txt, etc.).");
                                        }
                                    }
                                }

                                let memory_nodes = if let Some(cm) = &compression_manager {
                                    memory.retrieve_with_rerank(
                                        "User approved workflow progression",
                                        &Some(rt_env.project_id.as_str()),
                                        5,
                                        Some(cm.embedder()),
                                        None,
                                    )
                                } else {
                                    memory.retrieve(
                                        "User approved workflow progression",
                                        &Some(rt_env.project_id.as_str()),
                                        5,
                                    )
                                };
                                let accessed_ids: Vec<&str> =
                                    memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                memory.reinforce_accessed(&accessed_ids);
                                let memory_ctx = memory.format_memory_context(&memory_nodes, false);

                                let effective_messages =
                                    if let Some((cached, prev_count)) = compressed_cache {
                                        let pc = *prev_count;
                                        let new_msgs =
                                            &session.messages[pc.min(session.messages.len())..];
                                        let mut combined = cached.clone();
                                        combined.extend_from_slice(new_msgs);
                                        combined
                                    } else {
                                        session.messages.clone()
                                    };

                                let turn_messages = context_builder.build(
                                    &current_system_prompt,
                                    &memory_ctx,
                                    &effective_messages,
                                    context_window,
                                );

                                app.agent_running = true;
                                app.status = "Thinking...".to_string();
                                let effort = ox_core::context::estimate_effort(
                                    "User approved",
                                    session.messages.len(),
                                );
                                let planning = effort == ox_core::context::EffortLevel::High;
                                let provider = Arc::clone(provider);
                                let tx = agent_tx.clone();
                                let registry = Arc::clone(tool_registry);
                                let ctx = Arc::clone(tool_ctx);
                                let cancel_token = interrupt_ctrl.token();
                                let tm = Arc::clone(&trust_manager);
                                let ac = Arc::clone(&agent_config);
                                let (ui_to_agent_tx, ui_to_agent_rx) =
                                    mpsc::unbounded_channel::<UiToAgentEvent>();
                                app.ui_to_agent_tx = Some(ui_to_agent_tx);

                                let workflow_engine_clone = app.workflow_engine.clone();

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
                                        planning,
                                        workflow_engine_clone,
                                    )
                                    .await;
                                });
                            }
                        }

                        // Check if a council discuss was queued
                        if let Some((question, rounds, verbose)) = app.pending_discuss.take() {
                            let council_config = config.council.clone();
                            let models_config = config.models.clone();
                            let ctx_messages = session.messages.clone();
                            let agent_tx_council = agent_tx.clone();
                            tokio::spawn(async move {
                                use ox_core::council::orchestrator::CouncilOrchestrator;
                                let orch = CouncilOrchestrator::new(models_config, council_config);
                                match orch
                                    .convene(&question, &ctx_messages, rounds, verbose)
                                    .await
                                {
                                    Ok(council_session) => {
                                        let _ =
                                            agent_tx_council.send(AgentToUiEvent::CouncilDone {
                                                session: council_session,
                                            });
                                    }
                                    Err(e) => {
                                        let _ = agent_tx_council.send(AgentToUiEvent::Error(
                                            format!("Council failed: {}", e),
                                        ));
                                    }
                                }
                            });
                        }
                    }
                    UserInput::Text(text) => {
                        // Handle spec edit mode
                        if app.spec_edit_mode {
                            app.spec_edit_mode = false;
                            app.spec_content = text.clone();
                            app.spec_active = true;

                            // Save to file
                            if let Some(ref project_root) = rt_env.project_root {
                                match context::save_spec(
                                    project_root,
                                    &config.spec.file_path,
                                    &text,
                                ) {
                                    Ok(path) => {
                                        app.output.push_system(&format!(
                                            "✅ Spec saved to {} ({} chars)",
                                            path,
                                            text.len()
                                        ));
                                    }
                                    Err(e) => {
                                        app.output
                                            .push_error(&format!("Failed to save spec: {}", e));
                                    }
                                }
                            } else {
                                app.output.push_system(&format!(
                                    "✅ Spec set ({} chars, not persisted - no project root)",
                                    text.len()
                                ));
                            }

                            app.output.push_system(
                                "Spec mode activated. AI will use this spec for task planning.",
                            );
                            app.dirty = true;
                        } else if app.agent_running {
                            // Handle interjection during agent execution
                            let priority = if text.starts_with('!') {
                                InterjectionPriority::Urgent
                            } else {
                                InterjectionPriority::Normal
                            };
                            let content = text.trim_start_matches('!').to_string();

                            if let Some(tx) = &app.ui_to_agent_tx {
                                let _ = tx.send(UiToAgentEvent::Interjection(content.clone()));
                            }

                            interjection_buf.push(content.clone(), priority);

                            let prefix = if priority == InterjectionPriority::Urgent {
                                "(urgent!)"
                            } else {
                                "(queued)"
                            };
                            app.output.push_line(OutputLine::System(format!(
                                "{} {}",
                                prefix,
                                content.trim()
                            )));
                            app.scroll_to_bottom();
                            app.dirty = true;
                        } else if let Some(provider) = provider {
                            // Build system prompt dynamically to include latest spec content AND workflow step instructions
                            let mut current_system_prompt = context::build_system_prompt(
                                &rt_env,
                                &tool_registry,
                                None,
                                Some(&config.behavior_rules),
                                if app.spec_active && !app.spec_content.is_empty() {
                                    Some(&app.spec_content)
                                } else {
                                    None
                                },
                            );

                            // Add workflow step instructions if in Spec or Council mode (use cached data)
                            if let Some(ref wf_info) = app.workflow_display {
                                if let Some(ref step_prompt) = wf_info.step_prompt {
                                    current_system_prompt
                                        .push_str("\n\n## Current Workflow Step\n\n");

                                    // Replace {project_ox_dir} placeholder with actual path
                                    let project_ox_dir = rt_env
                                        .project_ox_dir
                                        .as_ref()
                                        .map(|p| p.to_string_lossy().to_string())
                                        .unwrap_or_else(|| ".ox".to_string());
                                    let mut processed_prompt =
                                        step_prompt.replace("{project_ox_dir}", &project_ox_dir);
                                    
                                    // 🚨 Replace {REQUIREMENT_NAME} placeholder with actual requirement name
                                    if let Some(ref req_name) = wf_info.requirement_name {
                                        processed_prompt = processed_prompt.replace("{REQUIREMENT_NAME}", req_name);
                                        processed_prompt = processed_prompt.replace("{YOUR_NAME}", req_name);
                                        processed_prompt = processed_prompt.replace("{IDENTIFIED_NAME}", req_name);
                                        processed_prompt = processed_prompt.replace("{requirement_name}", req_name);
                                    }

                                    current_system_prompt.push_str(&processed_prompt);

                                    // Add tool restriction warnings
                                    if !wf_info.allows_code_modification {
                                        current_system_prompt.push_str("\n\n⚠️ IMPORTANT: You CAN use tools (file_read, file_search, etc.) but you CANNOT modify source code files (.rs, .py, .js, etc.) in this step. You can only create/modify documentation files (.md, .txt, etc.).");
                                    }
                                }
                            }

                            let user_msg = Message::user(&text);
                            if let Err(e) = session.append_message(user_msg) {
                                tracing::error!("Failed to persist user message: {e}");
                            }

                            // Check if this is feedback during a confirmation step
                            let mut should_rewind = false;
                            let mut rewind_to_step = None;

                            if let Some(ref wf_info) = app.workflow_display {
                                // Detect if user is providing feedback instead of confirmation
                                let is_feedback = text.contains("修改")
                                    || text.contains("改")
                                    || text.contains("调整")
                                    || text.contains("优化")
                                    || text.contains("不对")
                                    || text.contains("错误")
                                    || text.to_lowercase().contains("revise")
                                    || text.to_lowercase().contains("modify")
                                    || text.to_lowercase().contains("change")
                                    || text.to_lowercase().contains("update");

                                if is_feedback {
                                    match wf_info.step_name.as_str() {
                                        "Await Spec Confirmation" => {
                                            // User is giving feedback on spec.md, go back to Step 2 (generate_spec)
                                            should_rewind = true;
                                            rewind_to_step = Some(1); // index 1 = generate_spec
                                            app.output.push_system("📝 Detected feedback on spec. Returning to Step 2 for revision...");
                                        }
                                        "Await Task Confirmation" => {
                                            // User is giving feedback on task.md, go back to Step 4 (generate_task)
                                            should_rewind = true;
                                            rewind_to_step = Some(3); // index 3 = generate_task
                                            app.output.push_system("📝 Detected feedback on task plan. Returning to Step 4 for revision...");
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            // Rewind workflow if needed
                            if should_rewind {
                                if let Some(step_idx) = rewind_to_step {
                                    if let Some(ref mut engine_arc) = app.workflow_engine {
                                        if let Ok(mut engine) = engine_arc.try_lock() {
                                            if let Err(e) = engine.go_to_step(step_idx) {
                                                app.output.push_error(&format!(
                                                    "Failed to rewind workflow: {}",
                                                    e
                                                ));
                                            }
                                        }
                                    }
                                }
                                
                                // 🚨 CRITICAL: Add system message to inform LLM about user feedback
                                let feedback_msg = Message::system(&format!(
                                    "📝 User provided revision feedback:\n{}\n\nPlease revise your work based on this feedback.",
                                    text
                                ));
                                if let Err(e) = session.append_message(feedback_msg) {
                                    tracing::error!("Failed to persist feedback message: {e}");
                                }
                            }

                            let memory_nodes = if let Some(cm) = &compression_manager {
                                memory.retrieve_with_rerank(
                                    &text,
                                    &Some(rt_env.project_id.as_str()),
                                    5,
                                    Some(cm.embedder()),
                                    None,
                                )
                            } else {
                                memory.retrieve(&text, &Some(rt_env.project_id.as_str()), 5)
                            };
                            let accessed_ids: Vec<&str> =
                                memory_nodes.iter().map(|n| n.id.as_str()).collect();
                            memory.reinforce_accessed(&accessed_ids);
                            let memory_ctx = memory.format_memory_context(&memory_nodes, false);

                            // Build effective messages using the latest compressed cache from SQLite
                            let effective_messages = if let Some((cached, prev_count)) =
                                compressed_cache
                            {
                                let pc = *prev_count;
                                let new_msgs = &session.messages[pc.min(session.messages.len())..];
                                let mut combined = cached.clone();
                                combined.extend_from_slice(new_msgs);
                                combined
                            } else {
                                session.messages.clone()
                            };

                            let turn_messages = context_builder.build(
                                &current_system_prompt, // Use dynamically built prompt
                                &memory_ctx,
                                &effective_messages,
                                context_window,
                            );
                            app.agent_running = true;
                            app.status = "Thinking...".to_string();
                            let effort =
                                ox_core::context::estimate_effort(&text, session.messages.len());
                            let planning = effort == ox_core::context::EffortLevel::High;
                            let provider = Arc::clone(provider);
                            let tx = agent_tx.clone();
                            let registry = Arc::clone(tool_registry);
                            let ctx = Arc::clone(tool_ctx);
                            let cancel_token = interrupt_ctrl.token();
                            let tm = Arc::clone(&trust_manager);
                            let ac = Arc::clone(&agent_config);
                            let (ui_to_agent_tx, ui_to_agent_rx) =
                                mpsc::unbounded_channel::<UiToAgentEvent>();
                            app.ui_to_agent_tx = Some(ui_to_agent_tx);

                            // Clone workflow engine Arc for the async task
                            let workflow_engine_clone = app.workflow_engine.clone();

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
                                    planning,
                                    workflow_engine_clone,
                                )
                                .await;
                            });
                        } else {
                            app.output
                                .push_line(OutputLine::System(format!("[echo] {}", text.trim())));
                        }
                    }
                }
                app.scroll_to_bottom();
                app.user_scrolled = false;
            }
        }
        (KeyCode::Backspace, _) => {
            helpers::handle_editing_key(app, &key);
        }
        (KeyCode::Delete, _) => {
            helpers::handle_editing_key(app, &key);
        }
        (KeyCode::Left, _) => {
            helpers::handle_editing_key(app, &key);
        }
        (KeyCode::Right, _) => {
            helpers::handle_editing_key(app, &key);
        }
        (KeyCode::Up, KeyModifiers::SHIFT) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::Down, KeyModifiers::SHIFT) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::Up, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::Down, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::Home, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::End, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::PageUp, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::PageDown, _) => {
            helpers::handle_navigation_key(app, &key);
        }
        (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            helpers::handle_char_input(app, ch);
        }
        _ => {}
    }
}
