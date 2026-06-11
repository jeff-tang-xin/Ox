# Main Flow Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor the main flow of Ox project — split `main.rs` from 2300→150 lines, eliminate 3x duplicated pre-turn context building, unify session management, split engine.rs, parallelize startup.

**Architecture:** Create `AppRuntime` struct to hold all subsystem state (eliminating 19-param functions), extract handlers for key events/agent events/sessions/pre-turn into `handlers/` module, split `run_agent_turn` into focused sub-modules under `agent/`, parallelize independent init steps.

**Tech Stack:** Rust 2024 edition, tokio async runtime, ratatui TUI, crossterm

---

### Task 1: Create `AppRuntime` struct

**Files:**
- Create: `crates/ox-cli/src/runtime.rs`

- [ ] **Step 1: Create AppRuntime struct**

```rust
// crates/ox-cli/src/runtime.rs
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use ox_core::agent::AgentToUiEvent;
use ox_core::agent::interrupt::InterruptController;
use ox_core::config::{AgentConfig, OxConfig};
use ox_core::context::ContextBuilder;
use ox_core::cost::CostTracker;
use ox_core::knowledge::KnowledgeEngine;
use ox_core::llm::{LlmProvider, ProviderResolveInfo};
use ox_core::memory::MemoryManager;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry;
use ox_core::context::compressed_store::CompressedContextStore;
use crate::slash_commands::CommandRegistry;

/// Holds all shared state for the application, eliminating the need for
/// functions with 15+ parameters. Created once at startup and passed by reference.
pub struct AppRuntime {
    pub config: OxConfig,
    pub agent_config: Arc<AgentConfig>,
    pub rt_env: RuntimeEnvironment,
    pub provider: Option<Arc<dyn LlmProvider>>,
    pub resolve_info: Option<ProviderResolveInfo>,
    pub tool_registry: Arc<ToolRegistry>,
    pub command_registry: CommandRegistry,
    pub context_builder: ContextBuilder,
    pub context_window: u32,
    pub knowledge_engine: Arc<tokio::sync::RwLock<KnowledgeEngine>>,
    pub memory: Arc<MemoryManager>,
    pub cost_tracker: CostTracker,
    pub trust_manager: Arc<std::sync::Mutex<TrustManager>>,
    pub compressed_ctx_store: Arc<CompressedContextStore>,
    pub interrupt_ctrl: InterruptController,
    pub agent_tx: mpsc::UnboundedSender<AgentToUiEvent>,
    pub tool_ctx: Arc<ToolContext>,
    pub model_name: String,
}
```

- [ ] **Step 2: Add `pub mod runtime;` to main.rs**

In `crates/ox-cli/src/main.rs`, add `pub mod runtime;` after existing module declarations.

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: PASS (module added but not yet wired)

---

### Task 2: Extract Pre-Turn Pipeline

**Files:**
- Create: `crates/ox-cli/src/handlers/mod.rs`
- Create: `crates/ox-cli/src/handlers/pre_turn.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create handlers module**

```rust
// crates/ox-cli/src/handlers/mod.rs
pub mod pre_turn;
```

- [ ] **Step 2: Create pre_turn.rs with PreTurnPipeline**

```rust
// crates/ox-cli/src/handlers/pre_turn.rs
use ox_core::agent::AgentToUiEvent;
use ox_core::context::{self, ContextBuilder, UserIntent, TurnContext};
use ox_core::knowledge::retrieval;
use ox_core::message::Message;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::tools::ToolRegistry;
use std::sync::Arc;
use tokio::sync::mpsc;

pub enum TurnVariant {
    Normal,
    Onboarding { prompt_text: String },
    SlashSkill { prompt: String, description: String },
    SlashGeneral { prompt: String },
}

pub struct PreTurnContext {
    pub turn_messages: Vec<Message>,
    pub planning: bool,
    pub knowledge_context: String,
}

/// Unified pre-turn pipeline — single code path for all LLM invocations.
/// Handles: injection scan → knowledge retrieval → git/dir context → system prompt → context builder
pub async fn prepare_turn(
    config: &ox_core::config::OxConfig,
    rt_env: &RuntimeEnvironment,
    tool_registry: &Arc<ToolRegistry>,
    context_builder: &ContextBuilder,
    context_window: u32,
    knowledge_engine: &Option<Arc<tokio::sync::RwLock<ox_core::knowledge::KnowledgeEngine>>>,
    user_text: &str,
    session_messages: &[Message],
    compressed_cache: &Option<(Vec<Message>, usize)>,
    variant: TurnVariant,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
    session_id: &str,
    status_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> (Vec<Message>, bool) {
    // 1. Get workflow step info if active
    let (step_memory_layers, step_prompt, step_idx) = get_workflow_step_info(workflow_engine);

    // 2. Knowledge retrieval (step-aware)
    let _ = status_tx.send(AgentToUiEvent::Status("🔍 Retrieving knowledge...".to_string()));
    let knowledge_context_str = if let Some(ref k_engine) = knowledge_engine {
        match k_engine.try_read() {
            Ok(engine) => {
                let result = if step_memory_layers.is_empty() {
                    retrieval::run_retrieval(&engine, user_text, session_id, 3000)
                } else {
                    retrieval::run_retrieval_for_step(&engine, user_text, session_id, 3000, &step_memory_layers)
                };
                match result {
                    Ok(inj) => retrieval::format_context_for_prompt(&inj),
                    Err(_) => String::new(),
                }
            }
            Err(_) => String::new(),
        }
    } else { String::new() };

    // 3. Git + Dir context (spawn_blocking for I/O)
    let _ = status_tx.send(AgentToUiEvent::Status("📊 Gathering context...".to_string()));
    let tr = Arc::clone(tool_registry);
    let rt_env_clone = rt_env.clone();
    let behavior_rules = config.behavior_rules.clone();
    let compressed_cache_clone = compressed_cache.clone();
    let messages_clone = session_messages.to_vec();
    let context_builder_clone = context_builder.clone();
    let step_prompt_clone = step_prompt.clone();
    let user_text_clone = user_text.to_string();
    let knowledge_ctx = knowledge_context_str;
    let use_refined = config.context.use_refined_context;

    let blocking_result = tokio::task::spawn_blocking(move || {
        let git_log = context::gather_git_context(&rt_env_clone.working_dir);
        let git_diff = context::gather_diff_context(&rt_env_clone.working_dir);
        let dir_tree = context::gather_dir_context(&rt_env_clone.working_dir);

        let turn_ctx = TurnContext {
            git_log: None, git_diff_stat: None, dir_structure: None,
            recent_summary: None, relevant_symbols: None,
        };
        let system_prompt = context::build_system_prompt_with_step(
            &rt_env_clone, &tr, UserIntent::General,
            Some(&behavior_rules), None, &turn_ctx,
            step_prompt_clone.as_deref(), step_idx,
        );

        let effective_messages = if let Some((cached, prev_count)) = compressed_cache_clone {
            let start_idx = (*prev_count).min(messages_clone.len());
            let new_msgs = if start_idx < messages_clone.len() { &messages_clone[start_idx..] } else { &[] };
            let mut combined = cached.clone();
            combined.extend_from_slice(new_msgs);
            combined
        } else { messages_clone };

        let mut turn_messages = crate::helpers::build_context_with_option(
            &context_builder_clone, &system_prompt, "",
            &effective_messages, context_window, use_refined,
        );

        // Inject knowledge + background info as one system message
        let mut bg_parts = Vec::new();
        if !knowledge_ctx.is_empty() { bg_parts.push(knowledge_ctx); }
        if let Some(ref log) = git_log { bg_parts.push(format!("【参考-Git日志】\n{}", log)); }
        if let Some(ref diff) = git_diff { bg_parts.push(format!("【参考-未提交变更】\n{}", diff)); }
        if let Some(ref dir) = dir_tree { bg_parts.push(format!("【参考-目录结构】\n{}", dir)); }
        if !bg_parts.is_empty() { turn_messages.push(Message::system(&bg_parts.join("\n\n"))); }

        let effort = ox_core::context::estimate_effort(&user_text_clone, effective_messages.len());
        let planning = effort == ox_core::context::EffortLevel::High;

        Ok::<_, String>((turn_messages, planning))
    }).await;

    match blocking_result {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => {
            tracing::error!("Pre-turn pipeline failed: {}", e);
            (vec![Message::user(user_text)], false)
        }
        Err(e) => {
            tracing::error!("Pre-turn pipeline panicked: {}", e);
            (vec![Message::user(user_text)], false)
        }
    }
}

fn get_workflow_step_info(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
) -> (Vec<String>, Option<String>, usize) {
    if let Some(ref wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            let step = engine.current_step();
            let layers = step.map(|s| s.memory_layers.clone()).unwrap_or_default();
            let prompt = step.and_then(|s| if s.step_prompt.is_empty() { None } else { Some(s.step_prompt.clone()) });
            let idx = engine.get_current_step_index();
            return (layers, prompt, idx);
        }
    }
    (Vec::new(), None, 0)
}
```

- [ ] **Step 3: Add `pub mod handlers;` to main.rs**

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: PASS

---

### Task 3: Extract Session Handler (de-duplicate 3x sidebar rebuild)

**Files:**
- Create: `crates/ox-cli/src/handlers/session_handler.rs`
- Modify: `crates/ox-cli/src/handlers/mod.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create session_handler.rs**

```rust
// crates/ox-cli/src/handlers/session_handler.rs
use std::path::Path;
use std::sync::Arc;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use crate::terminal::app::{App, SessionAction, SessionEntry};
use crate::terminal::output_pane::OutputLine;
use crate::helpers;

/// Rebuild the sidebar session list from disk — called exactly once, shared by all session ops.
pub fn rebuild_sidebar(
    app: &mut App,
    sessions_root: &Path,
    active_project_id: &str,
    active_session_name: &str,
) {
    app.sessions.clear();
    if !sessions_root.exists() { return; }
    let Ok(project_dirs) = std::fs::read_dir(sessions_root) else { return };

    for project_entry in project_dirs.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() { continue;

        let project_id = project_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let archived = Session::list_archived(&project_path);
        for (filename, info) in archived {
            app.sessions.push(SessionEntry {
                id: filename,
                project_id: project_id.clone(),
                info,
                is_active: false,
            });
        }
    }

    app.sessions.insert(0, SessionEntry {
        id: "session.jsonl".to_string(),
        project_id: active_project_id.to_string(),
        info: active_session_name.to_string(),
        is_active: true,
    });
}

/// Handle SessionAction::New — archive current, create new session.
pub fn handle_session_new(
    app: &mut App,
    session: &mut Session,
    rt_env: &RuntimeEnvironment,
    memory: &Arc<ox_core::memory::MemoryManager>,
    agent_running: bool,
) -> Result<(), String> {
    let session_dir = rt_env.ox_home_dir.join("sessions").join(&rt_env.project_id);

    if agent_running {
        let new_s = Session::new(&session_dir, &rt_env.project_id)
            .map_err(|e| format!("Failed to create session: {e}"))?;
        *session = new_s;
        Ok(())
    } else {
        // Trigger memory promotion for meaningful sessions
        if session.messages.len() >= 10 {
            if let Some(result) = memory.run_promotion_pipeline(
                &rt_env.project_id,
                &rt_env.working_dir.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
            ) {
                match result {
                    Ok(report) => { app.output.push_system(&format!("\n🧠 Memory Promotion Complete:\n{}", report)); }
                    Err(e) => { tracing::error!("Memory promotion failed: {e}"); }
                }
            }
        }
        // Archive current
        if let Err(e) = session.archive(&session_dir) {
            tracing::warn!("Failed to archive current session: {e}");
        }
        let default_wd = rt_env.working_dir.to_string_lossy().to_string();
        let mut new_s = Session::new(&session_dir, &rt_env.project_id)
            .map_err(|e| format!("Failed to create session: {e}"))?;
        if let Err(e) = new_s.update_working_dir(&default_wd) {
            tracing::warn!("Failed to set default working dir: {e}");
        }
        *session = new_s;
        app.output.clear();
        app.output.push_system("New session started.");
        helpers::refresh_header_info(app, rt_env, true);
        app.message_count = 0;
        Ok(())
    }
}

/// Handle SessionAction::Resume — load archived session.
pub fn handle_session_resume(
    app: &mut App,
    session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    filename: &str,
    agent_running: bool,
) -> Result<(), String> {
    // Find the session entry by ID or display name
    let sessions_root = rt_env.ox_home_dir.join("sessions");
    let target = app.sessions.iter()
        .find(|s| s.id == filename || s.display_name().contains(filename))
        .ok_or_else(|| format!("Session '{}' not found.", filename))?;

    let session_path = target.full_path(&sessions_root);
    let parent_dir = session_path.parent()
        .ok_or_else(|| "Invalid session path".to_string())?;

    let archived = Session::load_archived(parent_dir, &target.id)
        .map_err(|e| format!("Failed to load session: {e}"))?
        .ok_or_else(|| format!("Session '{}' not found.", filename))?;

    *session = archived;

    // Restore working directory
    if let Some(ref wd) = session.meta.working_dir {
        if let Ok(path) = std::path::PathBuf::from(wd).canonicalize() {
            let _ = std::env::set_current_dir(&path);
            rt_env.working_dir = path.clone();
            app.working_dir = path.display().to_string();
        }
    }

    helpers::replay_session_history(app, &session.messages, rt_env, true);
    app.output.push_system(&format!("Session restored: {} messages from {}", session.messages.len(), filename));
    app.dirty = true;
    app.scroll_to_bottom();
    Ok(())
}
```

- [ ] **Step 2: Update handlers/mod.rs** — add `pub mod session_handler;`

- [ ] **Step 3: Wire into main.rs — replace 3x duplicated sidebar rebuilds**

In `run_app`, replace the sidebar rebuild in each `SessionAction` branch with calls to `handlers::session_handler::rebuild_sidebar(app, &sessions_root, &rt_env.project_id, &helpers::session_display_name(&session))`.

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: PASS

---

### Task 4: Extract Agent Event Handler

**Files:**
- Create: `crates/ox-cli/src/handlers/agent_handler.rs`
- Modify: `crates/ox-cli/src/handlers/mod.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create agent_handler.rs**

Extract the `agent_ev = agent_rx.recv()` arm from `main.rs` into `handle_agent_event`:

```rust
// crates/ox-cli/src/handlers/agent_handler.rs
use ox_core::agent::AgentToUiEvent;
use crate::terminal::app::App;
use crate::terminal::output_pane::OutputLine;

pub fn handle_agent_event(
    app: &mut App,
    ev: AgentToUiEvent,
    session: &mut ox_core::message::Session,
    background_session: &mut Option<ox_core::message::Session>,
) -> AgentEventResult {
    // Match on all AgentToUiEvent variants, handling each one
    // TextChunk → app.output.push_streaming_chunk
    // ToolStart → app.output.push_line(OutputLine::Tool...)
    // ToolResult → app.output.push_line(OutputLine::ToolResult...) + implicit feedback
    // TurnDone → finalize streaming, parse Plan/Done, persist, cost recording
    // Error → finalize streaming, clear agent_running
    // Status → app.status update
    // ToolConfirmationRequest → set pending_confirmation
    // ToolOutputChunk → streaming chunk
    // ToolProgress → tool log
    // BudgetExceeded → confirmation prompt
    // WorkingDirChanged → runtime::change_directory
    // IterationLimitReached → confirmation prompt
    // WorkflowCompleted → auto-reflection
    // (all existing logic preserved exactly)
}

pub enum AgentEventResult {
    Normal,
    InterjectionTriggered,
    BackgroundSessionDone,
}
```

- [ ] **Step 2: Wire into main.rs event loop**

Replace the `agent_ev = agent_rx.recv()` arm body with a call to `handle_agent_event(app, ev, &mut session, &mut background_session)`.

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: PASS

---

### Task 5: Extract Key Handler

**Files:**
- Create: `crates/ox-cli/src/handlers/key_handler.rs`
- Modify: `crates/ox-cli/src/handlers/mod.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create key_handler.rs**

Extract `handle_key_event` from `main.rs` into a free function with reduced parameter count (using AppRuntime once wired):

```rust
// crates/ox-cli/src/handlers/key_handler.rs
use crossterm::event::{KeyCode, KeyModifiers, KeyEvent};
use crate::terminal::app::App;

pub fn handle_key(app: &mut App, key: KeyEvent, runtime: &mut AppRuntimeContext) {
    // Fast path: printable chars
    // Confirmation keys (Y/N/T)
    // Control keys (Ctrl+A/E/U/K/W/C/D)
    // Enter → submit input
    // Backspace/Delete/Left/Right
    // Navigation (arrows, PgUp/PgDn, Home/End)
    // Character input
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: PASS

---

### Task 6: Extract Event Loop

**Files:**
- Create: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create event_loop.rs**

Extract the main `loop { }` from `run_app` into an `EventLoop` struct:

```rust
// crates/ox-cli/src/event_loop.rs
pub struct EventLoop {
    pub indexing_monitor: IndexingMonitor,
    // channels, etc.
}

impl EventLoop {
    pub async fn run(&mut self, app: &mut App, runtime: &mut AppRuntime, ...) -> anyhow::Result<()> {
        loop {
            // Onboarding trigger
            // Implicit feedback
            // Indexing progress drain
            // Render
            // tokio::select! { events / agent_events }
        }
    }
}

pub struct IndexingMonitor {
    phase_rx: UnboundedReceiver<String>,
    progress_rx: UnboundedReceiver<(usize, usize, usize)>,
    done_rx: UnboundedReceiver<usize>,
}

impl IndexingMonitor {
    pub fn drain(&mut self, app: &mut App) -> bool { /* ... */ }
}
```

- [ ] **Step 2: Simplify main.rs to ~150 lines**

`main()` → init logging, panic hook, load config, create provider, setup terminal, create AppRuntime, create EventLoop, call event_loop.run(), restore terminal.

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: PASS

---

### Task 7: Split Agent Engine

**Files:**
- Create: `crates/ox-core/src/agent/tool_executor.rs`
- Create: `crates/ox-core/src/agent/workflow_guard.rs`
- Create: `crates/ox-core/src/agent/error_recovery.rs`
- Create: `crates/ox-core/src/agent/stream_handler.rs`
- Create: `crates/ox-core/src/agent/context_injector.rs`
- Modify: `crates/ox-core/src/agent/mod.rs`
- Modify: `crates/ox-core/src/agent/engine.rs`

- [ ] **Step 1: Create context_injector.rs** — extract task anchoring + knowledge re-injection

```rust
// crates/ox-core/src/agent/context_injector.rs
use crate::message::Message;
use crate::tools::ToolContext;
use std::sync::Arc;

/// Inject task anchoring + periodic knowledge refresh into messages.
pub fn inject_context(
    messages: &mut Vec<Message>,
    user_task: &Option<String>,
    iteration: u32,
    tool_ctx: &ToolContext,
) {
    // Task anchoring every iteration
    // Knowledge refresh every 3 iterations
    // (logic from engine.rs lines 253-289)
}
```

- [ ] **Step 2: Create stream_handler.rs** — extract LLM streaming event processing

```rust
// crates/ox-core/src/agent/stream_handler.rs
use crate::llm::LlmStreamEvent;
// Extract the while-let loop that processes LLM stream events
```

- [ ] **Step 3: Create error_recovery.rs** — extract build/test failure auto-fix

```rust
// crates/ox-core/src/agent/error_recovery.rs
use crate::message::Message;

/// Analyze tool results for build/test failures and generate recovery prompts.
pub fn check_and_recover(messages: &mut Vec<Message>, new_messages: &[Message]) {
    // (logic from engine.rs lines 1566-1664)
}
```

- [ ] **Step 4: Create workflow_guard.rs** — extract workflow advance/validate logic

```rust
// crates/ox-core/src/agent/workflow_guard.rs
use crate::agent::engine::WorkflowEngine;

pub fn advance_on_output(engine: &mut WorkflowEngine, text: &str) -> Option<bool> { /* ... */ }
pub fn advance_after_tools(engine: &mut WorkflowEngine, text: &str) -> Option<bool> { /* ... */ }
pub fn validate_tool_call(engine: &WorkflowEngine, tool_name: &str, args: &serde_json::Value) -> Result<(), String> { /* ... */ }
```

- [ ] **Step 5: Create tool_executor.rs** — extract single tool call lifecycle

```rust
// crates/ox-core/src/agent/tool_executor.rs
use crate::agent::AgentToUiEvent;
use crate::message::Message;
use crate::tools::{ToolRegistry, ToolContext};
use tokio::sync::mpsc;

/// Execute a single tool call — parse args, validate, confirm, execute, handle result.
pub async fn execute_tool_call(
    tc: &crate::message::ToolCall,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    ui_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
    // ... other context
) -> ToolExecResult { /* ... */ }
```

- [ ] **Step 6: Slim down engine.rs** — `run_agent_turn` now ~200 lines, delegates to sub-modules

- [ ] **Step 7: Verify compilation**

Run: `cargo check -p ox-core -p ox-cli`
Expected: PASS

---

### Task 8: Parallelize Startup

**Files:**
- Modify: `crates/ox-cli/src/runtime.rs`

- [ ] **Step 1: Refactor AppRuntime::init to use parallel init**

```rust
impl AppRuntime {
    pub async fn init(config: OxConfig, ...) -> anyhow::Result<Self> {
        // Phase 1: Independent groups
        let (dirs_result, session_result, cmd_registry) = tokio::join!(
            init_directories(&rt_env),
            init_session(&rt_env, &config),
            init_commands(),
        );

        let tool_registry = init_tools(&rt_env)?;

        // Phase 2: Knowledge + Memory in parallel
        let (knowledge, memory) = tokio::join!(
            init_knowledge(&db_dir, &config, &rt_env),
            init_memory(&rt_env, &config),
        );

        // Phase 3: FileWatcher (depends on KnowledgeEngine)
        KnowledgeEngine::start_file_watcher(Arc::clone(&knowledge));

        // ...
    }
}
```

- [ ] **Step 2: Verify compilation and test startup time**

Run: `cargo check -p ox-cli`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor: optimize main flow — split main.rs, unify pre-turn pipeline, deduplicate session management, split engine, parallelize startup"
```

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```
