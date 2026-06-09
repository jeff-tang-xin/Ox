mod terminal;
pub mod slash_commands;
pub mod middleware;
pub mod helpers;
pub mod keyword_extraction;  // 🆕 Keyword extraction from LLM responses

use ox_core::tools::intent_classifier;
use ox_core::context::refinement;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{KeyCode, KeyModifiers};
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
use ox_core::cost::{self, CostTracker};
use ox_core::llm::{self, LlmProvider, ProviderResolveInfo};
use ox_core::memory::MemoryManager;
use ox_core::message::{Message, Session};
use ox_core::runtime;
use ox_core::safety::injection;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};
use terminal::app::{App, PendingConfirmation, PlanItem, PlanItemStatus, SessionAction, UserInput, WorkflowState};
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
    let (provider, resolve_info, provider_error) = create_provider(&config);
    if let Some(ref err) = provider_error {
        tracing::warn!("Provider init failed (will retry on /model): {}", err);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run the application
    let result = run_app(&mut terminal, &config, rt_env, provider, resolve_info, provider_error).await;

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Initialize logging to file (~/.ox/logs/ox.log) with rotation (max 10MB, keep 3 backups).
fn init_logging() -> anyhow::Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let log_dir = home.join(".ox").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file_path = log_dir.join("ox.log");

    // Rotate: if log > 10MB, shift ox.log → ox.log.1 → ox.log.2 → ox.log.3 (delete oldest)
    const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024; // 10MB
    if let Ok(meta) = std::fs::metadata(&log_file_path) {
        if meta.len() > MAX_LOG_SIZE {
            for i in (1..3).rev() {
                let old = log_dir.join(format!("ox.log.{}", i));
                let new = log_dir.join(format!("ox.log.{}", i + 1));
                if old.exists() { let _ = std::fs::rename(&old, &new); }
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

        // Forcefully set filter to capture all INFO logs from ox_core and ox_cli
        let filter = tracing_subscriber::EnvFilter::new("ox_core=info,ox_cli=info,tracing=info");
        
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .with_filter(filter);
            
        tracing_subscriber::registry()
            .with(file_layer)
            .init();
        
        // Verify logging is working
        tracing::info!("✅ Logging initialized successfully. Writing to: {:?}", log_file_path);
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

/// Create LLM provider from config, returning provider and error details.
fn create_provider(
    config: &OxConfig,
) -> (Option<Arc<dyn LlmProvider>>, Option<ProviderResolveInfo>, Option<String>) {
    match llm::create_provider_with_info(&config.models.default, &config.models) {
        Ok((p, info)) => (Some(Arc::from(p)), Some(info), None),
        Err(e) => {
            let msg = format!("{}", e);
            tracing::warn!("No LLM provider: {}. Running in echo mode.", msg);
            (None, None, Some(msg))
        }
    }
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &OxConfig,
    mut rt_env: runtime::RuntimeEnvironment,
    mut provider: Option<Arc<dyn LlmProvider>>,
    mut resolve_info: Option<ProviderResolveInfo>,
    provider_error: Option<String>,
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
    app.working_dir = rt_env.working_dir.display().to_string();
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
                // Restore plan items from session metadata
                if !s.meta.plan_json.is_empty() {
                    if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&s.meta.plan_json) {
                        for item in items {
                            if let (Some(file), Some(status)) = (item["file"].as_str(), item["status"].as_str()) {
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
                // Replay session history into output pane.
                helpers::replay_session_history(&mut app, &s.messages, &rt_env, provider.is_some());
                s
            }
            None => Session::new(&session_dir, &rt_env.project_id)?,
        }
    } else {
        Session::new(&session_dir, &rt_env.project_id)?
    };
    // 🚨 FIX: Do NOT truncate ToolResult content when loading from disk.
    // Truncation should only happen when building context for LLM, not in storage.
    // Users need to see full tool output in the UI.
    // Populate sidebar with archived sessions from ALL projects.
    {
        let sessions_root = rt_env.ox_home_dir.join("sessions");
        if sessions_root.exists() {
            // Iterate through all project directories
            if let Ok(project_dirs) = std::fs::read_dir(&sessions_root) {
                for project_entry in project_dirs.flatten() {
                    let project_path = project_entry.path();
                    if !project_path.is_dir() {
                        continue;
                    }
                    
                    // Extract project name from directory name
                    let project_id = project_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    
                    // Load sessions from this project
                    let archived = Session::list_archived(&project_path);
                    for (filename, info) in archived {
                        app.sessions.push(terminal::app::SessionEntry {
                            id: filename,
                            project_id: project_id.clone(),
                            info,
                            is_active: false,
                        });
                    }
                }
            }
        }
    }
    
    // Insert current session at the top
    app.sessions.insert(
        0,
        terminal::app::SessionEntry {
            id: "session.jsonl".to_string(),
            project_id: rt_env.project_id.clone(),
            info: helpers::session_display_name(&session),
            is_active: true,
        },
    );

    // Create tool registry (tool context will be created after memory initialization)
    let mut tool_registry = ToolRegistry::new();
    
    // Load Skills from filesystem (includes the auto-generated project-info if created)
    if let Err(e) = tool_registry.load_skills(&rt_env) {
        tracing::warn!("Failed to load skills: {}", e);
    }
    
    let tool_registry = Arc::new(tool_registry);

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

    // Load project persona if available (L3 progressive disclosure)
    let persona_content = rt_env.project_root.as_ref().and_then(|root| {
        let layer_mgr = ox_core::memory::layering::LayerManager::new(root);
        layer_mgr.load_persona_whitebox(&rt_env.project_id).ok().flatten()
            .map(|p| p.to_markdown())
    });

    // Initial system prompt (not used for agent turns, built dynamically)
    let system_prompt = context::build_system_prompt(
        &rt_env,
        &tool_registry,
        persona_content.as_deref(),
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
        CostTracker::load_or_create(&std::env::temp_dir()).unwrap_or_else(|e2| {
            tracing::error!("Failed to create cost tracker even with temp dir: {}", e2);
            // Last resort: create with current directory (will use default values)
            CostTracker::load_or_create(std::path::Path::new(".")).unwrap()
        })
    });

    // Memory system -- system-level: ~/.ox/db/memories_*.db
    let memory = MemoryManager::init(&rt_env.ox_home_dir, &rt_env.project_id, &config.memory)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to init memory system: {e}");
            let temp = std::env::temp_dir();
            MemoryManager::init(&temp, &rt_env.project_id, &config.memory).unwrap_or_else(|e2| {
                tracing::error!("Failed to create memory system even with temp dir: {}", e2);
                // This is unrecoverable - memory system is core to Ox
                tracing::error!("Cannot initialize memory system. Exiting.");
                std::process::exit(1);
            })
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

    // Initialize AST-based code indexer (with embedding config for deferred VectorStore init)
    let code_indexer = Arc::new(tokio::sync::Mutex::new(
        ox_core::symbol::CodeIndexer::new(&rt_env.working_dir, config.embedding.clone()),
    ));

    // 🚀 Start background project indexing (non-blocking)
    let indexer_clone = Arc::clone(&code_indexer);
    let working_dir_clone = rt_env.working_dir.clone();
    let memory_for_vector = Arc::clone(&memory_arc);
    let embedding_config = config.embedding.clone();
    let db_dir_for_memory = db_dir.clone();
    let (index_progress_tx, mut index_progress_rx) = mpsc::unbounded_channel::<(usize, usize, usize)>();
    let (index_done_tx, mut index_done_rx) = mpsc::unbounded_channel::<usize>();
    tokio::spawn(async move {
        tracing::info!("[INDEXER] Starting background project indexing...");

        // Initialize vector store + index project while holding the lock
        let total_symbols;
        let vs_for_embed;
        let symbols_for_embed;
        {
            let mut idx = indexer_clone.lock().await;
            idx.init_vector_store().await;
            vs_for_embed = idx.get_vector_store();
            symbols_for_embed = idx.get_symbols();
            match idx.index_project(Some(index_progress_tx)).await {
                Ok(count) => {
                    tracing::info!("[INDEXER] ✅ Indexed {} symbols from {:?}", count, working_dir_clone);
                    total_symbols = count;
                }
                Err(e) => {
                    tracing::warn!("[INDEXER] ❌ Indexing failed: {}. Will rely on auto-indexing via file_read.", e);
                    total_symbols = 0;
                }
            }
        } // Lock released — agent can use indexer now!

        // Signal indexing complete (agent can start searching NOW!)
        let _ = index_done_tx.send(total_symbols);

        // 🚀 Async embedding — NO indexer lock! Only locks vector_store independently.
        tokio::spawn(async move {
            let all_symbols: Vec<_> = {
                let lock = symbols_for_embed.read().await;
                lock.clone()
            };
            if all_symbols.is_empty() { return; }
            tracing::info!("[VECTOR_STORE] Background embedding {} symbols (no indexer lock)...", all_symbols.len());
            let mut vs_guard = vs_for_embed.lock().await;
            if let Some(ref mut vs) = *vs_guard {
                match vs.insert_symbols_batch(&all_symbols) {
                    Ok(count) => tracing::info!("[VECTOR_STORE] ✅ Embedded {} symbols in background", count),
                    Err(e) => tracing::warn!("[VECTOR_STORE] ❌ Background embedding failed: {}", e),
                }
            }
        });

        // 🧠 Initialize memory vector store for semantic memory search
        memory_for_vector.init_vector_store(&embedding_config, &db_dir_for_memory);

        // Start file system watcher for incremental updates
        if let Err(e) = ox_core::symbol::CodeIndexer::start_watcher(indexer_clone).await {
            tracing::warn!("[INDEXER] Failed to start file watcher: {}. Auto-indexing via file_read still works.", e);
        } else {
            tracing::info!("[INDEXER] 📡 File watcher started for real-time updates");
        }
    });
    
    // Set indexing state on app
    app.indexing = true;
    app.status = "⏳ Indexing project...".to_string();
    app.output.push_system("🔍 Checking project symbols... (incremental indexing)");

    let mut tool_ctx = Arc::new(ToolContext::new(
        rt_env.clone(),
        rt_env.working_dir.clone(),
        Arc::new(config.clone()),
        Arc::clone(&memory_arc),
        Arc::clone(&code_indexer),
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
            .unwrap_or_else(|_| {
                // In-memory fallback - won't persist but won't crash
                // This should virtually never fail
                match ox_core::context::compressed_store::CompressedContextStore::open_in_memory() {
                    Ok(store) => store,
                    Err(e) => {
                        tracing::error!("Even in-memory store failed: {}", e);
                        // Last resort: use a no-op implementation
                        panic!("Cannot create compressed context store: {}", e);
                    }
                }
            })
        }),
    );

    // Tick counter for spinner animation.
    let mut tick_count: u64 = 0;

    // Cached compressed context: (compressed_messages, source_msg_count).
    // source_msg_count = number of JSONL messages absorbed into the compressed context.
    let mut compressed_cache: Option<(Vec<Message>, usize)> =
        compressed_ctx_store.load(&session.meta.id).unwrap_or(None);

    // Session action signaled from slash commands.
    let _session_action: SessionAction = SessionAction::None;

    // Holds the old session when switching during agent run.
    let mut background_session: Option<Session> = None;

    // Initialize Workflow Engine in App
    app.init_workflow_engine(&session.meta.id, &session.meta);
    
    // Set code_indexer in app for slash command access
    app.code_indexer = Some(Arc::clone(&code_indexer));

    // 🧠 Project onboarding: queue if either conventions or architecture skill is missing
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
                 - Language, framework, build tool (from project_detect + config files)\n\
                 - Naming conventions (scan source files for patterns)\n\
                 - Code style (indent, quotes, line length from linter config)\n\
                 - Import ordering and grouping conventions\n\n\
                 ## File 2: .ox/skills/project-architecture.md\n\
                 - Directory structure and module layout\n\
                 - Layer boundaries (handlers → services → repositories?)\n\
                 - MUST rules (patterns that must be followed, from existing code)\n\
                 - MUST NOT rules (anti-patterns to avoid, from linter rules)\n\
                 - Error handling patterns (Result/Option/exception style)\n\
                 - Key dependencies and their roles\n\n\
                 **Process**:\n\
                 1. Run `project_detect` first to identify language/framework.\n\
                 2. Read config files (Cargo.toml, package.json, pyproject.toml, etc.).\n\
                 3. Scan source directories for naming patterns, error handling, module structure.\n\
                 4. Create BOTH files using `file_write`.\n\
                 5. Keep each file 200-400 words. Use real examples from the codebase.\n\n\
                 After creating both files, respond with a brief summary of what you found.",
                root.display()
            );
        }
    }

    loop {
        // 🧠 Trigger onboarding after indexing completes
        if needs_onboarding && !app.indexing {
            needs_onboarding = false;
            if let Some(ref provider) = provider {
                tracing::info!("[ONBOARDING] Starting project scan...");
                app.output.push_system("🔍 First time in this project. I will now scan the codebase to learn its conventions and architecture.");
                app.output.push_system("   This generates two files in .ox/skills/: project-conventions.md + project-architecture.md");
                let llm_msg = Message::user(&onboarding_prompt_text);
                let _ = session.append_message(llm_msg);
                let memory_nodes = memory_arc.retrieve_with_rerank(&onboarding_prompt_text, &Some(rt_env.project_id.as_str()), 5);
                memory_arc.reinforce_accessed(&memory_nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>());
                let memory_ctx = memory_arc.format_memory_context(&memory_nodes, false);
                let turn_messages = helpers::build_context_with_option(
                    &context_builder,
                    &system_prompt,
                    &memory_ctx,
                    &session.messages,
                    context_window,
                    config.context.use_refined_context,
                );
                app.agent_running = true;
                app.status = "Scanning project...".to_string();
                let p = Arc::clone(provider);
                let tx = agent_tx.clone();
                let reg = Arc::clone(&tool_registry);
                let ctx = Arc::clone(&tool_ctx);
                let cancel = interrupt_ctrl.token();
                let tm = Arc::clone(&trust_manager);
                let ac = Arc::clone(&agent_config);
                let (ui_tx, ui_rx) = mpsc::unbounded_channel();
                app.ui_to_agent_tx = Some(ui_tx);
                let wf = app.workflow_engine.clone();
                tokio::spawn(async move {
                    agent::run_agent_turn(p, turn_messages, reg, ctx, tx, ui_rx, cancel, tm, ac, false, wf).await;
                });
            } else {
                tracing::warn!("[ONBOARDING] Skipped — no LLM provider available");
                let err_detail = provider_error.as_ref()
                    .map(|e| format!("\n   🔍 Reason: {e}"))
                    .unwrap_or_default();
                app.output.push_system(&format!(
                    "⚠️ LLM provider not available.{}\n\
                     To fix:\n\
                     • Set env var: OX_OPENAI_API_KEY=your-key\n\
                     • Or in ~/.ox/config.toml:\n\
                       [models.providers.openai]\n\
                       api_key = \"your-key\"\n\
                       base_url = \"https://api.openai.com/v1\"  # or your compatible endpoint\n\
                     📝 Full details: ~/.ox/logs/ox.log",
                    err_detail
                ));
            }
        }

        // === IMPLICIT FEEDBACK: Detect overrides before user input ===
        // === IMPLICIT FEEDBACK: Detect overrides before user input ===
        let override_signals = app.override_detector.detect_overrides();
        
        // Use middleware to process implicit feedback
        middleware::feedback::process_implicit_feedback(&mut app, &override_signals);
        
        // Update EMA metrics periodically
        middleware::feedback::update_feedback_metrics(&mut app, &memory_arc);
        // === END IMPLICIT FEEDBACK DETECTION ===

        // ── Drain indexing progress updates ──
        if app.indexing {
            // Check for progress updates
            while let Ok((files_done, total_files, symbols)) = index_progress_rx.try_recv() {
                app.index_files_done = files_done;
                app.index_total_files = total_files;
                app.index_symbols = symbols;
                if total_files > 0 {
                    let pct = (files_done * 100) / total_files.max(1);
                    app.status = format!("⏳ Indexing {}/{} files ({} symbols, {}%)", files_done, total_files, symbols, pct);
                } else {
                    app.status = format!("⏳ Indexing... {} symbols found", symbols);
                }
                // Increment tick to animate spinner during indexing
                tick_count = tick_count.wrapping_add(1);
                app.spinner_frame = tick_count;
                app.dirty = true;
            }
            // Check if indexing is done
            if let Ok(total) = index_done_rx.try_recv() {
                app.indexing = false;
                app.index_symbols = total;
                app.status = String::new();
                app.output.push_system(&format!(
                    "✅ Indexing complete: {} symbols indexed. Ready to chat!",
                    total
                ));
                app.dirty = true;
                // Force immediate re-render to clear indexing UI and show completion
                terminal.draw(|frame| render::render(frame, &mut app, tick_count))?;
                app.dirty = false;
                app.mark_spinner_rendered();
            }
        }

        // Only re-render when needed (dirty or spinner animation changed).
        if app.needs_render() {
            terminal.draw(|frame| render::render(frame, &mut app, tick_count))?;
            app.dirty = false;
            app.mark_spinner_rendered();
        }

        // Async event loop: wait for crossterm event OR agent event.
        // Use biased to prioritize user input over agent events.
        tokio::select! {
            biased;
            ev = events.recv() => {
                match ev {
                    Some(Event::Key(first_key)) => {
                        // 🆕 Paste detection: batch rapid keystrokes into a single input
                        let mut keys = vec![first_key];
                        while let Some(ev) = events.try_recv() {
                            match ev {
                                Event::Key(k) => keys.push(k),
                                _ => {} // skip non-key events
                            }
                        }
                        if keys.len() > 3 {
                            // Likely a paste — batch all printable chars as one input
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
                        // Single key or small batch — process normally
                        for key in keys {
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
                                &compressed_cache,
                                &command_registry,
                            );
                        }
                        // Process session switch action from app.
                        match std::mem::replace(&mut app.session_action, SessionAction::None) {
                            SessionAction::New => {
                                // Archive current session (it stays in the old context).
                                let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);
                                if app.agent_running {
                                    // Move current session to background, create new one
                                    let project_id = rt_env.project_id.clone();
                                    let new_s = Session::new(&session_dir, &project_id).unwrap_or_else(|e| {
                                        tracing::error!("Failed to create new session: {}", e);
                                        tracing::error!("Cannot create new session. Exiting.");
                                        std::process::exit(1);
                                    });
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
                                        Ok(mut s) => {
                                            // 🧠 Run memory promotion pipeline before archiving
                                            if session.messages.len() >= 10 {  // Only promote meaningful sessions
                                                tracing::info!("🚀 Triggering memory promotion for session with {} messages", session.messages.len());
                                                if let Some(result) = memory_arc.run_promotion_pipeline(
                                                    &rt_env.project_id,
                                                    &rt_env.working_dir.file_name()
                                                        .map(|n| n.to_string_lossy().to_string())
                                                        .unwrap_or_else(|| "unknown".to_string()),
                                                ) {
                                                    match result {
                                                        Ok(report) => {
                                                            app.output.push_system(&format!(
                                                                "\n🧠 Memory Promotion Complete:\n{}",
                                                                report
                                                            ));
                                                        }
                                                        Err(e) => {
                                                            tracing::error!("Memory promotion failed: {}", e);
                                                        }
                                                    }
                                                }
                                            }
                                            
                                            // ✅ Archive current session before creating new one
                                            if let Err(e) = session.archive(&session_dir) {
                                                tracing::warn!("Failed to archive current session: {e}");
                                            }
                                            
                                            // ✅ Set default working directory for new session
                                            let default_wd = rt_env.working_dir.to_string_lossy().to_string();
                                            if let Err(e) = s.update_working_dir(&default_wd) {
                                                tracing::warn!("Failed to set default working dir: {}", e);
                                            }
                                            
                                            session = s;
                                            app.output.clear();
                                            app.output.push_system("New session started.");
                                            helpers::refresh_header_info(&mut app, &rt_env, provider.is_some());
                                            app.message_count = 0;

                                            // 🔄 Re-render sidebar after /new command
                                            app.sessions.clear();
                                            let sessions_root = rt_env.ox_home_dir.join("sessions");
                                            if sessions_root.exists() {
                                                if let Ok(project_dirs) = std::fs::read_dir(&sessions_root) {
                                                    for project_entry in project_dirs.flatten() {
                                                        let project_path = project_entry.path();
                                                        if !project_path.is_dir() {
                                                            continue;
                                                        }
                                                        
                                                        let project_id = project_path
                                                            .file_name()
                                                            .map(|n| n.to_string_lossy().to_string())
                                                            .unwrap_or_else(|| "unknown".to_string());
                                                        
                                                        let archived = Session::list_archived(&project_path);
                                                        for (fname, info) in archived {
                                                            app.sessions.push(terminal::app::SessionEntry {
                                                                id: fname,
                                                                project_id: project_id.clone(),
                                                                info,
                                                                is_active: false,
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                            app.sessions.insert(
                                                0,
                                                terminal::app::SessionEntry {
                                                    id: "session.jsonl".to_string(),
                                                    project_id: rt_env.project_id.clone(),
                                                    info: helpers::session_display_name(&session),
                                                    is_active: true,
                                                },
                                            );

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
                                // Find session entry by ID or display name
                                let target_entry = app.sessions.iter()
                                    .find(|s| s.id == filename || s.display_name().contains(&filename));
                                
                                if let Some(entry) = target_entry {
                                    let sessions_root = rt_env.ox_home_dir.join("sessions");
                                    let session_path = entry.full_path(&sessions_root);
                                    let parent_dir = session_path.parent().unwrap_or(&session_dir);
                                    
                                    match Session::load_archived(parent_dir, &entry.id) {
                                        Ok(Some(archived)) => {
                                            if app.agent_running {
                                                background_session = Some(std::mem::replace(&mut session, archived));
                                                app.ui_to_agent_tx = None;
                                                compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                            } else {
                                                // ✅ DO NOT archive current session - just replace it
                                                session = archived;
                                            
                                                // ✅ Restore session working directory
                                                if let Some(ref wd) = session.meta.working_dir {
                                                    if let Ok(path) = std::path::PathBuf::from(wd).canonicalize() {
                                                        if let Err(e) = std::env::set_current_dir(&path) {
                                                            tracing::warn!("Failed to restore working dir: {}", e);
                                                        } else {
                                                            rt_env.working_dir = path.clone();
                                                            // ✅ Update app.working_dir for status bar display (use absolute path)
                                                            app.working_dir = path.display().to_string();
                                                            app.output.push_system(&format!(
                                                                "Restored working directory: {}",
                                                                path.display()
                                                            ));
                                                        }
                                                    }
                                                }
                                                
                                                helpers::replay_session_history(&mut app, &session.messages, &rt_env, provider.is_some());
                                                app.output.push_system(&format!(
                                                    "Session restored: {} messages from {}",
                                                    session.messages.len(), filename
                                                ));
                                                
                                                // 🔄 Re-render sidebar after /resume command
                                                app.sessions.clear();
                                                let sessions_root = rt_env.ox_home_dir.join("sessions");
                                                if sessions_root.exists() {
                                                    if let Ok(project_dirs) = std::fs::read_dir(&sessions_root) {
                                                        for project_entry in project_dirs.flatten() {
                                                            let project_path = project_entry.path();
                                                            if !project_path.is_dir() {
                                                                continue;
                                                            }
                                                                                                        
                                                            let project_id = project_path
                                                                .file_name()
                                                                .map(|n| n.to_string_lossy().to_string())
                                                                .unwrap_or_else(|| "unknown".to_string());
                                                                                                        
                                                            let archived = Session::list_archived(&project_path);
                                                            for (fname, info) in archived {
                                                                app.sessions.push(terminal::app::SessionEntry {
                                                                    id: fname,
                                                                    project_id: project_id.clone(),
                                                                    info,
                                                                    is_active: false,
                                                                });
                                                            }
                                                        }
                                                    }
                                                }
                                                app.sessions.insert(
                                                    0,
                                                    terminal::app::SessionEntry {
                                                        id: "session.jsonl".to_string(),
                                                        project_id: parent_dir.file_name()
                                                            .map(|n| n.to_string_lossy().to_string())
                                                            .unwrap_or_else(|| rt_env.project_id.clone()),
                                                        info: helpers::session_display_name(&session),
                                                        is_active: true,
                                                    },
                                                );
                                                
                                                // Load compressed context for resumed session
                                                compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                            }
                                        }
                                        Ok(None) => {
                                            app.output.push_system(&format!("Session '{}' not found.", filename));
                                        }
                                        Err(e) => {
                                            app.output.push_system(&format!("Failed to resume: {e}"));
                                        }
                                    }
                                } else {
                                    app.output.push_system(&format!("Session '{}' not found in list.", filename));
                                }
                            }
                            SessionAction::SwitchNext => {
                                // Smart switch: find current session index and switch to next (or previous if at end)
                                let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);
                                
                                // Find the index of the current active session
                                let current_index = app.sessions.iter().position(|s| s.is_active);
                                
                                if let Some(idx) = current_index {
                                    // Determine direction: default forward, reverse if at end
                                    let total = app.sessions.len();
                                    let next_idx = if idx + 1 < total {
                                        idx + 1  // Go forward
                                    } else {
                                        idx.saturating_sub(1)  // At end, go backward
                                    };
                                    
                                    // Make sure we're not staying on the same session
                                    if next_idx != idx {
                                        // Clone needed data to avoid borrow conflicts
                                        let (entry_id, entry_project_id) = if let Some(entry) = app.sessions.get(next_idx) {
                                            (entry.id.clone(), entry.project_id.clone())
                                        } else {
                                            continue;
                                        };
                                        
                                        let sessions_root = rt_env.ox_home_dir.join("sessions");
                                        let session_path = std::path::PathBuf::from(&sessions_root)
                                            .join(&entry_project_id)
                                            .join(&entry_id);
                                        let parent_dir = session_path.parent().unwrap_or(&session_dir);
                                        
                                        // Resume the selected session
                                        if app.agent_running {
                                            match Session::load_archived(parent_dir, &entry_id) {
                                                Ok(Some(archived)) => {
                                                    background_session = Some(std::mem::replace(&mut session, archived));
                                                    app.ui_to_agent_tx = None;
                                                    compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                                }
                                                _ => {}
                                            }
                                        } else {
                                            match Session::load_archived(parent_dir, &entry_id) {
                                                Ok(Some(archived)) => {
                                                    // ✅ DO NOT archive current session - just replace it
                                                    // Archive only happens on /new command
                                                    session = archived;
                                                    
                                                    // Restore working directory
                                                    if let Some(ref wd) = session.meta.working_dir {
                                                        if let Ok(path) = std::path::PathBuf::from(wd).canonicalize() {
                                                            if let Err(e) = std::env::set_current_dir(&path) {
                                                                tracing::warn!("Failed to restore working dir: {}", e);
                                                            } else {
                                                                rt_env.working_dir = path.clone();
                                                                // ✅ Update app.working_dir for status bar display (use absolute path)
                                                                app.working_dir = path.display().to_string();
                                                                app.output.push_system(&format!(
                                                                    "Restored working directory: {}",
                                                                    path.display()
                                                                ));
                                                            }
                                                        }
                                                    }
                                                    
                                                    helpers::replay_session_history(&mut app, &session.messages, &rt_env, provider.is_some());
                                                    app.output.push_system(&format!(
                                                        "Session switched: {} messages from {}",
                                                        session.messages.len(), entry_id
                                                    ));
                                                    
                                                    // ✅ Force UI refresh after session switch
                                                    app.dirty = true;
                                                    app.scroll_to_bottom();
                                                    
                                                    // Re-render sidebar
                                                    app.sessions.clear();
                                                    let sessions_root = rt_env.ox_home_dir.join("sessions");
                                                    if sessions_root.exists() {
                                                        if let Ok(project_dirs) = std::fs::read_dir(&sessions_root) {
                                                            for project_entry in project_dirs.flatten() {
                                                                let project_path = project_entry.path();
                                                                if !project_path.is_dir() {
                                                                    continue;
                                                                }
                                                                
                                                                let project_id = project_path
                                                                    .file_name()
                                                                    .map(|n| n.to_string_lossy().to_string())
                                                                    .unwrap_or_else(|| "unknown".to_string());
                                                                
                                                                let archived = Session::list_archived(&project_path);
                                                                for (fname, info) in archived {
                                                                    app.sessions.push(terminal::app::SessionEntry {
                                                                        id: fname,
                                                                        project_id: project_id.clone(),
                                                                        info,
                                                                        is_active: false,
                                                                    });
                                                                }
                                                            }
                                                        }
                                                    }
                                                    app.sessions.insert(
                                                        0,
                                                        terminal::app::SessionEntry {
                                                            id: "session.jsonl".to_string(),
                                                            project_id: parent_dir.file_name()
                                                                .map(|n| n.to_string_lossy().to_string())
                                                                .unwrap_or_else(|| rt_env.project_id.clone()),
                                                            info: helpers::session_display_name(&session),
                                                            is_active: true,
                                                        },
                                                    );
                                                    
                                                    compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                                                }
                                                Ok(None) => {
                                                    app.output.push_system(&format!("Session '{}' not found.", entry_id));
                                                }
                                                Err(e) => {
                                                    app.output.push_system(&format!("Failed to resume: {e}"));
                                                }
                                            }
                                        }
                                    } else {
                                        app.output.push_system("Only one session available.");
                                    }
                                } else {
                                    app.output.push_system("No active session found.");
                                }
                            }
                            SessionAction::None => {}
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

                        // Agent running or indexing needs spinner animation updates.
                        // Only mark dirty if spinner frame actually changed
                        if (app.agent_running || app.indexing) && app.spinner_frame != app.last_spinner_frame {
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

                            // 🆕 Parse structured LLM output (Plan/Done blocks) + completion tracking
                            let mut plan_files: Vec<String> = Vec::new();
                            let mut plan_lines = String::new();
                            let mut done_files: Vec<String> = Vec::new();
                            let mut done_lines = String::new();
                            for msg in &new_messages {
                                if let Message::Assistant { content, .. } = msg {
                                    // Match ## Plan at start of line only (not inside code blocks)
                                    if let Some(plan_start) = content.find("\n## Plan").or_else(|| {
                                        if content.starts_with("## Plan") { Some(0) } else { None }
                                    }) {
                                        let plan_start = if content.starts_with("## Plan") { 0 } else { plan_start + 1 };
                                        let plan_text = &content[plan_start..];
                                        let plan_end = plan_text.find("\n## Done").unwrap_or(plan_text.len());
                                        for line in plan_text[..plan_end].lines().skip(1) {
                                            let t = line.trim();
                                            if t.starts_with("- File:") || t.starts_with("- **File:**") {
                                                let f = t.trim_start_matches("- File:").trim_start_matches("- **File:**").trim().trim_matches('`');
                                                plan_files.push(f.to_string());
                                            }
                                            if t.starts_with("- ") && plan_lines.len() < 200 {
                                                plan_lines.push_str(&format!("  {}\n", t));
                                            }
                                        }
                                    }
                                    // Match ## Done at start of line only
                                    if let Some(done_start) = content.find("\n## Done").or_else(|| {
                                        if content.starts_with("## Done") { Some(0) } else { None }
                                    }) {
                                        let done_start = if content.starts_with("## Done") { 0 } else { done_start + 1 };
                                        let done_text = &content[done_start..];
                                        for line in done_text.lines().skip(1).take(6) {
                                            let t = line.trim();
                                            if t.starts_with("- Created:") || t.starts_with("- Modified:") {
                                                let entry = t.trim_start_matches("- Created:").trim_start_matches("- Modified:").trim();
                                                if let Some(path) = entry.trim_matches('`').split('`').next() {
                                                    let path = path.split(" — ").next().unwrap_or(path).trim();
                                                    if !path.is_empty() { done_files.push(path.to_string()); }
                                                }
                                            }
                                            if t.starts_with("- ") && done_lines.len() < 200 {
                                                done_lines.push_str(&format!("  {}\n", t));
                                            }
                                        }
                                    }
                                }
                            }
                            // Show Plan (compact: file names only)
                            if !plan_files.is_empty() {
                                let names: Vec<_> = plan_files.iter().map(|f| {
                                    f.rsplit('/').next().unwrap_or(f)
                                }).collect();
                                app.output.push_line(OutputLine::System(format!("📋 Plan: {}", names.join(", "))));
                            }
                            // Show Done with completion status
                            // 🆕 Update plan tracking state
                            app.plan_items = plan_files.iter().map(|f| PlanItem {
                                file: f.clone(),
                                status: if done_files.contains(f) { PlanItemStatus::Done } else { PlanItemStatus::Pending },
                            }).collect();
                            for item in &mut app.plan_items {
                                if !plan_files.contains(&item.file) && item.status == PlanItemStatus::Pending {
                                    item.status = PlanItemStatus::Cancelled;
                                }
                            }
                            // Persist plan to session metadata
                            let plan_data: Vec<serde_json::Value> = app.plan_items.iter().map(|p| {
                                serde_json::json!({
                                    "file": p.file,
                                    "status": match p.status { PlanItemStatus::Done => "done", PlanItemStatus::Pending => "pending", PlanItemStatus::Cancelled => "cancelled" }
                                })
                            }).collect();
                            if let Ok(json) = serde_json::to_string(&plan_data) {
                                target_session.meta.plan_json = json;
                            }
                            // Refresh header to show plan status
                            helpers::refresh_header_info(&mut app, &rt_env, provider.is_some());

                            if !done_files.is_empty() {
                                let status = if !plan_files.is_empty() {
                                    let planned: std::collections::HashSet<_> = plan_files.iter().collect();
                                    let done: std::collections::HashSet<_> = done_files.iter().collect();
                                    let missing: Vec<_> = planned.difference(&done).collect();
                                    if missing.is_empty() { "✅" } else { "⚠️" }
                                } else { "" };
                                let names: Vec<_> = done_files.iter().map(|f| f.rsplit('/').next().unwrap_or(f)).collect();
                                app.output.push_line(OutputLine::System(format!("{status} Done: {}", names.join(", "))));
                            } else if !plan_files.is_empty() {
                                app.output.push_line(OutputLine::System("⏳ Awaiting verification...".to_string()));
                            }

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
                            let _recent_boundary = {
                                let mut user_count = 0usize;
                                let mut boundary = prev_count;
                                // 使用安全的切片方法
                                for (i, m) in target_session.messages.iter().enumerate().rev() {
                                    if i >= prev_count {
                                        continue;
                                    }
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
                            
                            // 🚨 FIX: Do NOT permanently truncate ToolResult content.
                            // Truncation should only happen in context building for LLM API calls.
                            // Users need to see full tool output in the UI for debugging and verification.
                            // The session.jsonl file should preserve complete data.
                            
                            // Save new messages to session
                            for msg in &new_messages {
                                if let Err(e) = target_session.append_message(msg.clone()) {
                                    tracing::error!("Failed to persist message: {e}");
                                }
                            }
                            cost_tracker.record(&model_name, &usage);
                            memory_arc.update_from_turn(&new_messages, &rt_env.project_id, &rt_env.project_language);

                            tracing::info!(
                                "[AGENT TURN] ✅ Turn completed successfully, {} new messages",
                                new_messages.len()
                            );

                            // 🆕 Extract keywords from LLM response for semantic learning
                            let mut keywords_extracted_count = 0;
                            for msg in &new_messages {
                                if let Message::Assistant { content, .. } = msg {
                                    // Try to extract keywords from the response
                                    if let Some(extracted) = keyword_extraction::extract_keywords_from_response(content) {
                                        keywords_extracted_count += 1;
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
                                    
                                    // 🆕 Extract intent classification from LLM response
                                    if let Some(intent_info) = intent_classifier::extract_intent_from_llm_response(content) {
                                        tracing::info!(
                                            "[INTENT] Detected: {:?} (confidence: {:.2}), tools: {:?}",
                                            intent_info.intent,
                                            intent_info.confidence,
                                            intent_info.suggested_tools
                                        );
                                        
                                        // Use intent info for memory search decision
                                        if intent_info.should_search_memory {
                                            if let Some(ref query) = intent_info.memory_query {
                                                tracing::info!(
                                                    "[MEMORY SEARCH] Triggered by intent: query='{}', scope={:?}",
                                                    query,
                                                    intent_info.memory_scope
                                                );
                                                // Note: Memory search is already handled by the system prompt instruction
                                                // This log confirms the intent was detected
                                            }
                                        }
                                    }
                                }
                            }
                            
                            // 🆕 Store refined memory summaries if enabled
                            if config.memory.store_refined_memories && !new_messages.is_empty() {
                                if let Some(summary) = refinement::generate_memory_summary(&new_messages) {
                                    tracing::info!(
                                        "[MEMORY REFINEMENT] Generated summary: {} chars, {} tools",
                                        summary.key_insights.len(),
                                        summary.tools_used.len()
                                    );
                                    
                                    // Create a memory node from the refined summary
                                    let mem_content = format!(
                                        "Topic: {}\n\nKey Insights:\n{}\n\nTools Used: {}",
                                        summary.topic,
                                        summary.key_insights,
                                        summary.tools_used.join(", ")
                                    );
                                    
                                    let mem_node = ox_core::memory::MemoryNode::new(
                                        mem_content,
                                        ox_core::memory::MemoryNodeType::BestPractice,
                                        Some(rt_env.project_id.clone()),
                                        rt_env.project_language.clone(),
                                        ox_core::memory::MemorySource::RefinedSummary,
                                    );
                                    
                                    // Store the refined memory
                                    memory_arc.store(mem_node);
                                }
                            }
                            
                            if keywords_extracted_count == 0 && !new_messages.is_empty() {
                                tracing::debug!(
                                    "[KEYWORD EXTRACTION] ⚠️ No keywords extracted from {} assistant messages",
                                    new_messages.iter().filter(|m| matches!(m, Message::Assistant { .. })).count()
                                );
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

                            if background_session.is_some() {
                                background_session = None;
                                app.output.push_system("Background session completed and saved.");
                            } else {
                                app.agent_running = false;
                                app.status = String::new();
                                app.pending_confirmation = None;
                                app.message_count = session.messages.len();
                                
                                // 🧠 Periodic memory promotion (every 50 messages)
                                if app.message_count > 0 && app.message_count % 50 == 0 {
                                    tracing::info!("🚀 Periodic memory promotion triggered at message {}", app.message_count);
                                    if let Some(result) = memory_arc.run_promotion_pipeline(
                                        &rt_env.project_id,
                                        &rt_env.working_dir.file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_else(|| "unknown".to_string()),
                                    ) {
                                        match result {
                                            Ok(report) => {
                                                app.output.push_system(&format!(
                                                    "\n🧠 Periodic Memory Promotion:\n{}",
                                                    report
                                                ));
                                            }
                                            Err(e) => {
                                                tracing::error!("Periodic memory promotion failed: {}", e);
                                            }
                                        }
                                    }
                                }
                                
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
                                        
                                        // Retrieve memories for the interjection
                                        let memory_nodes = memory_arc.retrieve(&text, &Some(rt_env.project_id.as_str()), 5);
                                        let accessed_ids: Vec<&str> = memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                        memory_arc.reinforce_accessed(&accessed_ids);
                                        let memory_ctx = memory_arc.format_memory_context(&memory_nodes, false);
                                        let turn_messages = helpers::build_context_with_option(
                                            &context_builder,
                                            &system_prompt,
                                            &memory_ctx,
                                            &session.messages,
                                            context_window,
                                            config.context.use_refined_context,
                                        );
                                        app.agent_running = true;
                                        app.status = "🧠 Thinking...".to_string();
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
                        AgentToUiEvent::WorkingDirChanged(new_dir) => {
                            let target = new_dir.display().to_string();
                            match runtime::change_directory(&mut rt_env, &target) {
                                runtime::DirectoryChangeResult::Success { new_dir, project_changed } => {
                                    app.output.push_line(OutputLine::System(format!(
                                        "Working directory: {}",
                                        new_dir.display()
                                    )));
                                    helpers::refresh_header_info(&mut app, &rt_env, provider.is_some());

                                    // ✅ Update session working directory and persist
                                    let working_dir_str = new_dir.to_string_lossy().to_string();
                                    if let Err(e) = session.update_working_dir(&working_dir_str) {
                                        tracing::warn!("Failed to update session working dir: {}", e);
                                    }

                                    // Update tool_ctx for next agent turn.
                                    tool_ctx = Arc::new(ToolContext::new(
                                        rt_env.clone(),
                                        new_dir.clone(),
                                        Arc::new(config.clone()),
                                        Arc::clone(&memory_arc),
                                        Arc::clone(&code_indexer),
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
                        AgentToUiEvent::WorkflowCompleted { task_description, execution_summary } => {
                            // Trigger auto-reflection to update Skills
                            tracing::info!(
                                "[AUTO-REFLECT] Workflow completed. Task: {}, Summary: {}",
                                task_description,
                                execution_summary
                            );
                            
                            // 🧠 Implement actual reflection logic
                            if let Some(ref llm_provider) = provider {
                                let project_root = rt_env.working_dir.clone();
                                
                                match ox_core::agent::auto_reflect::AutoReflector::new(
                                    Arc::clone(llm_provider),
                                    &project_root,
                                ) {
                                    Ok(reflector) => {
                                        app.output.push_line(OutputLine::System(
                                            "\n🤖 Auto-reflection in progress...".to_string()
                                        ));
                                        
                                        // Spawn async task for reflection
                                        let conversation_history = session.messages.clone();
                                        let tx_clone = agent_tx.clone();
                                        let task_desc = task_description.clone();
                                        let exec_summary = execution_summary.clone();
                                        
                                        tokio::spawn(async move {
                                            match reflector.reflect_on_workflow(
                                                &task_desc,
                                                &exec_summary,
                                                &conversation_history,
                                            ).await {
                                                Ok(Some(skill_id)) => {
                                                    let _ = tx_clone.send(AgentToUiEvent::Status(
                                                        format!("✅ Skill created: {}", skill_id)
                                                    ));
                                                }
                                                Ok(None) => {
                                                    tracing::debug!("[AUTO-REFLECT] No skill generated (LLM returned empty content)");
                                                }
                                                Err(e) => {
                                                    tracing::error!("[AUTO-REFLECT] Reflection failed: {}", e);
                                                    let _ = tx_clone.send(AgentToUiEvent::Error(
                                                        format!("Auto-reflection failed: {}", e)
                                                    ));
                                                }
                                            }
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!("[AUTO-REFLECT] Failed to initialize AutoReflector: {}", e);
                                        app.output.push_line(OutputLine::System(
                                            "⚠️ Auto-reflection unavailable (initialization failed)".to_string()
                                        ));
                                    }
                                }
                            } else {
                                tracing::debug!("[AUTO-REFLECT] LLM provider not available, skipping reflection");
                                app.output.push_line(OutputLine::System(
                                    "⚠️ Auto-reflection unavailable (no LLM provider)".to_string()
                                ));
                            }
                        }
                    }
                }
            }
        }

        if app.should_quit {
            // ✅ DO NOT archive on exit - keep session.jsonl for auto-restore
            // Session will be archived only when user explicitly creates a new one (/new)
            
            // Flush memory to disk
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
    _model_name: &str,
    rt_env: &mut runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    interjection_buf: &mut InterjectionBuffer,
    _resolve_info: &Option<ProviderResolveInfo>,
    config: &OxConfig,
    agent_config: &Arc<AgentConfig>,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    command_registry: &slash_commands::CommandRegistry,
) {
    // Fast path: simple printable characters go straight to input buffer
    if let KeyCode::Char(ch) = key.code {
        if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) {
            if ch != 'y' && ch != 'Y' && ch != 'n' && ch != 'N' && ch != 't' && ch != 'T' {
                app.input.insert_char(ch);
                app.dirty = true;
                return;
            }
        }
    }

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
                                slash_commands::CommandResult::LlmRequest { prompt, description } => {
                                    // Convert to user message and let main flow handle LLM call
                                    app.output.push_system(&format!("🤖 {}", description));
                                    
                                    // Create a user message with the generated prompt
                                    let llm_msg = Message::user(&prompt);
                                    if let Err(e) = session.append_message(llm_msg) {
                                        tracing::error!("Failed to persist message: {e}");
                                    }
                                    
                                    // Set flag to trigger LLM call in the same iteration
                                    // Fall through to normal text processing below
                                    // We'll handle this by setting a temporary text variable
                                    let temp_text = prompt.clone();
                                    
                                    // Continue with normal LLM flow (same as UserInput::Text)
                                    if let Some(provider) = provider {
                                        // Build system prompt
                                        let current_system_prompt = context::build_system_prompt(
                                            &rt_env,
                                            &tool_registry,
                                            None,
                                            Some(&config.behavior_rules),
                                            None,
                                        );

                                        let memory_nodes = memory.retrieve_with_rerank(&temp_text, &Some(rt_env.project_id.as_str()), 5);
                                        let accessed_ids: Vec<&str> =
                                            memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                        memory.reinforce_accessed(&accessed_ids);
                                        let memory_ctx = memory.format_memory_context(&memory_nodes, false);

                                        let effective_messages =
                                            if let Some((cached, prev_count)) = compressed_cache {
                                                let pc = *prev_count;
                                                let start_idx = pc.min(session.messages.len());
                                                // 使用安全的切片方法
                                                let new_msgs = if start_idx < session.messages.len() {
                                                    &session.messages[start_idx..]
                                                } else {
                                                    &[]
                                                };
                                                let mut combined = cached.clone();
                                                combined.extend_from_slice(new_msgs);
                                                combined
                                            } else {
                                                session.messages.clone()
                                            };

                                        let turn_messages = helpers::build_context_with_option(
                                            &context_builder,
                                            &current_system_prompt,
                                            &memory_ctx,
                                            &effective_messages,
                                            context_window,
                                            config.context.use_refined_context,
                                        );

                                        app.agent_running = true;
                                        app.status = "Generating skill...".to_string();
                                        let effort = ox_core::context::estimate_effort(
                                            &temp_text,
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

                        // Workflow approval is no longer supported in unified mode.
                        // The LLM decides autonomously whether to plan before acting.

                    }
                    UserInput::Text(text) => {
                        if app.indexing {
                            // Block message sending during indexing
                            app.output.push_system("⏳ Please wait — indexing in progress...");
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
                            // ⚡ Show status immediately — UI renders this on the next tick
                            app.status = "⏳ Preparing...".to_string();
                            app.dirty = true;

                            // 🛡️ Scan user input for prompt injection before it reaches the LLM
                            let user_text = if injection::is_suspicious(&text) {
                                let result = injection::detect(&text);
                                let categories: Vec<String> = result.matches.iter()
                                    .map(|m| format!("{:?}", m.category))
                                    .collect();
                                tracing::warn!(
                                    "🛡️ Prompt injection detected in user input: categories={:?}",
                                    categories
                                );
                                app.output.push_line(OutputLine::System(format!(
                                    "⚠️ Prompt injection detected and sanitized: {}",
                                    categories.join(", ")
                                )));
                                injection::sanitize(&text)
                            } else {
                                text.clone()
                            };

                            // Save user message (fast disk write)
                            let user_msg = Message::user(&user_text);
                            if let Err(e) = session.append_message(user_msg) {
                                tracing::error!("Failed to persist user message: {e}");
                            }

                            // Check and handle workflow feedback (fast string matching)
                            let mut should_rewind = false;
                            let mut rewind_to_step = None;

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
                                    match wf_info.step_name.as_str() {
                                        "Await Spec Confirmation" => {
                                            should_rewind = true;
                                            rewind_to_step = Some(1);
                                            app.output.push_system("📝 Detected feedback on spec. Returning to Step 2 for revision...");
                                        }
                                        "Await Task Confirmation" => {
                                            should_rewind = true;
                                            rewind_to_step = Some(3);
                                            app.output.push_system("📝 Detected feedback on task plan. Returning to Step 4 for revision...");
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if should_rewind {
                                if let Some(step_idx) = rewind_to_step {
                                    if let Some(ref mut engine_arc) = app.workflow_engine {
                                        if let Ok(mut engine) = engine_arc.try_lock() {
                                            if let Err(e) = engine.go_to_step(step_idx) {
                                                app.output.push_error(&format!("Failed to rewind workflow: {}", e));
                                            }
                                        }
                                    }
                                }
                                let feedback_msg = Message::system(&format!(
                                    "📝 User provided revision feedback:\n{}\n\nPlease revise your work based on this feedback.",
                                    user_text
                                ));
                                if let Err(e) = session.append_message(feedback_msg) {
                                    tracing::error!("Failed to persist feedback message: {e}");
                                }
                            }

                            // ── Capture all state for the background task ──
                            let provider = Arc::clone(provider);
                            let memory = memory.clone();
                            let context_builder = context_builder.clone();
                            let tool_registry = Arc::clone(&tool_registry);
                            let tool_ctx = Arc::clone(&tool_ctx);
                            let trust_manager = Arc::clone(&trust_manager);
                            let agent_config = Arc::clone(&agent_config);
                            let rt_env = rt_env.clone();
                            let session_messages = session.messages.clone();
                            let compressed_cache_data = compressed_cache.clone();
                            let behavior_rules = config.behavior_rules.clone();
                            let use_refined = config.context.use_refined_context;
                            let workflow_display = app.workflow_display.clone();
                            let workflow_engine = app.workflow_engine.clone();
                            let (ui_to_agent_tx, ui_to_agent_rx) =
                                mpsc::unbounded_channel::<UiToAgentEvent>();
                            app.ui_to_agent_tx = Some(ui_to_agent_tx);
                            let tx = agent_tx.clone();
                            let cancel_token = interrupt_ctrl.token();
                            app.agent_running = true;

                            // ⭐ Defer heavy work to a background thread so the event loop
                            // stays responsive and "Thinking..." renders immediately.
                            let code_indexer_for_ctx = app.code_indexer.clone();
                            let user_text_for_symbols = user_text.clone();
                            tokio::spawn(async move {
                                // ── AST symbol search (async, before blocking) ──
                                let _ = tx.send(AgentToUiEvent::Status("🔍 Searching symbols...".to_string()));
                                let relevant_symbols_str = if let Some(ref indexer) = code_indexer_for_ctx {
                                    let idx = indexer.lock().await;
                                    match idx.search(&user_text_for_symbols, 8).await {
                                        Ok(result) if !result.symbols.is_empty() => {
                                            let mut s = String::new();
                                            for sym in &result.symbols {
                                                s.push_str(&format!(
                                                    "- [{}] `{}` @ {}:{}-{}\n",
                                                    sym.kind, sym.name,
                                                    sym.file_path, sym.start_line, sym.end_line
                                                ));
                                            }
                                            Some(s)
                                        }
                                        _ => None,
                                    }
                                } else {
                                    None
                                };

                                // ── All heavy synchronous work runs on a blocking thread ──
                                let tr = tool_registry.clone();
                                let status_tx = tx.clone(); // For status updates from blocking thread
                                let blocking_result = tokio::task::spawn_blocking(move || {
                                    let working_dir = &rt_env.working_dir;

                                    // 1. Git context (subprocess calls)
                                    let _ = status_tx.send(AgentToUiEvent::Status("📊 Gathering git context...".to_string()));
                                    let turn_ctx = context::TurnContext {
                                        git_log: context::gather_git_context(working_dir),
                                        git_diff_stat: context::gather_diff_context(working_dir),
                                        dir_structure: None,
                                        recent_summary: None,
                                        relevant_symbols: relevant_symbols_str,
                                    };

                                    // 2. Build system prompt
                                    let mut system_prompt = context::build_system_prompt_with_context(
                                        &rt_env,
                                        &tr,
                                        None,
                                        Some(&behavior_rules),
                                        None,
                                        &turn_ctx,
                                    );

                                    // 3. Workflow step instructions (if any)
                                    if let Some(ref wf_info) = workflow_display {
                                        if let Some(ref step_prompt) = wf_info.step_prompt {
                                            system_prompt.push_str("\n\n## Current Workflow Step\n\n");
                                            let project_ox_dir = rt_env.project_ox_dir
                                                .as_ref()
                                                .map(|p| p.to_string_lossy().to_string())
                                                .unwrap_or_else(|| ".ox".to_string());
                                            let mut processed = step_prompt.replace("{project_ox_dir}", &project_ox_dir);
                                            if let Some(ref req_name) = wf_info.requirement_name {
                                                for pat in &["{REQUIREMENT_NAME}", "{YOUR_NAME}", "{IDENTIFIED_NAME}", "{requirement_name}"] {
                                                    processed = processed.replace(pat, req_name);
                                                }
                                            }
                                            system_prompt.push_str(&processed);
                                            if !wf_info.allows_code_modification {
                                                system_prompt.push_str("\n\n⚠️ IMPORTANT: You CAN use tools (file_read, file_search, etc.) but you CANNOT modify source code files (.rs, .py, .js, etc.) in this step. You can only create/modify documentation files (.md, .txt, etc.).");
                                            }
                                        }
                                    }

                                    // 4. Memory retrieval (SQLite queries)
                                    let _ = status_tx.send(AgentToUiEvent::Status("🧠 Retrieving memories...".to_string()));
                                    let project_id: Option<&str> = if rt_env.project_id.is_empty() {
                                        None
                                    } else {
                                        Some(rt_env.project_id.as_str())
                                    };
                                    let memory_nodes = memory.retrieve_with_rerank(&user_text, &project_id, 5);
                                    let accessed_ids: Vec<&str> = memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                    memory.reinforce_accessed(&accessed_ids);
                                    let memory_ctx = memory.format_memory_context(&memory_nodes, false);

                                    // 5. Build effective messages with compressed cache
                                    let effective_messages = if let Some((cached, prev_count)) = compressed_cache_data {
                                        let start_idx = prev_count.min(session_messages.len());
                                        let new_msgs = if start_idx < session_messages.len() {
                                            &session_messages[start_idx..]
                                        } else {
                                            &[]
                                        };
                                        let mut combined = cached;
                                        combined.extend_from_slice(new_msgs);
                                        combined
                                    } else {
                                        session_messages
                                    };

                                    // 6. Token-aware context building
                                    let _ = status_tx.send(AgentToUiEvent::Status("📐 Building context...".to_string()));
                                    let turn_messages = helpers::build_context_with_option(
                                        &context_builder,
                                        &system_prompt,
                                        &memory_ctx,
                                        &effective_messages,
                                        context_window,
                                        use_refined,
                                    );

                                    // 7. Effort estimate
                                    let effort = ox_core::context::estimate_effort(&user_text, effective_messages.len());
                                    let planning = effort == ox_core::context::EffortLevel::High;

                                    Ok::<_, String>((turn_messages, planning))
                                }).await;

                                // Handle spawn_blocking errors — send error to UI so user sees it
                                let (turn_messages, planning) = match blocking_result {
                                    Ok(Ok(result)) => result,
                                    Ok(Err(e)) => {
                                        tracing::error!("[BACKGROUND] Blocking task failed: {}", e);
                                        let _ = tx.send(AgentToUiEvent::Error(format!(
                                            "Preparing context failed: {}", e
                                        )));
                                        return;
                                    }
                                    Err(e) => {
                                        tracing::error!("[BACKGROUND] Blocking task panicked: {}", e);
                                        let _ = tx.send(AgentToUiEvent::Error(format!(
                                            "Background task crashed: {}", e
                                        )));
                                        return;
                                    }
                                };

                                // ── Context ready, calling LLM ──
                                let _ = tx.send(AgentToUiEvent::Status("🌐 Calling LLM...".to_string()));
                                agent::run_agent_turn(
                                    provider,
                                    turn_messages,
                                    tool_registry,
                                    tool_ctx,
                                    tx,
                                    ui_to_agent_rx,
                                    cancel_token,
                                    trust_manager,
                                    agent_config,
                                    planning,
                                    workflow_engine,
                                )
                                .await;
                            });

                            app.scroll_to_bottom();
                            app.user_scrolled = false;
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
