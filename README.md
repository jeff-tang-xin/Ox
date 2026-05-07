# Ox

> 🐂 AI 编程助手 — 终端里的智能体，懂你的项目、记得你的偏好、能写代码。

Ox 是一个基于 Rust 开发的 AI 编程助手，通过精美的 TUI 界面连接 LLM，为开发者提供代码读取、搜索、编写和执行能力。

---

## 🌟 核心亮点

### 1. 🧠 智能记忆系统
- **SQLite 持久化**：记忆永不过期，重启后自动恢复
- **DEWMA + ACT-R 衰减模型**：模拟人类遗忘曲线，重要信息长期保留
- **Janitor 自动清理**：后台清理低价值节点，保持系统高效

### 2. 🗜️ 上下文压缩 (KadaneDial)
- **BGE 嵌入模型**：本地运行，无需 API 调用
- **动态选择最优片段**：基于语义相关性自动压缩长对话
- **三种精度级别**：small (~130MB) / base (~420MB) / large (~1.2GB)

### 3. 🏛️ 多模型议会 (Council)
- **多角色辩论**：GPT-4o / Claude / DeepSeek 同时参与
- **提案→评审→反驳→仲裁**：结构化决策流程
- **防止单一模型偏见**：汇聚多模型智慧

---

## ✨ 完整特性

| 类别 | 特性 |
|------|------|
| **界面** | Ratatui TUI + Markdown 渲染 + 语法高亮 + 鼠标滚轮 |
| **LLM** | OpenAI / Anthropic / DeepSeek，自动降级回显模式 |
| **工具** | 12+ 内置工具（文件、搜索、Shell、Git、记忆查询） |
| **工作流** | 三模式系统：Free / Spec / Council |
| **安全** | 信任管理 + 危险操作确认 |
| **成本** | 实时 Token/费用追踪 |

---

## 🏗️ 项目结构

```
Ox/
├── crates/
│   ├── ox-cli/                    # CLI 可执行文件 — TUI 应用入口
│   │   └── src/
│   │       ├── main.rs            # 主入口 (~2600 行)
│   │       └── terminal/          # TUI 组件
│   │           ├── app.rs         # 应用状态
│   │           ├── event.rs        # 事件轮询
│   │           ├── input_pane.rs   # 输入框
│   │           ├── output_pane.rs  # 输出面板
│   │           ├── markdown.rs     # Markdown 渲染
│   │           └── render.rs        # 布局渲染
│   └── ox-core/                   # 核心库
│       └── src/
│           ├── agent/             # 智能体 + 工作流引擎
│           ├── llm/               # LLM 提供商
│           ├── memory/            # SQLite 记忆系统
│           ├── embedding/         # BGE 嵌入 + KadaneDial
│           ├── council/           # 多模型辩论
│           ├── tools/             # 内置工具集
│           └── config/            # 配置管理
├── docs/                          # 技术文档
└── Cargo.toml                     # Workspace 根
```

---

## 🚀 首次使用指南

### 1. 构建项目

```bash
# 克隆项目
git clone https://github.com/your-repo/Ox.git
cd Ox

# 调试构建
cargo build

# 或发布构建（推荐，更快）
cargo build --release
```

### 2. 初始化 Git 仓库 ⚠️

> **重要**：Ox 需要项目目录是 Git 仓库才能索引文件。

```bash
# 如果你的项目还不是 Git 仓库
git init
git add .
git commit -m "Initial commit"
```

### 3. 运行 Ox

```bash
# 运行构建版本
./target/release/ox

# 或开发版本
cargo run
```

首次运行时，Ox 会自动创建配置目录 `~/.ox/`。

### 4. 配置 API 密钥

**方式一：环境变量（推荐）**

```bash
# Linux/macOS
export OX_OPENAI_API_KEY="sk-..."
export OX_ANTHROPIC_API_KEY="sk-ant-..."

# Windows PowerShell
$env:OX_OPENAI_API_KEY="sk-..."
$env:OX_ANTHROPIC_API_KEY="sk-ant-..."

# 然后运行
./target/release/ox
```

**方式二：配置文件**

运行 `/init` 命令，或手动创建 `~/.ox/config.toml`：

```toml
[models]
default = "gpt-4o"

[models.providers.openai]
api_key = "sk-..."

[models.providers.anthropic]
api_key = "sk-ant-..."
```

### 5. 下载嵌入模型（可选）

开启上下文压缩需要下载 BGE 嵌入模型：

```bash
# 进入 Ox REPL 后执行
/download-model                    # 下载默认模型 bge-small-zh-v1.5 (~130MB)
/download-model bge-base-zh-v1.5   # 下载 base 模型 (~420MB)
/download-model bge-large-zh-v1.5  # 下载 large 模型 (~1.2GB)
```

模型存放路径：`~/.ox/models/`

### 6. 无 API 密钥模式

如果未配置 API 密钥，Ox 以**回显模式**运行 —— 原样返回你的输入，适合测试 TUI 界面。

---

## 📁 数据存储

| 路径 | 用途 |
|------|------|
| `~/.ox/config.toml` | 用户配置 |
| `~/.ox/sessions/` | 会话历史（JSONL，按项目隔离） |
| `~/.ox/db/` | SQLite 数据库（记忆、压缩、费用） |
| `~/.ox/models/` | BGE 嵌入模型文件 |
| `~/.ox/logs/` | 日志文件 |
| `<项目>/.ox/` | 项目级配置（任务计划等） |

---

## ⌨️ 常用命令

### 会话管理
| 命令 | 说明 |
|------|------|
| `/help [topic]` | 显示帮助 |
| `/exit` 或 `Ctrl+D` | 退出 |
| `/new` | 新会话（归档当前） |
| `/sessions` | 查看归档会话 |
| `/resume <file>` | 恢复会话 |

### 工具信任
| 命令 | 说明 |
|------|------|
| `/trust <tool>` | 信任工具（跳过确认） |
| `/trust --all` | 信任所有非危险工具 |
| `/untrust` | 撤销所有信任 |

### 工作模式
| 命令 | 说明 |
|------|------|
| `/free` | 自由模式（默认） |
| `/spec on [内容]` | 规范模式 |
| `/council start <topic>` | 启动辩论 |

### 记忆与成本
| 命令 | 说明 |
|------|------|
| `/memory` | 记忆统计 |
| `/remember <内容>` | 存储记忆 |
| `/cost` | 费用统计 |
| `/plan` | 任务计划 |

### 配置
| 命令 | 说明 |
|------|------|
| `/init` | 创建默认配置 |
| `/debug` | 调试信息 |
| `/download-model` | 下载 BGE 嵌入模型 |

### 快捷键
| 快捷键 | 功能 |
|--------|------|
| `Enter` | 发送消息 |
| `Ctrl+C` | 中断当前响应 |
| `↑/↓` | 历史消息导航 |
| `Ctrl+A/E` | 行首/行尾 |
| 鼠标滚轮 | 滚动输出 |

---

## 🛠️ 内置工具

| 工具 | 功能 |
|------|------|
| `file_read` | 读取文件 |
| `file_write` | 写入文件 |
| `file_patch` | 搜索替换 |
| `file_list` | 列出目录 |
| `file_search` | 按名搜索 |
| `code_search` | 正则/文本搜索 |
| `shell_exec` | 执行 Shell |
| `git_status/diff/commit` | Git 操作 |
| `memory_search` | 记忆查询 |
| `project_detect` | 检测项目类型 |
| `web_fetch` | 抓取网页 |

---

## 🔄 三种工作模式

| 模式 | 用途 | 代码修改 | 步数 |
|------|------|----------|------|
| **Free** | 日常对话 | ✅ 允许 | 1 步 |
| **Spec** | 任务开发 | ⚠️ 仅最后一步 | 6 步 |
| **Council** | 架构决策 | ❌ 禁止 | 6 步 |

### Free Mode（自由模式）
- 默认模式，无约束
- 激活: `/free`

### Spec Mode（规范模式）
- 基于规范的计划→审查→执行流程
- 激活: `/spec on "任务描述"`

### Council Mode（议会模式）
- 多模型辩论，禁止代码修改
- 激活: `/council start "辩论主题"`

---

## ⚙️ 完整配置示例

```toml
# ~/.ox/config.toml

[models]
default = "gpt-4o"

[models.providers.openai]
api_key = "sk-..."

[models.providers.anthropic]
api_key = "sk-ant-..."

[context]
history_ratio = 0.10       # 历史占上下文窗口比例
memory_ratio = 0.02       # 记忆占上下文比例

[cost]
max_monthly_cost = 5.0     # 月度上限（美元）
max_daily_cost = 2.0      # 日度上限

[memory]
max_nodes = 1000           # 最大记忆节点

[council]
default_rounds = 2         # 默认辩论轮次
```

---

## ⚙️ 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust (edition 2024) |
| 异步运行时 | Tokio |
| TUI | Ratatui + Crossterm |
| HTTP | Reqwest |
| 高亮 | Syntect |
| 数据库 | rusqlite (bundled SQLite) |
| 嵌入模型 | Candle + BGE (ModelScope) |
| 日志 | tracing |

---

## 📜 License

MIT License - Copyright (c) 2026 Jeff Tang

---

## 📚 相关文档

- `docs/Workflow_Engine_完整调用流程.md` - 工作流引擎详解
- `docs/main_rs_analysis.md` - main.rs 重构建议

---

**Made with ❤️ by Jeff Tang**
