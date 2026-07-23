pub mod app_runtime;
pub mod event_loop;
pub mod handlers;
pub mod helpers;
pub mod keyword_extraction;
pub mod middleware;
pub mod slash_commands;
mod terminal;

use std::fs::OpenOptions;
use std::io;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::KeyCode;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use fs2::FileExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use ox_core::agent::interjection::{InterjectionBuffer, InterjectionPriority};
use ox_core::agent::interrupt::InterruptController;
use ox_core::agent::ui_event::UiToAgentEvent;
use ox_core::agent::{self, AgentToUiEvent};
use ox_core::config::{AgentConfig, OxConfig};
use ox_core::context::{self, ContextBuilder};
use ox_core::cost::CostTracker;
use ox_core::llm::{self, LlmProvider, ProviderResolveInfo};
use ox_core::message::{Message, Session};
use ox_core::runtime;
use ox_core::safety::TrustManager;
use ox_core::safety::injection;
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
    install_panic_hook();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let head = args.first().map(String::as_str);
    match head {
        None => run_tui().await,
        Some("--help" | "-h" | "help") => {
            print_help();
            Ok(())
        }
        Some(other) => {
            eprintln!("ox: unknown subcommand `{}`\n", other);
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "Usage: ox [SUBCOMMAND]\n\n\
         (no args)     Launch the interactive TUI (default).\n\
         help          Print this message.\n"
    );
}

async fn run_tui() -> anyhow::Result<()> {
    init_logging()?;

    let config = OxConfig::load(None)?;
    let rt_env = runtime::detect_runtime();

    // Single-instance guard (OS-level file lock — auto-released on any exit,
    // including panic/kill/power-off, so no stale-lock cleanup is ever needed).
    let _instance_lock = match acquire_instance_lock(&rt_env) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ox: {}", e);
            std::process::exit(1);
        }
    };

    let (provider, resolve_info, provider_error) = create_provider(&config);
    if let Some(ref err) = provider_error {
        tracing::warn!("Provider init failed (will retry on /model): {}", err);
    }

    use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_app(
        &mut terminal,
        &config,
        rt_env,
        provider,
        resolve_info,
        provider_error,
    )
    .await;

    let mut stdout = io::stdout();
    let _ = stdout.execute(DisableBracketedPaste);
    disable_raw_mode()?;
    stdout.execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ============================================================================
// Helper Functions (unchanged)
// ============================================================================

/// Acquire an exclusive per-project lock; return the held File (must live for app lifetime).
///
/// Uses `fs2::FileExt::try_lock_exclusive()` — an OS-level advisory lock
/// (Windows: `LockFileEx`, Unix: `flock`). The kernel releases the lock the
/// moment the process handle disappears, so panic / kill / power-off leave no
/// stale lock behind. The lock-file contents (pid / started_at / working_dir)
/// are informational only — used just to give a helpful error when another
/// instance is holding the lock.
fn acquire_instance_lock(rt_env: &runtime::RuntimeEnvironment) -> anyhow::Result<std::fs::File> {
    // Prefer <project_root>/.ox/ox.lock; fall back to <working_dir>/.ox/ox.lock.
    let lock_dir: PathBuf = rt_env
        .project_ox_dir
        .clone()
        .unwrap_or_else(|| rt_env.working_dir.join(".ox"));
    std::fs::create_dir_all(&lock_dir)?;
    let lock_path = lock_dir.join("ox.lock");

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;

    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            // Overwrite with fresh diagnostics (previous holder is gone).
            let _ = file.set_len(0);
            let mut f = &file;
            let _ = writeln!(
                f,
                "pid={}\nstarted_at={}\nworking_dir={}",
                std::process::id(),
                chrono::Local::now().to_rfc3339(),
                rt_env.working_dir.display(),
            );
            Ok(file)
        }
        Err(_) => {
            let info = std::fs::read_to_string(&lock_path).unwrap_or_default();
            let info_trim = info.trim();
            let detail = if info_trim.is_empty() {
                String::new()
            } else {
                format!("\n  {}", info_trim.replace('\n', "\n  "))
            };
            anyhow::bail!(
                "another ox instance is already running in this project\n  lock: {}{}\nIf you are certain no ox is running, delete the lock file and retry.",
                lock_path.display(),
                detail
            )
        }
    }
}

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
        tracing_subscriber::registry().with(file_layer).init();
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
        // Log to tracing so panics appear in the same log output as agent logs
        let msg = info.to_string();
        tracing::error!("[PANIC] {msg}");
        if let Some(location) = info.location() {
            tracing::error!("[PANIC] at {}:{}", location.file(), location.line());
        }
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_panic(info);
    }));
}

fn create_provider(
    config: &OxConfig,
) -> (
    Option<Arc<dyn LlmProvider>>,
    Option<ProviderResolveInfo>,
    Option<String>,
) {
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
        app.header_info
            .push("Type a message or /help for commands. /exit to quit.".to_string());
    } else {
        app.header_info
            .push("No API key. Set env var or config. Running in echo mode.".to_string());
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
        &rt_env,
        &tool_registry,
        ox_core::context::UserIntent::General,
        Some(&config.behavior_rules),
        None,
        None,
        config.agent.unified_tool_mode,
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

    // ── Knowledge Engine — disabled (embedding/vector retrieval removed) ──
    // KnowledgeEngine + embedding stack fully removed — tree-sitter + GitNexus only.
    let ema_metrics_path = rt_env.ox_home_dir.join("ema_metrics.json");
    if let Err(e) = app
        .ema_manager
        .load_from_file("code_accept_rate", &ema_metrics_path)
    {
        tracing::warn!("Failed to load EMA history: {}", e);
    }

    // Indexing — disabled
    app.indexing = false;
    app.status = String::new();

    // ── Git check: warn if not a git project ──
    let project_root = rt_env.effective_project_root();
    let is_git_project = project_root.join(".git").exists();
    if !is_git_project {
        app.output.push_system(
            "⚠️ 当前目录不是 Git 项目。GitNexus 需要 Git 才能索引代码图谱。\n\
             💡 请运行 `git init` 初始化项目，然后重启 Ox。",
        );
    }

    // ── GitNexus code graph (MCP) — mandatory ──
    // Probe the toolchain synchronously (cheap, just PATH lookups). GitNexus is a
    // required component: when launchable we bring it up in the background and the
    // per-turn gate blocks the first prompt until it's ready; when the toolchain
    // is missing we surface install guidance and the gate blocks agent turns until
    // it's fixed. The Arc is held for the whole session (dropping it kills the
    // child) and threaded into the tool context so `code_graph` can reach it.
    let (gitnexus, gitnexus_launchable, gitnexus_hint) = {
        let availability = ox_core::mcp::detect(&config.gitnexus);
        app.output.push_system(&availability.summary());
        let hint = availability.hint();
        if let Some(ref h) = hint {
            app.output.push_system(&format!("ℹ️ {h}"));
        }
        // Additional check: if git is missing, warn about GitNexus dependency
        if !is_git_project {
            app.output.push_system(
                "⚠️ GitNexus 需要 Git 项目才能工作。请先初始化 Git 仓库。",
            );
        }
        let svc = Arc::new(ox_core::mcp::GitNexusService::new(
            config.gitnexus.clone(),
            project_root,
        ));
        // The background index+start spawn is deferred until `agent_tx` exists
        // (below) so it can report readiness to the UI scrollback.
        (svc, availability.is_launchable() && is_git_project, hint)
    };

    // Wire GitNexus service into App for slash commands
    app.gitnexus = Some(Arc::clone(&gitnexus));

    // ── Tool context ──
    // ── Memory Store (SQLite-backed session memory) ──
    let memory_store = {
        let db_path = rt_env.effective_project_root().join(".ox").join("memory.db");
        ox_core::memory::store::MemoryStore::open(&db_path)
            .map(|s| Arc::new(s))
            .map_err(|e| tracing::warn!("[MEMORY] Failed to open memory.db: {e}"))
            .ok()
    };

    // ── Summarizer provider for memory-graph offload (optional small model) ──
    let summarizer: Option<Arc<dyn ox_core::llm::LlmProvider>> = {
        let name = config.agent.collaboration.summarizer_model.trim();
        if name.is_empty() {
            None
        } else {
            match ox_core::llm::create_provider_with_info(name, &config.models) {
                Ok((p, _)) => Some(Arc::from(p)),
                Err(e) => {
                    tracing::warn!("[MEMORY] summarizer_model `{name}` init failed: {e}");
                    None
                }
            }
        }
    };

    let mut tool_ctx = Arc::new(
        ToolContext::new(
            rt_env.clone(),
            rt_env.working_dir.clone(),
            Arc::new(config.clone()),
        )
        .with_gitnexus(Some(Arc::clone(&gitnexus)))
        .with_memory_store(memory_store)
        .with_summarizer(summarizer),
    );

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

    // ── GitNexus background bring-up (index + MCP server) ──
    // Deferred to here so it can report progress/readiness to the UI. On first
    // launch (no index yet) we eagerly build it so the very first questions
    // already benefit from the code graph (higher accuracy). Reindex still runs
    // ONLY when missing/stale; a fresh index skips straight to starting the
    // reader. The CLI writer and MCP reader never touch KuzuDB concurrently
    // because `start()` runs after `analyze()` completes.
    if gitnexus_launchable {
        let bg = Arc::clone(&gitnexus);
        let ui = agent_tx.clone();
        let auto_index = config.gitnexus.auto_index;
        tokio::spawn(async move {
            // FIX: Check if index exists and is valid first
            let index_valid = bg.index_is_valid().await;

            // FIX: If index doesn't exist or is invalid, need to run init first
            if auto_index && !index_valid {
                let _ = ui.send(AgentToUiEvent::SystemNotice(
                    "🔨 GitNexus：初始化代码图谱索引...".to_string(),
                ));

                // Run init first to create .gitnexus directory
                match bg.cli_init().await {
                    Ok(r) if r.success => {
                        tracing::info!("[GitNexus] init succeeded");
                    }
                    Ok(r) => {
                        tracing::warn!("[GitNexus] init exited {:?}: {}", r.exit_code, r.stderr.trim());
                        // Continue anyway - analyze might still work
                    }
                    Err(e) => {
                        tracing::warn!("[GitNexus] init failed: {e}");
                        // Continue anyway - analyze might still work
                    }
                }

                let _ = ui.send(AgentToUiEvent::SystemNotice(
                    "🔨 GitNexus：开始构建代码图谱，首次索引可能需要1-3分钟…".to_string(),
                ));
                // Spawn a progress ticker so the user sees the index is still running.
                let tick_ui = ui.clone();
                let ticker = tokio::spawn(async move {
                    let mut secs: u64 = 0;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                        secs += 15;
                        let msg = format!("🔗 GitNexus：仍在构建代码图谱…（已用时 {}s）", secs);
                        let _ = tick_ui.send(AgentToUiEvent::Status(msg));
                    }
                });

                let analyze_result = {
                    bg.set_building(true);
                    let r = bg.cli_analyze().await;
                    bg.set_building(false);
                    r
                };
                ticker.abort(); // stop the progress ticker

                match &analyze_result {
                    Ok(r) if r.success => {
                        // Try to extract a summary from the last non-empty stdout line.
                        let summary = r.stdout.lines()
                            .filter(|l| !l.trim().is_empty())
                            .last()
                            .map(|l| l.to_string())
                            .unwrap_or_else(|| "索引完成".to_string());
                        tracing::info!("[GitNexus] analyze complete: {summary}");
                        let _ = ui.send(AgentToUiEvent::SystemNotice(
                            format!("✅ GitNexus 索引构建完成：{summary}"),
                        ));
                    }
                    Ok(r) => {
                        tracing::warn!(
                            "[GitNexus] analyze exited {:?}: {}",
                            r.exit_code,
                            r.stderr.trim()
                        );
                        let _ = ui.send(AgentToUiEvent::SystemNotice(format!(
                            "⚠️ GitNexus 索引构建异常（exit {:?}）：{}",
                            r.exit_code,
                            r.stderr.lines().filter(|l| !l.trim().is_empty()).last()
                                .unwrap_or("(unknown error)")
                        )));
                    }
                    Err(e) => {
                        tracing::warn!("[GitNexus] analyze failed: {e}");
                        let _ = ui.send(AgentToUiEvent::SystemNotice(format!(
                            "❌ GitNexus 索引构建失败：{e}"
                        )));
                    }
                }
            }
            // Clear any lingering status line before MCP start.
            let _ = ui.send(AgentToUiEvent::Status(String::new()));

            match bg.start().await {
                Ok(_) => {
                    tracing::info!("[GitNexus] MCP server ready");
                    if bg.index_is_valid().await {
                        let _ = ui.send(AgentToUiEvent::SystemNotice(
                            "✅ GitNexus 代码图谱已就绪：find_symbol 将带出调用关系，提问会先做语义预检索。"
                                .to_string(),
                        ));
                    } else {
                        let _ = ui.send(AgentToUiEvent::SystemNotice(
                            "⚠️ GitNexus MCP server 已启动，但代码图谱索引为空或无效。请执行 /index build 构建索引。"
                                .to_string(),
                        ));
                    }
                }
                Err(e) => {
                    tracing::warn!("[GitNexus] start failed: {e}");
                    let _ = ui.send(AgentToUiEvent::SystemNotice(format!(
                        "⛔ GitNexus（必需）启动失败，提问将被阻止直到修复：{e}"
                    )));
                }
            }
        });
    } else if config.gitnexus.enabled {
        // Mandatory but the toolchain is missing (Node/npx or launcher). Record
        // the reason so the per-turn gate blocks with guidance, and surface a
        // prominent banner now. (If GitNexus is explicitly disabled in config we
        // skip this and leave it as an opt-out escape hatch.)
        let reason = gitnexus_hint.clone().unwrap_or_else(|| {
            "GitNexus 不可用：请安装 Node.js（提供 npx），或在 ~/.ox/config.toml 的 [gitnexus] command 指定可执行文件。"
                .to_string()
        });
        gitnexus.mark_unavailable(reason.clone()).await;
        app.output.push_system(&format!(
            "⛔ GitNexus 是必需组件，但当前不可用 — 修复前提问会被阻止：\n{reason}"
        ));
    }

    // ── In-process GitNexus periodic watcher (same lifecycle as ox) ──
    // Spawns a background task that periodically runs `gitnexus status` +
    // `gitnexus analyze` to keep the code graph fresh. The task is aborted when
    // the main event loop exits, so it lives exactly as long as ox itself.
    let _watcher_handle: Option<tokio::task::JoinHandle<()>> = if gitnexus_launchable {
        let svc = Arc::clone(&gitnexus);
        Some(tokio::spawn(async move {
            const INTERVAL: Duration = Duration::from_secs(300);
            loop {
                // status
                match svc.cli_status().await {
                    Ok(r) => tracing::info!(
                        "[GitNexus Watcher] status exit={:?} success={}",
                        r.exit_code, r.success
                    ),
                    Err(e) => tracing::warn!("[GitNexus Watcher] status error: {e}"),
                }
                // analyze (skip if initial index build still in progress)
                if !svc.is_building() {
                    svc.set_building(true);
                    match svc.cli_analyze().await {
                        Ok(r) => tracing::info!(
                            "[GitNexus Watcher] analyze exit={:?} success={}",
                            r.exit_code, r.success
                        ),
                        Err(e) => tracing::warn!("[GitNexus Watcher] analyze error: {e}"),
                    }
                    svc.set_building(false);
                }
                tokio::time::sleep(INTERVAL).await;
            }
        }))
    } else {
        None
    };

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

    // ── Onboarding check ──
    let mut needs_onboarding = false;
    let mut onboarding_prompt_text = String::new();
    let project_root = rt_env.effective_project_root();
    if ox_core::agent::onboarding::needs_project_onboarding(&project_root) {
        let _ = ox_core::agent::onboarding::prepare_project_for_onboarding(&project_root);
        needs_onboarding = true;
        onboarding_prompt_text =
            ox_core::agent::onboarding::build_onboarding_user_prompt(&project_root);
    }

    // ========================================================================
    // MAIN EVENT LOOP
    // ========================================================================
    loop {
        // ── Onboarding trigger ──
        if needs_onboarding {
            needs_onboarding = false;
            app.output
                .push_system("🔍 首次进入本项目 — 将生成项目规范与业务指导 Skill…");
            app.output
                .push_system("   → .ox/skills/project-conventions.md（项目规范）");
            app.output
                .push_system("   → .ox/skills/project-business-guide.md（业务指导）");
            if ox_core::agent::onboarding::is_greenfield_project(&project_root) {
                app.output.push_system(
                    "   ℹ️ 未检测到工程标记 — 将基于当前目录创建初始 Skill（任意语言/stack）",
                );
            }
            if let Some(p) = &provider {
                app.status = "正在分析项目…".to_string();
                let _ = session.append_message(Message::user(&onboarding_prompt_text));

                let pre_turn_result = handlers::pre_turn::prepare_turn(
                    config,
                    &rt_env,
                    &tool_registry,
                    &context_builder,
                    context_window,
                    &onboarding_prompt_text,
                    &session.messages,
                    &compressed_cache,
                    TurnVariant::Onboarding {
                        prompt_text: onboarding_prompt_text.clone(),
                    },
                    &None, // 不走 4 步工作流，直接探索 + 写 Skill
                    &session.meta.id,
                    &agent_tx,
                    None, // onboarding 不做语义预检索
                )
                .await;

                let turn_messages = pre_turn_result.turn_messages;
                let planning = pre_turn_result.planning;

                let turn_id = agent_handler::prepare_agent_spawn(&mut app, &mut interrupt_ctrl);
                app.agent_running = true;
                let tx = agent_tx.clone();
                let reg = Arc::clone(&tool_registry);
                let ctx = Arc::clone(&tool_ctx);
                let cancel = interrupt_ctrl.token();
                let tm = Arc::clone(&trust_manager);
                let ac = Arc::clone(&agent_config);
                let (ui_tx, ui_rx) = mpsc::unbounded_channel();
                app.ui_to_agent_tx = Some(ui_tx);
                let p_clone = Arc::clone(p);
                tokio::spawn(async move {
                    agent::run_agent_turn(
                        p_clone,
                        agent::collaboration::RoleProviders::default(),
                        turn_messages,
                        reg,
                        ctx,
                        tx,
                        ui_rx,
                        cancel,
                        tm,
                        ac,
                        planning,
                        None,
                        turn_id,
                    )
                    .await;
                });
            }
        }

        // ── Implicit feedback ──
        let override_signals = app.override_detector.detect_overrides();
        middleware::feedback::process_implicit_feedback(&mut app, &override_signals);
        middleware::feedback::update_feedback_metrics(&mut app, &ema_metrics_path);

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
                    Some(Event::Paste(data)) => {
                        app.input.insert_str(&data);
                        app.dirty = true;
                    }
                    Some(Event::Key(first_key)) => {
                        // Legacy paste fallback: batch rapid keystrokes
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
                                &provider, &agent_tx, &tool_registry,
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
                                action, &mut rt_env, &sessions_root,
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
                    Some(Event::Resize(_, _)) => {
                        app.output.invalidate_cache();
                        app.dirty = true;
                    }
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
                        &provider, &agent_tx, &tool_registry,
                        &mut tool_ctx, &context_builder, context_window,
                        &mut cost_tracker, &trust_manager, &mut model_name,
                        &mut rt_env, &mut interrupt_ctrl,
                        &mut interjection_buf, config, &agent_config,
                        &compressed_cache,
                        &system_prompt,
                    );
                }
            }
        }

        if app.should_quit {
            suspend_workflow_on_exit(&mut app, &mut session);
            break;
        }
    }

    // Abort the in-process GitNexus watcher so it dies with ox.
    if let Some(h) = _watcher_handle {
        h.abort();
    }

    Ok(())
}

// ============================================================================
// Event Processing Helpers
// ============================================================================

/// Archive interrupted workflow state before the process exits.
fn suspend_workflow_on_exit(app: &mut App, session: &mut Session) {
    if let Some(ref wf) = app.workflow_engine {
        if let Ok(mut engine) = wf.try_lock() {
            let was_active = !engine.is_workflow_complete()
                && engine.get_variable("_current_user_request").is_some();
            engine.finalize_interrupted_on_exit();
            if was_active {
                if let Some(task) = engine.get_variable("_current_user_request") {
                    if !task.trim().is_empty()
                        && !session.messages.iter().any(|m| {
                            matches!(m, Message::System { content } if ox_core::agent::user_round::is_interrupt_boundary(content))
                        })
                    {
                        let _ = session.append_message(Message::system(
                            &ox_core::agent::user_round::format_interrupt_boundary_message(&task),
                        ));
                    }
                }
            }
        }
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
        && key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);
    let is_ctrl_d = matches!(key.code, KeyCode::Char('d'))
        && key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);

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
        KeyResult::ParkMenuShortcut(ch) => {
            agent_handler::handle_park_menu_shortcut(app, ch);
            app.scroll_to_bottom();
            app.user_scrolled = false;
        }
        KeyResult::FindingsToggle(n) => {
            agent_handler::toggle_finding_in_panel(app, n);
            app.scroll_to_bottom();
            app.user_scrolled = false;
        }
        KeyResult::FindingsConfirm => {
            let wd = rt_env.effective_project_root();
            // If agent is running (business gate waiting), send confirmation
            // directly to the gate via channel — DON'T spawn a new turn.
            if app.agent_running {
                if let Some(ref tx) = app.ui_to_agent_tx {
                    let _ = tx.send(ox_core::agent::ui_event::UiToAgentEvent::ScopeConfirmed);
                    app.output.push_system("✅ 确认已发送 — 等待 agent 继续...");
                }
                // Confirmation accepted by the gate — dismiss the confirm panel so
                // it doesn't linger after `c` (the agent now resumes implementing).
                app.clear_workflow_confirmation();
                app.scroll_to_bottom();
                app.user_scrolled = false;
                app.dirty = true;
                return;
            }
            let (handled, spawn_turn) = try_apply_workflow_command(app, "/confirm", &wd);
            if handled && spawn_turn {
                spawn_agent_turn_for_text(
                    app,
                    "确认实施",
                    session,
                    provider,
                    agent_tx,
                    tool_registry,
                    tool_ctx,
                    context_builder,
                    context_window,
                    config,
                    agent_config,
                    trust_manager,
                    rt_env,
                    interrupt_ctrl,
                    compressed_cache,
                    false,
                );
            } else if handled {
                app.output.push_system("（请先 1-9 选择要修复的 finding）");
            }
            app.scroll_to_bottom();
            app.user_scrolled = false;
        }
        KeyResult::FindingsDiscuss => {
            agent_handler::enter_findings_discuss_mode(app);
            app.scroll_to_bottom();
            app.user_scrolled = false;
        }
        KeyResult::UnifiedDeliverConfirm => {
            if !agent_handler::send_unified_deliver_ack(app) {
                app.output.push_system("无法发送交付确认（agent 未运行）");
            }
        }
        KeyResult::UnifiedFinish(finished) => {
            if !agent_handler::send_unified_finish_ack(app, finished) {
                app.output
                    .push_system("无法发送 finish 确认（agent 未运行）");
            }
        }
        KeyResult::InputSubmitted(input) => {
            // Clear any stale gate (delivery/scope) when user submits new input
            app.clear_workflow_confirmation();
            match input {
                UserInput::Exit => {
                    app.output.push_system("Goodbye.");
                    app.should_quit = true;
                }
                UserInput::SlashCommand { cmd, args } => {
                    process_slash_command(
                        app,
                        &cmd,
                        &args,
                        session,
                        rt_env,
                        config,
                        cost_tracker,
                        trust_manager,
                        provider,
                        agent_tx,
                        tool_registry,
                        tool_ctx,
                        context_builder,
                        context_window,
                        interrupt_ctrl,
                        agent_config,
                        model_name,
                        command_registry,
                        compressed_cache,
                    );
                }
                UserInput::Text(text) => {
                    process_text_input(
                        app,
                        &text,
                        session,
                        background_session,
                        provider,
                        agent_tx,
                        tool_registry,
                        tool_ctx,
                        context_builder,
                        context_window,
                        config,
                        agent_config,
                        trust_manager,
                        rt_env,
                        interrupt_ctrl,
                        interjection_buf,
                        compressed_cache,
                        model_name,
                        cost_tracker,
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
    compressed_cache: &Option<(Vec<Message>, usize)>,
) {
    let workflow_line = if args.is_empty() {
        format!("/{cmd}")
    } else {
        format!("/{cmd} {args}")
    };
    let (wf_handled, spawn_turn) =
        try_apply_workflow_command(app, &workflow_line, &rt_env.effective_project_root());
    if wf_handled {
        if spawn_turn {
            spawn_agent_turn_for_text(
                app,
                "确认实施",
                session,
                provider,
                agent_tx,
                tool_registry,
                tool_ctx,
                context_builder,
                context_window,
                config,
                agent_config,
                trust_manager,
                rt_env,
                interrupt_ctrl,
                compressed_cache,
                false,
            );
        }
        app.dirty = true;
        return;
    }

    if let Some(meta) = command_registry.get_command(cmd) {
        let result = (meta.handler)(
            app,
            args,
            session,
            rt_env,
            config,
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
            slash_commands::CommandResult::LlmRequest {
                prompt,
                description,
                skip_workflow,
            } => {
                spawn_agent_turn_from_slash(
                    app,
                    &prompt,
                    &description,
                    skip_workflow,
                    session,
                    provider,
                    agent_tx,
                    tool_registry,
                    tool_ctx,
                    context_builder,
                    context_window,
                    interrupt_ctrl,
                    agent_config,
                    trust_manager,
                    rt_env,
                    config,
                    compressed_cache,
                );
            }
            _ => {}
        }
    } else {
        app.output.push_system(&format!(
            "Unknown command: /{}. Type /help for available commands.",
            cmd
        ));
    }
    app.dirty = true;
}

/// Try workflow commands (`/fix`, `/confirm`, `/findings`, …). Returns `(handled, spawn_turn)`.
fn try_apply_workflow_command(
    app: &mut App,
    line: &str,
    working_dir: &std::path::Path,
) -> (bool, bool) {
    agent_handler::apply_workflow_slash(app, line, working_dir)
}

/// Spawn an agent turn for user text (optionally skip `begin_user_round` after scope confirm).
#[allow(clippy::too_many_arguments)]
fn spawn_agent_turn_for_text(
    app: &mut App,
    text: &str,
    session: &mut Session,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    config: &OxConfig,
    agent_config: &Arc<AgentConfig>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    rt_env: &runtime::RuntimeEnvironment,
    interrupt_ctrl: &mut InterruptController,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    begin_round: bool,
) {
    if provider.is_none() {
        app.output
            .push_line(OutputLine::System(format!("[echo] {}", text.trim())));
        return;
    }

    let turn_id = agent_handler::prepare_agent_spawn(app, interrupt_ctrl);

    app.status = "⏳ Preparing...".to_string();
    app.dirty = true;
    app.workflow_interrupted = false;

    if begin_round {
        if let Some(wf) = app.workflow_engine.clone() {
            if let Ok(mut engine) = wf.try_lock() {
                if !engine.accepts_user_round_input(text) {
                    app.output.push_system(
                        "",
                    );
                    app.status.clear();
                    app.dirty = true;
                    return;
                }
                if engine.workflow_preserves_on_user_input(text) {
                    let step_idx = engine.get_current_step_index();
                    app.output.push_system(&format!(
                        "💬 workflow 介入 — 继续当前任务 (步骤 {})，不会重置会话",
                        step_idx + 1,
                    ));
                }
                let new_round = engine.begin_user_round(text);
                if new_round {
                    let task = engine
                        .get_variable("_current_user_request")
                        .unwrap_or_else(|| text.to_string());
                    let _ = session.append_message(Message::system(
                        ox_core::agent::user_round::format_round_boundary_message(&task),
                    ));
                }
            }
        }
    } else if let Some(wf) = app.workflow_engine.clone() {
        if let Ok(engine) = wf.try_lock() {
            ox_core::agent::user_round::set_turn_user_input(&engine, text);
            if ox_core::agent::phase::get(&engine)
                == ox_core::agent::phase::SingleFlowPhase::Implement
            {
                let anchor = ox_core::agent::findings::load_or_migrate(&engine)
                    .map(|s| s.scope_confirm_summary())
                    .unwrap_or_else(|| text.to_string());
                let _ = session.append_message(Message::system(
                    ox_core::agent::user_round::format_round_boundary_message(&format!(
                        "实施修复 — {anchor}"
                    )),
                ));
            }
        }
    }

    let _ = session.append_message(Message::user(text));

    let rt_env_clone = rt_env.clone();
    let config_clone = config.clone();
    let tool_registry_clone = Arc::clone(tool_registry);
    let context_builder_clone = context_builder.clone();
    let session_messages = session.messages.clone();
    let compressed_cache_data = compressed_cache.clone();
    let workflow_engine_clone = app.workflow_engine.clone();
    let session_id = session.meta.id.clone();
    let tx = agent_tx.clone();
    let provider_clone = Arc::clone(provider.as_ref().unwrap());
    let tool_ctx_clone = Arc::clone(tool_ctx);
    let cancel_token = interrupt_ctrl.token();
    let tm = Arc::clone(trust_manager);
    let ac = Arc::clone(agent_config);
    let text = text.to_string();

    let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
    app.ui_to_agent_tx = Some(ui_to_agent_tx);
    app.agent_running = true;

    tokio::spawn(async move {
        let status_tx = tx.clone();
        // Mandatory GitNexus gate: block until the code graph is ready (or abort
        // with guidance if it's unavailable). Instant once ready.
        if let Some(svc) = tool_ctx_clone.gitnexus.clone() {
            if let Err(msg) = await_gitnexus_ready(&svc, &status_tx).await {
                let _ = status_tx.send(AgentToUiEvent::Error(msg));
                return;
            }
            // Reindex between turns (not during code_graph calls) so the
            // index is fresh without slowing down mid-turn queries.
            svc.reindex_if_dirty().await;
        }

        // Memory archival is now handled by the unified budget-offload path in
        // `run_agent_turn` (memory_offload::offload_if_over_budget): the ReAct
        // log is clustered into memory-graph nodes when prompt tokens cross 80%
        // of the window. The former `_prev_turn_memory` → `save_raw` consolidation
        // (which referenced a nonexistent method) was retired here.

        // ── Periodic L1→L2 memory-graph consolidation (time-gated) ──
        // Sessions are permanent, so consolidation is periodic (not on session
        // end): lazily checked here at turn start. Returns L3 (Skill) candidates
        // which we route through the existing user-confirmed SkillDraftReady flow.
        if let Some(ref store) = tool_ctx_clone.memory_store {
            let session_id = workflow_engine_clone
                .as_ref()
                .and_then(|wf| wf.try_lock().ok())
                .map(|e| e.session_id())
                .unwrap_or_else(|| "default".to_string());
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let interval_hours: u32 = 24;
            let default_provider = provider_clone.clone();
            let summarizer = tool_ctx_clone.summarizer.clone();
            let status_for_consolidate = status_tx.clone();
            let candidates = ox_core::memory::memory_offload::consolidate_if_due(
                store,
                summarizer,
                &default_provider,
                &session_id,
                interval_hours,
                now_unix,
                |s| {
                    let _ = status_for_consolidate.send(AgentToUiEvent::Status(s));
                },
            )
            .await;
            // Route L3 candidates to the user-confirmed Skill draft flow.
            for c in candidates {
                let _ = store.mark_promoted_l3(c.graph_id);
                let desc: String = c.summary.chars().take(80).collect();
                let _ = status_tx.send(AgentToUiEvent::SkillDraftReady {
                    skill_id: format!("memory-l3-{}", c.graph_id),
                    content: c.skill_draft,
                    description: desc,
                });
            }
        }

        let result = handlers::pre_turn::prepare_turn(
            &config_clone,
            &rt_env_clone,
            &tool_registry_clone,
            &context_builder_clone,
            context_window,
            &text,
            &session_messages,
            &compressed_cache_data,
            TurnVariant::Normal,
            &workflow_engine_clone,
            &session_id,
            &status_tx,
            tool_ctx_clone.gitnexus.clone(),
        )
        .await;

        let _ = status_tx.send(AgentToUiEvent::Status("🌐 Calling LLM...".to_string()));
        agent::run_agent_turn(
            provider_clone,
            agent::collaboration::RoleProviders::default(),
            result.turn_messages,
            tool_registry_clone,
            tool_ctx_clone,
            tx,
            ui_to_agent_rx,
            cancel_token,
            tm,
            ac,
            result.planning,
            workflow_engine_clone,
            turn_id,
        )
        .await;
    });
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
    // Indexing is disabled globally; keep the guard as a no-op cheap check.
    let _ = config;
    if app.indexing {
        app.output
            .push_system("⏳ Please wait — indexing in progress...");
        app.dirty = true;
        return;
    }

    let trimmed = text.trim();
    if trimmed.starts_with('/') {
        // If agent is running (gate waiting), send confirmation directly.
        if app.agent_running && (trimmed == "/confirm" || trimmed == "/fix") {
            if let Some(ref tx) = app.ui_to_agent_tx {
                let _ = tx.send(ox_core::agent::ui_event::UiToAgentEvent::ScopeConfirmed);
            }
            app.scroll_to_bottom();
            app.user_scrolled = false;
            app.dirty = true;
            return;
        }
        let (wf_handled, spawn_turn) =
            try_apply_workflow_command(app, trimmed, &rt_env.effective_project_root());
        if wf_handled {
            if spawn_turn {
                spawn_agent_turn_for_text(
                    app,
                    "确认实施",
                    session,
                    provider,
                    agent_tx,
                    tool_registry,
                    tool_ctx,
                    context_builder,
                    context_window,
                    config,
                    agent_config,
                    trust_manager,
                    rt_env,
                    interrupt_ctrl,
                    compressed_cache,
                    false,
                );
            }
            app.scroll_to_bottom();
            app.user_scrolled = false;
            app.dirty = true;
            return;
        }
    }

    // ── Skill draft confirmation (only explicit ok/save/dismiss — never steal normal chat) ──
    if let Some(draft) = app.pending_skill_draft.take() {
        let t = text.trim().to_lowercase();
        let t = t.strip_prefix('/').unwrap_or(&t);
        let save = matches!(t, "ok" | "y" | "yes" | "保存" | "确认" | "好" | "save");
        let dismiss = matches!(
            t,
            "n" | "no" | "skip" | "取消" | "放弃" | "discard" | "忽略"
        );

        if save {
            let root = rt_env.effective_project_root();
            match ox_core::agent::auto_reflect::AutoReflector::save_content_to_project(
                &root,
                &draft.content,
            ) {
                Ok(id) => {
                    app.output.push_system(&format!("✅ Skill 已保存: {id}"));
                    let _ =
                        ox_core::agent::skill_reflect_buffer::SkillReflectBuffer::clear_disk(&root);
                    app.status.clear();
                }
                Err(e) => app.output.push_error(&format!("保存 Skill 失败: {e}")),
            }
            app.dirty = true;
            return;
        }
        if dismiss {
            let root = rt_env.effective_project_root();
            let _ = ox_core::agent::skill_reflect_buffer::SkillReflectBuffer::clear_disk(&root);
            app.output.push_system("❌ Skill 聚合草稿已丢弃。");
            app.status.clear();
            app.dirty = true;
            return;
        }
        // User started a new task — drop the suggestion and continue with their message.
        app.output
            .push_system("ℹ️ Skill 建议已忽略，继续处理你的输入。");
        app.status.clear();
        app.dirty = true;
        // fall through — do not return
    }

    if app.agent_running {
        let parked_resume = app
            .workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .map(|e| e.is_workflow_parked())
            .unwrap_or(false);

        if !parked_resume {
            let blocked = app
                .workflow_engine
                .as_ref()
                .and_then(|wf| wf.try_lock().ok())
                .map(|e| !e.allows_midflight_interjection())
                .unwrap_or(false);
            if blocked {
                app.output.push_system(
                    "",
                );
                app.scroll_to_bottom();
                app.dirty = true;
                return;
            }
        }

        // Interjection during agent execution — live channel only (no local buffer duplicate)
        let priority = if text.starts_with('!') {
            InterjectionPriority::Urgent
        } else {
            InterjectionPriority::Normal
        };
        let content = text.trim_start_matches('!').to_string();
        let delivered = if let Some(tx) = &app.ui_to_agent_tx {
            tx.send(UiToAgentEvent::Interjection(content.clone()))
                .is_ok()
        } else {
            false
        };
        if !delivered {
            interjection_buf.push(content.clone(), priority);
        }
        // Clear stale gate confirmation state — user is discussing, not confirming.
        app.workflow_awaiting_confirmation = None;
        let step_hint = if parked_resume {
            "park-resume".to_string()
        } else if let Some(name) = app
            .workflow_engine
            .as_ref()
            .and_then(|wf| wf.try_lock().ok())
            .filter(|e| e.workflow_preserves_on_user_input(&content))
            .and_then(|e| e.current_step().map(|s| s.name.clone()))
        {
            format!("workflow·{name}")
        } else {
            "queued".to_string()
        };
        let prefix = if priority == InterjectionPriority::Urgent {
            "(urgent!)"
        } else {
            "(介入)"
        };
        app.output.push_line(OutputLine::System(format!(
            "{} [{step_hint}] {}",
            prefix,
            content.trim()
        )));
        app.scroll_to_bottom();
        app.user_scrolled = false;
        app.dirty = true;
        return;
    }

    if provider.is_none() {
        app.output
            .push_line(OutputLine::System(format!("[echo] {}", text.trim())));
        return;
    }

    // Show status immediately
    app.status = "⏳ Preparing...".to_string();
    app.dirty = true;

    // Reset interrupt flag on new user input
    app.workflow_interrupted = false;

    // Injection scan
    let text = if injection::is_suspicious(text) {
        let result = injection::detect(text);
        let categories: Vec<String> = result
            .matches
            .iter()
            .map(|m| format!("{:?}", m.category))
            .collect();
        tracing::warn!("🛡️ Prompt injection detected: categories={:?}", categories);
        app.output.push_line(OutputLine::System(format!(
            "⚠️ Prompt injection detected and sanitized: {}",
            categories.join(", ")
        )));
        injection::sanitize(text)
    } else {
        text.to_string()
    };

    // New user round: archive previous task, or append workflow guidance mid-flight
    if let Some(wf) = app.workflow_engine.clone() {
        if let Ok(mut engine) = wf.try_lock() {
            if !engine.accepts_user_round_input(&text) {
                app.output.push_system(
                    "",
                );
                app.dirty = true;
                return;
            }
            if engine.workflow_preserves_on_user_input(&text) {
                let step_idx = engine.get_current_step_index();
                app.output.push_system(&format!(
                    "💬 workflow 介入 — 继续当前任务 (步骤 {})，不会重置会话",
                    step_idx + 1,
                ));
            }
            let new_round = engine.begin_user_round(&text);
            if new_round {
                let task = engine
                    .get_variable("_current_user_request")
                    .unwrap_or_else(|| text.clone());
                let _ = session.append_message(Message::system(
                    ox_core::agent::user_round::format_round_boundary_message(&task),
                ));
            }
            let (mode, banner) = ox_core::agent::phase::workspace_mode_event(&engine);
            if ox_core::agent::phase::get(&engine)
                == ox_core::agent::phase::SingleFlowPhase::Implement
            {
                app.clear_workflow_confirmation();
            }
            let _ = agent_tx.send(AgentToUiEvent::WorkspaceModeChanged { mode, banner });
        }
    }

    let _ = session.append_message(Message::user(&text));

    // Workflow feedback detection
    detect_workflow_feedback(app, session, &text);

    // Spawn agent turn with pre-turn pipeline
    let rt_env_clone = rt_env.clone();
    let config_clone = config.clone();
    let tool_registry_clone = Arc::clone(tool_registry);
    let context_builder_clone = context_builder.clone();
    let session_messages = session.messages.clone();
    let compressed_cache_data = compressed_cache.clone();
    let workflow_engine_clone = app.workflow_engine.clone();
    let session_id = session.meta.id.clone();
    let tx = agent_tx.clone();
    let provider_clone = Arc::clone(provider.as_ref().unwrap());
    let tool_ctx_clone = Arc::clone(tool_ctx);
    let cancel_token = interrupt_ctrl.token();
    let tm = Arc::clone(trust_manager);
    let ac = Arc::clone(agent_config);

    let turn_id = agent_handler::prepare_agent_spawn(app, interrupt_ctrl);

    let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
    app.ui_to_agent_tx = Some(ui_to_agent_tx);
    app.agent_running = true;

    tokio::spawn(async move {
        let status_tx = tx.clone();
        // Mandatory GitNexus gate (see Normal-turn handler above).
        if let Some(svc) = tool_ctx_clone.gitnexus.clone() {
            if let Err(msg) = await_gitnexus_ready(&svc, &status_tx).await {
                let _ = status_tx.send(AgentToUiEvent::Error(msg));
                return;
            }
        }
        let result = handlers::pre_turn::prepare_turn(
            &config_clone,
            &rt_env_clone,
            &tool_registry_clone,
            &context_builder_clone,
            context_window,
            &text,
            &session_messages,
            &compressed_cache_data,
            TurnVariant::Normal,
            &workflow_engine_clone,
            &session_id,
            &status_tx,
            tool_ctx_clone.gitnexus.clone(),
        )
        .await;

        let _ = status_tx.send(AgentToUiEvent::Status("🌐 Calling LLM...".to_string()));
        agent::run_agent_turn(
            provider_clone,
            agent::collaboration::RoleProviders::default(),
            result.turn_messages,
            tool_registry_clone,
            tool_ctx_clone,
            tx,
            ui_to_agent_rx,
            cancel_token,
            tm,
            ac,
            result.planning,
            workflow_engine_clone,
            turn_id,
        )
        .await;
    });

    app.scroll_to_bottom();
    app.user_scrolled = false;
}

/// Detect workflow feedback in user text and trigger rewind if needed.
fn detect_workflow_feedback(app: &mut App, session: &mut Session, text: &str) {
    if let Some(ref wf_info) = app.workflow_display {
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
                    text
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
    skip_workflow: bool,
    session: &mut Session,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    context_builder: &ContextBuilder,
    context_window: u32,
    interrupt_ctrl: &mut InterruptController,
    agent_config: &Arc<AgentConfig>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    rt_env: &runtime::RuntimeEnvironment,
    config: &OxConfig,
    compressed_cache: &Option<(Vec<Message>, usize)>,
) {
    let _ = config;
    if app.indexing {
        app.output
            .push_system("⏳ Please wait — indexing in progress...");
        app.dirty = true;
        return;
    }

    app.output.push_system(&format!("🤖 {}", description));
    let _ = session.append_message(Message::user(prompt));

    if provider.is_none() {
        return;
    }

    let turn_id = agent_handler::prepare_agent_spawn(app, interrupt_ctrl);
    app.status = "Generating...".to_string();

    let provider = Arc::clone(provider.as_ref().unwrap());
    let tx = agent_tx.clone();
    let registry = Arc::clone(tool_registry);
    let ctx = Arc::clone(tool_ctx);
    let context_builder = context_builder.clone();
    let cancel_token = interrupt_ctrl.token();
    let tm = Arc::clone(trust_manager);
    let ac = Arc::clone(agent_config);
    let wf = if skip_workflow {
        None
    } else {
        app.workflow_engine.clone()
    };
    let config = config.clone();
    let rt_env = rt_env.clone();
    let session_messages = session.messages.clone();
    let session_id = session.meta.id.clone();
    let compressed_cache = compressed_cache.clone();
    let prompt = prompt.to_string();
    let description = description.to_string();
    let turn_variant = if skip_workflow {
        TurnVariant::Onboarding {
            prompt_text: prompt.clone(),
        }
    } else {
        TurnVariant::SlashCommand {
            prompt: prompt.clone(),
            description,
        }
    };

    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
    app.ui_to_agent_tx = Some(ui_tx);

    tokio::spawn(async move {
        let status_tx = tx.clone();
        let result = handlers::pre_turn::prepare_turn(
            &config,
            &rt_env,
            &registry,
            &context_builder,
            context_window,
            &prompt,
            &session_messages,
            &compressed_cache,
            turn_variant,
            &wf,
            &session_id,
            &status_tx,
            None, // slash/onboarding 不做语义预检索
        )
        .await;

        let _ = status_tx.send(AgentToUiEvent::Status("🌐 Calling LLM...".to_string()));
        agent::run_agent_turn(
            provider,
            agent::collaboration::RoleProviders::default(),
            result.turn_messages,
            registry,
            ctx,
            tx,
            ui_rx,
            cancel_token,
            tm,
            ac,
            result.planning,
            wf,
            turn_id,
        )
        .await;
    });
}

/// Process a session action (New/Resume/SwitchNext).
fn process_session_action(
    app: &mut App,
    session: &mut Session,
    background_session: &mut Option<Session>,
    action: SessionAction,
    rt_env: &mut runtime::RuntimeEnvironment,
    sessions_root: &std::path::Path,
    compressed_ctx_store: &Arc<ox_core::context::compressed_store::CompressedContextStore>,
    compressed_cache: &mut Option<(Vec<Message>, usize)>,
    has_provider: bool,
) {
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);

    match action {
        SessionAction::New => {
            if app.agent_running {
                let new_s = Session::new(&session_dir, &rt_env.project_id).unwrap_or_else(|e| {
                    tracing::error!("Cannot create new session: {e}");
                    std::process::exit(1);
                });
                *background_session = Some(std::mem::replace(session, new_s));
                app.ui_to_agent_tx = None;
                app.init_workflow_engine(&session.meta.id, &session.meta);
                *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
            } else {
                let _ =
                    session_handler::handle_session_new(app, session, rt_env);
                app.init_workflow_engine(&session.meta.id, &session.meta);
                *compressed_cache = compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
            }
        }
        SessionAction::Resume { filename } => {
            if app.agent_running {
                // Move current session to background, load new one
                let sessions_root = rt_env.ox_home_dir.join("sessions");
                let target = app
                    .sessions
                    .iter()
                    .find(|s| s.id == filename || s.display_name().contains(&filename));
                if let Some(entry) = target {
                    let session_path = std::path::PathBuf::from(&sessions_root)
                        .join(&entry.project_id)
                        .join(&entry.id);
                    let parent_dir = session_path.parent().unwrap_or(&session_dir);
                    if let Ok(Some(archived)) = Session::load_archived(parent_dir, &entry.id) {
                        *background_session = Some(std::mem::replace(session, archived));
                        app.ui_to_agent_tx = None;
                        app.init_workflow_engine(&session.meta.id, &session.meta);
                        *compressed_cache =
                            compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                    }
                }
            } else {
                if let Err(e) = session_handler::handle_session_resume(
                    app,
                    session,
                    rt_env,
                    &filename,
                    has_provider,
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
                let next_idx = if idx + 1 < total {
                    idx + 1
                } else {
                    idx.saturating_sub(1)
                };
                if next_idx != idx {
                    if let Some(entry) = app.sessions.get(next_idx) {
                        let entry_id = entry.id.clone();
                        let entry_project_id = entry.project_id.clone();
                        let sessions_root = rt_env.ox_home_dir.join("sessions");
                        let session_path = std::path::PathBuf::from(&sessions_root)
                            .join(&entry_project_id)
                            .join(&entry_id);
                        let parent_dir = session_path.parent().unwrap_or(&session_dir);

                        if app.agent_running {
                            if let Ok(Some(archived)) =
                                Session::load_archived(parent_dir, &entry_id)
                            {
                                *background_session = Some(std::mem::replace(session, archived));
                                app.ui_to_agent_tx = None;
                                app.init_workflow_engine(&session.meta.id, &session.meta);
                                *compressed_cache =
                                    compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                            }
                        } else {
                            if let Err(e) = session_handler::handle_session_resume(
                                app,
                                session,
                                rt_env,
                                &entry_id,
                                has_provider,
                            ) {
                                app.output.push_system(&format!("Failed to switch: {e}"));
                                return;
                            }
                            app.init_workflow_engine(&session.meta.id, &session.meta);
                            *compressed_cache =
                                compressed_ctx_store.load(&session.meta.id).unwrap_or(None);
                        }
                    }
                }
            }
        }
        SessionAction::None => {}
    }

    // Rebuild sidebar after any session change
    session_handler::rebuild_sidebar(
        app,
        sessions_root,
        &rt_env.project_id,
        &helpers::session_display_name(session),
    );
}

/// Process an agent event from the agent task.
#[allow(clippy::too_many_arguments)]
/// Mandatory GitNexus gate (run at the start of a Normal turn).
///
/// - Returns `Ok(())` immediately once the code graph is ready.
/// - Waits with a 60s timeout (with periodic status updates) while it is still coming
///   up — including the first-run index build.
/// - Returns `Err(guidance)` when GitNexus is unavailable or failed after timeout,
///   so the caller aborts the turn and surfaces the message (input is re-enabled via
///   the resulting `Error` event).
/// - Automatically attempts restart if the service appears hung.
async fn await_gitnexus_ready(
    svc: &ox_core::mcp::GitNexusService,
    status_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> Result<(), String> {
    use ox_core::mcp::GitNexusStatus;

    const MAX_WAIT_SECS: u64 = 60;  // Max wait time before giving up
    const CHECK_INTERVAL_MS: u64 = 500;
    const RESTART_THRESHOLD_SECS: u64 = 30;  // Attempt restart after 30s

    let start_time = std::time::Instant::now();
    let mut announced = false;
    let mut restart_attempted = false;

    loop {
        // Check timeout
        let elapsed = start_time.elapsed().as_secs();
        if elapsed >= MAX_WAIT_SECS {
            return Err(format!(
                "⛔ GitNexus 等待超时（{}秒），请检查 GitNexus 状态后重试。\n\
                 提示：可运行 `gitnexus status` 查看状态，或 `gitnexus analyze` 重新构建索引。",
                MAX_WAIT_SECS
            ));
        }

        if svc.is_ready().await {
            return Ok(());
        }

        match svc.status().await {
            GitNexusStatus::Failed(reason) => {
                // Don't wait on permanent failure
                return Err(format!(
                    "⛔ GitNexus（必需）不可用，已阻止本次提问：\n{reason}\n修复后请重启 Ox 再试。"
                ));
            }
            // Explicit opt-out in config — let the turn proceed without the graph.
            GitNexusStatus::Disabled => return Ok(()),
            // NotStarted / Starting (incl. first-run index build): keep waiting.
            // Try auto-restart if we've been waiting too long
            _ => {
                // Attempt restart if stuck for too long
                if elapsed >= RESTART_THRESHOLD_SECS && !restart_attempted {
                    restart_attempted = true;
                    let _ = status_tx.send(AgentToUiEvent::Status(
                        "⚠️ GitNexus 响应缓慢，尝试自动重启...".to_string(),
                    ));
                    // Try to restart the service
                    if let Err(e) = svc.start().await {
                        tracing::warn!("[GitNexus] Auto-restart failed: {}", e);
                    } else {
                        let _ = status_tx.send(AgentToUiEvent::Status(
                            "🔄 GitNexus 已重启，等待重新连接...".to_string(),
                        ));
                    }
                }

                if !announced || elapsed % 10 == 0 {
                    let _ = status_tx.send(AgentToUiEvent::Status(format!(
                        "⏳ 等待 GitNexus 代码图谱就绪…（{}秒）",
                        elapsed
                    )));
                    announced = true;
                }

                // Track consecutive failures for potential recovery
                tokio::time::sleep(std::time::Duration::from_millis(CHECK_INTERVAL_MS)).await;
            }
        }
    }
}

fn process_agent_event(
    app: &mut App,
    ev: AgentToUiEvent,
    session: &mut Session,
    background_session: &mut Option<Session>,
    provider: &Option<Arc<dyn LlmProvider>>,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
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
    system_prompt: &str,
) {
    let target_session = background_session.as_mut().unwrap_or(session);

    match ev {
        AgentToUiEvent::TextChunk(text) => {
            agent_handler::handle_text_chunk(app, &text);
        }
        AgentToUiEvent::ToolStart {
            name,
            id: _,
            detail,
        } => {
            agent_handler::handle_tool_start(app, &name, &detail);
        }
        AgentToUiEvent::ToolResult {
            name,
            output,
            is_error,
        } => {
            agent_handler::handle_tool_result(app, &name, &output, is_error, target_session);
        }
        AgentToUiEvent::ToolProgress {
            tool_call_id,
            tool_name,
            message,
            progress_percent,
        } => {
            agent_handler::handle_tool_progress(
                app,
                tool_call_id,
                tool_name,
                message,
                progress_percent,
            );
        }
        AgentToUiEvent::TurnDone {
            turn_id,
            new_messages,
            usage,
        } => {
            let result = agent_handler::handle_turn_done(
                app,
                turn_id,
                session,
                background_session,
                &new_messages,
                &usage,
                provider.is_some(),
                rt_env,
                tool_registry,
                cost_tracker,
                model_name,
                compressed_cache,
                agent_tx,
                tool_ctx,
                config,
                interrupt_ctrl,
                interjection_buf,
                context_builder,
                context_window,
                agent_config,
                trust_manager,
                provider,
                system_prompt,
            );

            match result {
                HandleResult::Normal => {
                    // ── Workflow step orchestration: check if next step should auto-run ──
                    spawn_next_workflow_step_if_needed(
                        app,
                        session,
                        provider,
                        agent_tx,
                        tool_registry,
                        tool_ctx,
                        context_builder,
                        context_window,
                        interrupt_ctrl,
                        agent_config,
                        trust_manager,
                        config,
                        rt_env,
                        system_prompt,
                    );
                }
                HandleResult::BackgroundDone => {}
                HandleResult::InterjectionTriggered { turn_messages, .. } => {
                    let turn_id = agent_handler::prepare_agent_spawn(app, interrupt_ctrl);
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
                    let mut turn_messages = turn_messages;
                    if let Some(ref wf_engine) = app.workflow_engine {
                        if let Ok(engine) = wf_engine.try_lock() {
                            let block = engine.durable_memory_block();
                            if !block.is_empty() {
                                turn_messages.push(Message::system(&block));
                            }
                        }
                    }
                    tokio::spawn(async move {
                        agent::run_agent_turn(
                            p,
                            agent::collaboration::RoleProviders::default(),
                            turn_messages,
                            registry,
                            ctx,
                            tx,
                            ui_rx,
                            cancel,
                            tm,
                            ac,
                            false,
                            wf,
                            turn_id,
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
        AgentToUiEvent::SystemNotice(msg) => {
            app.output.push_system(&msg);
            if !app.user_scrolled {
                app.scroll_to_bottom();
            }
            app.dirty = true;
        }
        AgentToUiEvent::ToolConfirmationRequest {
            tool_call_id,
            tool_name,
            args_summary,
            safety_level,
            high_risk_warning,
        } => {
            agent_handler::handle_tool_confirmation(
                app,
                tool_call_id,
                tool_name,
                args_summary,
                safety_level,
                &high_risk_warning,
            );
        }
        AgentToUiEvent::ToolOutputChunk {
            tool_call_id: _,
            chunk,
        } => {
            agent_handler::handle_tool_output_chunk(app, &chunk);
        }
        AgentToUiEvent::BudgetExceeded {
            total_tokens,
            estimated_cost,
        } => {
            agent_handler::handle_budget_exceeded(app, total_tokens, estimated_cost);
        }
        AgentToUiEvent::IterationLimitReached { iteration } => {
            agent_handler::handle_iteration_limit(app, iteration);
        }
        AgentToUiEvent::WorkingDirChanged(new_dir) => {
            let carry_gitnexus = tool_ctx.gitnexus.clone();
            if let Some(new_ctx) = agent_handler::handle_working_dir_changed(
                app,
                session,
                rt_env,
                new_dir,
                provider.is_some(),
                config,
                carry_gitnexus,
            ) {
                *tool_ctx = new_ctx;
            }
            app.dirty = true;
        }
        AgentToUiEvent::WorkflowParked { message } => {
            app.output.push_system(&message);
            app.dirty = true;
        }
        AgentToUiEvent::ReasoningChunk(text) => {
            agent_handler::handle_reasoning_chunk(app, &text);
        }
        // FindingsPanel — no-op: findings are rendered as markdown in chat area.
        AgentToUiEvent::FindingsPanel { .. } => {}
        AgentToUiEvent::ScopeConfirmPrompt { summary } => {
            app.output.push_line(OutputLine::Markdown(summary));
            app.workflow_awaiting_confirmation = Some(4);
            app.scroll_to_bottom();
            app.user_scrolled = false;
            app.dirty = true;
        }
        AgentToUiEvent::WorkspaceModeChanged { mode, banner } => {
            app.workflow_phase_line = mode.clone();
            if mode == "execute_impl" {
                app.clear_workflow_confirmation();
            }
            if !banner.is_empty() {
                app.output.push_system(&banner);
            }
            app.dirty = true;
        }
        AgentToUiEvent::WorkflowCompleted {
            task_description,
            execution_summary,
        } => {
            agent_handler::handle_workflow_completed(
                app,
                session,
                provider,
                rt_env,
                agent_tx,
                task_description,
                execution_summary,
                agent_config,
            );
        }
        AgentToUiEvent::PlanReviewReady { markdown } => {
            agent_handler::handle_plan_review_ready(app, &markdown);
        }
        AgentToUiEvent::WorkflowAwaitingConfirmation { step_idx, message } => {
            agent_handler::handle_workflow_awaiting_confirmation(app, step_idx, &message);
        }
        AgentToUiEvent::SkillReflectRoundSaved {
            round,
            threshold,
            task_summary,
        } => {
            agent_handler::handle_skill_reflect_round_saved(app, round, threshold, &task_summary);
        }
        AgentToUiEvent::SkillDraftReady {
            skill_id,
            content,
            description,
        } => {
            agent_handler::handle_skill_draft_ready(app, skill_id, content, description);
        }
        AgentToUiEvent::DeliverPreview {
            tool_call_id,
            kind,
            content,
        } => {
            agent_handler::handle_deliver_preview(app, &tool_call_id, &kind, &content);
        }
        AgentToUiEvent::FinishPreview {
            tool_call_id,
            summary,
        } => {
            agent_handler::handle_finish_preview(app, &tool_call_id, &summary);
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
    interrupt_ctrl: &mut InterruptController,
    agent_config: &Arc<AgentConfig>,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    config: &OxConfig,
    rt_env: &runtime::RuntimeEnvironment,
    _system_prompt: &str,
) {
    if let Some(ref wf) = app.workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if engine.is_single_step() {
                return;
            }
        }
    }

    // ── Don't auto-spawn if user interrupted the previous step ──
    if app.workflow_interrupted {
        tracing::info!("[WORKFLOW] Skipping auto-spawn: user interrupted previous step");
        return;
    }

    let (step_prompt, step_idx, should_continue, awaiting_confirmation) =
        if let Some(ref wf) = app.workflow_engine {
            if let Ok(engine) = wf.try_lock() {
                let prompt = engine.get_step_system_prompt();
                let idx = engine.get_current_step_index();
                let cont = engine.is_workflow_active() && !engine.is_workflow_complete();
                let waiting = engine.is_current_step_waiting_confirmation();
                (prompt, idx, cont, waiting)
            } else {
                (None, 0, false, false)
            }
        } else {
            (None, 0, false, false)
        };

    if !should_continue || provider.is_none() {
        return;
    }

    // ── Don't auto-spawn if workflow is waiting for user confirmation ──
    if awaiting_confirmation {
        tracing::info!("[WORKFLOW] Skipping auto-spawn: waiting for user confirmation");
        return;
    }

    // Build FRESH system prompt with current step's instructions
    let system_prompt = context::build_system_prompt_with_step(
        rt_env,
        tool_registry,
        ox_core::context::UserIntent::General,
        Some(&config.behavior_rules),
        None,
        &context::TurnContext {
            git_log: None,
            git_diff_stat: None,
            dir_structure: None,
            recent_summary: None,
            relevant_symbols: None,
        },
        step_prompt.as_deref(),
        step_idx,
        config.agent.unified_tool_mode,
    );

    // Minimal context: system prompt + session messages (previous step outputs)
    let mut turn_messages = crate::helpers::build_context_with_option(
        context_builder,
        &system_prompt,
        &session.messages,
        context_window,
        false,
    );

    // Inject user-round anchor + durable memory for workflow spawns (pre_turn skips these).
    if let Some(ref wf) = app.workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            let ur = engine.user_round_memory_block();
            if !ur.is_empty() {
                turn_messages.push(Message::system(&ur));
            }
            let block = engine.durable_memory_block();
            if !block.is_empty() {
                turn_messages.push(Message::system(&block));
            }
        }
    }

    let turn_id = agent_handler::prepare_agent_spawn(app, interrupt_ctrl);
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
            p,
            agent::collaboration::RoleProviders::default(),
            turn_messages,
            registry,
            ctx,
            tx,
            ui_rx,
            cancel,
            tm,
            ac,
            false,
            wf,
            turn_id,
        )
        .await;
    });

    app.scroll_to_bottom();
    app.dirty = true;
}
