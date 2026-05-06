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
- **🔍 智能查询** — LLM 主动调用 `memory_search` 工具，支持自然语言查询、来源追踪、置信度评估
- **💾 查询缓存** — 60秒 TTL + LRU 淘汰，重复查询性能提升 50x
- **🧬 自我进化** — 基于记忆系统的自主学习，LLM 通过查询获取项目知识和用户风格，动态调整行为
- **💬 议会系统** — 多模型辩论机制，提案→评审→反驳→仲裁，支持能力学习和领域分类
- **🗜️ 智能压缩** — BGE 嵌入模型 + KadaneDial 算法，自动压缩长对话历史，节省 Token

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
│           ├── memory/      # 记忆系统（SQLite 存储、检索、缓存、衰减）
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
│               ├── memory_search.rs    # 记忆搜索（LLM 主动查询知识）
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
| `memory_search` | 🔍 **记忆搜索** — LLM 主动查询项目知识和最佳实践 |
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
| `/remember <content>` | 存储为 Style 记忆（用户明确指示） |
| `/forget <keyword>` | 删除匹配的记忆 |
| `/memory` | 显示记忆统计和最近 8 条记忆（含来源、置信度） |

**提示**：在对话中，LLM 会自动调用 `memory_search` 工具查询相关知识，无需手动操作。

### 反馈系统
| 命令 | 说明 |
|------|------|
| `/feedback <category>` | 提供反馈（good/bad/unsafe） |

### 议会系统
| 命令 | 说明 |
|------|------|
| `/discuss [question]` | 启动议会辩论（--rounds N, --verbose） |
| `/council <action>` | 议会操作（last/stats） |

### 嵌入模型
| 命令 | 说明 |
|------|------|
| `/download-model [name]` | 下载 BGE 嵌入模型（默认：bge-small-zh-v1.5） |

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

## 🗜️ 智能压缩系统详解

Ox 使用 BGE 嵌入模型和 KadaneDial 算法，自动压缩长对话历史，节省 Token 成本同时保持上下文连贯性。

### 工作原理
1. **语义编码**：使用 BGE 模型将对话历史编码为向量
2. **相关性评分**：计算当前查询与历史消息的余弦相似度
3. **KadaneDial 算法**：基于增益选择最相关的连续片段
4. **智能保留**：始终保留最近的消息和系统提示

### 支持的模型
从魔塔社区（ModelScope）下载：
- **bge-small-zh-v1.5** (~130MB) - 快速，适合大多数场景
- **bge-base-zh-v1.5** (~420MB) - 平衡性能
- **bge-large-zh-v1.5** (~1.2GB) - 最佳质量，较慢

### 配置示例
在 `~/.ox/config.toml` 中：
```toml
[models.embedding]
enabled = true
model_path = "~/.ox/models/bge-small-zh-v1.5"
threshold = 0.0           # Z-score 阈值（越高越严格）
stop_threshold = 0.5      # 增益低于此值时停止
max_segments = 5          # 最大保留片段数
keep_recent = 4           # 始终保留最近 4 条消息
chunk_threshold_tokens = 256  # 超过此 token 数时分块
max_chunk_tokens = 512    # 每块最大 token 数

[context]
history_ratio = 0.10      # 历史预算占上下文窗口的比例（10%）
```

### 使用流程
```bash
# 1. 下载模型
/download-model                    # 默认 bge-small-zh-v1.5
/download-model bge-base-zh-v1.5   # 或指定其他模型

# 2. 编辑配置文件，启用压缩
# 在 ~/.ox/config.toml 中设置 enabled = true

# 3. 重启 Ox，自动生效
# 当对话历史超过预算时，会自动触发压缩
```

### 压缩效果
- **Token 节省**：通常可减少 50-70% 的历史 token 使用
- **语义保持**：通过向量相似度确保保留关键信息
- **自动触发**：无需手动操作，根据 `history_ratio` 自动判断
- **透明可控**：可在 `/debug` 中查看压缩状态

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

### 🔍 智能查询系统

Ox 支持 LLM 主动调用 `memory_search` 工具来检索相关知识，让 AI 比用户更懂项目。

#### 工作原理
1. **LLM 自主决策**：在任务分析、设计、编码阶段，LLM 判断需要查询知识
2. **自然语言查询**：使用日常语言描述需要的信息
3. **双层专家系统**：
   - **Project Expert**：项目专属记忆（架构、约定、历史决策）
   - **Global Expert**：跨项目通用知识（最佳实践、设计模式）
4. **智能排序**：基于深度、类型、置信度综合评分
5. **缓存优化**：60秒 TTL + LRU 淘汰，重复查询性能提升 50x

#### 查询参数
```json
{
  "query": "authentication architecture and JWT setup",
  "scope": "project",        // "project" | "global" | "both"
  "max_results": 5           // 1-20，默认 5
}
```

#### 输出格式
```
🔍 Found 3 relevant knowledge items for 'authentication architecture':

1. [Project] [architectural] (Depth: 2 | Confidence: ████████░░ 80%)
   Source: 🤖 Extracted by LLM
   JWT authentication middleware is implemented in src/middleware/auth.rs

2. [Project] [style] (Depth: 3 | Confidence: █████████░ 90%)
   Source: 👤 User explicitly stated
   Prefer using bearer tokens in Authorization header

3. [Global] [best_practice] (Depth: 2 | Confidence: ███████░░░ 70%)
   Source: 💬 From user feedback
   Always validate JWT expiration and issuer claims

💡 Tip: Use this information to inform your approach. If you need more details, try a more specific query.
```

#### 典型使用场景

**场景 1：了解项目架构**
```
LLM: “我需要实现用户认证，先查询项目的认证架构”
→ memory_search({"query": "authentication architecture", "scope": "project"})
```

**场景 2：检查编码约定**
```
LLM: “这个项目的错误处理约定是什么？”
→ memory_search({"query": "error handling conventions and AppError type", "scope": "project"})
```

**场景 3：查找最佳实践**
```
LLM: “Rust Web API 的常见模式有哪些？”
→ memory_search({"query": "common Rust web API patterns", "scope": "global"})
```

**场景 4：避免已知问题**
```
LLM: “之前遇到过 async/await 的问题吗？”
→ memory_search({"query": "async/await issues and solutions", "scope": "both"})
```

#### 查询技巧
- ✅ **具体化类型**：指明需要的是“architecture”、“conventions”、“preferences”等
- ✅ **包含技术关键词**：如 Rust、axum、tokio、sqlx 等
- ✅ **明确范围**：项目特定用 `"project"`，通用知识用 `"global"`
- ❌ **避免模糊**：不要只说“告诉我关于认证”，要说“认证架构和 JWT 配置”

#### 性能优化
- **查询缓存**：相同查询 60 秒内直接返回缓存结果
- **LRU 淘汰**：最多保留 100 条缓存，自动移除最旧的
- **线程安全**：使用 Mutex 保护并发访问
- **Debug 日志**：缓存命中时记录 `tracing::debug!("Cache hit for query: {}")`

### 使用示例
```bash
# 存储记忆
/remember 我喜欢在 Rust 项目中使用 snake_case 命名

# 查看记忆统计
/memory

# 删除记忆
/forget 某个关键词
```

### 知识演化
- **访问频率追踪**：自动记录哪些查询最频繁
- **记忆强化**：被访问的记忆自动增加深度和权重
- **智能推荐**：未来可基于查询模式预测需要的知识

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

## 🧬 自我进化系统

Ox 的自我进化能力基于**记忆系统 + LLM 主动查询**实现，而非传统的固定人格维度。

### 核心设计理念

**传统方式（已过时）**：
- 固定的 PersonaVector（favors_safety_over_speed、prefers_conciseness 等）
- 通过反馈信号进行数值调整
- 缺乏上下文理解能力

**新方式（当前实现）**：
```
LLM 任务执行 → 主动调用 memory_search → 获取相关知识 → 动态调整行为
```

### 工作原理

#### 1. **知识存储阶段**
用户和项目相关的信息被自动或手动存储到记忆系统：

| 来源 | 示例 |
|------|------|
| `/remember` 命令 | "我喜欢函数式编程风格" |
| 工具观察 | "项目使用 axum + tokio 架构" |
| LLM 提取 | "错误处理使用 AppError 类型" |
| 议会结论 | "优先使用 Result 而非 panic" |
| 用户反馈 | "代码太啰嗦，需要更简洁" |

#### 2. **知识查询阶段**
LLM 在任务执行的不同阶段主动调用 `memory_search` 工具：

```rust
// 任务分析阶段
memory_search("authentication architecture and JWT setup")
→ 返回项目的认证架构和 JWT 配置

// 代码编写阶段  
memory_search("error handling conventions")
→ 返回 AppError 类型定义和使用模式

// 风格调整阶段
memory_search("user coding style preferences")
→ 返回用户的编码偏好（简洁性、函数式风格等）
```

#### 3. **行为调整阶段**
LLM 根据查询结果动态调整自己的行为：

- **架构决策**：遵循项目的技术栈和设计模式
- **代码风格**：匹配用户的编码习惯（如偏好 `map/filter/reduce` 而非循环）
- **错误处理**：使用项目约定的错误类型和处理方式
- **安全性**：遵守项目的安全规范和最佳实践

### 优势对比

| 维度 | 旧方式 (PersonaVector) | 新方式 (Memory System) |
|------|------------------------|------------------------|
| **灵活性** | 固定维度，难以扩展 | 自然语言，无限扩展 |
| **上下文理解** | 无，仅数值调整 | 完整语义理解 |
| **可解释性** | 低，数值含义模糊 | 高，直接展示知识内容 |
| **学习能力** | 慢，需多次反馈 | 快，一次存储即可用 |
| **跨项目** | 不支持 | 支持 global scope |
| **追溯性** | 无历史记录 | 完整的来源和时间戳 |

### 实际应用示例

#### 场景 1: 新项目初始化
```
User: "创建一个新的 Rust web API 项目"

LLM 思考：
1. 我需要了解这个项目的技术栈约定
2. 调用: memory_search("Rust web API tech stack and architecture", scope="project")
3. 获得: "项目使用 axum + tokio + sqlx，错误处理使用 AppError"
4. 行动: 按照项目约定生成代码
```

#### 场景 2: 代码风格调整
```
User: "这个函数写得太冗长了"

LLM 思考：
1. 用户偏好更简洁的代码
2. 存储: /remember "用户偏好简洁的代码风格，避免冗余"
3. 下次任务前调用: memory_search("user code style preferences")
4. 获得: "用户偏好简洁性，喜欢函数式编程风格"
5. 行动: 使用 map/filter/reduce，减少中间变量
```

#### 场景 3: 跨项目知识复用
```
User: "在新项目中实现认证功能"

LLM 思考：
1. 我需要了解通用的认证最佳实践
2. 调用: memory_search("JWT authentication best practices", scope="global")
3. 获得: "使用 HS256 算法，token 过期时间 24h，刷新 token 7天"
4. 行动: 按照最佳实践实现认证
```

### 与记忆系统的集成

自我进化系统完全依赖记忆系统的以下特性：

1. **多类型记忆**
   - `Style`: 用户偏好（替代旧的 persona 维度）
   - `Architectural`: 架构决策
   - `BestPractice`: 最佳实践
   - `AntiPattern`: 需要避免的模式

2. **智能检索**
   - 语义搜索：理解查询意图
   - 置信度评估：优先返回高质量知识
   - 来源追踪：知道知识的来源和可信度

3. **衰减算法**
   - DEWMA/ACT-R：自动降低过时知识的权重
   - Janitor：清理低价值记忆
   - 深度递增：频繁访问的知识变得更重要

4. **查询缓存**
   - 60秒 TTL：避免重复查询
   - LRU 淘汰：保持缓存新鲜度
   - 性能提升：50x 加速

### 迁移指南

如果您之前使用了 PersonaVector，可以将其转换为记忆：

```bash
# 旧方式
/persona evolve MoreConcise

# 新方式（推荐）
/remember "用户偏好简洁的代码风格，避免冗余的解释和中间变量"

# LLM 会自动查询
memory_search("user code style preferences")
→ 获得上述记忆并应用到后续任务中
```

### 未来演进

1. **主动推理** (Phase 3)
   - LLM 根据当前任务自动推测可能需要的相关知识
   - 在用户提出需求前就准备好上下文

2. **知识演化** (Phase 3)
   - 从 LLM 的查询频率中学习哪些知识最重要
   - 自动强化高频使用的记忆

3. **双层专家系统** (Phase 3)
   - Global Expert: 跨项目的通用经验
   - Project Expert: 项目专属的深度理解

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
| 哈希 | Blake3 + DefaultHasher |
| 嵌入模型 | Candle + BGE (ModelScope) |
| 并发原语 | Mutex, Arc, RefCell |

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
