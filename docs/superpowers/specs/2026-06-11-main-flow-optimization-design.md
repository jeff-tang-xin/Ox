# Main Flow Optimization Design

**Date:** 2026-06-11
**Status:** Approved
**Scope:** Full main flow refactoring — `main.rs`, `engine.rs`, pre-turn pipeline, session management, startup

---

## Motivation

The Ox project's main flow has accumulated significant technical debt:

- `main.rs` at 2300+ lines with `run_app` alone at 1200+ lines
- `engine.rs` `run_agent_turn` at 1800+ lines — tool execution, workflow advancement, error recovery, build/test auto-fix all in one function
- Pre-turn context building logic duplicated 3 times (onboarding, slash commands, normal text input)
- Session management logic (New/Resume/SwitchNext) duplicated 3 times with identical sidebar rebuild code
- Subsystem initialization is fully serial despite no hard dependencies between several components

## Design Goals

1. **Single Responsibility**: Each file/module < 400 lines, one clear purpose
2. **DRY**: Eliminate all duplicated context-building and session-management logic
3. **Parallelizable startup**: Independent subsystems initialize concurrently
4. **Backward compatible**: No changes to external API, message formats, session storage, or tool schemas

---

## Architecture Changes

### 1. `main.rs` Decomposition

**Current:** ~2300 lines in `main.rs`, `run_app` ~1200 lines, `handle_key_event` 19 parameters

**Target:**

```
crates/ox-cli/src/
├── main.rs                    (~150 lines) — entry + terminal setup/teardown
├── runtime.rs                 (NEW, ~200 lines) — AppRuntime struct, subsystem init
├── event_loop.rs              (NEW, ~300 lines) — tokio::select! event loop
├── handlers/
│   ├── mod.rs                 (NEW)
│   ├── key_handler.rs         (NEW, ~250 lines) — user key events dispatch
│   ├── agent_handler.rs       (NEW, ~300 lines) — AgentToUiEvent processing
│   ├── session_handler.rs     (NEW, ~200 lines) — unified session switch logic
│   └── pre_turn.rs            (NEW, ~250 lines) — unified pre-turn pipeline
├── slash_commands/            (existing, minor changes)
├── terminal/                  (existing, no changes)
├── helpers/                   (existing, minor changes)
└── middleware/                 (existing, no changes)
```

**Key struct — `AppRuntime`:**

```rust
pub struct AppRuntime {
    pub config: OxConfig,
    pub rt_env: RuntimeEnvironment,
    pub provider: Option<Arc<dyn LlmProvider>>,
    pub resolve_info: Option<ProviderResolveInfo>,
    pub tool_registry: Arc<ToolRegistry>,
    pub command_registry: CommandRegistry,
    pub context_builder: ContextBuilder,
    pub context_window: u32,
    pub knowledge_engine: Arc<RwLock<KnowledgeEngine>>,
    pub memory: Arc<MemoryManager>,
    pub cost_tracker: CostTracker,
    pub trust_manager: Arc<Mutex<TrustManager>>,
    pub agent_config: Arc<AgentConfig>,
    pub compressed_ctx_store: Arc<CompressedContextStore>,
    pub interrupt_ctrl: InterruptController,
    pub agent_tx: UnboundedSender<AgentToUiEvent>,
}
```

This eliminates the 19-parameter `handle_key_event` signature — it becomes `fn handle(app: &mut App, runtime: &AppRuntime, key: KeyEvent)`.

### 2. Unified Pre-Turn Pipeline

**Current:** 3 copy-pasted context-building paths:
- Onboarding trigger (main.rs ~line 596-665)
- Slash command LlmRequest (main.rs ~line 1848-1943)
- Normal user text input (main.rs ~line 1992-2251)

**Target:** Single `PreTurnPipeline` with a `TurnVariant` enum:

```rust
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

pub async fn prepare_turn(
    runtime: &AppRuntime,
    user_text: &str,
    session_messages: &[Message],
    compressed_cache: &Option<(Vec<Message>, usize)>,
    variant: TurnVariant,
    workflow_engine: &Option<Arc<Mutex<WorkflowEngine>>>,
    session_id: &str,
) -> Result<PreTurnContext>
```

Pipeline steps (all three callers share):
1. Injection scan + sanitize
2. Knowledge retrieval (step-aware if workflow active)
3. Git/Dir context gathering (via `spawn_blocking`)
4. System prompt building
5. Context builder assembly (with compressed cache merge)
6. Effort estimation → planning flag

All three call sites reduced from ~150 lines each to ~20 lines each. ~250 lines of duplicated code eliminated.

### 3. Agent Engine Split

**Current:** `run_agent_turn` is ~1800 lines in `engine.rs`

**Target structure:**

```
crates/ox-core/src/agent/
├── engine.rs                 (~200 lines) — top-level loop skeleton
├── tool_executor.rs          (NEW, ~300 lines) — single tool_call lifecycle
├── workflow_guard.rs         (NEW, ~200 lines) — workflow advance/validate
├── error_recovery.rs         (NEW, ~150 lines) — build/test failure auto-fix
├── stream_handler.rs         (NEW, ~150 lines) — LLM stream event processing
├── context_injector.rs       (NEW, ~100 lines) — task anchoring + knowledge re-injection
│
├── mod.rs                    (unchanged)
├── auto_reflect.rs           (unchanged)
├── context_offloader.rs      (unchanged)
├── enforcer.rs               (unchanged)
├── interjection.rs           (unchanged)
├── interrupt.rs              (unchanged)
├── intervention.rs           (unchanged)
├── progress.rs               (unchanged)
├── session.rs                (unchanged)
├── task_canvas.rs            (unchanged)
├── ui_event.rs               (unchanged)
└── workflow.rs               (minor changes)
```

**`tool_executor.rs`** — extracts tool execution lifecycle:
- Parse + clean arguments (think tags, JSON validation)
- Safety level check + confirmation flow
- Tool execution with retry (transient failures)
- Result offloading + injection scan
- Verify-after-edit injection
- Progress event emission

**`workflow_guard.rs`** — extracts workflow-related logic:
- `advance_on_output`: check if LLM text output should advance workflow
- `advance_after_tools`: check if tool execution should advance workflow
- Path validation (Spec/Council mode)
- Confirmation flag management

**`error_recovery.rs`** — extracts build/test failure self-repair:
- Detect `shell_exec` with build/test commands
- Parse exit codes
- Generate structured recovery prompts (attempt 1/2/3)
- Exhaustion detection (≥3 attempts)

### 4. Session Management Unification

**Current:** `SessionAction::New`, `SessionAction::Resume`, `SessionAction::SwitchNext` each contain ~100 lines of nearly identical sidebar rebuild logic.

**Target:** `session_handler.rs`:

```rust
pub struct SessionManager;

impl SessionManager {
    /// Unified session switch handling — single path for New/Resume/SwitchNext.
    pub fn handle_session_action(
        app: &mut App,
        session: &mut Session,
        action: SessionAction,
        rt_env: &RuntimeEnvironment,
        agent_running: bool,
    ) -> Result<Option<Session>>

    /// Rebuild sidebar session list from disk — called exactly once, shared by all paths.
    fn rebuild_sidebar(app: &mut App, sessions_root: &Path, active_project_id: &str)
}
```

~300 lines of duplicated code → ~150 lines total. One place to fix sidebar bugs.

### 5. Startup Parallelization

**Current:** Fully serial initialization (~10 steps in sequence).

**Target:** Two-phase parallel init:

```rust
// Phase 1: Independent groups run in parallel
let (session_init, tool_system, cmd_registry) = tokio::join!(
    init_session_and_dirs(&rt_env, &config),     // ~50ms
    init_tool_system(&rt_env),                    // ~100ms (skill loading)
    init_slash_commands(),                         // ~1ms
);

let (knowledge, memory) = tokio::join!(
    init_knowledge_engine(&db_dir, &config),      // ~300ms (embedding model load)
    init_memory_system(&rt_env, &config),          // ~50ms
);

// Phase 2: Depends on KnowledgeEngine being ready
KnowledgeEngine::start_file_watcher(Arc::clone(&knowledge));  // fire-and-forget
```

Estimated startup reduction: ~600ms → ~400ms (embedding model load is the bottleneck, but memory/system/tools init now overlaps with it).

### 6. Background Indexing Monitoring Extraction

**Current:** Indexing progress channel draining is inline in the main loop (~50 lines).

**Target:** Encapsulated into `IndexingMonitor` struct in `event_loop.rs`:

```rust
struct IndexingMonitor {
    phase_rx: UnboundedReceiver<String>,
    progress_rx: UnboundedReceiver<(usize, usize, usize)>,
    done_rx: UnboundedReceiver<usize>,
}

impl IndexingMonitor {
    fn drain(&mut self, app: &mut App) -> (bool, u64) // (dirty, tick_delta)
}
```

---

## What Does NOT Change

- **External API**: `AgentToUiEvent`, tool schemas, slash command signatures — all preserved
- **Session storage**: JSONL format, `Session::load`/`Session::archive` — identical behavior
- **UI rendering**: `terminal/` module untouched
- **Config format**: `OxConfig`, `AgentConfig` — no schema changes
- **Message types**: `Message` enum — no additions or changes

## Verification Strategy

1. **Compile check**: `cargo build` must succeed at each refactoring step
2. **Existing functionality**: Manual smoke test — startup, send message, /new, /resume, /help, session switch
3. **No behavioral diffs**: Session JSONL files before and after must be structurally equivalent

## Risk Assessment

- **Low risk**: All changes are internal refactoring, no API/schema changes
- **Mitigation**: Incremental commits — each file extraction is its own commit, buildable independently
- **Rollback**: Each commit can be reverted independently without affecting others
