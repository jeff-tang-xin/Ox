# Ox

> 🐂 AI Programming Assistant — An intelligent agent that runs in your terminal.

Ox is a Rust-based AI programming assistant with a beautiful TUI interface. It connects to LLM providers (OpenAI, Anthropic) and provides a rich set of tools for reading, writing, searching, and executing code — all from the comfort of your terminal.

## ✨ Features

- **🖥️ Terminal UI** — Full-screen ratatui-based interface with markdown rendering and syntax highlighting
- **🤖 Multi-LLM Support** — Works with OpenAI and Anthropic API providers, with a fallback echo mode
- **🔧 Rich Toolset** — 12+ built-in tools for file operations, code search, shell execution, git integration, and more
- **🛡️ Safety System** — Trust management and confirmation prompts for destructive operations
- **💰 Cost Tracking** — Real-time cost tracking for LLM API usage
- **⚡ Slash Commands** — Quick actions via `/` commands
- **🔄 Interrupt & Interjection** — Cancel or inject messages mid-stream

## 📦 Project Structure

```
Ox/
├── crates/
│   ├── ox-cli/          # CLI binary — TUI app entry point
│   │   └── src/
│   │       ├── main.rs
│   │       └── terminal/   # TUI rendering & event handling
│   │           ├── app.rs
│   │           ├── event.rs
│   │           ├── input_pane.rs
│   │           ├── output_pane.rs
│   │           ├── markdown.rs
│   │           └── render.rs
│   └── ox-core/         # Core library — agent, LLM, tools, etc.
│       └── src/
│           ├── agent/       # Agent loop, interjection, interrupt, UI events
│           ├── config/      # Configuration loading & management
│           ├── context/     # Context builder, system prompt, effort levels
│           ├── cost/        # LLM cost tracker
│           ├── llm/         # LLM provider abstraction (OpenAI, Anthropic)
│           ├── message/     # Message & session management
│           ├── runtime/     # Runtime environment detection
│           ├── safety/      # Trust & safety management
│           ├── slash/       # Slash command handler
│           └── tools/       # All built-in tools
│               ├── code_search.rs
│               ├── file_list.rs
│               ├── file_patch.rs
│               ├── file_read.rs
│               ├── file_search.rs
│               ├── file_write.rs
│               ├── git_commit.rs
│               ├── git_diff.rs
│               ├── git_status.rs
│               ├── project_detect.rs
│               ├── shell_exec.rs
│               └── web_fetch.rs
├── docs/
├── Cargo.toml            # Workspace root
├── Cargo.lock
├── LICENSE               # MIT License
└── README.md
```

## 🚀 Getting Started

### Prerequisites

- Rust 1.85+ (edition 2024)
- An LLM API key (OpenAI or Anthropic)

### Build

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release
```

### Run

```bash
# Run directly
cargo run

# Or run the release binary
./target/release/ox
```

### Configuration

Ox loads configuration from `~/.ox/config.toml` (or uses defaults if the file is missing). Set your API keys via environment variables:

```bash
# For OpenAI
export OPENAI_API_KEY="sk-..."

# For Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
```

If no API key is found, Ox runs in **echo mode** — it echoes back your input without calling any LLM, useful for testing the TUI.

## 🛠️ Built-in Tools

| Tool | Description |
|------|-------------|
| `file_read` | Read file contents |
| `file_write` | Write/create files |
| `file_patch` | Apply search-and-replace patches |
| `file_list` | List files & directories |
| `file_search` | Search files by name pattern |
| `code_search` | Search code by text/regex pattern |
| `shell_exec` | Execute shell commands |
| `git_status` | Show git working tree status |
| `git_diff` | Show git changes |
| `git_commit` | Stage files & create commits |
| `project_detect` | Detect project type & language |
| `web_fetch` | Fetch URL content |

## ⚙️ Tech Stack

| Category | Technology |
|----------|------------|
| Language | Rust (edition 2024) |
| Async Runtime | Tokio |
| HTTP Client | Reqwest |
| Terminal UI | Ratatui + Crossterm |
| Syntax Highlighting | Syntect |
| Serialization | Serde + Serde_json + TOML |
| Error Handling | Anyhow + Thiserror |
| Logging | Tracing |
| Hashing | Blake3 |

## 📜 License

This project is licensed under the [MIT License](LICENSE).

Copyright (c) 2026 Jeff Tang