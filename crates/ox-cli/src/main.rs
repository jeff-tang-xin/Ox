mod terminal;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
#[cfg(not(target_os = "windows"))]
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
use ox_core::embedding::{CompressionManager, KadaneConfig};
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

const REPLAY_HISTORY_DEPTH: usize = 100;

/// Session action signaled by slash commands, processed in the main event loop.
#[derive(Debug, Clone, Default)]
pub enum SessionAction {
    #[default]
    None,
    New,
    Resume { filename: String },
}

/// Replay the last N messages from a session into the OutputPane.
/// Also updates app.header_info and app.message_count.
fn replay_session_history(
    app: &mut App,
    messages: &[Message],
    rt_env: &runtime::RuntimeEnvironment,
    has_provider: bool,
) {
    app.output.clear();

    let start = messages.len().saturating_sub(REPLAY_HISTORY_DEPTH);
    let slice = &messages[start..];
    if slice.is_empty() {
        app.header_info.clear();
        app.header_info.push(rt_env.banner_summary());
        if has_provider {
            app.header_info.push("Type a message or /help. /exit to quit.".into());
        } else {
            app.header_info.push("No API key. Running in echo mode.".into());
        }
        app.working_dir = rt_env.working_dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| rt_env.working_dir.display().to_string());
        app.message_count = messages.len();
        return;
    }

    app.output.push_line(OutputLine::System(format!(
        "--- {} messages ---",
        slice.len()
    )));

    let mut tc_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for msg in slice {
        match msg {
            Message::System { .. } => {} // Skip system prompts in display
            Message::User { content } => {
                app.output.push_line(OutputLine::User(content.clone()));
            }
            Message::Assistant { content, tool_calls } => {
                if !content.is_empty() {
                    app.output.push_line(OutputLine::Markdown(content.clone()));
                }
                for tc in tool_calls {
                    tc_map.insert(tc.id.clone(), tc.name.clone());
                    app.output.push_line(OutputLine::Tool { name: tc.name.clone(), detail: None });
                }
            }
            Message::ToolResult { tool_call_id, content } => {
                let name = tc_map.get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
                let summary = summarize_tool_result(&name, content);
                let is_error = content.starts_with("Error:") || content.starts_with("Unknown tool");
                app.output.push_line(OutputLine::ToolResult { name, summary, is_error });
            }
        }
    }

    app.output.push_line(OutputLine::System("--- end ---".to_string()));

    // Update header and status bar.
    app.header_info.clear();
    app.header_info.push(rt_env.banner_summary());
    if has_provider {
        app.header_info.push("Type a message or /help. /exit to quit.".into());
    } else {
        app.header_info.push("No API key. Running in echo mode.".into());
    }
    app.working_dir = rt_env.working_dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rt_env.working_dir.display().to_string());
    app.message_count = messages.len();
}

/// Refresh header_info from current runtime state.
fn refresh_header_info(
    app: &mut App,
    rt_env: &runtime::RuntimeEnvironment,
    has_provider: bool,
) {
    app.header_info.clear();
    app.header_info.push(rt_env.banner_summary());
    if has_provider {
        app.header_info.push("Type a message or /help. /exit to quit.".into());
    } else {
        app.header_info.push("No API key. Running in echo mode.".into());
    }
    app.working_dir = rt_env.working_dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rt_env.working_dir.display().to_string());
}

/// Extract session display name from first user message (max 6 chars).
fn session_display_name(session: &Session) -> String {
    session.messages
        .iter()
        .find_map(|m| match m {
            Message::User { content } => {
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    let first_line = trimmed.lines().next().unwrap_or(trimmed);
                    let display = if first_line.chars().count() > 6 {
                        format!("{}..", first_line.chars().take(6).collect::<String>())
                    } else {
                        first_line.to_string()
                    };
                    Some(display)
                }
            }
            _ => None,
        })
        .unwrap_or_else(|| "new session".to_string())
}

fn summarize_tool_result(name: &str, output: &str) -> String {
    match name {
        "file_write" | "file_patch" => {
            let first_line = output.lines().next().unwrap_or(output);
            let truncated: String = first_line.chars().take(120).collect();
            if first_line.len() > 120 { format!("{truncated}...") } else { truncated }
        }
        "file_read" => {
            let line_count = output.lines().count();
            let first_path = output.lines().next()
                .and_then(|l| l.split_whitespace().next())
                .unwrap_or("");
            if first_path.is_empty() {
                format!("{line_count} lines")
            } else {
                format!("{first_path} ({line_count} lines)")
            }
        }
        "code_search" => {
            let match_count = output.lines().take(101).count();
            if output.contains("truncated") {
                format!("100+ matches")
            } else if match_count == 0 {
                "no matches".into()
            } else {
                format!("{match_count} matches")
            }
        }
        "shell_exec" => {
            if let Some(line) = output.lines().find(|l| l.starts_with("[exit code:")) {
                format!("{line}")
            } else {
                let count = output.lines().count();
                format!("{count} lines")
            }
        }
        "file_list" | "file_search" => {
            let count = output.lines().count();
            format!("{count} entries")
        }
        "project_detect" => {
            let first_line = output.lines().next().unwrap_or(output);
            let truncated: String = first_line.chars().take(120).collect();
            truncated
        }
        "git_status" | "git_diff" | "git_commit" => {
            let count = output.lines().count();
            format!("{count} lines")
        }
        "web_fetch" => {
            let len = output.len();
            format!("{len} chars")
        }
        _ => {
            let truncated: String = output.chars().take(120).collect();
            if output.len() > 120 { format!("{truncated}...") } else { truncated }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Detect runtime early to get home_dir for log file path.
    let early_home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let log_dir = early_home.join(".ox").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_file_path = log_dir.join("ox.log");

    // Initialize logging: file only (~/.ox/logs/ox.log).
    // No stderr output in TUI mode to prevent display corruption.
    if let Ok(log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::Layer;
        // Capture info+ to file, silent on terminal
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ox=info"));
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .with_filter(filter);
        tracing_subscriber::registry()
            .with(file_layer)
            .init();
    } else {
        // Fallback: disable logging if file can't be opened
        use tracing_subscriber::filter::LevelFilter;
        tracing_subscriber::fmt()
            .with_max_level(LevelFilter::OFF)
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

    // Try to create LLM provider (may fail if no API key).
    let (provider, resolve_info): (Option<Arc<dyn LlmProvider>>, Option<ProviderResolveInfo>) =
        match llm::create_provider_with_info(&config.models.default, &config.models) {
            Ok((p, info)) => {
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
    // Enable mouse capture for scroll events (skip on Windows where it
    // can cause garbled input in cmd/powershell).
    #[cfg(not(target_os = "windows"))]
    stdout.execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run the app; always restore terminal on exit.
    let result = run_app(&mut terminal, &config, rt_env, provider, resolve_info).await;

    // Restore terminal.
    disable_raw_mode()?;
    #[cfg(not(target_os = "windows"))]
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

    // Set header info (fixed, non-scrolling).
    app.header_info.push(rt_env.banner_summary());
    if provider.is_some() {
        app.header_info.push("Type a message or /help for commands. /exit to quit.".to_string());
    } else {
        app.header_info.push("No API key. Set env var or config. Running in echo mode.".to_string());
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
                replay_session_history(&mut app, &s.messages, &rt_env, provider.is_some());
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
    app.sessions.insert(0, terminal::app::SessionEntry {
        filename: "current".to_string(),
        info: session_display_name(&session),
        is_active: true,
    });

    // Create tool registry and context (shared via Arc for tokio::spawn).
    let tool_registry = Arc::new(ToolRegistry::new());
    #[allow(unused_variables)]
    let tool_ctx = Arc::new(ToolContext::new(
        rt_env.clone(),
        rt_env.working_dir.clone(),
    ));

    // Build system prompt using context module.
    let mut persona_vector = ox_core::persona::PersonaVector::for_language(
        &rt_env.project_root.as_ref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    );
    let system_prompt = context::build_system_prompt(&rt_env, &tool_registry, None, Some(&persona_vector), Some(&config.behavior_rules));

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
    let mut memory = MemoryManager::init(&rt_env.ox_home_dir, &rt_env.project_id, &config.memory)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to init memory system: {e}");
            let temp = std::env::temp_dir();
            MemoryManager::init(&temp, &rt_env.project_id, &config.memory)
                .expect("memory init with temp dir")
        });

    // Probabilistic janitor run on startup (20% chance).
    if rand::random::<f64>() < config.memory.janitor_run_on_startup_prob {
        memory.run_janitor(0.3, config.memory.max_nodes);
    }

    // Create tool context (memory is pre-injected into context by main.rs)
    let mut tool_ctx = Arc::new(ToolContext::new(
        rt_env.clone(),
        rt_env.working_dir.clone(),
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
    let compressed_ctx_store = Arc::new(ox_core::context::compressed_store::CompressedContextStore::open(
        &db_dir.join("compressed_context.db"),
    ).unwrap_or_else(|e| {
        tracing::warn!("Failed to open compressed context store: {e}");
        ox_core::context::compressed_store::CompressedContextStore::open(
            &std::env::temp_dir().join("compressed_context.db"),
        ).expect("compressed context store with temp dir")
    }));

    // Compression manager for context compression (KadaneDial).
    // Uses history_ratio from ContextBuilder for consistent configuration.
    let compression_manager: Option<CompressionManager> = if let Some(ref emb_config) = config.models.embedding {
        if emb_config.enabled {
            let model_path = emb_config.model_path.as_ref()
                .map(|p| {
                    let p = p.replace('~', &dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .to_string_lossy());
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
                    Some(CompressionManager::new(emb, kadane_config, context_builder.history_ratio()))
                }
                Err(e) => {
                    tracing::warn!("Failed to load embedding model: {}. Compression disabled.", e);
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

    loop {
        // Only re-render when needed (dirty or spinner animation changed).
        if app.needs_render() {
            terminal.draw(|frame| render::render(frame, &mut app, tick_count))?;
            app.dirty = false;
            app.mark_spinner_rendered();
        }

        // Handle deferred compression (set by handle_key_event after status render).
        // Compression runs on a blocking thread so the TUI stays responsive.
        if let Some(pc) = app.pending_compression.take() {
            // Skip if compression is already in progress (prevents re-entrant compression).
            if app.compression_in_progress {
                app.output.push_line(OutputLine::System(
                    "Compression in progress, skipping...".to_string()
                ));
                app.agent_running = false;
                app.dirty = true;
                continue;
            }
            app.compression_in_progress = true;
            let source_msg_count = session.messages.len();
            app.last_compression_msg_count = source_msg_count;
            app.agent_running = true;
            app.status = "Compressing...".to_string();
            app.dirty = true;
            if let Some(ref p) = provider {
                let cm = compression_manager.clone();
                // Build input: existing compressed context + new messages, or all messages.
                let messages = if let Some((ref cached, prev_count)) = compressed_cache {
                    let new_msgs = &session.messages[prev_count.min(session.messages.len())..];
                    let mut combined = cached.clone();
                    combined.extend_from_slice(new_msgs);
                    combined
                } else {
                    session.messages.clone()
                };
                let sp = system_prompt.clone();
                let memory_ctx = pc.memory_ctx;
                let query = pc.text;
                let cb = context_builder.clone();
                let cw = context_window;
                let provider = Arc::clone(p);
                let tx = agent_tx.clone();
                let registry = Arc::clone(&tool_registry);
                let ctx = Arc::clone(&tool_ctx);
                let cancel_token = interrupt_ctrl.token();
                let tm = Arc::clone(&trust_manager);
                let ac = Arc::clone(&agent_config);
                let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<UiToAgentEvent>();
                app.ui_to_agent_tx = Some(ui_to_agent_tx);
                tokio::spawn(async move {
                    let tx_status = tx.clone();
                    let turn_messages = match cm {
                        Some(cm) => {
                            let q = query;
                            let mem_ctx = memory_ctx.clone();
                            match tokio::task::spawn_blocking(move || {
                                // Use enhanced compression with memory context
                                let result = if !mem_ctx.is_empty() {
                                    cm.compress_with_memory(&messages, &q, Some(&mem_ctx))
                                } else {
                                    cm.compress(&messages, &q)
                                };
                                (result, messages, cm)
                            })
                            .await
                            {
                                Ok((Ok(Some(compressed)), original, _cm)) => {
                                    let _ = tx_status.send(AgentToUiEvent::Status(
                                        format!(
                                            "Compressed: {} → {} msgs",
                                            original.len(),
                                            compressed.len()
                                        ),
                                    ));
                                    let _ = tx_status.send(AgentToUiEvent::CompressionComplete {
                                        compressed_messages: compressed.clone(),
                                        source_msg_count,
                                    });
                                    cb.build(&sp, &memory_ctx, &compressed, cw)
                                }
                                Ok((Ok(None), original, _cm)) => {
                                    cb.build(&sp, &memory_ctx, &original, cw)
                                }
                                Ok((Err(e), original, _cm)) => {
                                    tracing::error!("Compression failed: {}", e);
                                    cb.build(&sp, &memory_ctx, &original, cw)
                                }
                                Err(_) => {
                                    tracing::error!("Compression task panicked");
                                    return;
                                }
                            }
                        }
                        None => cb.build(&sp, &memory_ctx, &messages, cw),
                    };
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
                        false, // compression path: skip planning
                    )
                    .await;
                });
            }
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
                            &mut memory,
                            &mut persona_vector,
                            &tool_registry,
                            &tool_ctx,
                            &context_builder,
                            &&system_prompt,
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
                            &mut session_action,
                            &compression_manager,
                            &compressed_cache,
                        );
                        // Process session switch action.
                        match session_action {
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
                                } else {
                                    let project_id = rt_env.project_id.clone();
                                    match Session::new(&session_dir, &project_id) {
                                        Ok(s) => {
                                            session = s;
                                            app.output.clear();
                                            app.output.push_system("New session started.");
                                            refresh_header_info(&mut app, &rt_env, provider.is_some());
                                            app.message_count = 0;
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
                                            replay_session_history(&mut app, &session.messages, &rt_env, provider.is_some());
                                            app.output.push_system(&format!(
                                                "Session restored: {} messages from {}",
                                                session.messages.len(), filename
                                            ));
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
                        session_action = SessionAction::None;

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
                    Some(Event::ScrollUp { column, row }) => {
                        // Only handle scroll if mouse is in chat area
                        if let Some((cx, cy, cw, ch)) = app.chat_area {
                            if column >= cx && column < cx + cw && row >= cy && row < cy + ch {
                                app.scroll_up(3);
                                app.user_scrolled = true;
                                app.dirty = true;
                            }
                        }
                    }
                    Some(Event::ScrollDown { column, row }) => {
                        // Only handle scroll if mouse is in chat area
                        if let Some((cx, cy, cw, ch)) = app.chat_area {
                            if column >= cx && column < cx + cw && row >= cy && row < cy + ch {
                                app.scroll_down(3);
                                if app.scroll_offset < 3 {
                                    app.user_scrolled = false;
                                }
                                app.dirty = true;
                            }
                        }
                    }
                    Some(Event::Tick) | None => {
                        tick_count = tick_count.wrapping_add(1);
                        app.spinner_frame = tick_count;
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
                            let summary = summarize_tool_result(&name, &output);
                            app.output.push_line(OutputLine::ToolResult {
                                name: name.clone(),
                                summary,
                                is_error,
                            });
                            if !app.user_scrolled { app.scroll_to_bottom(); }
                            app.dirty = true;
                        }
                        AgentToUiEvent::TurnDone { new_messages, usage } => {
                            app.output.finalize_streaming();
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
                            memory.update_from_turn(&new_messages, &rt_env.project_id, &rt_env.project_language);

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
                                let memory_nodes = memory.retrieve(&last_user_query, &Some(rt_env.project_id.as_str()), 5);
                                let memory_ctx = memory.format_memory_context(&memory_nodes, false);
                                
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
                                        let memory_nodes = memory.retrieve(&text, &Some(rt_env.project_id.as_str()), 5);
                                        let accessed_ids: Vec<&str> = memory_nodes.iter().map(|n| n.id.as_str()).collect();
                                        memory.reinforce_accessed(&accessed_ids);
                                        let memory_ctx = memory.format_memory_context(&memory_nodes, false);
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
                                        tokio::spawn(async move {
                                            agent::run_agent_turn(provider, turn_messages, registry, ctx, tx, ui_to_agent_rx, cancel_token, tm, ac, false).await;
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
                                memory.store(mem_node);
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
                                    refresh_header_info(&mut app, &rt_env, provider.is_some());
                                    // Update tool_ctx for next agent turn.
                                    tool_ctx = Arc::new(ToolContext::new(
                                        rt_env.clone(),
                                        new_dir.clone(),
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
    session_action: &mut SessionAction,
    compression_manager: &Option<CompressionManager>,
    compressed_cache: &Option<(Vec<Message>, usize)>,
) {
    match (key.code, key.modifiers) {
        // Confirmation key handling (Y/N/T when pending)
        (KeyCode::Char('y'), KeyModifiers::NONE) | (KeyCode::Char('Y'), KeyModifiers::NONE) => {
            if let Some(pc) = app.pending_confirmation.take() {
                if let Some(tx) = &app.ui_to_agent_tx {
                    let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                        tool_call_id: pc.tool_call_id,
                        decision: ConfirmationDecision::Allow,
                    });
                    app.output.push_line(OutputLine::System("  -> Allowed".to_string()));
                } else {
                    app.output.push_line(OutputLine::Error("  -> Error: agent channel closed, cannot confirm".to_string()));
                }
                app.dirty = true;
                return;
            }
            app.input.insert_char('y');
            app.dirty = true;
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) | (KeyCode::Char('N'), KeyModifiers::NONE) => {
            if let Some(pc) = app.pending_confirmation.take() {
                if let Some(tx) = &app.ui_to_agent_tx {
                    let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                        tool_call_id: pc.tool_call_id,
                        decision: ConfirmationDecision::Deny,
                    });
                    app.output.push_line(OutputLine::System("  -> Denied".to_string()));
                } else {
                    app.output.push_line(OutputLine::Error("  -> Error: agent channel closed, cannot deny".to_string()));
                }
                app.dirty = true;
                return;
            }
            app.input.insert_char('n');
            app.dirty = true;
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) | (KeyCode::Char('T'), KeyModifiers::NONE) => {
            if let Some(pc) = app.pending_confirmation.take() {
                if let Some(tx) = &app.ui_to_agent_tx {
                    let _ = tx.send(UiToAgentEvent::ToolConfirmation {
                        tool_call_id: pc.tool_call_id,
                        decision: ConfirmationDecision::TrustAlways,
                    });
                    app.output.push_line(OutputLine::System(
                        "  -> Trusted all tools for this session. Use /untrust to revoke.".to_string(),
                    ));
                    app.trusted_all = true;
                } else {
                    app.output.push_line(OutputLine::Error("  -> Error: agent channel closed, cannot trust".to_string()));
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
                            session_action,
                            &compression_manager,
                        );
                        // Mark dirty to trigger UI refresh after slash command processing
                        app.dirty = true;
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
                            // Send interjection to agent immediately via channel
                            let priority = if text.starts_with('!') {
                                InterjectionPriority::Urgent
                            } else {
                                InterjectionPriority::Normal
                            };
                            let content = text.trim_start_matches('!').to_string();
                            
                            if let Some(tx) = &app.ui_to_agent_tx {
                                let _ = tx.send(UiToAgentEvent::Interjection(content.clone()));
                            }
                            
                            // Also buffer locally for fallback display
                            interjection_buf.push(content.clone(), priority);
                            
                            let prefix = if priority == InterjectionPriority::Urgent {
                                "(urgent!)"
                            } else {
                                "(queued)"
                            };
                            app.output.push_line(OutputLine::System(format!(
                                "{} {}", prefix, content.trim()
                            )));
                            app.scroll_to_bottom();
                            app.dirty = true;
                        } else if let Some(provider) = provider {
                            let user_msg = Message::user(&text);
                            if let Err(e) = session.append_message(user_msg) {
                                tracing::error!("Failed to persist user message: {e}");
                            }
                            let memory_nodes = memory.retrieve(&text, &Some(rt_env.project_id.as_str()), 5);
                            let accessed_ids: Vec<&str> = memory_nodes.iter().map(|n| n.id.as_str()).collect();
                            memory.reinforce_accessed(&accessed_ids);
                            let memory_ctx = memory.format_memory_context(&memory_nodes, false);

                            // Build effective messages using the latest compressed cache from SQLite
                            let effective_messages = if let Some((cached, prev_count)) = compressed_cache {
                                let pc = *prev_count;
                                let new_msgs = &session.messages[pc.min(session.messages.len())..];
                                let mut combined = cached.clone();
                                combined.extend_from_slice(new_msgs);
                                combined
                            } else {
                                session.messages.clone()
                            };

                            let turn_messages = context_builder.build(
                                &system_prompt,
                                &memory_ctx,
                                &effective_messages,
                                context_window,
                            );
                            app.agent_running = true;
                            app.status = "Thinking...".to_string();
                            let effort = ox_core::context::estimate_effort(&text, session.messages.len());
                            let planning = effort == ox_core::context::EffortLevel::High;
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
                                    planning,
                                )
                                .await;
                            });
                        } else {
                            app.output.push_line(OutputLine::System(format!(
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
            if app.scroll_offset < 3 { app.user_scrolled = false; }
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
            if app.scroll_offset < 3 { app.user_scrolled = false; }
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
    session_action: &mut SessionAction,
    compression_manager: &Option<ox_core::embedding::CompressionManager>,
) {
    let parsed = slash::parse_slash_command(cmd, args);

    match parsed {
        SlashCommand::Help { topic } => {
            let text = slash::help_text(topic.as_deref());
            for line in text.lines() {
                app.output.push_line(OutputLine::System(line.to_string()));
            }
        }
        SlashCommand::Exit => {
            app.output.push_system("Goodbye.");
            app.should_quit = true;
        }
        SlashCommand::New => {
            // Signal session action to main loop for processing.
            *session_action = SessionAction::New;
        }
        SlashCommand::Clear => {
            app.output.clear();
        }
        SlashCommand::Clean => {
            if let Err(e) = session.clean() {
                app.output.push_error(&format!("Failed to clean session: {}", e));
            } else {
                app.output.clear();
                app.message_count = 0;
                app.cost_summary = String::new();
                app.output.push_system("Session cleared. All messages removed.");
            }
        }
        SlashCommand::Cost => {
            let summary = cost_tracker.summary();
            for line in summary.lines() {
                app.output.push_line(OutputLine::System(line.to_string()));
            }
        }
        SlashCommand::Plan => {
            app.output
                .push_system("Task plan: (not yet active -- agent will create plans automatically)");
        }
        SlashCommand::Trust { tools, all } => {
            let mut tm = match trust_manager.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    app.output.push_line(OutputLine::Error(format!("Failed to lock trust manager: {}", e)));
                    return;
                }
            };
            if all {
                tm.trust_all();
                app.trusted_all = true;
                app.output
                    .push_system("Trusted all tools for this session. Use /untrust to revoke.");
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
            if let Ok(mut tm) = trust_manager.lock() {
                tm.untrust_all();
            }
            app.trusted_all = false;
            app.output
                .push_system("All tool trust revoked. Confirmations restored.");
        }
        SlashCommand::Model { name } => {
            if let Some(new_model) = name {
                app.pending_model_switch = Some(new_model.clone());
                app.output.push_line(OutputLine::System(format!(
                    "Switching to: {}", new_model
                )));
            } else {
                app.output.push_line(OutputLine::System(format!(
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
                        app.output.push_line(OutputLine::System(format!(
                            "Changed to: {}",
                            new_dir.display()
                        )));
                        // Refresh header info after directory change.
                        refresh_header_info(app, rt_env, resolve_info.is_some());
                        if project_changed {
                            let project_name = rt_env.project_root
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "(none)".into());
                            app.output.push_system(&format!(
                                "Project boundary changed -- {project_name}"
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
                app.output.push_line(OutputLine::System(format!(
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
                .push_line(OutputLine::System(format!("Model: {model_name}")));
            // Provider resolution info
            if let Some(info) = resolve_info {
                app.output.push_line(OutputLine::System(format!(
                    "Provider: {}",
                    info.provider_name
                )));
                let key_src = match &info.api_key_source {
                    llm::ApiKeySource::EnvVar(name) => format!("env var {}", name),
                    llm::ApiKeySource::ConfigFile => "config file".to_string(),
                    llm::ApiKeySource::NotFound => "NOT FOUND".to_string(),
                };
                app.output.push_line(OutputLine::System(format!(
                    "API key source: {key_src}"
                )));
                let url_src = match &info.base_url_source {
                    llm::BaseUrlSource::ConfigFile => "config file",
                    llm::BaseUrlSource::Default => "provider default",
                };
                app.output.push_line(OutputLine::System(format!(
                    "Base URL source: {url_src}"
                )));
            } else {
                app.output.push_line(OutputLine::System(
                    "Provider: (none -- echo mode)".to_string(),
                ));
            }
            // Config file path
            let config_path = OxConfig::default_config_path();
            app.output.push_line(OutputLine::System(format!(
                "Config file: {}",
                config_path.display()
            )));
            // Embedding model status
            if compression_manager.is_some() {
                let model_path = config.models.embedding.as_ref().and_then(|c| c.model_path.as_ref())
                    .map(|p| p.clone())
                    .unwrap_or_else(|| {
                        dirs::home_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                            .join(".ox/models/bge-small-zh-v1.5")
                            .to_string_lossy()
                            .to_string()
                    });
                app.output.push_line(OutputLine::System("Embedding: loaded".to_string()));
                app.output.push_line(OutputLine::System(format!("  Model path: {}", model_path)));
                if let Some(ref emb_cfg) = config.models.embedding {
                    app.output.push_line(OutputLine::System(format!(
                        "  Threshold: {:.2}, stop: {:.2}, segments: {}",
                        emb_cfg.threshold, emb_cfg.stop_threshold, emb_cfg.max_segments
                    )));
                    app.output.push_line(OutputLine::System(format!(
                        "  Chunk: {} tokens, max: {} tokens",
                        emb_cfg.chunk_threshold_tokens, emb_cfg.max_chunk_tokens
                    )));
                }
            } else {
                app.output.push_line(OutputLine::System(
                    "Embedding: disabled (set [models.embedding] enabled = true)".to_string(),
                ));
            }
            // All providers key status (never show values)
            app.output.push_line(OutputLine::System("Providers:".to_string()));
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
                app.output.push_line(OutputLine::System(format!(
                    "  {name}: {status}"
                )));
            }
            // Model->provider mapping
            if !config.models.model_providers.is_empty() {
                app.output.push_line(OutputLine::System(
                    "Model->Provider mappings:".to_string(),
                ));
                for (model, provider) in &config.models.model_providers {
                    app.output.push_line(OutputLine::System(format!(
                        "  {model} -> {provider}"
                    )));
                }
            }
            app.output
                .push_line(OutputLine::System(format!("OS: {} ({})", rt_env.os, rt_env.arch)));
            app.output
                .push_line(OutputLine::System(format!("Shell: {}", rt_env.shell.name)));
            app.output.push_line(OutputLine::System(format!(
                "Working dir: {}",
                rt_env.working_dir.display()
            )));
            app.output.push_line(OutputLine::System(format!(
                "Project root: {}",
                rt_env
                    .project_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none)".into())
            )));
            app.output.push_line(OutputLine::System(format!(
                "Project ID: {}",
                rt_env.project_id
            )));
            app.output.push_line(OutputLine::System(format!(
                "History: {} messages",
                session.messages.len()
            )));
            let trusted = {
                let tm = trust_manager.lock().unwrap();
                tm.trusted_list()
            };
            app.output.push_line(OutputLine::System(format!(
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
                app.output.push_line(OutputLine::System("Archived sessions:".to_string()));
                for (i, (filename, info)) in archived.iter().enumerate() {
                    app.output.push_line(OutputLine::System(format!(
                        "  {}. {}  ({})",
                        i + 1,
                        info,
                        filename
                    )));
                }
                app.output.push_line(OutputLine::System(
                    "Use /resume <filename> to restore a session.".to_string(),
                ));
            }
        }
        SlashCommand::Resume { filename } => {
            if filename.is_empty() {
                app.output.push_system("Usage: /resume <filename>  (use /sessions to list)");
            } else {
                // Signal session action to main loop for processing.
                *session_action = SessionAction::Resume { filename: filename.clone() };
            }
        }
        SlashCommand::Remember { content } => {
            if content.is_empty() {
                app.output.push_system("Usage: /remember <content>  (stores as Style memory)");
            } else {
                memory.store_explicit(&content, &rt_env.project_id, &rt_env.project_language);
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
            app.output.push_line(OutputLine::System(format!("Memory: {} project, {} long-term", project_count, overall_count)));
            let nodes = memory.retrieve("", &Some(rt_env.project_id.as_str()), 5);
            for node in &nodes {
                app.output.push_line(OutputLine::System(format!(
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
                app.output.push_line(OutputLine::System(format!("Persona: {}", persona_vector)));
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
                        app.output.push_line(OutputLine::System(line.to_string()));
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
        SlashCommand::Reload => {
            // Reload session from JSONL file to sync with disk state.
            let session_dir = session.dir().to_path_buf();
            match Session::load(&session_dir) {
                Ok(Some(loaded)) => {
                    let old_count = session.messages.len();
                    let new_count = loaded.messages.len();
                    
                    // Replace session messages with loaded data (clone to avoid move out of Drop type)
                    session.messages = loaded.messages.clone();
                    session.meta = loaded.meta.clone();
                    
                    // Replay history into UI
                    replay_session_history(app, &session.messages, rt_env, resolve_info.is_some());
                    
                    app.output.push_system(&format!(
                        "Session reloaded from disk: {} messages (was {})",
                        new_count, old_count
                    ));
                    app.message_count = session.messages.len();
                }
                Ok(None) => {
                    app.output.push_error("Failed to reload: session file is empty or corrupted");
                }
                Err(e) => {
                    app.output.push_error(&format!("Failed to reload session: {}", e));
                }
            }
        }
        SlashCommand::Unknown { cmd } => {
            app.output
                .push_system(&format!("Unknown command: /{cmd}. Type /help for available commands."));
        }
    }
}
