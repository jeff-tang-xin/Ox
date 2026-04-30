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
- **🧠 记忆系统** — SQLite 持久化，项目记忆与长期记忆分离，DEWMA/ACT-R 衰减算法，Janitor 自动清理
- **🎭 人格系统** — 按编程语言自适应 PersonaVector（Rust/Python/Go），支持冻结保护
- **💬 议会系统** — 多模型辩论机制，提案→评审→反驳→仲裁，支持能力学习和领域分类
- **🔄 自进化机制** — 基于显式反馈和被动记忆分析的自主学习，透明可控的人格演化

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
│           ├── council/     # 议会系统（多模型辩论）
│           ├── embedding/   # 嵌入和压缩管理
│           ├── llm/         # LLM 提供商抽象（OpenAI、Anthropic）
│           ├── memory/      # 记忆系统（SQLite 存储、检索、衰减）
│           ├── persona/     # 人格向量与演化逻辑
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

## 🏗️ 核心架构

### 智能体循环
Ox 的核心是一个异步智能体循环，能够处理用户输入、调用 LLM、执行工具并流式输出结果。

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

### 配置文件结构

```toml
# ~/.ox/config.toml
[llm]
default_provider = "openai"  # 或 "anthropic"

[cost]
max_monthly_cost = 10.0      # 月度成本上限（美元）

[memory]
max_nodes = 1000             # 最大记忆节点数
trace_tau = 2.0              # DEWMA 衰减参数

[council]
default_rounds = 2           # 默认辩论轮次
max_rounds = 5               # 最大辩论轮次

[persona]
max_trait_change = 0.1       # 单次人格变化最大值
```

### 目录结构

Ox 在以下位置存储数据：

| 路径 | 用途 |
|------|------|
| `~/.ox/config.toml` | 用户配置文件 |
| `~/.ox/sessions/` | 会话持久化（按项目隔离） |
| `~/.ox/db/` | SQLite 数据库（记忆、压缩上下文、费用追踪） |
| `~/.ox/logs/` | 日志文件 |
| `<项目根目录>/.ox/` | 项目级配置（任务计划、会话历史） |

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

## ⌨️ Slash 命令

### 会话管理
| 命令 | 说明 |
|------|------|
| `/help [topic]` | 显示帮助（主题：trust, cost, plan） |
| `/exit` | 退出 Ox |
| `/new` | 开始新会话（归档当前会话） |
| `/clean` | 清除当前会话的所有消息 |
| `/clear` | 清屏 |
| `/sessions` | 列出归档的会话 |
| `/resume <file>` | 恢复归档的会话 |
| `/reload` | 从磁盘重新加载会话（JSONL） |

### 工具信任
| 命令 | 说明 |
|------|------|
| `/trust <tool>` | 信任一个工具（本会话跳过确认） |
| `/trust --all` | 信任所有非危险工具 |
| `/untrust` | 撤销所有信任 |

### 模型与成本
| 命令 | 说明 |
|------|------|
| `/model [name]` | 显示或切换模型 |
| `/cost` | 显示 token 使用和费用摘要 |

### 任务计划
| 命令 | 说明 |
|------|------|
| `/plan` | 显示当前任务计划 |

### 目录与配置
| 命令 | 说明 |
|------|------|
| `/cd [path]` | 显示或更改工作目录 |
| `/init` | 创建默认配置（~/.ox/config.toml） |
| `/debug` | 显示调试信息 |

### 记忆系统
| 命令 | 说明 |
|------|------|
| `/remember <content>` | 存储为 Style 记忆 |
| `/forget <keyword>` | 删除匹配的记忆 |
| `/memory` | 显示记忆统计和最近记忆 |

### 人格系统
| 命令 | 说明 |
|------|------|
| `/persona [action]` | 人格操作（show/freeze/unfreeze/evolve） |

### 反馈系统
| 命令 | 说明 |
|------|------|
| `/feedback <category>` | 提供反馈（good/bad/unsafe） |

### 议会系统
| 命令 | 说明 |
|------|------|
| `/discuss [question]` | 启动议会辩论（--rounds N, --verbose） |
| `/council <action>` | 议会操作（last/stats） |

## 💬 议会系统详解

Ox 的议会系统允许多个 AI 模型就复杂问题进行辩论，通过提案→评审→反驳→仲裁的流程得出更可靠的结论。

### 辩论流程
1. **提案阶段**：各模型独立提出解决方案
2. **交叉评审**：每个模型评审其他模型的提案并打分
3. **反驳阶段**：模型根据评审意见修正自己的提案
4. **仲裁阶段**：指定模型综合各方观点得出最终结论

### 使用示例
```bash
# 启动辩论
/discuss 这个函数应该用递归还是迭代？

# 指定轮次和详细模式
/discuss --rounds 3 --verbose 如何设计缓存策略？

# 查看上次辩论结果
/council last

# 查看模型能力统计
/council stats
```

## 🧠 记忆系统详解

Ox 拥有强大的混合记忆系统，能够记住项目上下文、用户偏好和最佳实践。

### 记忆类型
- **Fact**: 工具观察到的事实（depth=1）
- **Architectural**: 架构决策（depth=2）
- **Business**: 业务逻辑（depth=2）
- **Style**: 用户偏好（depth=3）
- **AntiPattern**: 反模式（depth=2）
- **Council**: 议会结论（depth=3）

### 记忆来源
- **UserExplicit**: 用户明确指示（/remember 命令）
- **ToolObservation**: 工具执行结果
- **LlmExtraction**: LLM 提取的知识
- **CouncilConclusion**: 议会辩论结论
- **Feedback**: 用户反馈

### 衰减算法
- **项目记忆**: DEWMA 衰减算法
- **长期记忆**: ACT-R MCM 衰减算法
- **Janitor**: 自动清理低价值记忆

### 使用示例
```bash
# 存储记忆
/remember 我喜欢在 Rust 项目中使用 snake_case 命名

# 查看记忆
/memory

# 删除记忆
/forget 某个关键词
```

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

## 🎭 人格系统与自进化机制

Ox 拥有自适应人格系统和自进化能力，能够根据用户反馈和历史交互自动调整行为风格。

### 人格维度
- **favors_safety_over_speed**: 安全性优先于速度（0.0-1.0）
- **prefers_conciseness**: 偏好简洁性（0.0-1.0）
- **code_style_strictness**: 代码风格严格度（0.0-1.0）
- **refuses_unsafe_code**: 拒绝不安全代码（固定为 true，不可演化）
- **forbidden_phrases**: 禁止使用的表达方式
- **moral_priorities**: 价值优先级（如"安全性"、"性能"）

### 双重演化路径

#### 1. 显式反馈（用户主动）
```bash
# 正面反馈 - 强化相关记忆
/feedback good

# 负面反馈 - 触发人格调整
/feedback bad

# 安全违规报告
/feedback unsafe
```

#### 2. 被动演化（自动触发）
- **每轮对话后自动分析**：基于记忆模式自主学习
- **Style 记忆分析**：如果 >5 条强调“简洁”，提高 `prefers_conciseness`
- **AntiPattern 分析**：如果 >3 条涉及安全，提高 `favors_safety_over_speed`
- **透明通知**：每次演化都会显示调整内容

### 人格管理
```bash
# 查看当前人格状态和配置
/persona show

# 冻结人格（停止所有演化）
/persona freeze

# 解冻人格（恢复演化）
/persona unfreeze

# 手动触发一次自我评估
/persona evolve
```

### 配置选项
在 `~/.ox/config.toml` 中：
```toml
[persona]
auto_evolve = true          # 启用自动演化（默认 true）
max_trait_change = 0.1      # 单次最大变化幅度（默认 0.1）
frozen = false              # 初始冻结状态（默认 false）
```

### 演化约束
- **安全锁定**：`refuses_unsafe_code` 永远不可被演化修改
- **变化上限**：单次演化不超过 `max_trait_change`
- **范围限制**：所有数值保持在 [0.0, 1.0]
- **冻结保护**：`frozen = true` 时禁止所有演化
- **证据驱动**：需要足够的记忆模式证据才触发演化

## 🛡️ 安全系统

Ox 内置了强大的安全机制，确保所有操作都在可控范围内进行。

### 安全特性
- **信任管理**: 对工具调用的确认机制
- **危险操作保护**: Dangerous 级别操作永不跳过确认
- **数据脱敏**: 自动检测并脱敏敏感信息
- **沙箱执行**: 可选的命令执行沙箱

### 信任命令
```bash
# 信任特定工具（本会话跳过确认）
/trust file_write

# 信任所有非危险工具
/trust --all

# 撤销所有信任
/untrust
```

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

## 🤝 贡献指南

欢迎贡献代码、报告问题或提出建议！

### 开发环境设置
```bash
# 克隆仓库
git clone https://github.com/your-username/Ox.git
cd Ox

# 安装依赖
cargo build

# 运行测试
cargo test

# 运行开发版本
cargo run
```

### 代码规范
- 遵循 Rust 官方风格指南
- 所有公共 API 必须有文档注释
- 新功能需要包含单元测试
- 提交前运行 `cargo clippy` 和 `cargo fmt`

## 🐛 问题反馈

如果您遇到问题或有建议，请在 GitHub 上创建 Issue。
