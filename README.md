# Ox 🐂

> **终端 AI 编程助手 — 先读代码，再动手；记住上下文，强制纪律，写生产级代码**

Ox 是一个用 Rust 构建的终端 AI 编程助手，拥有精美的 TUI 界面。它不只是聊天 —— 它**编辑前必先读文件**、**写入前必须出方案**、**跨会话记忆上下文**、**在代码层面强制编码纪律**，而非仅靠提示词约束。

## 为什么选择 Ox？

| 痛点 | Ox 的解决方案 |
|------|--------------|
| AI 猜测文件内容 | **先读后改** — 代码层面强制 LLM 修改前必须先读取目标文件 |
| AI 不规划就改代码 | **先方案后编辑** — `file_write` / `edit_file` 前必须提出修改方案 |
| 上下文爆炸 | **智能卸载** — 长输出保存到 `.ox/refs/`，上下文仅保留摘要（节省 60%+ Token） |
| 跨会话无记忆 | **四层渐进记忆** (L0→L3)，SQLite + Markdown 混合存储 |
| 不理解项目结构 | **符号感知搜索** — tree-sitter AST + 本地向量嵌入，支持 7 种语言 |
| 工具执行不安全 | **分层安全机制** — Safe / 需确认 / 危险，带会话级信任管理器 |

---

## ✨ 核心特性

- **17 个内置工具** — 文件读写/编辑、ripgrep 代码搜索、AST 符号搜索、Shell 执行、Git 操作、网页抓取、记忆搜索、项目检测
- **四层记忆系统** — L0 原始对话 → L1 原子事实 → L2 场景块 → L3 项目人设（SQLite + Markdown + 本地向量库）
- **上下文精炼与卸载** — 自动压缩冗长对话；长工具输出卸载到 `.ox/refs/`，保留摘要 + 引用
- **强制规则** — `plan_before_edit`、`read_before_edit`、`steps_before_shell`、`impact_analysis` — 可配置，Rust 代码层面强制执行
- **符号感知搜索** — tree-sitter AST 解析 + Candle 本地向量嵌入 + TriviumDB 向量搜索（Rust、Python、JS/TS、C++、Go、Java）
- **自动反思与技能生成** — 工作流结束后分析执行轨迹，提取可复用模式为 Markdown 技能
- **分层安全** — 三个安全级别，会话级信任域，提示注入防御
- **交互式反馈** — 随时中断、隐式反馈检测、EMA 趋势追踪
- **多 LLM 提供商** — OpenAI、Anthropic、DeepSeek 及任何 OpenAI 兼容 API

---

## 🏗️ 架构

```
Ox/
├── crates/
│   ├── ox-core/              # 核心库 — 纯逻辑，不依赖 TUI
│   │   └── src/
│   │       ├── agent/        # Agent 循环、引擎、强制器、会话、自动反思、卸载器
│   │       ├── llm/          # LLM 提供商 (OpenAI, Anthropic, DeepSeek)，SSE，分词器
│   │       ├── tools/        # Tool trait + 17 个工具实现
│   │       ├── context/      # 上下文构建、压缩、精炼
│   │       ├── memory/       # 记忆节点、存储、分层、混合存储、向量
│   │       ├── skill/        # 技能加载（系统/全局/项目），生成
│   │       ├── config/       # TOML 配置，serde 默认值
│   │       ├── safety/       # 信任管理器、注入防御、路径清洗
│   │       ├── symbol/       # AST 提取、嵌入、向量存储
│   │       ├── feedback/     # 反馈追踪和 EMA 趋势
│   │       ├── message/      # 消息类型、会话持久化
│   │       ├── cost/         # Token 成本追踪
│   │       └── runtime/      # 环境检测
│   └── ox-cli/               # 终端 UI 二进制
│       └── src/
│           ├── terminal/     # Ratatui TUI（应用、渲染、事件、输入/输出、Markdown）
│           ├── slash_commands/  # /help, /config, /memory, /skill, /trust, /model…
│           ├── middleware/    # 请求/响应中间件（反馈、插话）
│           └── helpers/      # 格式化、会话、输入工具
└── .ox/skills/               # 项目级技能文件
```

### 层级边界

```
ox-cli (TUI) ──mpsc 通道──▶ ox-core (业务逻辑)
  terminal / slash_commands ──▶ agent ──▶ tools / llm / memory / context
```

- **ox-cli** 拥有 TUI 和用户交互；依赖 `ox-core`
- **ox-core** 是纯逻辑；不依赖 TUI 类型
- **Agent** 编排：接收消息 → 调用 LLM → 分发工具 → 发送 UI 事件
- **Tools** 实现 `Tool` trait；`ToolRegistry` 按名称分发
- **Enforcer** 在工具执行前拦截，验证规则

---

## 🚀 快速开始

### 安装

```bash
git clone https://github.com/jeff-tang-xin/Ox.git
cd Ox
cargo build --release
```

二进制文件：`target/release/ox`（Linux/macOS）或 `target/release/ox.exe`（Windows）。

### 配置 API Key

```bash
# OpenAI（或兼容端点）
export OPENAI_API_KEY=sk-...
export OPENAI_BASE_URL=https://api.openai.com/v1   # 可选

# Anthropic
export ANTHROPIC_API_KEY=sk-ant-...

# DeepSeek
export DEEPSEEK_API_KEY=sk-...

# 通用覆盖（最高优先级）
export OX_OPENAI_API_KEY=sk-...
```

Windows PowerShell：
```powershell
$env:OPENAI_API_KEY="sk-..."
```

### 启动

```bash
ox                  # TUI 模式（默认）
ox "解释这段代码"    # 单次模式
ox --no-tui "实现快速排序"  # 无 TUI 模式
```

---

## ⚡ 斜杠命令

| 命令 | 说明 |
|------|------|
| `/exit` | 退出程序 |
| `/clear` | 清除会话 |
| `/debug` | 切换调试模式 |
| `/cost` | 显示 Token 成本 |
| `/reload` | 重载配置 |
| `/cd <路径>` | 切换工作目录 |
| `/cancel` | 取消当前操作 |
| `/plan` | 查看会话方案 |
| `/model <名称>` | 切换模型 |
| `/skill` | 管理技能 |
| `/trust` | 管理信任模式 |
| `/system` | 查看/编辑系统提示词 |
| `/Y` / `/N` | 确认 / 拒绝 |
| `/O <文本>` | 提出替代方案或反馈 |
| `/memory show` | 显示当前记忆 |
| `/memory search <查询>` | 搜索记忆 |
| `/memory transform` | 触发记忆转化（L0→L1→L2→L3） |

---

## 🛠️ 内置工具 (17)

| 工具 | 安全级别 | 说明 |
|------|---------|------|
| `file_read` | 安全 | 读取文件内容（多编码、行号） |
| `file_list` | 安全 | 列出目录结构 |
| `file_search` | 安全 | 按 glob 模式搜索文件 |
| `code_search` | 安全 | Ripgrep 驱动的正则代码搜索 |
| `find_symbol` | 安全 | AST 语义符号搜索（7 种语言） |
| `memory_search` | 安全 | 搜索记忆知识库 |
| `recall` | 安全 | 按 node_id 取回卸载内容 |
| `project_detect` | 安全 | 检测项目语言/框架 |
| `web_fetch` | 安全 | 抓取 URL 内容 |
| `git_status` | 安全 | 查看 Git 工作树状态 |
| `git_diff` | 安全 | 查看 Git 差异 |
| `shell_exec` | 危险 | 执行 Shell 命令 |
| `file_write` | 需确认 | 创建或覆盖文件 |
| `edit_file` | 需确认 | 精确文本编辑（单次/批量、模糊匹配） |
| `delete_range` | 需确认 | 按锚点删除代码块 |
| `content_validation` | 内部 | 写入前验证内容 |
| `intent_classifier` | 内部 | 分类用户意图用于上下文组装 |

---

## 🛡️ 强制规则

在 Rust 代码中强制执行 —— 不是靠提示词。可在 `config.toml` 中配置：

| 规则 | 默认 | 说明 |
|------|------|------|
| `plan_before_edit` | ✅ | `file_write` / `edit_file` 前必须提出方案 |
| `read_before_edit` | ✅ | 必须先读取目标文件 |
| `steps_before_shell` | ✅ | `shell_exec` 前必须列出步骤 |
| `impact_analysis` | ✅ | 修改源码前必须搜索调用方/依赖方 |

自定义模式：`custom_plan_patterns`、`custom_step_patterns`。

---

## 🧠 记忆架构

```
L0 原始对话 ──精炼──▶ L1 原子事实
     (SQLite)               (SQLite)

L1 原子事实   ──聚类──▶ L2 场景块
                            (Markdown)

L2 场景块     ──抽象──▶ L3 项目人设
                            (Markdown)
```

| 层级 | 存储 | 用途 |
|------|------|------|
| **L0** 原始 | SQLite | 完整对话日志 |
| **L1** 事实 | SQLite | 精炼后的原子事实 |
| **L2** 场景 | Markdown | 聚类场景，人可编辑 |
| **L3** 人设 | Markdown | 项目级模式与偏好 |

混合存储：**SQLite**（L0–L1）快速查询 + **Markdown**（L2–L3）在 `.ox/knowledge/` 中人类可读 + **本地向量**（Candle + TriviumDB）语义检索。

---

## 🔧 配置

配置文件：`~/.ox/config.toml`（备选：`~/.config/ox/config.toml`，覆盖：`OX_CONFIG_PATH`）

```toml
[models]
default = "gpt-4o"
adaptive_thinking = true
effort_level = "high"

[models.providers.openai]
api_key = "sk-..."          # 或环境变量 OPENAI_API_KEY
base_url = "https://api.openai.com/v1"
max_tokens = 4096

[models.providers.anthropic]
api_key = "sk-ant-..."      # 或环境变量 ANTHROPIC_API_KEY
max_tokens = 8192

[agent]
max_iterations = 25

[safety]
confirm_dangerous_ops = true

[enforcement_rules]
plan_before_edit = true
read_before_edit = true
steps_before_shell = true
impact_analysis = true

[embedding]
model = "all-MiniLM-L6-v2"
```

---

## 🪞 技能系统

Markdown 文件，向 LLM 上下文注入行为指导：

| 作用域 | 位置 | 说明 |
|--------|------|------|
| **系统** | 内置（`ox-core/src/skill/builtin/`） | 始终加载 |
| **全局** | `~/.ox/skills/` | 用户级，所有项目共享 |
| **项目** | `.ox/skills/` | 项目特定，可版本控制 |

**内置技能：** coding-principles、concise-communication、engineering-practices

**自动生成：** 工作流结束后，Auto-Reflector 分析执行轨迹，在 `.ox/skills/` 中生成新技能。

---

## 📦 核心依赖

| 分类 | 依赖 |
|------|------|
| 异步 | `tokio`, `tokio-util`, `futures`, `async-trait` |
| LLM / HTTP | `reqwest`, `serde`, `serde_json`, `toml` |
| TUI | `ratatui`, `crossterm`, `syntect`, `pulldown-cmark`, `unicode-width` |
| 存储 | `rusqlite`, `blake3` |
| 代码搜索 | `grep`, `ignore`, `termcolor`, `encoding_rs` |
| AST / 嵌入 | `tree-sitter`（7 语言）, `candle-core/nn/transformers`, `hf-hub`, `tokenizers` |
| 向量库 | `triviumdb` |
| 日志 / 错误 | `tracing`, `anyhow` |

---

## 📄 许可证

MIT
