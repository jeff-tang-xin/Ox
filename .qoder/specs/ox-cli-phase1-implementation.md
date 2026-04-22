# Ox CLI Phase 1 Implementation Plan

## Context

Ox is a Rust-based AI programming CLI agent with REPL interface. The project is greenfield — only a 3700+ line design document exists at `f:\rust\Ox\docs\Ox-CLI-技术设计文档.md`. Phase 1 (REPL Skeleton) needs to deliver a working CLI that can: start up, show a Split-View terminal, stream LLM responses, execute tools, manage sessions, and track costs.

User requirements: Full Phase 1 scope, ratatui Split-View from day one, OpenAI + Anthropic support.

## Cargo Workspace Structure

Two-crate workspace — binary + library:

```
f:\rust\Ox\
├── Cargo.toml                  # workspace root
├── crates/
│   ├── ox-cli/                 # binary: terminal UI, event loop, rendering
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       └── terminal/
│   │           ├── mod.rs
│   │           ├── app.rs          # App state machine (central UI state)
│   │           ├── event.rs        # crossterm event polling (std::thread)
│   │           ├── render.rs       # ratatui layout + draw
│   │           ├── output_pane.rs  # scrollable output, streaming chunks
│   │           ├── input_pane.rs   # line editing, history, cursor
│   │           └── markdown.rs     # syntect code highlighting
│   └── ox-core/                # library: all business logic (zero terminal deps)
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── config/             # OxConfig, TOML loading, defaults
│           ├── message/            # Message enum, Session, TaskPlan
│           ├── llm/                # LlmProvider trait, OpenAI, Anthropic, tokenizer
│           ├── tools/              # Tool trait, ToolRegistry, 12 built-in tools
│           ├── context/            # ContextBuilder, system prompt, effort level
│           ├── agent/              # run_agent_turn, InterruptController, InputBuffer
│           ├── safety/             # SafetyChecker, TrustManager, DataSanitizer
│           ├── cost/               # CostTracker, budget alerts
│           ├── runtime/            # RuntimeEnvironment, project detection, /cd
│           ├── slash/              # SlashCommand parsing + handlers
│           └── shutdown/           # graceful shutdown, signal hooks
├── docs/
├── LICENSE
└── README.md
```

**Rationale**: ox-core has zero terminal dependencies, independently testable with `cargo test`. ox-cli owns everything that touches raw terminal (ratatui, crossterm).

## Key Dependencies

**Workspace shared** (`[workspace.dependencies]`):
- `tokio = "1"` (full), `tokio-util = "0.7"` (CancellationToken)
- `serde = "1"` (derive), `serde_json = "1"`, `toml = "0.8"`
- `reqwest = "0.12"` (stream, json), `futures = "0.3"`
- `async-trait = "0.1"`, `thiserror = "2"`, `tracing = "0.1"`
- `uuid = "1"` (v4), `chrono = "0.4"` (serde)
- `blake3 = "1"`, `which = "6"`, `dirs = "5"`, `regex = "1"`, `glob = "0.3"`

**ox-cli additional**: `ratatui = "0.29"`, `crossterm = "0.28"`, `syntect = "5"`

## Implementation Milestones (10 steps)

### M1: Binary + Split-View terminal echo

**Goal**: `cargo run` shows split terminal, type text → echoes in output pane, `/exit` quits.

**Create**:
- Workspace `Cargo.toml` + both crate Cargo.toml
- `ox-cli/src/main.rs` — tokio::main, crossterm raw mode, ratatui Terminal, event loop, restore on exit
- `ox-cli/src/terminal/app.rs` — `App { output_lines, input_buffer, cursor_pos, scroll_offset, should_quit }`
- `ox-cli/src/terminal/event.rs` — **std::thread** (not tokio) polls crossterm events, sends via `tokio::sync::mpsc::UnboundedSender<Event>`
- `ox-cli/src/terminal/render.rs` — `Layout::vertical([Percentage(85), Min(3)])`, output = Paragraph with scroll, input = Paragraph with cursor
- `ox-cli/src/terminal/output_pane.rs` — `push_line()`, `push_streaming_chunk()`
- `ox-cli/src/terminal/input_pane.rs` — char insert, backspace, cursor movement, Enter emit, Up/Down history
- `ox-core/src/lib.rs` — empty shell

**Architecture**: Main event loop is `tokio::select!` over: (1) event_rx from crossterm thread, (2) tick_interval 33ms for render, (3) agent_event_rx (added M3).

**Verify**: `cargo run` → bordered split terminal → type "hello" → appears in output → `/exit` → clean exit.

---

### M2: Config + runtime detection

**Goal**: Load config.toml with defaults, detect OS/shell/project root, show startup banner.

**Create**:
- `ox-core/src/config/mod.rs` — `OxConfig` with all sections from design doc Section 19, `#[serde(default)]` on every field, `load()` merges file + defaults
- `ox-core/src/config/defaults.rs` — `impl Default` for all config structs
- `ox-core/src/runtime/mod.rs` — `RuntimeEnvironment`, `Os`, `ShellInfo`, `detect_runtime()`, `detect_shell()` (Windows: pwsh→powershell→cmd; Unix: $SHELL)
- `ox-core/src/runtime/project.rs` — `find_project_root()` walks up for .git/Cargo.toml/package.json/etc., `compute_project_id()` via blake3

**Verify**: Start in Rust project → banner: "Project: Ox (Rust) | Windows (pwsh) | f:\rust\Ox".

---

### M3: LLM streaming

**Goal**: Type message → streams to OpenAI/Anthropic → tokens appear character-by-character in output pane.

**Create**:
- `ox-core/src/llm/mod.rs` — `LlmProvider` trait (`stream_chat`), `LlmStreamEvent` enum (TextDelta, ToolCallStart, ToolCallArgumentsDelta, ToolCallEnd, Done, Error)
- `ox-core/src/llm/openai.rs` — `OpenAiProvider`: POST to chat/completions with `stream:true`, parse SSE, yield events via mpsc
- `ox-core/src/llm/anthropic.rs` — `AnthropicProvider`: POST to /v1/messages with `stream:true`, parse Anthropic SSE format
- `ox-core/src/llm/tokenizer.rs` — `Tokenizer` trait, `WhitespaceEstimator` (chars/4)
- `ox-core/src/message/mod.rs` — `Message` enum (User/Assistant/System/ToolCall/ToolResult/UserInterjection), `TokenUsage`, `#[serde(tag = "type")]`

**Integration**: Agent event channel `AgentEvent::StreamChunk(String)` → main loop → `output_pane.push_streaming_chunk()` → render.

API keys: env vars `OX_OPENAI_API_KEY` / `OX_ANTHROPIC_API_KEY` override config.

**Verify**: Set API key env var → type "What is Rust?" → tokens stream into output pane.

---

### M4: Session persistence

**Goal**: Conversation persisted to JSONL, auto-restore on restart.

**Create**:
- `ox-core/src/message/session.rs` — `Session { id, project_id, messages, metadata, file_path }`, `load(path)` reads JSONL (skip malformed last line for crash safety), `append_message()` writes one JSON line + flush, `archive()` moves to sessions/, `new()`
- `ox-core/src/message/task_plan.rs` — `TaskPlan`, `TaskItem`, `TaskStatus`, JSON persistence at `.ox/task_plan.json`

**Session format**: `.ox/session.jsonl`, append-only. Each message = one JSON line.

**Verify**: Conversation → `/exit` → restart → "Session restored (N messages)" → history visible.

---

### M5: Tool system + basic tools

**Goal**: Tool trait, registry, 12 built-in tools.

**Create**:
- `ox-core/src/tools/mod.rs` — `Tool` trait (async_trait: name, description, parameters_schema, execute, safety_level), `ToolRegistry`, `ToolOutput`, `to_llm_tools_schema()`
- 12 tool files: `file_read.rs`, `file_write.rs`, `file_patch.rs`, `file_list.rs`, `file_search.rs`, `code_search.rs`, `shell_exec.rs`, `project_detect.rs`, `git_status.rs`, `git_diff.rs`, `git_commit.rs`, `web_fetch.rs`

**Critical tools**:
- **shell_exec**: `tokio::process::Command` with shell from RuntimeEnvironment, pipe stdout/stderr, stream lines to terminal via agent events, truncate last 50 lines as tool_result for LLM, timeout via `tokio::time::timeout`, CancellationToken → `child.kill()`
- **file_patch**: SearchReplace (count occurrences: 0=error, >1=error, 1=replace), InsertAfterLine, DeleteLines
- **project_detect**: Check marker files, parse project language

**Verify**: Unit tests for each tool with `#[tokio::test]`.

---

### M6: Agent Turn loop

**Goal**: Full LLM → tool_calls → execute → loop → text response cycle.

**Create**:
- `ox-core/src/agent/mod.rs` — `run_agent_turn()`: build context → loop { stream LLM → if tool_calls: execute each → push results → continue; if text: break } → persist session

**Key rules** (from design doc):
- `Message::assistant_tool_calls()` **MUST** precede tool_result messages
- Safety check before each tool execution
- Accumulate `total_usage` across loop iterations

**Verify**: "Read Cargo.toml and tell me what dependencies are listed" → file_read → LLM summarizes. "Create hello.txt with 'Hello World'" → file_write → confirmation → writes → confirms.

---

### M7: Context builder + cost tracking

**Goal**: Percentage-based token budget, cost tracking with `/cost`.

**Create**:
- `ox-core/src/context/mod.rs` — `ContextBuilder` with ratio-based budgets (2/2/36/59 of model window), `build()` assembles: system prompt → memory placeholder (empty for P1) → history (newest-first fill until budget) → current input
- `ox-core/src/context/system_prompt.rs` — Template with P1-P4 principles, runtime env, project info, persona placeholder
- `ox-core/src/context/effort.rs` — `EffortLevel` enum + `estimate_effort()` heuristic
- `ox-core/src/cost/mod.rs` — `CostTracker`, record per-call, daily/monthly totals, budget alerts, persist to `.ox/cost_tracking.json`

**Verify**: Multi-turn conversation → `/cost` shows tokens + USD breakdown.

---

### M8: Safety + TrustManager + slash commands

**Goal**: 3-tier safety, batch confirmation, Phase 1 slash commands.

**Create**:
- `ox-core/src/safety/mod.rs` — `SafetyChecker` (command blacklist, high_risk_apis, 3-ignore force block), `TrustManager` (session-scoped HashSet)
- `ox-core/src/safety/sanitizer.rs` — `DataSanitizer` regex patterns (phone, email, ID, bank card, password)
- `ox-core/src/slash/mod.rs` — `parse_slash_command()`, dispatch
- `ox-core/src/slash/handlers.rs` — /help, /exit, /new, /clear, /model, /cost, /plan, /trust, /untrust, /cd, /config, /feedback (stub), /debug

**Verify**: `/trust file_write` → file_write skips confirmation → `/untrust` → confirmation returns.

---

### M9: Interrupt + interjection + shutdown

**Goal**: Ctrl+C interrupts, user types during agent work, clean shutdown.

**Create**:
- `ox-core/src/agent/interrupt.rs` — `InterruptController { cancel_token, last_ctrl_c }`, double-click (1s) = force
- `ox-core/src/agent/interjection.rs` — `InputBuffer` (mpsc unbounded), `UserInterjection`, Normal vs Urgent (`!` prefix), `drain()` + `try_recv_urgent()`
- `ox-core/src/shutdown/mod.rs` — `shutdown()` (cancel agent → flush buffer → save TaskPlan → flush session → write clean_shutdown marker → restore terminal), `register_shutdown_hooks()`, panic hook

**Ctrl+C in crossterm raw mode**: comes as `KeyCode::Char('c') + CONTROL`. If agent idle → shutdown. If agent running → first=interrupt, double=force, third=shutdown.

**Verify**: During LLM streaming → Ctrl+C → stops, partial text preserved. During agent work → type in input pane → injected at next boundary. `/exit` → clean exit → restart → all data intact.

---

### M10: Polish — markdown, directory switch, remaining

**Goal**: Syntax-highlighted code blocks, `/cd` with project boundary detection, all tools tested end-to-end.

**Create**:
- `ox-cli/src/terminal/markdown.rs` — `StreamRenderer` tracks code block state, syntect highlights accumulated code buffer, converts to ratatui colored Spans
- `ox-core/src/runtime/directory.rs` — `DirectoryChangeResult`, `detect_project_boundary()`, `handle_project_switch()` (save old state → switch RuntimeEnvironment → load new state → rebuild system prompt)

**Verify**: Full E2E — "Read all Rust files, find TODOs, create summary" → agent uses file_list → file_read → code_search → file_write with streaming output, syntax-highlighted code blocks, proper safety confirmations.

## Phase 2 Interfaces (Prepared but not implemented)

- `ContextBuilder.build()` takes `memory_context: &str` — pass `""` in Phase 1
- `agent/mod.rs` calls `ctx.memory.update_from_turn()` — no-op trait impl in Phase 1
- `OxConfig` loads `[memory]`/`[persona]`/`[council]` sections — not consumed yet
- `PersonaVector` fields exist in system prompt template — use static defaults

## Verification Plan

After M10, run this end-to-end sequence:

1. `cargo build` — compiles without errors
2. `cargo test -p ox-core` — all unit tests pass
3. Set `OX_OPENAI_API_KEY` → `cargo run` → startup banner with project/OS/shell info
4. Type "Hello, introduce yourself" → LLM streams response with markdown rendering
5. "Read Cargo.toml" → file_read tool → LLM summarizes
6. "Create test.txt with some content" → file_write → confirmation → success
7. `/trust file_write` → "Write another file" → no confirmation needed
8. `/cost` → shows token breakdown
9. `/plan` → shows task plan (or "No active plan")
10. Ctrl+C during streaming → interrupts cleanly
11. `/cd ..` → directory switch, project boundary detection
12. `/exit` → clean shutdown → restart → session restored
