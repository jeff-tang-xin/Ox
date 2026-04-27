# Ox

> 🐂 AI 编程助手 — 一个运行在终端里的智能体，懂你的项目、记得你的偏好、能写代码。

Ox 是一个基于 Rust 开发的 AI 编程助手，拥有精美的 TUI 界面。它连接 LLM 提供商（OpenAI、Anthropic），提供丰富的工具集用于读取、写入、搜索和执行代码——全部在终端中完成。

## ✨ 特性

- **🖥️ 终端界面** — 全屏 ratatui 界面，支持 Markdown 渲染和语法高亮
- **🤖 多 LLM 支持** — 支持 OpenAI 和 Anthropic API，无 API 时自动降级为回显模式
- **🔧 丰富工具集** — 12+ 内置工具：文件操作、代码搜索、Shell 执行、Git 集成等
- **🛡️ 安全体系** — 信任管理和破坏性操作确认机制
- **💰 成本追踪** — 实时统计 LLM API 调用费用
- **📝 会话持久化** — 自动保存对话历史，重启后恢复
- **⚡ 斜杠命令** — `/help`、`/clear`、`/cost` 等快捷操作
- **🔄 中断与插话** — 支持流式输出中取消或插入新消息
- **🖱️ 鼠标滚轮** — 支持鼠标滚动浏览历史输出
- **🧠 记忆系统** (Phase 2) — SQLite 持久化，自动提取项目上下文和编码偏好
- **🎭 人格系统** (Phase 2) — 自适应 PersonaVector，按编程语言差异化
- **📊 反馈系统** (Phase 2) — 显式 `/feedback` + 隐式行为信号采集

## 📦 项目结构

```
Ox/
├── crates/
│   ├── ox-cli/          # CLI 可执行文件 — TUI 应用入口
│   │   └── src/
│   │       ├── main.rs
│   │       └── terminal/        # TUI 渲染与事件处理
│   │           ├── app.rs           # 应用状态
│   │           ├── event.rs         # 事件轮询（键盘/鼠标）
│   │           ├── input_pane.rs    # 输入框
│   │           ├── output_pane.rs   # 输出面板（带渲染缓存）
│   │           ├── markdown.rs      # Markdown 渲染
│   │           └── render.rs        # 整体布局渲染
│   └── ox-core/         # 核心库 — 智能体、LLM、工具等
│       └── src/
│           ├── agent/       # 智能体循环、插话、中断、UI 事件
│           ├── config/      # 配置加载与管理 (~/.ox/config.toml)
│           ├── context/     # 上下文构建器、系统提示词、努力等级
│           ├── cost/        # LLM 费用追踪
│           ├── llm/         # LLM 提供商抽象（OpenAI、Anthropic）
│           ├── memory/      # 记忆系统（SQLite 存储、提取、衰减）
│           ├── message/     # 消息协议与会话管理
│           ├── runtime/     # 运行时环境检测
│           ├── safety/      # 信任与安全管理
│           ├── slash/       # 斜杠命令处理器
│           └── tools/       # 全部内置工具
│               ├── code_search.rs      # 代码搜索（正则/文本）
│               ├── file_list.rs        # 列出文件/目录
│               ├── file_patch.rs       # 搜索替换补丁
│               ├── file_read.rs        # 读取文件
│               ├── file_search.rs      # 按名称搜索文件
│               ├── file_write.rs       # 写入/创建文件
│               ├── git_commit.rs       # 提交 Git 变更
│               ├── git_diff.rs         # 查看 Git 差异
│               ├── git_status.rs       # 查看 Git 状态
│               ├── project_detect.rs   # 项目类型/语言检测
│               ├── shell_exec.rs       # 执行 Shell 命令
│               └── web_fetch.rs        # 抓取网页内容
├── docs/                          # 技术文档
├── Cargo.toml                     # Workspace 根
├── Cargo.lock
├── LICENSE                        # MIT 许可证
└── README.md
```

## 🚀 快速开始

### 环境要求

- Rust 1.85+ (edition 2024)
- LLM API 密钥（OpenAI 或 Anthropic，可选）

### 构建

```bash
# 调试构建
cargo build

# 发布构建（优化版）
cargo build --release
```

### 运行

```bash
# 直接运行
cargo run

# 或运行发布版二进制
./target/release/ox
```

### 配置

Ox 从 `~/.ox/config.toml` 加载配置（文件不存在时使用默认值）。通过环境变量设置 API 密钥：

```bash
# OpenAI
export OPENAI_API_KEY="sk-..."

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
```

如果未找到 API 密钥，Ox 以**回显模式**运行——原样返回你的输入，不调用任何 LLM，适合测试 TUI 界面。

### 目录结构

Ox 在以下位置存储数据：

| 路径 | 用途 |
|------|------|
| `~/.ox/config.toml` | 用户配置文件 |
| `~/.ox/sessions/` | 会话持久化（按项目隔离） |
| `~/.ox/db/` | SQLite 数据库（记忆、费用追踪） |
| `~/.ox/logs/` | 日志文件 |
| `~/.ox/skills/` | 技能文件 |
| `~/.ox/memory/` | 记忆文件 |
| `<项目根目录>/.ox/` | 项目级配置（项目信息、技能、记忆） |

## 🛠️ 内置工具

| 工具 | 说明 |
|------|------|
| `file_read` | 读取文件内容 |
| `file_write` | 写入/创建文件 |
| `file_patch` | 应用搜索替换补丁 |
| `file_list` | 列出文件和目录 |
| `file_search` | 按文件名模式搜索 |
| `code_search` | 按文本/正则搜索代码 |
| `shell_exec` | 执行 Shell 命令 |
| `git_status` | 查看 Git 工作区状态 |
| `git_diff` | 查看 Git 变更 |
| `git_commit` | 暂存并提交 |
| `project_detect` | 检测项目类型和语言 |
| `web_fetch` | 抓取网页内容 |

## ⌨️ 快捷键

| 快捷键 | 功能 |
|--------|------|
| `Enter` | 发送消息 |
| `Ctrl+C` | 中断当前 LLM 响应 |
| `Ctrl+D` | 退出 Ox |
| `↑ / ↓` | 历史消息导航 |
| `Shift+↑ / ↓` | 滚动输出面板 |
| `PageUp / PageDown` | 翻页 |
| `Ctrl+A / Ctrl+E` | 光标移至行首/行尾 |
| `Ctrl+U` | 删除光标前内容 |
| 鼠标滚轮 | 滚动输出面板 |

## ⚙️ 技术栈

| 类别 | 技术 |
|------|------|
| 语言 | Rust (edition 2024) |
| 异步运行时 | Tokio |
| HTTP 客户端 | Reqwest |
| 终端 UI | Ratatui + Crossterm |
| 语法高亮 | Syntect |
| 序列化 | Serde + Serde_json + TOML |
| 错误处理 | Anyhow + Thiserror |
| 日志 | Tracing |
| 数据库 | SQLite (rusqlite) |
| 哈希 | Blake3 |

## 📜 License

本项目采用 [MIT 许可证](LICENSE)。

Copyright (c) 2026 Jeff Tang
