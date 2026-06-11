mod terminal;
pub mod slash_commands;
pub mod middleware;
pub mod helpers;
pub mod keyword_extraction;
pub mod app_runtime;
pub mod handlers;
pub mod event_loop;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::KeyCode;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use ox_core::agent::{self, AgentToUiEvent};
use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::interrupt::InterruptController;
use ox_core::agent::ui_event::UiToAgentEvent;
use ox_core::config::{AgentConfig, OxConfig};
use ox_core::context::{self, ContextBuilder};
use ox_core::cost::CostTracker;
use ox_core::knowledge::KnowledgeEngine;
use ox_core::llm::{self, LlmProvider, ProviderResolveInfo};
use ox_core::memory::MemoryManager;
use ox_core::message::{Message, Session};
use ox_core::runtime;
use ox_core::safety::injection;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};
use terminal::app::{App, PlanItem, PlanItemStatus, SessionAction, UserInput};
use terminal::event::{Event, EventHandler};
use terminal::output_pane::OutputLine;
use terminal::render;

// ── Handler imports ──
use handlers::agent_handler::{self, HandleResult};
use handlers::key_handler::{self, KeyResult};
use handlers::pre_turn::TurnVariant;
use handlers::session_handler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging()?;
    install_panic_hook();

    let config = OxConfig::load(None)?;
    let rt_env = runtime::detect_runtime();

    let (provider, resolve_info, provider_error) = create_provider(&config);
    if let Some(ref err) = provider_error {
        tracing::warn!("Provider init failed (will retry on /model): {}", err);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_app(
        &mut terminal, &config, rt_env, provider, resolve_info, provider_error,
    )
    .await;

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ============================================================================
// Helper Functions (unchanged)
// ============================================================================

fn init_logging() -> anyhow::Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let log_dir = home.join(".ox").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file_path = log_dir.join("ox.log");

    const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(&log_file_path) {
        if meta.len() > MAX_LOG_SIZE {
            for i in (1..3).rev() {
                let old = log_dir.join(format!("ox.log.{}", i));
                let new = log_dir.join(format!("ox.log.{}", i + 1));
                if old.exists() {
                    let _ = std::fs::rename(&old, &new);
                }
            }
            let _ = std::fs::rename(&log_file_path, log_dir.join("ox.log.1"));
        }
    }

    if let Ok(log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
    {
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = tracing_subscriber::EnvFilter::new("ox_core=info,ox_cli=info,tracing=info");
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .with_filter(filter);
        tracing_subscriber::registry()
            .with(file_layer)
            .init();
        tracing::info!("✅ Logging initialized. Writing to: {:?}", log_file_path);
    } else {
        use tracing_subscriber::filter::LevelFilter;
        tracing_subscriber::fmt()
            .with_max_level(LevelFilter::OFF)
            .init();
    }
    Ok(())
}

fn install_panic_hook() {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_panic(info);
    }));
}

fn create_provider(
    config: &OxConfig,
) -> (Option<Arc<dyn LlmProvider>>, Option<ProviderResolveInfo>, Option<String>) {
    match llm::create_provider_with_info(&config.models.default, &config.models) {
        Ok((p, info)) => (Some(Arc::from(p)), Some(info), None),
        Err(e) => {
            let msg = format!("{}", e);
            (None, None, Some(msg))
        }
    }
}

// ============================================================================
// Main Application
// ============================================================================

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &OxConfig,
    mut rt_env: runtime::RuntimeEnvironment,
    mut provider: Option<Arc<dyn LlmProvider>>,
    mut resolve_info: Option<ProviderResolveInfo>,
    _provider_error: Option<String>,
) -> anyhow::Result<()> {
    // ── Directory structure ──
    {
        let ox = &rt_env.ox_home_dir;
        for sub in &["sessions", "db", "logs", "skills", "memory"] {
            let _ = std::fs::create_dir_all(ox.join(sub));
        }
        if let Some(ref proj_ox) = rt_env.project_ox_dir {
            let _ = std::fs::create_dir_all(proj_ox.join("skills"));
            let _ = std::fs::create_dir_all(proj_ox.join("memory"));
        }
    }

    let mut app = App::new();

    // Status bar
    app.model_name = provider
        .as_ref()
        .map(|p| p.model_name().to_string())
        .unwrap_or_else(|| "echo".to_string());
    app.working_dir = rt_env.working_dir.display().to_string();
    app.message_count = 0;

    // Header
    app.header_info.push(rt_env.banner_summary());
    if provider.is_some() {
        app.header_info.push("Type a message or /help for commands. /exit to quit.".to_string());
    } else {
        app.header_info.push("No API key. Set env var or config. Running in echo mode.".to_string());
    }

    if !OxConfig::config_exists() {
        app.output.push_system(
            "No config file found. Run /init to create ~/.ox/config.toml with default settings.",
        );
    }

    // ── Session ──
    let sessions_root = rt_env.ox_home_dir.join("sessions");
    let session_dir = sessions_root.join(&rt_env.project_id);
    let mut session = if config.session.auto_restore {
        match Session::load(&session_dir)? {
            Some(s) => {
                app.output.push_line(OutputLine::System(format!(
                    "Session restored ({} messages)",
                    s.user_message_count()
                )));
                // Restore plan items
                if !s.meta.plan_json.is_empty() {
                    if let Ok(items) =
                        serde_json::from_str::<Vec<serde_json::Value>>(&s.meta.plan_json)
                    {
                        for item in items {
                            if let (Some(file), Some(status)) =
                                (item["file"].as_str(), item["status"].as_str())
                            {
                                app.plan_items.push(PlanItem {
                                    file: file.to_string(),
                                    status: match status {
                                        "done" => PlanItemStatus::Done,
                                        "cancelled" => PlanItemStatus::Cancelled,
                                        _ => PlanItemStatus::Pending,
                                    },
                                });
                            }
                        }
                    }
                }
                helpers::replay_session_history(&mut app, &s.messages, &rt_env, provider.is_some());
                s
            }
            None => Session::new(&session_dir, &rt_env.project_id)?,
        }
    } else {
        Session::new(&session_dir, &rt_env.project_id)?
    };

    // Initial sidebar population
    session_handler::rebuild_sidebar(
        &mut app,
        &sessions_root,
        &rt_env.project_id,
        &helpers::session_display_name(&session),
    );

    // ── Subsystem initialization ──
    let tool_registry = Arc::new(ToolRegistry::new());
    if let Err(e) = tool_registry.load_skills(&rt_env) {
        tracing::warn!("Failed to load skills: {}", e);
    }
    let command_registry = slash_commands::CommandRegistry::new();

    // Load spec if auto_load enabled
    if config.spec.auto_load {
        if let Some(ref project_root) = rt_env.project_root {
            match context::load_spec(project_root, &config.spec.file_path) {
                Ok(content) if !content.is_empty() => {
                    app.activate_spec_mode(content);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to load spec: {}", e);
                }
            }
        }
    }

    // Initial system prompt (for interjections, not main turns)
    let system_prompt = context::build_system_prompt(
        &rt_env, &tool_registry, ox_core::context::UserIntent::General,
        Some(&config.behavior_rules), None, None,
    );

    let context_builder = ContextBuilder::from_config(&config.context);
    let context_window = provider
        .as_ref()
        .map(|p| p.context_window_size())
        .unwrap_or(128_000);

    let db_dir = rt_env.ox_home_dir.join("db");
    let mut cost_tracker = CostTracker::load_or_create(&db_dir).unwrap_or_else(|e| {
        tracing::warn!("Failed to load cost tracker: {e}");
        CostTracker::load_or_create(&std::env::temp_dir()).unwrap()
    });

    // ── Knowledge Engine + Memory: parallel init ──
    let db_path = db_dir.join("knowledge.tdb");
    let db_path_str = db_path.to_string_lossy().to_string();
    let config_clone = config.clone();
    let rt_env_clone = rt_env.clone();
    let ox_home = rt_env.ox_home_dir.clone();
    let project_id = rt_env.project_id.clone();
    let mem_config = config.memory.clone();

    let (knowledge_engine, memory_arc) = {
        let knowledge_fut = tokio::task::spawn_blocking(move || {
            let embedding_model = ox_core::knowledge::embedding::load_shared(&config_clone.embedding)
                .unwrap_or_else(|e| {
                    panic!("Embedding model required for KnowledgeEngine: {e}");
                });
            KnowledgeEngine::new(
                &db_path_str, embedding_model, &config_clone.embedding, &rt_env_clone.working_dir,
            )
            .unwrap_or_else(|e| {
                tracing::error!("Failed to create KnowledgeEngine: {e}");
                std::process::exit(1);
            })
        });

        let memory_fut = tokio::task::spawn_blocking(move || {
            MemoryManager::init(&ox_home, &project_id, &mem_config)
                .unwrap_or_else(|e| {
                    tracing::warn!("Failed to init memory: {e}");
                    MemoryManager::init(&std::env::temp_dir(), &project_id, &mem_config)
                        .unwrap_or_else(|_| panic!("Fatal: cannot init memory"))
                })
        });

        let (knowledge, memory) = tokio::join!(knowledge_fut, memory_fut);
        let knowledge = knowledge.expect("KnowledgeEngine init panicked");
        let memory = memory.expect("Memory init panicked");

        (Arc::new(tokio::sync::RwLock::new(knowledge)), Arc::new(memory))
    };

    KnowledgeEngine::start_file_watcher(Arc::clone(&knowledge_engine));

    if let Err(e) = app
        .ema_manager
        .load_from_store("code_accept_rate", memory_arc.overall_store())
    {
        tracing::warn!("Failed to load EMA history: {}", e);
    }

    if rand::random::<f64>() < config.memory.janitor_run_on_startup_prob {
        memory_arc.run_janitor(0.3, config.memory.max_nodes);
    }

    // ── Background indexing ──
    let knowledge_for_index = Arc::clone(&knowledge_engine);
    let (index_progress_tx, mut index_progress_rx) =
        mpsc::unbounded_channel::<(usize, usize, usize)>();
    let (index_phase_tx, mut index_phase_rx) = mpsc::unbounded_channel::<String>();
    let (index_done_tx, mut index_done_rx) = mpsc::unbounded_channel::<usize>();
    tokio::spawn(async move {
        let _ = index_phase_tx.send("parsing".to_string());
        let phase1_result = {
            let engine = knowledge_for_index.read().await;
            engine.collect_all_symbols(None)
        };
        let all_entities = match phase1_result {
            Ok((entities, _)) => entities,
            Err(e) => {
                tracing::warn!("[INDEXER] Phase 1 failed: {e}");
                let _ = index_done_tx.send(0);
                return;
            }
        };
        if all_entities.is_empty() {
            let _ = index_done_tx.send(0);
            return;
        }
        let _ = index_phase_tx.send("embedding".to_string());
        const CHUNK: usize = 30;
        let total_entities = all_entities.len();
        let mut embedded = 0;
        while embedded < total_entities {
            let count = {
                let mut engine = knowledge_for_index.write().await;
                match engine.embed_and_store_chunk(&all_entities, embedded, CHUNK) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("[INDEXER] Embedding chunk failed at {}: {e}", embedded);
                        0
                    }
                }
            };
            embedded += count;
            let _ = index_progress_tx.send((embedded, total_entities, embedded));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        tracing::info!("[INDEXER] ✅ All done: {} entities embedded", total_entities);
        let _ = index_done_tx.send(total_entities);
    });
    app.indexing = true;
    app.status = "⏳ Background indexing... (chat ready)".to_string();

    // ── Tool context ──
    let mut tool_ctx = Arc::new(ToolContext::new(
        rt_env.clone(), rt_env.working_dir.clone(),
        Arc::new(config.clone()), Arc::clone(&knowledge_engine),
    ));

    let mut model_name = provider
        .as_ref()
        .map(|p| p.model_name().to_string())
        .unwrap_or_default();

    let trust_manager = Arc::new(std::sync::Mutex::new(TrustManager::new()));
    let mut interrupt_ctrl = InterruptController::new();
    let mut interjection_buf = InterjectionBuffer::new();
    let mut events = EventHandler::new(Duration::from_millis(33));
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentToUiEvent>();
    let agent_config = Arc::new(config.agent.clone());

    let compressed_ctx_store = Arc::new(
        ox_core::context::compressed_store::CompressedContextStore::open(
            &db_dir.join("compressed_context.db"),
        )
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to open compressed context store: {e}");
            ox_core::context::compressed_store::CompressedContextStore::open(
                &std::env::temp_dir().join("compressed_context.db"),
            )
            .unwrap()
        }),
    );

    let mut tick_count: u64 = 0;
    let mut compressed_cache: Option<(Vec<Message>, usize)> =
        compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
    let mut background_session: Option<Session> = None;

    // Workflow engine
    app.init_workflow_engine(&session.meta.id, &session.meta);
    app.knowledge_engine = Some(Arc::clone(&knowledge_engine));

    // ── Onboarding check ──
    let mut needs_onboarding = false;
    let mut onboarding_prompt_text = String::new();
    if let Some(ref root) = rt_env.project_root {
        let conventions = root.join(".ox").join("skills").join("project-conventions.md");
        let architecture = root.join(".ox").join("skills").join("project-architecture.md");
        if !conventions.exists() || !architecture.exists() {
            needs_onboarding = true;
            onboarding_prompt_text = format!(
                "You just opened a new project at `{}`. This is the FIRST time Ox has seen this project.\n\n\
                 Generate TWO skill files by analyzing the codebase:\n\n\
                 ## File 1: .ox/skills/project-conventions.md\n\
                 - Language, framework, build tool\n\
                 - Naming conventions, code style, import ordering\n\n\
                 ## File 2: .ox/skills/project-architecture.md\n\
                 - Directory structure and module layout\n\
                 - Layer boundaries, MUST/MUST NOT rules\n\
                 - Error handling patterns, key dependencies\n\n\
                 **Process**: Use project_detect, read config files, scan source dirs, create both files.\n\
                 When done, output `## Done` — Do NOT rewrite or touch the files again.",
                root.display()
            );
        }
    }

    // ========================================================================
    // MAIN EVENT LOOP
    // ========================================================================
    loop {
        // ── Onboarding trigger ──
        if needs_onboarding && !app.indexing {
            needs_onboarding = false;
            app.output.push_system(
                "🔍 First time in this project. Scanning codebase to learn conventions...",
            );
            if let Some(ref wf) = app.workflow_engine {
                if let Ok(mut engine) = wf.try_lock() {
                    engine.activate_workflow("five_step_pipeline").ok();
                    engine.reset_workflow();
                }
            }
            if let Some(p) = &provider {
                app.status = "Scanning project...".to_string();
                let _ = session.append_message(Message::user(&onboarding_prompt_text));

                let pre_turn_result = handlers::pre_turn::prepare_turn(
                    config, &rt_env, &tool_registry, &context_builder, context_window,
                    &Some(Arc::clone(&knowledge_engine)), &onboarding_prompt_text,
                    &session.messages, &compressed_cache,
                    TurnVariant::Onboarding { prompt_text: onboarding_prompt_text.clone() },
                    &app.workflow_engine, &session.meta.id, &agent_tx,
                )
                .await;

                let turn_messages = pre_turn_result.turn_messages;
                let planning = pre_turn_result.planning;

                app.agent_running = true;
                let tx = agent_tx.clone();
                let reg = Arc::clone(&tool_registry);
                let ctx = Arc::clone(&tool_ctx);
                let cancel = interrupt_ctrl.token();
                let tm = Arc::clone(&trust_manager);
                let ac = Arc::clone(&agent_config);
                let (ui_tx, ui_rx) = mpsc::unbounded_channel();
                app.ui_to_agent_tx = Some(ui_tx);
                let wf = app.workflow_engine.clone();
                let p_clone = Arc::clone(p);
                tokio::spawn(async move {
                    agent::run_agent_turn(
                        p_clone, turn_messages, reg, ctx, tx, ui_rx,
                        cancel, tm, ac, planning, wf,
                    )
                    .await;
                });
            }
        }

        // ── Implicit feedback ──
        let override_signals = app.override_detector.detect_overrides();
        middleware::feedback::process_implicit_feedback(&mut app, &override_signals);
        middleware::feedback::update_feedback_metrics(&mut app, &memory_arc);

        // ── Drain indexing progress ──
        if app.indexing {
            drain_indexing_progress(
                &mut app, &mut index_phase_rx, &mut index_progress_rx,
                &mut index_done_rx, &mut tick_count,
            );
        }

        // ── Render ──
        if app.needs_render() {
            terminal.draw(|frame| render::render(frame, &mut app, tick_count))?;
            app.dirty = false;
            app.mark_spinner_rendered();
        }

        // ── Event select ──
        tokio::select! {
            biased;
            ev = events.recv() => {
                match ev {
                    Some(Event::Key(first_key)) => {
                        // Paste detection: batch rapid keystrokes
                        let mut keys = vec![first_key];
                        while let Some(ev) = events.try_recv() {
                            if let Event::Key(k) = ev { keys.push(k); }
                        }
                        if keys.len() > 3 {
                            let pasted: String = keys.iter()
                                .filter_map(|k| {
                                    if let KeyCode::Char(c) = k.code { Some(c) } else { None }
                                })
                                .collect();
                            if pasted.len() > 1 {
                                app.input.insert_str(&pasted);
                                app.dirty = true;
                                continue;
                            }
                        }
                        for key in keys {
                            process_key_event(
                                &mut app, key, &mut session, &mut background_session,
                                &provider, &agent_tx, &memory_arc, &tool_registry,
                                &mut tool_ctx, &context_builder, context_window,
                                &mut cost_tracker, &trust_manager, &mut model_name,
                                &mut rt_env, &mut interrupt_ctrl,
                                &mut interjection_buf, &resolve_info,
                                config, &agent_config, &compressed_ctx_store,
                                &mut compressed_cache, &command_registry,
                            );
                        }

                        // Process session action
                        if !matches!(app.session_action, SessionAction::None) {
                            let action = std::mem::replace(&mut app.session_action, SessionAction::None);
                            process_session_action(
                                &mut app, &mut session, &mut background_session,
                                action, &mut rt_env, &memory_arc, &sessions_root,
                                &compressed_ctx_store, &mut compressed_cache,
                                provider.is_some(),
                            );
                            app.dirty = true;
                        }

                        // Model switch
                        if let Some(new_model_name) = app.pending_model_switch.take() {
                            match llm::create_provider_with_info(&new_model_name, &config.models) {
                                Ok((new_provider, new_info)) => {
                                    provider = Some(Arc::from(new_provider));
                                    resolve_info = Some(new_info);
                                    model_name = provider.as_ref()
                                        .map(|p| p.model_name().to_string())
                                        .unwrap_or_default();
                                    app.model_name = model_name.clone();
                                }
                                Err(e) => {
                                    app.output.push_system(&format!(
                                        "Failed to switch to '{}': {e}", new_model_name
                                    ));
                                }
                            }
                        }
                    }
                    Some(Event::Resize(_, _)) => { app.dirty = true; }
                    Some(Event::Tick) | None => {
                        tick_count = tick_count.wrapping_add(1);
                        app.spinner_frame = tick_count;
                        app.update_workflow_display();
                        if (app.agent_running || app.indexing)
                            && app.spinner_frame != app.last_spinner_frame
                        {
                            app.dirty = true;
                        }
                    }
                }
            }
            agent_ev = agent_rx.recv() => {
                if let Some(ev) = agent_ev {
                    process_agent_event(
                        &mut app, ev, &mut session, &mut background_session,
                        &provider, &agent_tx, &memory_arc, &tool_registry,
                        &mut tool_ctx, &context_builder, context_window,
                        &mut cost_tracker, &trust_manager, &mut model_name,
                        &mut rt_env, &mut interrupt_ctrl,
                        &mut interjection_buf, config, &agent_config,
                        &compressed_cache, &knowledge_engine,
                        &system_prompt,
                    );
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

// ============================================================================
// Event Processing Helpers
// ============================================================================

/// Drain indexing progress channels and update app state.
fn drain_indexing_progress(
    app: &mut App,
    phase_rx: &mut mpsc::UnboundedReceiver<String>,
    progress_rx: &mut mpsc::UnboundedReceiver<(usize, usize, usize)>,
    done_rx: &mut mpsc::UnboundedReceiver<usize>,
    tick_count: &mut u64,
) {
    if let Ok(phase) = phase_rx.try_recv() {
        app.status = if phase == "parsing" {
            "AST parsing…".to_string()
        } else {
            "Embedding vectors…".to_string()
        };
        app.dirty = true;
    }
    while let Ok((a, b, c)) = progress_rx.try_recv() {
        if app.status.starts_with("Embedding") {
            let pct = if b > 0 { (a * 100) / b } else { 0 };
            app.status = format!("Embedding {}/{} entities ({}%)", a, b, pct);
        } else {
            let pct = if b > 0 { (a * 100) / b } else { 0 };
            app.status = format!("AST {}/{} files, {} sym ({}%)", a, b, c, pct);
        }
        *tick_count = tick_count.wrapping_add(1);
        app.spinner_frame = *tick_count;
        app.dirty = true;
    }
    if let Ok(total) = done_rx.try_recv() {
        app.indexing = false;
        app.index_symbols = total;
        app.status = String::new();
        app.output.push_system(&format!(
            "✅ Indexing complete: {} symbols indexed. Ready to chat!", total
        ));
        app.dirty = true;
    }
}

/// Process a single key event.
#[allow(clippy::too_many_arguments)]
fn process_key_event(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    session: &mut Session,
    background_session: &mut Option<Session>,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    memory: &Arc<MemoryManager>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &mut Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    model_name: &mut String,
    rt_env: &mut runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    interjection_buf: &mut InterjectionBuffer,
    _resolve_info: &Option<ProviderResolveInfo>,
    config: &OxConfig,
    agent_config: &Arc<AgentConfig>,
    _compressed_ctx_store: &Arc<ox_core::context::compressed_store::CompressedContextStore>,
    compressed_cache: &mut Option<(Vec<Message>, usize)>,
    command_registry: &slash_commands::CommandRegistry,
) {
    // Handle Ctrl+C/D — check both with modifiers and without (cross-platform)
    let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
        && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
    let is_ctrl_d = matches!(key.code, KeyCode::Char('d'))
        && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);

    if is_ctrl_c || is_ctrl_d {
        helpers::handle_interrupt_key(app, &key, interrupt_ctrl);
        return;
    }

    let result = key_handler::handle_key(app, key);

    match result {
        KeyResult::Handled => {}
        KeyResult::Interrupt => {
            // Fallback: key_handler detected interrupt via pattern match
            helpers::handle_interrupt_key(app, &key, interrupt_ctrl);
        }
        KeyResult::InputSubmitted(input) => {
            match input {
                UserInput::Exit => {
                    app.output.push_system("Goodbye.");
                    app.should_quit = true;
                }
                UserInput::SlashCommand { cmd, args } => {
                    process_slash_command(
                        app, &cmd, &args, session, rt_env, config,
                        memory, cost_tracker, trust_manager,
                        provider, agent_tx, tool_registry, tool_ctx,
                        context_builder, context_window,
                        interrupt_ctrl, agent_config,
                        model_name, command_registry,
                    );
                }
                UserInput::Text(text) => {
                    process_text_input(
                        app, &text, session, background_session,
                        provider, agent_tx, memory, tool_registry,
                        tool_ctx, context_builder, context_window,
                        config, agent_config, trust_manager,
                        rt_env, interrupt_ctrl, interjection_buf,
                        compressed_cache, model_name, cost_tracker,
                    );
                }
            }
            app.scroll_to_bottom();
            app.user_scrolled = false;
        }
    }
}

/// Process a slash command.
#[allow(clippy::too_many_arguments)]
fn process_slash_command(
    app: &mut App,
    cmd: &str,
    args: &str,
    session: &mut Session,
    rt_env: &mut runtime::RuntimeEnvironment,
    config: &OxConfig,
    memory: &Arc<MemoryManager>,
    cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &mut Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    interrupt_ctrl: &mut InterruptController,
    agent_config: &Arc<AgentConfig>,
    _model_name: &str,
    command_registry: &slash_commands::CommandRegistry,
) {
    if let Some(meta) = command_registry.get_command(cmd) {
        let result = (meta.handler)(
            app, args, session, rt_env, config, memory,
            cost_tracker, trust_manager,
        );
        match result {
            slash_commands::CommandResult::Error(msg) => {
                app.output.push_error(&msg);
            }
            slash_commands::CommandResult::Unknown(_) => {
                app.output.push_system(&format!(
                    "Unknown command: /{}. Type /help for available commands.", cmd
                ));
            }
            slash_commands::CommandResult::LlmRequest { prompt, description } => {
                spawn_agent_turn_from_slash(
                    app, &prompt, &description, session,
                    provider, agent_tx, tool_registry, tool_ctx,
                    context_builder, context_window,
                    interrupt_ctrl, agent_config, trust_manager,
                    rt_env, config, memory,
                );
            }
            _ => {}
        }
    } else {
        app.output.push_system(&format!(
            "Unknown command: /{}. Type /help for available commands.", cmd
        ));
    }
    app.dirty = true;
}

/// Process normal text input (or interjection during agent run).
#[allow(clippy::too_many_arguments)]
fn process_text_input(
    app: &mut App,
    text: &str,
    session: &mut Session,
    _background_session: &mut Option<Session>,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    _memory: &Arc<MemoryManager>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &mut Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    config: &OxConfig,
    agent_config: &Arc<AgentConfig>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    rt_env: &mut runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    interjection_buf: &mut InterjectionBuffer,
    compressed_cache: &mut Option<(Vec<Message>, usize)>,
    _model_name: &mut String,
    _cost_tracker: &mut CostTracker,
) {
    if app.indexing {
        app.output.push_system("⏳ Please wait — indexing in progress...");
        app.dirty = true;
        return;
    }

    if app.agent_running {
        // Interjection during agent execution
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
        app.output.push_line(OutputLine::System(format!("{} {}", prefix, content.trim())));
        app.scroll_to_bottom();
        app.user_scrolled = false;
        app.dirty = true;
        return;
    }

    if provider.is_none() {
        app.output.push_line(OutputLine::System(format!("[echo] {}", text.trim())));
        return;
    }

    // Show status immediately
    app.status = "⏳ Preparing...".to_string();
    app.dirty = true;

    // Injection scan
    let user_text = if injection::is_suspicious(text) {
        let result = injection::detect(text);
        let categories: Vec<String> =
            result.matches.iter().map(|m| format!("{:?}", m.category)).collect();
        tracing::warn!("🛡️ Prompt injection detected: categories={:?}", categories);
        app.output.push_line(OutputLine::System(format!(
            "⚠️ Prompt injection detected and sanitized: {}",
            categories.join(", ")
        )));
        injection::sanitize(text)
    } else {
        text.to_string()
    };

    // Save user message
    let _ = session.append_message(Message::user(&user_text));

    // Workflow feedback detection
    detect_workflow_feedback(app, session, &user_text);

    // Spawn agent turn with pre-turn pipeline
    let rt_env_clone = rt_env.clone();
    let config_clone = config.clone();
    let tool_registry_clone = Arc::clone(tool_registry);
    let context_builder_clone = context_builder.clone();
    let session_messages = session.messages.clone();
    let compressed_cache_data = compressed_cache.clone();
    let workflow_engine_clone = app.workflow_engine.clone();
    let session_id = session.meta.id.clone();
    let knowledge_engine_clone = app.knowledge_engine.clone();
    let tx = agent_tx.clone();
    let provider_clone = Arc::clone(provider.as_ref().unwrap());
    let tool_ctx_clone = Arc::clone(tool_ctx);
    let cancel_token = interrupt_ctrl.token();
    let tm = Arc::clone(trust_manager);
    let ac = Arc::clone(agent_config);

    let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
    app.ui_to_agent_tx = Some(ui_to_agent_tx);
    app.agent_running = true;

    tokio::spawn(async move {
        let status_tx = tx.clone();
        let result = handlers::pre_turn::prepare_turn(
            &config_clone, &rt_env_clone, &tool_registry_clone,
            &context_builder_clone, context_window,
            &knowledge_engine_clone, &user_text,
            &session_messages, &compressed_cache_data,
            TurnVariant::Normal,
            &workflow_engine_clone, &session_id, &status_tx,
        )
        .await;

        let _ = status_tx.send(AgentToUiEvent::Status("🌐 Calling LLM...".to_string()));
        agent::run_agent_turn(
            provider_clone, result.turn_messages, tool_registry_clone,
            tool_ctx_clone, tx, ui_to_agent_rx,
            cancel_token, tm, ac, result.planning, workflow_engine_clone,
        )
        .await;
    });

    app.scroll_to_bottom();
    app.user_scrolled = false;
}

/// Detect workflow feedback in user text and trigger rewind if needed.
fn detect_workflow_feedback(app: &mut App, session: &mut Session, user_text: &str) {
    if let Some(ref wf_info) = app.workflow_display {
        let is_feedback = user_text.contains("修改")
            || user_text.contains("改")
            || user_text.contains("调整")
            || user_text.contains("优化")
            || user_text.contains("不对")
            || user_text.contains("错误")
            || user_text.to_lowercase().contains("revise")
            || user_text.to_lowercase().contains("modify")
            || user_text.to_lowercase().contains("change")
            || user_text.to_lowercase().contains("update");
        if is_feedback {
            let rewind_step = match wf_info.step_name.as_str() {
                "Await Spec Confirmation" => Some(1),
                "Await Task Confirmation" => Some(3),
                _ => None,
            };
            if let Some(step_idx) = rewind_step {
                app.output.push_system(&format!(
                    "📝 Detected feedback. Returning to Step {} for revision...",
                    step_idx + 1
                ));
                if let Some(ref mut engine_arc) = app.workflow_engine {
                    if let Ok(mut engine) = engine_arc.try_lock() {
                        let _ = engine.go_to_step(step_idx);
                    }
                }
                let _ = session.append_message(Message::system(&format!(
                    "📝 User provided revision feedback:\n{}\n\nPlease revise your work based on this feedback.",
                    user_text
                )));
            }
        }
    }
}

/// Spawn an agent turn from a slash command's LlmRequest.
#[allow(clippy::too_many_arguments)]
fn spawn_agent_turn_from_slash(
    app: &mut App,
    prompt: &str,
    description: &str,
    session: &mut Session,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    interrupt_ctrl: &InterruptController,
    agent_config: &Arc<AgentConfig>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    rt_env: &runtime::RuntimeEnvironment,
    config: &OxConfig,
    memory: &Arc<MemoryManager>,
) {
    app.output.push_system(&format!("🤖 {}", description));
    let _ = session.append_message(Message::user(prompt));

    if let Some(p) = provider {
        let memory_nodes = memory.retrieve_with_rerank(prompt, &Some(rt_env.project_id.as_str()), 5);
        let accessed_ids: Vec<&str> = memory_nodes.iter().map(|n| n.id.as_str()).collect();
        memory.reinforce_accessed(&accessed_ids);
        let memory_ctx = memory.format_memory_context(&memory_nodes, false);

        let system_prompt = context::build_system_prompt(
            rt_env, tool_registry, ox_core::context::UserIntent::General,
            Some(&config.behavior_rules), None, None,
        );
        let turn_messages = helpers::build_context_with_option(
            context_builder, &system_prompt, &memory_ctx,
            &session.messages, context_window, config.context.use_refined_context,
        );

        let effort = ox_core::context::estimate_effort(prompt, session.messages.len());
        let planning = effort == ox_core::context::EffortLevel::High;

        app.agent_running = true;
        app.status = "Generating...".to_string();
        let provider = Arc::clone(p);
        let tx = agent_tx.clone();
        let registry = Arc::clone(tool_registry);
        let ctx = Arc::clone(tool_ctx);
        let cancel_token = interrupt_ctrl.token();
        let tm = Arc::clone(trust_manager);
        let ac = Arc::clone(agent_config);
        let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
        app.ui_to_agent_tx = Some(ui_tx);
        let wf = app.workflow_engine.clone();
        tokio::spawn(async move {
            agent::run_agent_turn(
                provider, turn_messages, registry, ctx, tx, ui_rx,
                cancel_token, tm, ac, planning, wf,
            )
            .await;
        });
    }
}

/// Process a session action (New/Resume/SwitchNext).
fn process_session_action(
    app: &mut App,
    session: &mut Session,
    background_session: &mut Option<Session>,
    action: SessionAction,
    rt_env: &mut runtime::RuntimeEnvironment,
    memory: &Arc<MemoryManager>,
    sessions_root: &std::path::Path,
    compressed_ctx_store: &Arc<ox_core::context::compressed_store::CompressedContextStore>,
    compressed_cache: &mut Option<(Vec<Message>, usize)>,
    has_provider: bool,
) {
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);

    match action {
        SessionAction::New => {
            if app.agent_running {
                let new_s = Session::new(&session_dir, &rt_env.project_id)
                    .unwrap_or_else(|e| {
                        tracing::error!("Cannot create new session: {e}");
                        std::process::exit(1);
                    });
                *background_session = Some(std::mem::replace(session, new_s));
                app.ui_to_agent_tx = None;
                app.init_workflow_engine(&session.meta.id, &session.meta);
                *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
            } else {
                let _ = session_handler::handle_session_new(app, session, rt_env, memory);
                app.init_workflow_engine(&session.meta.id, &session.meta);
                *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
            }
        }
        SessionAction::Resume { filename } => {
            if app.agent_running {
                // Move current session to background, load new one
                let sessions_root = rt_env.ox_home_dir.join("sessions");
                let target = app.sessions.iter()
                    .find(|s| s.id == filename || s.display_name().contains(&filename));
                if let Some(entry) = target {
                    let session_path = std::path::PathBuf::from(&sessions_root)
                        .join(&entry.project_id).join(&entry.id);
                    let parent_dir = session_path.parent().unwrap_or(&session_dir);
                    if let Ok(Some(archived)) = Session::load_archived(parent_dir, &entry.id) {
                        *background_session = Some(std::mem::replace(session, archived));
                        app.ui_to_agent_tx = None;
                        app.init_workflow_engine(&session.meta.id, &session.meta);
                        *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                    }
                }
            } else {
                if let Err(e) = session_handler::handle_session_resume(
                    app, session, rt_env, &filename, has_provider,
                ) {
                    app.output.push_system(&format!("Failed to resume: {e}"));
                    return;
                }
                app.init_workflow_engine(&session.meta.id, &session.meta);
                *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
            }
        }
        SessionAction::SwitchNext => {
            // Find next session in sidebar
            let current_idx = app.sessions.iter().position(|s| s.is_active);
            if let Some(idx) = current_idx {
                let total = app.sessions.len();
                let next_idx = if idx + 1 < total { idx + 1 } else { idx.saturating_sub(1) };
                if next_idx != idx {
                    if let Some(entry) = app.sessions.get(next_idx) {
                        let entry_id = entry.id.clone();
                        let entry_project_id = entry.project_id.clone();
                        let sessions_root = rt_env.ox_home_dir.join("sessions");
                        let session_path = std::path::PathBuf::from(&sessions_root)
                            .join(&entry_project_id).join(&entry_id);
                        let parent_dir = session_path.parent().unwrap_or(&session_dir);

                        if app.agent_running {
                            if let Ok(Some(archived)) = Session::load_archived(parent_dir, &entry_id) {
                                *background_session = Some(std::mem::replace(session, archived));
                                app.ui_to_agent_tx = None;
                                app.init_workflow_engine(&session.meta.id, &session.meta);
                                *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                            }
                        } else {
                            if let Err(e) = session_handler::handle_session_resume(
                                app, session, rt_env, &entry_id, has_provider,
                            ) {
                                app.output.push_system(&format!("Failed to switch: {e}"));
                                return;
                            }
                            app.init_workflow_engine(&session.meta.id, &session.meta);
                            *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                        }
                    }
                }
            }
        }
        SessionAction::None => {}
    }

    // Rebuild sidebar after any session change
    session_handler::rebuild_sidebar(
        app, sessions_root, &rt_env.project_id,
        &helpers::session_display_name(session),
    );
}

/// Process an agent event from the agent task.
#[allow(clippy::too_many_arguments)]
fn process_agent_event(
    app: &mut App,
    ev: AgentToUiEvent,
    session: &mut Session,
    background_session: &mut Option<Session>,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    memory: &Arc<MemoryManager>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &mut Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    model_name: &str,
    rt_env: &mut runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    interjection_buf: &mut InterjectionBuffer,
    config: &OxConfig,
    agent_config: &Arc<AgentConfig>,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    knowledge_engine: &Arc<tokio::sync::RwLock<KnowledgeEngine>>,
    system_prompt: &str,
) {
    let target_session = background_session.as_mut().unwrap_or(session);

    match ev {
        AgentToUiEvent::TextChunk(text) => {
            agent_handler::handle_text_chunk(app, &text);
        }
        AgentToUiEvent::ToolStart { name, id: _, detail } => {
            agent_handler::handle_tool_start(app, &name, &detail);
        }
        AgentToUiEvent::ToolResult { name, output, is_error } => {
            agent_handler::handle_tool_result(app, &name, &output, is_error, target_session);
        }
        AgentToUiEvent::ToolProgress { tool_call_id, tool_name, message, progress_percent } => {
            agent_handler::handle_tool_progress(app, tool_call_id, tool_name, message, progress_percent);
        }
        AgentToUiEvent::TurnDone { new_messages, usage } => {
            let result = agent_handler::handle_turn_done(
                app, session, background_session,
                &new_messages, &usage,
                provider.is_some(), rt_env, tool_registry,
                knowledge_engine, cost_tracker, model_name,
                compressed_cache, agent_tx, tool_ctx,
                config, memory, interrupt_ctrl,
                interjection_buf, context_builder,
                context_window, agent_config, trust_manager,
                provider, system_prompt,
            );

            match result {
                HandleResult::Normal => {
                    // ── Workflow step orchestration: check if next step should auto-run ──
                    spawn_next_workflow_step_if_needed(
                        app, session, provider, agent_tx, tool_registry,
                        tool_ctx, context_builder, context_window,
                        interrupt_ctrl, agent_config, trust_manager,
                        config, rt_env, system_prompt,
                    );
                }
                HandleResult::BackgroundDone => {}
                HandleResult::InterjectionTriggered { text: _, turn_messages, .. } => {
                    // Spawn new agent turn for interjection
                    app.agent_running = true;
                    app.status = "🧠 Thinking...".to_string();
                    let p = Arc::clone(provider.as_ref().unwrap());
                    let tx = agent_tx.clone();
                    let registry = Arc::clone(tool_registry);
                    let ctx = Arc::clone(tool_ctx);
                    let cancel = interrupt_ctrl.token();
                    let tm = Arc::clone(trust_manager);
                    let ac = Arc::clone(agent_config);
                    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
                    app.ui_to_agent_tx = Some(ui_tx);
                    let wf = app.workflow_engine.clone();
                    tokio::spawn(async move {
                        agent::run_agent_turn(
                            p, turn_messages, registry, ctx, tx, ui_rx,
                            cancel, tm, ac, false, wf,
                        )
                        .await;
                    });
                    app.scroll_to_bottom();
                    app.dirty = true;
                    app.message_count = session.messages.len();
                    app.cost_summary = cost_tracker.summary_short();
                }
            }
        }
        AgentToUiEvent::Error(err) => {
            agent_handler::handle_error(app, &err, background_session);
        }
        AgentToUiEvent::Status(status) => {
            agent_handler::handle_status(app, status);
        }
        AgentToUiEvent::ToolConfirmationRequest {
            tool_call_id, tool_name, args_summary, safety_level, high_risk_warning,
        } => {
            agent_handler::handle_tool_confirmation(
                app, tool_call_id, tool_name, args_summary, safety_level, &high_risk_warning,
            );
        }
        AgentToUiEvent::ToolOutputChunk { tool_call_id: _, chunk } => {
            agent_handler::handle_tool_output_chunk(app, &chunk);
        }
        AgentToUiEvent::BudgetExceeded { total_tokens, estimated_cost } => {
            agent_handler::handle_budget_exceeded(app, total_tokens, estimated_cost);
        }
        AgentToUiEvent::IterationLimitReached { iteration } => {
            agent_handler::handle_iteration_limit(app, iteration);
        }
        AgentToUiEvent::WorkingDirChanged(new_dir) => {
            if let Some(new_ctx) = agent_handler::handle_working_dir_changed(
                app, session, rt_env, new_dir, provider.is_some(),
                config, knowledge_engine,
            ) {
                *tool_ctx = new_ctx;
            }
            app.dirty = true;
        }
        AgentToUiEvent::WorkflowCompleted { task_description, execution_summary } => {
            agent_handler::handle_workflow_completed(
                app, session, provider, rt_env, agent_tx,
                task_description, execution_summary,
            );
        }
    }
}

/// Check if workflow has more steps and auto-spawn the next one
/// with a fresh system prompt including the current step's instructions.
fn spawn_next_workflow_step_if_needed(
    app: &mut App,
    session: &Session,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    interrupt_ctrl: &InterruptController,
    agent_config: &Arc<AgentConfig>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    config: &OxConfig,
    rt_env: &runtime::RuntimeEnvironment,
    _system_prompt: &str,
) {
    let (step_prompt, step_idx, should_continue) = if let Some(ref wf) = app.workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            let prompt = engine.get_step_system_prompt();
            let idx = engine.get_current_step_index();
            let cont = engine.is_workflow_active() && !engine.is_workflow_complete();
            (prompt, idx, cont)
        } else { (None, 0, false) }
    } else { (None, 0, false) };

    if !should_continue || provider.is_none() {
        return;
    }

    // Build FRESH system prompt with current step's instructions
    let system_prompt = context::build_system_prompt_with_step(
        rt_env, tool_registry,
        ox_core::context::UserIntent::General,
        Some(&config.behavior_rules), None,
        &context::TurnContext {
            git_log: None, git_diff_stat: None, dir_structure: None,
            recent_summary: None, relevant_symbols: None,
        },
        step_prompt.as_deref(),
        step_idx,
    );

    // Minimal context: system prompt + session messages (previous step outputs)
    let turn_messages = crate::helpers::build_context_with_option(
        context_builder, &system_prompt, "",
        &session.messages, context_window, false,
    );

    app.agent_running = true;
    let p = Arc::clone(provider.as_ref().unwrap());
    let tx = agent_tx.clone();
    let registry = Arc::clone(tool_registry);
    let ctx = Arc::clone(tool_ctx);
    let cancel = interrupt_ctrl.token();
    let tm = Arc::clone(trust_manager);
    let ac = Arc::clone(agent_config);
    let wf = app.workflow_engine.clone();
    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
    app.ui_to_agent_tx = Some(ui_tx);

    tokio::spawn(async move {
        let _ = tx.send(AgentToUiEvent::Status("🌐 Calling LLM...".to_string()));
        agent::run_agent_turn(
            p, turn_messages, registry, ctx, tx, ui_rx,
            cancel, tm, ac, false, wf,
        ).await;
    });

    app.scroll_to_bottom();
    app.dirty = true;
}
