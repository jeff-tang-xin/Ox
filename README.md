# Ox 🐂

> **Terminal AI Coding Assistant — Understands your project, remembers context, reflects and writes code**

Ox is a Rust-based terminal AI coding assistant that interacts with LLMs through a beautiful TUI interface, providing code reading, writing, searching, and execution capabilities. It supports **OpenAI**, **Anthropic**, **DeepSeek**, and any OpenAI-compatible API. Features **four-layer progressive memory**, **context refinement & offloading**, **auto-reflection Skill generation**, **symbol-aware search**, and **enforcement-based safety**.

---

## ✨ Core Features

| Feature | Description |
|---------|-------------|
| 🤖 **Smart Code Assistant** | File read/write/edit, code search (ripgrep), AST symbol search, shell execution, git status/diff, project detection |
| 🧠 **Four-Layer Progressive Memory** | L0 Raw Conversation → L1 Atomic Facts → L2 Scenario Chunks → L3 Project Persona (SQLite + Markdown hybrid storage) |
| 🔄 **Context Refinement** | Auto-condenses verbose conversations into compact summaries, removes thinking blocks, preserves key decisions |
| 📦 **Context Offloading** | Long tool outputs auto-saved to `.ox/refs/`, context keeps summary + reference only, 60%+ token savings |
| 🪞 **Auto-Reflection & Skill Generation** | Analyzes execution traces post-workflow, extracts reusable patterns into Skills (Markdown) |
| 🔍 **Symbol-Aware Search** | tree-sitter AST parsing + local vector embedding (Candle) for semantic symbol search across 7 languages |
| 🛡️ **Enforcement Rules** | Plan-before-edit, read-before-edit, steps-before-shell, impact-analysis — enforced at code level, not just prompting |
| ⚠️ **Layered Safety** | Three safety tiers (Safe / RequiresConfirmation / Dangerous), session-scoped trust manager, prompt injection defense |
| 💬 **Interactive Feedback** | Interrupt AI anytime, implicit feedback detection, EMA trend tracking |
| 🎯 **User Intent Detection** | Automatic classification (Exploration / CodeUnderstanding / CodeModification / General) for smart context assembly |

---

## 🏗️ Architecture

```
Ox/
├── crates/
│   ├── ox-core/              # Core library — pure logic, no TUI dependency
│   │   └── src/
│   │       ├── agent/        # Agent loop, engine, enforcer, session, auto-reflect, context offloader
│   │       ├── llm/          # LLM providers (OpenAI, Anthropic, DeepSeek), SSE streaming, tokenizer
│   │       ├── tools/        # Tool trait + 17 tool implementations
│   │       ├── context/      # Context building, compression, refinement, system prompt
│   │       ├── memory/       # Memory nodes, store, layering, hybrid storage, semantic, vector
│   │       ├── skill/        # Skill loading (System/Global/Project), generation
│   │       ├── config/       # TOML config with serde defaults + enforcement rules
│   │       ├── safety/       # Trust manager, injection defense, path sanitizer
│   │       ├── symbol/       # AST extraction, embedding, vector store (7 languages)
│   │       ├── feedback/     # Feedback tracking and EMA trend analysis
│   │       ├── message/      # Message types, session persistence
│   │       ├── cost/         # Token cost tracking
│   │       ├── runtime/      # Environment detection
│   │       └── slash/        # Slash command definitions
│   └── ox-cli/               # Terminal UI binary
│       └── src/
│           ├── terminal/     # Ratatui TUI (app, render, events, input/output panes, markdown)
│           ├── slash_commands/  # /help, /config, /memory, /skill, /trust, /model, etc.
│           ├── middleware/    # Request/response middleware (feedback, interjection)
│           └── helpers/      # Utility functions (formatting, session, input, context)
└── .ox/skills/               # Project-level skill files (Markdown)
```

### Layer Boundaries

```
ox-cli (TUI) ──channels──▶ ox-core (business logic)
  terminal / slash_commands ──▶ agent ──▶ tools / llm / memory / context
  UI events ⟷ Agent events (mpsc channels)
```

- **ox-cli** owns the TUI and user interaction; depends on `ox-core`
- **ox-core** is pure logic; never depends on TUI types
- **Agent** orchestrates: receives messages, calls LLM, dispatches tools, emits UI events
- **Tools** implement the `Tool` trait; `ToolRegistry` dispatches by name
- **Enforcer** intercepts tool calls before execution to validate rules

---

## 🚀 Quick Start

### 1. Install

```bash
git clone https://github.com/jeff-tang-xin/Ox.git
cd Ox
cargo build --release
```

Binary at `target/release/ox` (Linux/macOS) or `target/release/ox.exe` (Windows).

### 2. Configure API Key

Ox supports multiple LLM providers:

```bash
# OpenAI (or compatible API)
export OPENAI_API_KEY=sk-...
export OPENAI_BASE_URL=https://api.openai.com/v1   # optional custom endpoint

# Anthropic
export ANTHROPIC_API_KEY=sk-ant-...

# DeepSeek
export DEEPSEEK_API_KEY=sk-...

# Generic format (highest priority, overrides above)
export OX_OPENAI_API_KEY=sk-...
export OX_ANTHROPIC_API_KEY=sk-ant-...
```

Windows PowerShell:
```powershell
$env:OPENAI_API_KEY="sk-..."
```

### 3. Launch

```bash
ox
```

---

## 💬 Interaction

### TUI Mode (default)

Type directly in the terminal interface:

```
> Help me refactor the auth module's login logic
> Add user avatar upload feature
> Why is this test failing?
```

### Command-Line Mode

```bash
ox "Explain this code" --file src/auth.rs   # with file context
ox --no-tui "implement a quicksort"          # no TUI mode
```

---

## ⚡ Slash Commands

### Common Commands

| Command | Description |
|---------|-------------|
| `/exit` | Exit program |
| `/clear` | Clear current session |
| `/debug` | Toggle debug mode |
| `/cost` | Show token cost |
| `/reload` | Reload configuration |
| `/cd <path>` | Change working directory |
| `/cancel` | Cancel current operation |
| `/plan` | View session plan |
| `/model <name>` | Switch model |
| `/skill` | Manage Skills |
| `/trust` | Manage trust mode |
| `/system` | View/edit system prompt |

### Feedback Commands

| Command | Description |
|---------|-------------|
| `/Y` | Confirm / Agree |
| `/N` | Reject |
| `/O <text>` | Offer alternative or feedback |

### Memory Management

| Command | Description |
|---------|-------------|
| `/memory show` | Show current memories |
| `/memory search <query>` | Search memories |
| `/memory transform` | Manually trigger memory transform (L0→L1→L2→L3) |

---

## 🛠️ Built-in Tools

Ox has 17 built-in tools:

| Tool | Safety Level | Description |
|------|-------------|-------------|
| `file_read` | Safe | Read file content (multi-encoding, line numbers) |
| `file_list` | Safe | List directory structure |
| `file_search` | Safe | Search files by glob pattern |
| `code_search` | Safe | High-performance ripgrep regex code search |
| `find_symbol` | Safe | AST semantic symbol search (Rust, Python, JS/TS, C++, Go, Java) |
| `memory_search` | Safe | Search memory knowledge base |
| `recall` | Safe | Retrieve offloaded content by node_id |
| `project_detect` | Safe | Detect project language/framework |
| `web_fetch` | Safe | Fetch URL content |
| `git_status` | Safe | View git working tree status |
| `git_diff` | Safe | View git diff (staged/unstaged) |
| `shell_exec` | Dangerous | Execute shell commands |
| `file_write` | RequiresConfirmation | Create or overwrite file |
| `edit_file` | RequiresConfirmation | Precise text editing (single/multi-edit, replace-all, fuzzy match) |
| `delete_range` | RequiresConfirmation | Delete code block by start/end anchors |
| `content_validation` | Internal | Validate content before write operations |
| `intent_classifier` | Internal | Classify user intent for context assembly |

---

## 🛡️ Enforcement Rules

Ox enforces coding discipline at the **code level** — not just via prompting. These rules are configurable in `config.toml`:

| Rule | Default | Description |
|------|---------|-------------|
| `plan_before_edit` | ✅ | LLM must propose a plan before calling `file_write` / `edit_file` |
| `read_before_edit` | ✅ | LLM must read the target file first (prevents guessing content) |
| `steps_before_shell` | ✅ | LLM must list steps before calling `shell_exec` |
| `impact_analysis` | ✅ | LLM must search for callers/dependents before modifying source files |

Custom patterns can be added via `custom_plan_patterns` and `custom_step_patterns`.

---

## 🧠 Memory Architecture

### Four-Layer Progressive Memory

```
L0 Raw Conversation  ──refine──▶  L1 Atomic Facts
       (SQLite)                       (SQLite)

L1 Atomic Facts      ──cluster──▶  L2 Scenario Chunks
                                      (Markdown)

L2 Scenario Chunks   ──abstract──▶  L3 Project Persona
                                      (Markdown)
```

| Layer | Storage | Purpose |
|-------|---------|---------|
| **L0** Raw Conversation | SQLite | Full conversation logs, fast indexed queries |
| **L1** Atomic Facts | SQLite | Refined atomic facts extracted from conversations |
| **L2** Scenario Chunks | Markdown files | Clustered scenarios, human-readable & editable |
| **L3** Project Persona | Markdown files | Project-level patterns & preferences |

### Hybrid Storage

- **SQLite** (L0–L1): Fast indexed storage with bidirectional traceability via `node_id`
- **Markdown** (L2–L3): Human-readable "white-box" files in `.ox/knowledge/`, directly editable
- **Vector search**: Local Candle embeddings + TriviumDB for semantic retrieval

---

## 🔧 Configuration

### Config File Location

- **Default**: `~/.ox/config.toml`
- **Fallback**: `~/.config/ox/config.toml`
- **Custom**: Set `OX_CONFIG_PATH` environment variable

### Full Configuration Example

```toml
# ── Model Config ──────────────────────────────────────
[models]
default = "gpt-4o"
backup = ["claude-sonnet-4", "gpt-4-turbo"]
adaptive_thinking = true
effort_level = "high"


[models.providers.openai]
api_key = "sk-..."              # or use env var OPENAI_API_KEY
base_url = "https://api.openai.com/v1"
max_tokens = 4096

[models.providers.anthropic]
api_key = "sk-ant-..."          # or use env var ANTHROPIC_API_KEY
max_tokens = 8192

[models.providers.deepseek]
api_key = "sk-..."              # or use env var DEEPSEEK_API_KEY
base_url = "https://api.deepseek.com/v1"

# ── Agent Config ───────────────────────────────────────
[agent]
max_iterations = 25
max_per_turn_tokens = 500000

# ── Safety Config ──────────────────────────────────────
[safety]
enable_sandbox = false
confirm_dangerous_ops = true
high_risk_apis = [
    "Command::new",
    "remove_dir_all",
    "fs::remove_dir_all",
]

# ── Memory Config ──────────────────────────────────────
[memory]
max_nodes = 1000

# ── Enforcement Rules ──────────────────────────────────
[enforcement_rules]
enabled = true
plan_before_edit = true
read_before_edit = true
steps_before_shell = true
impact_analysis = true
# custom_plan_patterns = []
# custom_step_patterns = []

# ── Embedding Config ───────────────────────────────────
[embedding]
model = "all-MiniLM-L6-v2"
```

---

## 🪞 Skill System

Skills are Markdown files that inject behavioral guidance into the LLM context. Three scopes:

| Scope | Location | Description |
|-------|----------|-------------|
| **System** | Built-in (`ox-core/src/skill/builtin/`) | Hardcoded, always loaded |
| **Global** | `~/.ox/skills/` | User-level, applied to all projects |
| **Project** | `.ox/skills/` | Project-specific, version-controllable |

### Built-in Skills

- **coding-principles** — Think before coding, simplicity first, surgical changes, goal-driven execution
- **concise-communication** — Direct, no-fluff communication style
- **engineering-practices** — Universal engineering practices: file organization, documentation, testing

### Auto-Generated Skills

After completing a workflow, the **Auto-Reflector** analyzes the execution trace and can generate new project-level Skills in `.ox/skills/`.

---

## 📦 Key Dependencies

| Dependency | Role |
|-----------|------|
| `tokio` | Async runtime, channels, mutex |
| `reqwest` | HTTP client for LLM APIs |
| `serde` + `serde_json` + `toml` | Serialization everywhere |
| `ratatui` + `crossterm` | Terminal UI (ox-cli only) |
| `rusqlite` | Persistent storage (memory, sessions) |
| `blake3` | File content hashing |
| `grep` + `ignore` + `termcolor` | Ripgrep-based code search |
| `tree-sitter` (7 languages) | AST parsing for symbol extraction |
| `candle` + `hf-hub` + `tokenizers` | Local vector embeddings (MiniLM) |
| `triviumdb` | Embedded vector database |
| `syntect` | Syntax highlighting in TUI |
| `pulldown-cmark` | Markdown rendering |
| `tracing` | Structured logging |
| `anyhow` | Error handling |

---

## 📄 License

MIT
