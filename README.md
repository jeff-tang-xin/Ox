# Ox

> 🐂 AI 编程助手 — 一个运行在终端里的智能体，懂你的项目、记得你的偏好、能写代码。

Ox 是一个基于 Rust 开发的 AI 编程助手，拥有精美的 TUI 界面。它连接 LLM 提供商（OpenAI、Anthropic），提供丰富的工具集用于读取、写入、搜索和执行代码——全部在终端中完成。

---

## ✨ 特性

- 🖥️ **终端界面** — 全屏 ratatui 界面，支持 Markdown 渲染和语法高亮
- 🤖 **多 LLM 支持** — 支持 OpenAI 和 Anthropic API，无 API 时自动降级为回显模式
- 🛠️ **丰富工具集** — 12+ 内置工具：文件操作、代码搜索、Shell 执行、Git 集成等
- 🗂️ **智能文件索引** — SQLite + Git 集成，三层实时更新机制，多项目无缝切换
- 🔒 **安全体系** — 信任管理和破坏性操作确认机制
- 💰 **成本追踪** — 实时统计 LLM API 调用费用
- 💾 **会话持久化** — 自动保存对话历史，重启后恢复
- ⚡ **斜杠命令** — `/help`、`/clear`、`/cost` 等快捷操作
- ⏸️ **中断与插话** — 支持流式输出中取消或插入新消息
- 🖱️ **鼠标滚轮** — 支持鼠标滚动浏览历史输出
- 🧠 **记忆系统** — SQLite 持久化，项目记忆与长期记忆分离，DEWMA/ACT-R 衰减算法，Janitor 自动清理
- 🔍 **智能查询** — LLM 主动调用 `memory_search` 工具，支持自然语言查询、来源追踪、置信度评估
- ⚡ **查询缓存** — 60秒 TTL + LRU 淘汰，重复查询性能提升 50x
- 🌱 **自我进化** — 基于记忆系统的自主学习，LLM 通过查询获取项目知识和用户风格，动态调整行为
- 🏛️ **议会系统** — 多模型辩论机制，提案评审反驳仲裁，支持能力学习和领域分类
- 🗜️ **智能压缩** — BGE 嵌入模型 + KadaneDial 算法，自动压缩长对话历史，节省 Token
- 🎯 **多模式系统** — Free/Spec/Council 三种工作模式，独立工作流引擎管理
- ⚙️ **工作流引擎** — 基于状态机的步骤化执行，动态权限控制，代码修改限制

---

## 📁 项目结构

```
Ox/
├── crates/
│   ├── ox-cli/                    # CLI 可执行文件 — TUI 应用入口
│   │   └── src/
│   │       ├── main.rs            # 主入口 (~2600 行)
│   │       └── terminal/          # TUI 渲染与事件处理
│   │           ├── app.rs         # 应用状态
│   │           ├── event.rs       # 事件轮询（键盘/鼠标）
│   │           ├── input_pane.rs  # 输入框
│   │           ├── output_pane.rs # 输出面板（带渲染缓存）
│   │           ├── markdown.rs    # Markdown 渲染
│   │           └── render.rs      # 整体布局渲染
│   └── ox-core/                   # 核心库 — 智能体、LLM、工具等
│       └── src/
│           ├── agent/             # 智能体循环、插话、中断、UI 事件、工作流引擎
│           │   ├── mod.rs         # 主智能体循环
│           │   ├── engine.rs      # 工作流引擎（WorkflowEngine）
│           │   ├── workflow.rs    # 工作流定义（Free/Spec/Council）
│           │   ├── session.rs     # 会话状态跟踪
│           │   ├── interjection.rs # 插话机制
│           │   ├── interrupt.rs   # 中断控制器
│           │   └── ui_event.rs    # UI 事件通信
│           ├── config/            # 配置加载与管理 (~/.ox/config.toml)
│           ├── context/           # 上下文构建器、系统提示词、努力等级
│           ├── cost/              # LLM 费用追踪
│           ├── council/           # 议会系统（多模型辩论）
│           ├── embedding/         # 嵌入和压缩管理
│           ├── file_index/        # 文件索引系统 ⭐
│           │   ├── mod.rs         # FileIndexManager（单目录管理）
│           │   └── registry.rs    # FileIndexRegistry（多目录注册表）
│           ├── llm/               # LLM 提供商抽象（OpenAI、Anthropic）
│           ├── memory/            # 记忆系统（SQLite 存储、检索、缓存、衰减）
│           ├── message/           # 消息协议与会话管理
│           ├── runtime/           # 运行时环境检测
│           ├── safety/            # 信任与安全管理
│           ├── slash/             # 斜杠命令处理器
│           └── tools/             # 全部内置工具
│               ├── code_search.rs      # 代码搜索（正则/文本）
│               ├── file_list.rs        # 列出文件/目录（从数据库查询）
│               ├── file_patch.rs       # 搜索替换补丁
│               ├── file_read.rs        # 读取文件（支持 file_id/filename/path）
│               ├── file_search.rs      # 按名称搜索文件
│               ├── file_write.rs       # 写入/创建文件（实时更新索引）
│               ├── git_commit.rs       # 提交 Git 变更
│               ├── git_diff.rs         # 查看 Git 差异
│               ├── git_status.rs       # 查看 Git 状态
│               ├── memory_search.rs    # 记忆搜索（LLM 主动查询知识）
│               ├── project_detect.rs   # 项目类型/语言检测
│               ├── shell_exec.rs       # 执行 Shell 命令
│               └── web_fetch.rs        # 抓取网页内容
├── docs/                          # 技术文档
│   ├── Workflow_Engine_完整调用流程.md  # 工作流引擎详细文档
│   └── ...                        # 其他设计文档
├── Cargo.toml                     # Workspace 根
├── Cargo.lock
├── LICENSE                        # MIT 许可证
└── README.md
```

---

## 🏗️ 核心架构

### 智能体循环

Ox 的核心是一个异步智能体循环，能够处理用户输入、调用 LLM、执行工具并流式输出结果。

### 工作流引擎

- **多模式管理**: Free/Spec/Council 三种工作模式
- **状态机驱动**: 基于 `WorkflowState` 的状态转换
- **动态权限控制**: 根据当前步骤限制工具使用和代码修改
- **实时 UI 显示**: 每帧更新工作流进度和约束信息

### 文件索引系统

Ox 的文件索引系统通过 **SQLite + Git 集成 + 文件系统监听**，实现了智能、实时的项目文件管理。

#### 三层更新机制

```
新建/修改文件
    ↓
┌─────────────────────────────────────┐
│ 1. 工具执行 → 立即更新 (< 10ms)     │ ← Ox 内部操作
│ 2. 文件监听 → 实时捕获 (< 100ms)   │ ← 外部编辑器/Git
│ 3. 定期刷新 → 兜底保障 (每5分钟)    │ ← 全量扫描
└─────────────────────────────────────┘
    ↓
LLM 可查询（几乎实时）
```

#### 三种查询方式

**方式 1：使用 file_id（推荐）**
```json
{
  "tool": "file_read",
  "parameters": {
    "file_id": 123
  }
}
```
- ✅ 精确匹配，唯一结果
- 💡 LLM 从 `file_list` 输出中获取 ID

**方式 2：使用 filename**
```json
{
  "tool": "file_read",
  "parameters": {
    "filename": "main.rs"
  }
}
```
- ⚠️ 可能匹配多个文件，返回选项让 LLM 选择

**方式 3：使用 path（向后兼容）**
```json
{
  "tool": "file_read",
  "parameters": {
    "path": "src/main.rs"
  }
}
```
- ✅ 传统方式，完全兼容旧代码

#### 多项目支持

每个工作目录有独立的 SQLite 数据库：

```
~/.ox/db/file_indices/
├── file_index_a1b2c3d4.db  (项目 A)
├── file_index_e5f6g7h8.db  (项目 B)
└── file_index_i9j0k1l2.db  (项目 C)
```

**切换流程**：
1. 首次访问项目 A：创建索引 + 扫描 Git + 启动监听（~2秒）
2. 切换到项目 B：创建新索引 + 扫描 + 启动监听（~1.5秒）
3. 切回项目 A：**缓存命中，立即切换（< 1ms）** ⚡

### 会话管理

- **会话持久化**: 自动保存对话历史到 JSONL 文件
- **会话恢复**: 重启后自动恢复上次会话
- **会话归档**: 使用 `/new` 命令归档当前会话
- **多项目支持**: 按项目隔离会话和记忆

### 上下文管理

- **动态上下文**: 根据努力等级调整上下文大小
- **记忆注入**: 自动检索相关记忆并注入上下文
- **压缩存储**: 长会话自动压缩以节省空间
- **任务计划**: 持久化任务计划，跨会话保持

### 流式输出

- **实时渲染**: Markdown 和代码高亮实时渲染
- **中断支持**: 支持在流式输出中中断
- **插话机制**: 允许在 AI 工作时插入新消息

---

## 🚀 快速开始

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
default = "gpt-4o"                    # 默认模型

default_provider = "openai"           # 默认提供商（可选）

[models.providers.openai]
api_key = "sk-..."
model = "gpt-4o"                      # OpenAI 默认模型

[models.providers.anthropic]
api_key = "sk-ant-..."
model = "claude-3-5-sonnet-20241022"  # Anthropic 默认模型
```

**配置说明**：
- `default`: 全局默认模型名称
- `default_provider`: 默认使用的 LLM 提供商（`openai` / `anthropic`），不设置则自动选择第一个可用的
- `[models.providers.*]`: 各提供商的 API 密钥和默认模型

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
| `~/.ox/db/file_indices/` | 文件索引数据库（每个项目独立） |
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
| `file_read` | 读取文件（支持 file_id/filename/path） |
| `file_write` | 写入文件（实时更新索引） |
| `file_patch` | 搜索替换 |
| `file_list` | 列出目录（从数据库查询） |
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
default_provider = "openai"           # 默认提供商（可选）

[models.providers.openai]
api_key = "sk-..."
model = "gpt-4o"

[models.providers.anthropic]
api_key = "sk-ant-..."
model = "claude-3-5-sonnet-20241022"

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
| 文件监听 | notify |
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
