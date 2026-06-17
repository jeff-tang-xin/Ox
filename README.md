# Ox 🐂

[![Version](https://img.shields.io/badge/version-v2.0.0.02-blue.svg)](https://github.com/nicepkg/ox)
[![Rust](https://img.shields.io/badge/rust-edition%202024-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

> **Terminal AI coding assistant — read first, code second; enforce discipline, ship production-grade output**

Ox is a Rust-built terminal AI coding assistant that understands your codebase before writing a single line. It indexes your project, maintains context across sessions, enforces safety guardrails, and follows structured workflows to deliver reliable, production-quality code.

---

## ✨ Highlights

- **Codebase-aware** — Local BM25 + vector search + entity graph keep your full project in context
- **Structured workflow** — Perception → Plan → Execute → Verify, with dedicated phases for each stage
- **Safety-first** — 3-tier tool safety levels, prompt injection detection, data sanitization, path restrictions
- **Multi-LLM** — Unified adapter for OpenAI and Anthropic APIs with streaming SSE
- **Session persistence** — Full conversation history saved to SQLite, resume anytime
- **16 built-in tools** — File I/O, code search, symbol navigation, shell execution, git, and more
- **Extensible skills** — Load domain-specific instruction packs at runtime
- **Cost tracking** — Real-time token usage and API cost accounting

---

## 🏗 Architecture

```
Ox Workspace
├── crates/
│   ├── ox-core/          # Pure library — no TUI dependencies
│   │   └── src/
│   │       ├── agent/    # Engine, workflow, perception, preflight, enforcer, session
│   │       ├── config/   # OxConfig, AgentConfig, EnforcementRules
│   │       ├── context/  # Builder, budget, system prompt, skill injection
│   │       ├── cost/     # CostTracker (token + dollar accounting)
│   │       ├── feedback/ # EMA tracker, override detector, rollback
│   │       ├── knowledge/# BM25, vector store, entity graph, live update
│   │       ├── llm/      # LlmProvider trait, OpenAI, Anthropic, SSE adapter
│   │       ├── message/  # Message, Session, TaskPlan
│   │       ├── runtime/  # RuntimeEnvironment, directory, project
│   │       ├── safety/   # Injection detection, sanitizer, trust manager
│   │       ├── skill/    # Skill loading and generation
│   │       ├── slash/    # Slash-command dispatch
│   │       ├── symbol/   # AST extraction, embedding, vector store
│   │       └── tools/    # 16 built-in tools + Tool trait + ToolRegistry
│   └── ox-cli/           # Terminal UI binary
│       └── src/
│           ├── main.rs   # Entry point — #[tokio::main]
│           ├── handlers/ # Agent, key, session, pre-turn handlers
│           ├── middleware/# Feedback, interjection
│           ├── slash_commands/ # /help, /model, /session, /skill …
│           └── terminal/ # TUI app, input/output panes, markdown render
```

### Crate boundary

```
ox-cli  →  ox-core  →  (third-party crates only)
```

- **ox-core** is a pure library — it MUST NOT depend on `ratatui`, `crossterm`, `syntect`, or any TUI crate.
- **ox-cli** owns all rendering and user interaction; it MUST NOT call LLM APIs directly.
- Runtime communication uses `mpsc` channels with `AgentToUiEvent` / `UiToAgentEvent` — the only data crossing the boundary.

---

## 🚀 Getting Started

### Prerequisites

- **Rust** 1.85+ (edition 2024)
- **CMake** (for rusqlite bundled build)
- An LLM API key (OpenAI or Anthropic)

### Install

```bash
git clone https://github.com/your-org/ox.git
cd ox
cargo build --release
# Binary at target/release/ox
```

### Configure

Create `~/.ox/config.toml`:

```toml
[llm]
provider = "openai"          # "openai" | "anthropic"
api_key = "sk-..."
model = "gpt-4o"

[agent]
auto_confirm_safe = true     # auto-approve Safe tools
```

All fields have sensible defaults — Ox works with an empty config file.

### Run

```bash
ox                  # Start interactive TUI in current directory
ox index            # Build project index (BM25 + vectors)
ox index --full     # Full re-index including embeddings
```

---

## 🛠 Built-in Tools

| Tool | Safety | Description |
|------|--------|-------------|
| `file_read` | 🟢 Safe | Read file contents with line numbers |
| `file_list` | 🟢 Safe | List directory contents (single level) |
| `file_search` | 🟢 Safe | Search files by glob pattern |
| `find_symbol` | 🟢 Safe | Search symbols by name (AST + semantic) |
| `code_search` | 🟢 Safe | Ripgrep-powered content search |
| `memory_search` | 🟢 Safe | Query knowledge base |
| `git_status` | 🟢 Safe | Show git working tree status |
| `git_diff` | 🟢 Safe | Show git diff |
| `recall` | 🟢 Safe | Retrieve offloaded step results |
| `edit_file` | 🟡 Confirm | Replace text in a file |
| `file_write` | 🟡 Confirm | Create or overwrite a file |
| `delete_range` | 🟡 Confirm | Delete a contiguous text range |
| `load_skill` | 🟡 Confirm | Load an on-demand skill |
| `shell_exec` | 🔴 Dangerous | Execute shell commands |
| `git_commit` | 🔴 Dangerous | Create git commits |
| `web_fetch` | 🔴 Dangerous | Fetch web pages |

### Safety levels

- 🟢 **Safe** — Read-only, executed automatically
- 🟡 **RequiresConfirmation** — Modifies files, prompts user unless trusted
- 🔴 **Dangerous** — Always requires explicit user confirmation

---

## 🧠 Agent Workflow

Ox doesn't free-form chat — it follows a structured pipeline with dedicated phases:

```
┌─────────────┐   ┌─────────────┐   ┌────────────┐   ┌───────────┐   ┌──────────┐
│  Perception  │──▶│   Preflight  │──▶│    Plan     │──▶│  Execute  │──▶│  Verify  │
│  (intent +   │   │ (explore +   │   │ (step list │   │ (tool     │   │ (shell   │
│   pipeline)  │   │  pre-check)  │   │  + targets)│   │  calls)   │   │  checks) │
└─────────────┘   └─────────────┘   └────────────┘   └───────────┘   └──────────┘
      │                                                                   │
      │  fast pipeline: skip plan, go directly to execute                 │
      └───────────────────────────────────────────────────────────────────┘
```

- **Perception** — Classify intent (simple/standard/complex) and select pipeline
- **Preflight** — Explore codebase, gather context, validate assumptions before planning
- **Plan** — Generate ordered step list with target files and tools
- **Execute** — Run tool calls with enforcer intercepts and progress tracking
- **Verify** — Shell checks and result validation

Pipeline shortcuts:
- **Fast** (simple edits) → perception → execute directly
- **Standard** (multi-file changes) → full pipeline, skip preflight
- **Complex** (cross-module refactors) → full pipeline with progress tracking

The enforcer intercepts every tool call before execution, checking path restrictions, pattern blocks, and confirmation gates.

---

## 📚 Knowledge Engine

Ox builds a local knowledge index combining multiple signals:

| Layer | Technology | Purpose |
|-------|-----------|---------|
| Keyword search | BM25 | Fast fuzzy text matching |
| Semantic search | Candle embeddings + vector DB | Concept-level similarity |
| Entity graph | Entity extraction + graph | Cross-reference navigation |
| Live update | File watcher (notify) | Real-time index refresh on file changes |
| AST symbols | Tree-sitter (7 languages) | Structural code navigation |

Supported AST languages: Rust, Python, JavaScript, TypeScript, C++, Go, Java.

---

## ⌨️ Slash Commands

Inside the TUI, type `/` to access commands:

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/model` | Switch LLM model |
| `/session` | Manage sessions (list, switch, new) |
| `/skill` | List and manage skills |
| `/system` | View/edit system prompt |
| `/trust` | Manage tool trust levels |
| `/memory` | Inspect knowledge base |
| `/feedback` | View feedback state |
| `/index` | Re-index project |

---

## 🔧 Configuration

All configuration lives in `~/.ox/config.toml`:

```toml
[llm]
provider = "openai"
api_key = "sk-..."
model = "gpt-4o"
context_window_size = 128000

[agent]
auto_confirm_safe = true
max_iterations = 50

[enforcement]
block_patterns = []           # Regex patterns to block in tool args
restrict_to_project_dir = true

[feedback]
ema_alpha = 0.3
```

Every field has a default — you can start with an empty file and override only what you need.

---

## 🧩 Skills

Skills are instruction packs loaded at runtime from:

- **Built-in**: `coding-principles`, `concise-communication`, `engineering-practices`
- **Project**: `.ox/skills/*.md` in your project root
- **User**: `~/.ox/skills/*.md` in your home directory

Skills inject targeted guidance into the agent's context without modifying the workflow.

---

## 🧪 Development

```bash
cargo build          # Debug build
cargo build --release  # Release build
cargo test           # Run all tests
cargo fmt --check    # Check formatting
cargo clippy         # Lint
```

### Key dependencies

| Category | Crates |
|----------|--------|
| Async | tokio, tokio-util, futures, async-trait |
| LLM / HTTP | reqwest (stream + JSON) |
| Serialization | serde, serde_json, toml |
| TUI | ratatui, crossterm |
| Syntax | syntect, tree-sitter (+ 7 grammars) |
| Search | grep, grep-cli, ignore |
| ML / Embeddings | candle-core/nn/transformers, hf-hub, tokenizers |
| Database | rusqlite (bundled) |
| Vector DB | triviumdb |
| File watching | notify, walkdir |
| Error handling | anyhow |
| Logging | tracing, tracing-subscriber |

---

## 📄 License

MIT
