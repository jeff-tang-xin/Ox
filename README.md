# Ox

> 🐂 AI 编程助手 — 终端里的智能体，懂你的项目、记得你的偏好、能写代码。

Ox 是一个基于 Rust 开发的 AI 编程助手，通过精美的 TUI 界面连接 LLM，为开发者提供代码读取、搜索、编写和执行能力，同时具备强大的记忆系统和多模式工作流管理。

---

## 🌟 核心亮点

### 1. 🧠 智能记忆系统
- **SQLite 持久化**：记忆永不过期，重启后自动恢复
- **DEWMA + ACT-R 衰减模型**：模拟人类遗忘曲线，重要信息长期保留
- **Janitor 自动清理**：后台清理低价值节点，保持系统高效
- **分类记忆**：Fact / Style / Architectural / Pattern / Business 等 9 种类型

### 2. 🗜️ 上下文压缩 (KadaneDial)
- **BGE 嵌入模型**：本地运行，无需 API 调用
- **动态选择最优片段**：基于语义相关性自动压缩长对话
- **三种精度级别**：small (~130MB) / base (~420MB) / large (~1.2GB)

### 3. 🏛️ 多模型议会 (Council)
- **多角色辩论**：多个模型同时参与决策
- **提案→评审→反驳→仲裁**：结构化决策流程
- **防止单一模型偏见**：汇聚多模型智慧

### 4. 🔄 自我进化系统
- **EMA 趋势追踪**：指数移动平均算法追踪满意度
- **代码覆盖检测**：自动检测用户对 AI 代码的修改
- **回滚管理**：满意度下降时自动回溯
- **隐式反馈学习**：从代码接受率、工具成功率学习

### 5. 🎨 精美 TUI 界面
- **Ratatui 渲染引擎**：高性能跨平台终端 UI
- **Markdown 渲染**：支持代码高亮、链接渲染
- **流式输出**：实时显示 LLM 响应
- **鼠标支持**：滚轮滚动、点击交互

---

## 🏗️ 项目架构

```
Ox/
├── crates/
│   ├── ox-cli/                    # CLI 可执行文件
│   │   └── src/
│   │       ├── main.rs            # 主入口 (~2600 行)
│   │       └── terminal/          # TUI 组件
│   │           ├── app.rs         # 应用状态管理
│   │           ├── event.rs       # 事件轮询 (键盘/鼠标)
│   │           ├── input_pane.rs  # 多行输入框
│   │           ├── output_pane.rs # 输出面板
│   │           ├── markdown.rs    # Markdown 渲染
│   │           ├── scrollbar.rs   # 滚动条
│   │           └── render.rs      # 布局渲染
│   │
│   └── ox-core/                   # 核心库
│       └── src/
│           ├── agent/             # 🧠 智能体 + 工作流引擎
│           │   ├── engine.rs       # 工作流执行引擎
│           │   ├── workflow.rs     # 工作流定义 (Free/Spec/Council)
│           │   ├── session.rs      # 会话状态管理
│           │   ├── interjection.rs # 打断处理
│           │   ├── interrupt.rs    # 中断处理
│           │   └── intervention.rs # 干预机制
│           │
│           ├── memory/            # 🧠 记忆系统
│           │   ├── store.rs        # SQLite 存储
│           │   ├── node.rs        # 记忆节点
│           │   └── janitor.rs     # 自动清理
│           │
│           ├── embedding/         # 🗜️ 上下文压缩
│           │   ├── kadane_dial.rs  # KadaneDial 算法
│           │   ├── bge.rs         # BGE 嵌入模型
│           │   └── compression.rs  # 压缩引擎
│           │
│           ├── council/           # 🏛️ 议会系统
│           │   ├── orchestrator.rs # 辩论编排
│           │   ├── proposal.rs    # 提案阶段
│           │   ├── review.rs      # 评审阶段
│           │   └── arbitration.rs  # 仲裁阶段
│           │
│           ├── feedback/          # 🔄 自我进化
│           │   ├── override_detector.rs  # 代码覆盖检测
│           │   ├── ema_tracker.rs       # EMA 趋势追踪
│           │   └── rollback.rs          # 回滚管理
│           │
│           ├── llm/               # 🤖 LLM 提供商
│           │   ├── openai.rs      # OpenAI API
│           │   ├── anthropic.rs    # Anthropic API
│           │   ├── deepseek.rs     # DeepSeek API
│           │   └── tokenizer.rs   # Token 计数
│           │
│           ├── tools/             # 🛠️ 工具集 (15+)
│           │   ├── file_read.rs
│           │   ├── file_write.rs
│           │   ├── file_patch.rs
│           │   ├── file_list.rs
│           │   ├── file_search.rs
│           │   ├── code_search.rs
│           │   ├── shell_exec.rs
│           │   ├── git_*.rs
│           │   ├── memory_search.rs
│           │   └── web_fetch.rs
│           │
│           ├── context/           # 📝 上下文管理
│           ├── runtime/          # 🖥️ 运行时环境
│           ├── safety/           # 🔒 安全检查
│           ├── slash/             # ⌨️ 斜杠命令
│           ├── config/           # ⚙️ 配置管理
│           ├── cost/             # 💰 成本追踪
│           └── file_index/       # 📁 Git 感知索引
│
├── docs/                          # 技术文档
├── Cargo.toml                     # Workspace 根
└── README.md
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

# 发布构建（推荐）
cargo build --release
```

### 2. 初始化 Git 仓库 ⚠️

> **重要**：Ox 需要项目目录是 Git 仓库才能索引文件。

```bash
git init
git add .
git commit -m "Initial commit"
```

### 3. 运行 Ox

```bash
./target/release/ox
```

### 4. 配置 API 密钥

**方式一：环境变量（推荐）**

```bash
# Linux/macOS
export OX_OPENAI_API_KEY="sk-..."
export OX_ANTHROPIC_API_KEY="sk-ant-..."
export OX_DEEPSEEK_API_KEY="sk-..."

# Windows PowerShell
$env:OX_OPENAI_API_KEY="sk-..."
```

**方式二：配置文件** (`~/.ox/config.toml`)

```toml
[models]
default = "gpt-4o"

[models.providers.openai]
api_key = "sk-..."

[models.providers.anthropic]
api_key = "sk-ant-..."
```

### 5. 下载嵌入模型（可选）

```bash
# 进入 Ox REPL 后执行
/download-model                    # 下载默认模型 (~130MB)
/download-model bge-base-zh-v1.5  # 下载 base 模型 (~420MB)
```

---

## ⌨️ 完整命令参考

### 会话管理
| 命令 | 说明 |
|------|------|
| `/help [topic]` | 显示帮助 |
| `/exit` | 退出 |
| `/new` | 新会话 |
| `/sessions` | 查看历史会话 |
| `/resume <file>` | 恢复会话 |
| `/clear` | 清除输出 |
| `/debug` | 调试信息 |

### 工作模式
| 命令 | 说明 |
|------|------|
| `/free` | 自由模式（默认） |
| `/spec on <任务>` | 规范模式（6步工作流） |
| `/council start <主题>` | 议会辩论模式 |
| `/spec show` | 显示当前规范 |
| `/spec off` | 退出规范模式 |

### 记忆管理
| 命令 | 说明 |
|------|------|
| `/memory` | 查看记忆统计 |
| `/remember <内容>` | 存储记忆 |
| `/forget <关键词>` | 删除记忆 |
| `memory_search <query>` | 查询记忆 |

### 工具信任
| 命令 | 说明 |
|------|------|
| `/trust <tool>` | 信任工具 |
| `/trust --all` | 信任所有工具 |
| `/untrust` | 撤销信任 |

### 配置与成本
| 命令 | 说明 |
|------|------|
| `/init` | 创建默认配置 |
| `/cost` | 费用统计 |
| `/plan` | 任务计划 |
| `/model <name>` | 切换模型 |
| `/reload` | 重载配置 |
| `/download-model [name]` | 下载嵌入模型 |

### 快捷键
| 快捷键 | 功能 |
|--------|------|
| `Enter` | 发送消息 |
| `Ctrl+C` | 中断响应 |
| `↑/↓` | 历史导航 |
| `Ctrl+A/E` | 行首/行尾 |
| 鼠标滚轮 | 滚动输出 |

---

## 🛠️ 内置工具集

### 文件操作
| 工具 | 级别 | 说明 |
|------|------|------|
| `file_read` | Safe | 读取文件（支持行号范围） |
| `file_write` | Confirm | 写入/创建文件 |
| `file_patch` | Confirm | 搜索替换（精确匹配） |
| `file_list` | Safe | 列出目录内容 |
| `file_search` | Safe | glob 模式搜索 |
| `code_search` | Safe | 正则/文本搜索 |
| `file_index` | Safe | Git 感知索引搜索 |

### Shell 命令
| 工具 | 级别 | 说明 |
|------|------|------|
| `shell_exec` | Dangerous | 执行 Shell 命令 |

### Git 操作
| 工具 | 级别 | 说明 |
|------|------|------|
| `git_status` | Safe | 查看状态 |
| `git_diff` | Safe | 查看变更 |
| `git_commit` | Confirm | 提交变更 |

### 其他工具
| 工具 | 级别 | 说明 |
|------|------|------|
| `memory_search` | Safe | 记忆知识库查询 |
| `project_detect` | Safe | 检测项目类型/语言 |
| `web_fetch` | Safe | 抓取网页内容 |
| `content_validation` | Safe | 内容验证 |

### 工具安全级别
- **Safe**：无需确认，直接执行
- **Confirm**：需要用户确认
- **Dangerous**：高危操作，需要 `/trust --all` 或 `--dangerous` 确认

---

## 🧠 记忆系统详解

### 概述

Ox 的记忆系统模拟人类记忆机制设计，支持项目级和全局级记忆，通过科学的衰减算法确保重要信息长期保留。

### 记忆架构

```
┌─────────────────────────────────────────────────────────────┐
│                      Memory System                          │
├─────────────────────────────────────────────────────────────┤
│  ~/.ox/db/memories_overall.db     ← 全局记忆（跨项目）      │
│  ~/.ox/db/memories_<project>.db   ← 项目级记忆              │
├─────────────────────────────────────────────────────────────┤
│  WriteBuffer (容量: 10条 / 5秒刷新)                          │
│  ├── 即时写入: Style, Architectural, MetaSkill, Council    │
│  └── 延迟写入: Fact (批量刷新)                              │
└─────────────────────────────────────────────────────────────┘
```

### 记忆节点类型

| 类型 | 深度 | 半衰期 | 说明 | 来源 |
|------|------|--------|------|------|
| **Fact** | 1 | 短期 | 事实性信息 | 工具观察 |
| **Style** | 3 | 中期 | 代码风格偏好 | 用户指令 |
| **Architectural** | 2 | 中期 | 架构决策 | LLM 提取 |
| **AntiPattern** | 2 | 中期 | 反模式记录 | 错误观察 |
| **Business** | 2 | 中期 | 业务逻辑 | 工具观察 |
| **BestPractice** | 2 | 长期 | 最佳实践 | LLM 提取 |
| **Pattern** | 2 | 长期 | 设计模式 | LLM 提取 |
| **MetaSkill** | 3 | 长期 | 元技能 | LLM 提取 |
| **Council** | 3 | 长期 | 议会结论 | 辩论结论 |

### 衰减算法

#### 1. DEWMA（双指数加权移动平均）

适用于项目级记忆，结合短期和长期衰减：

```
衰减分数 = 0.7 × 短期衰减 + 0.3 × 长期衰减

短期衰减 = exp(-天数 / (半衰期 × 0.3))
长期衰减 = exp(-天数 / (半衰期 × 5.0))
```

#### 2. ACT-R（自适应控制理论）

适用于全局记忆，基于激活阈值：

```
衰减分数 = Σ(trace[i] × exp(-t/τ[i])) / n
综合分数 = 衰减分数 × 语言权重 + 深度 × 0.5
```

### Janitor 自动清理

后台任务定期清理低价值记忆：

| 深度 | 清理条件 |
|------|----------|
| 0-1 | 创建 30 天后 |
| 2 | 创建 60 天后 且 衰减 < 0.3 |
| 3+ | 衰减 < 0.1 |

### 记忆来源

| 来源 | 说明 |
|------|------|
| `UserExplicit` | 用户显式 `/remember` |
| `ToolObservation` | 工具执行结果观察 |
| `LlmExtraction` | LLM 自动提取 |
| `CouncilConclusion` | 议会辩论结论 |
| `Feedback` | 用户反馈信号 |

### WriteBuffer 机制

- **容量**：10 条记录
- **刷新间隔**：5 秒
- **即时写入类型**：Style、Architectural、MetaSkill、Council
- **延迟写入类型**：Fact（累积后批量刷新）

---

## 🗜️ 上下文压缩系统详解

### KadaneDial 算法

KadaneDial 是一种基于最大子数组思想的语义压缩算法，用于选择与当前查询最相关的对话片段。

### 算法流程

```
┌──────────────────────────────────────────────────────────────┐
│                    KadaneDial 压缩流程                       │
├──────────────────────────────────────────────────────────────┤
│  1. 嵌入编码                                                  │
│     └── BGE 模型将对话片段转为向量                            │
│                           ↓                                   │
│  2. 相似度计算                                                │
│     └── 计算片段与当前查询的余弦相似度                        │
│                           ↓                                   │
│  3. Z-Score 标准化                                            │
│     └── 归一化相似度分布，消除量纲差异                         │
│                           ↓                                   │
│  4. 增益计算                                                  │
│     └── gain = similarity - τ (阈值)                         │
│                           ↓                                   │
│  5. Kadane 搜索                                              │
│     └── 最大子数组搜索，找到增益最大的片段组合                 │
│                           ↓                                   │
│  6. 返回片段                                                  │
│     └── 输出语义最相关且连续的最佳片段组合                     │
└──────────────────────────────────────────────────────────────┘
```

### 核心参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `history_ratio` | 0.10 | 历史占上下文窗口比例 |
| `memory_ratio` | 0.02 | 记忆占上下文比例 |
| `chunk_threshold` | 150 | 分块阈值（tokens） |
| `max_chunk_tokens` | 300 | 最大块大小 |
| `kadane_threshold` | 0.5 | 增益阈值 τ |
| `kadane_max_segments` | 5 | 最大片段数 |
| `keep_recent` | 3 | 保留最近 N 条 |

### 压缩级别

| 级别 | 说明 | 触发条件 |
|------|------|----------|
| **Light** | 仅截断长工具结果 | Token 预算 < 70% |
| **Medium** | 语义相关性选择 | Token 预算 70-90% |
| **Heavy** | 更严格过滤 | Token 预算 > 90% |

### BGE 嵌入模型

| 模型 | 大小 | 维度 | 下载 |
|------|------|------|------|
| `bge-small-zh-v1.5` | ~130MB | 512 | `/download-model` |
| `bge-base-zh-v1.5` | ~420MB | 768 | `/download-model bge-base-zh-v1.5` |
| `bge-large-zh-v1.5` | ~1.2GB | 1024 | `/download-model bge-large-zh-v1.5` |

### 智能触发条件

压缩触发需要同时满足：

1. **Token 条件**：上下文预算达到 80%
2. **结构条件**（至少一个）：
   - 任务不完整（检测到未完成的实现步骤）
   - 工具交互增长（连续多个工具调用）
   - 主题漂移（偏离原始任务）

### Token 预算分配

```
┌─────────────────────────────────────────────────────────────┐
│                    Token 预算分配                            │
├─────────────────────────────────────────────────────────────┤
│  System Prompt  ██ 2%                                      │
│  Memory Context ██ 2%                                     │
│  History         ████████░░░░░░░░░░░░░░░░░░░░░░ 10%        │
│  Reply Reserve   ████████████████████████████░░░ 85%       │
│                                                             │
│  预留 Reply Reserve 确保模型有足够空间输出响应              │
└─────────────────────────────────────────────────────────────┘
```

---

## 🏛️ 议会系统详解

### 概述

议会模式是一种多模型辩论决策系统，通过结构化对话让多个模型从不同角度分析问题，最终汇聚共识。

### 议会流程

```
┌────────────────────────────────────────────────────────────────────┐
│                      Council Session                                │
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│  Phase 1: Proposal (提案)                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                        │
│  │Proposer₁ │→ │ 方案 A   │                                  │
│  └──────────┘  └──────────┘  ┌──────────┐                        │
│  ┌──────────┐  ┌──────────┐  │Proposer₂ │→ │ 方案 B   │                        │
│  └──────────┘  └──────────┘  └──────────┘  ┌──────────┐                        │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  │Proposer₃ │→ │ 方案 C   │                        │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘                        │
│                                                                    │
│  Phase 2: Cross-Review (交叉评审)                                  │
│  ┌──────────────────────────────────────────────────────────┐      │
│  │ Reviewer → 评审所有方案 (可行性、创新性、风险、成本)     │      │
│  └──────────────────────────────────────────────────────────┘      │
│                                                                    │
│  Phase 3: Rebuttal (反驳)                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                        │
│  │Proposer₁ │→ │回应批评  │                                  │
│  └──────────┘  └──────────┘  ┌──────────┐                        │
│  ┌──────────┐  ┌──────────┐  │Proposer₂ │→ │回应批评  │                        │
│  └──────────┘  └──────────┘  └──────────┘  ┌──────────┐                        │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  │Proposer₃ │→ │回应批评  │                        │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘                        │
│                                                                    │
│  Phase 4: Arbitration (仲裁)                                      │
│  ┌──────────────────────────────────────────────────────────┐      │
│  │ Arbiter → 最终推荐 + 置信度 + 关键分歧 + 来源依据        │      │
│  └──────────────────────────────────────────────────────────┘      │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

### 参与者角色

| 角色 | 说明 | 输入 |
|------|------|------|
| **Proposer** | 提出初始方案 | 辩论主题 |
| **Reviewer** | 评审所有方案 | 所有提案 |
| **Arbiter** | 综合评审，输出推荐 | 提案 + 评审 + 反驳 |

### 仲裁输出结构

```toml
{
  "recommendation": "推荐方案 B，原因...",
  "confidence": 0.85,
  "key_disagreements": [
    "A 认为 X，B 认为 Y",
    "C 忽略了 Z"
  ],
  "primary_source": "Proposer₂",
  "pros_cons": {
    "方案A": { "pro": "...", "con": "..." },
    "方案B": { "pro": "...", "con": "..." }
  }
}
```

### 议会特点

- **禁止代码修改**：议会模式只能讨论，不能执行代码
- **多轮辩论**：可配置辩论轮数（默认 2 轮）
- **收敛机制**：通过 Arbiter 整合分歧，避免无限辩论

### 使用示例

```bash
/council start "应该使用微服务还是单体架构"
/council start "选择哪种数据库方案"
/council start "如何优化系统性能"
```

---

## 🔄 自我进化系统详解

### 概述

自我进化系统通过隐式反馈和显式反馈持续优化 Ox 的行为，减少用户纠正次数。

### 系统架构

```
┌────────────────────────────────────────────────────────────────────┐
│                      Self-Evolution System                         │
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│  ┌─────────────────┐     ┌─────────────────┐                      │
│  │ OverrideDetector│────→│  EMA Tracker    │                      │
│  │ 代码覆盖检测    │     │  趋势追踪      │                      │
│  └─────────────────┘     └────────┬────────┘                      │
│           │                        │                               │
│           │                        ↓                               │
│           │               ┌─────────────────┐                      │
│           │               │ RollbackManager │                      │
│           │               │  回滚管理       │                      │
│           │               └────────┬────────┘                      │
│           │                        │                               │
│           ↓                        ↓                               │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │                    Satisfaction Score                        │  │
│  │  = 显式反馈率×权重 + 工具成功率×权重 + 代码接受率×权重       │  │
│  └─────────────────────────────────────────────────────────────┘  │
│                            │                                       │
│                            ↓                                       │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │                    Memory Update                             │  │
│  │  高满意度 → 强化记忆 / 低满意度 → 回滚 + 清理记忆           │  │
│  └─────────────────────────────────────────────────────────────┘  │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

### 1. 代码覆盖检测 (OverrideDetector)

检测用户对 Ox 编写代码的修改：

```rust
struct WriteRecord {
    content_hash: u64,    // Ox 写入时的内容哈希
    line_count: usize,     // 行数
    timestamp: Instant,    // 写入时间
}

struct OverrideSignal {
    path: PathBuf,
    change_ratio: f64,     // 0.0~1.0, 变化比例
    time_elapsed: Duration, // 距写入的时间
}
```

**检测逻辑**：
- 5 分钟内检测窗口
- 计算 `change_ratio = 修改行数 / 总行数`
- 删除文件视为 `change_ratio = 1.0`
- 高变化率 = 低满意度信号

### 2. EMA 趋势追踪 (EmatrendTracker)

指数移动平均算法追踪满意度趋势：

```rust
struct EmatrendTracker {
    current_value: f64,    // 当前 EMA 值
    trend: f64,            // 趋势方向 (-1.0~1.0)
    alpha: f64,            // 平滑因子 (0.1-0.3)
}

// 更新 EMA
ema = old + α × (new - old)
trend = current - old
```

**趋势判断**：
- `trend > 阈值` → 上升趋势
- `trend < -阈值` → 下降趋势
- 趋势显著时触发记忆更新

### 3. 回滚管理 (RollbackManager)

满意度下降到阈值时触发回滚：

```rust
degradation = baseline - current

if degradation > 0.2 {
    // 需要回滚
    RollbackDecision::NeedsRollback { ... }
}
```

**满意度评分公式**：

| 有显式反馈 | 无显式反馈 |
|-----------|-----------|
| `显式反馈率×0.4 + 工具成功率×0.3 + 代码接受率×0.3` | `显式反馈率×0.1 + 工具成功率×0.3 + 代码接受率×0.6` |

### 4. 反馈信号映射

```
OverrideSignal → ImplicitFeedback → Satisfaction Score
     ↓
change_ratio 高 → 负面信号 → 降低满意度
change_ratio 低 → 正面信号 → 提高满意度
```

### 使用方式

```bash
# 提供显式反馈
/good           # 正面反馈
/bad            # 负面反馈
/feedback <category>  # 指定类别反馈
```

---

## 🎨 TUI 界面系统详解

### 概述

Ox 使用 Ratatui 库构建高性能跨平台终端用户界面，支持 Markdown 渲染、语法高亮和鼠标交互。

### 界面布局

```
┌─────────────────────────────────────────────────────────────────┐
│  ┌─────────────────────────────────────────────────────────────┐│
│  │ Ox AI Assistant v0.1.0                            [模型]    ││
│  │ Project: my-project (Rust) | Windows (pwsh)                 ││
│  └─────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                                                             ││
│  │  [Assistant]  这是 Ox 的回复...                             ││
│  │                                                             ││
│  │  ```rust                                                    ││
│  │  fn main() {                                                ││
│  │      println!("Hello!");                                    ││
│  │  }                                                          ││
│  │  ```                                                        ││
│  │                                                             ││
│  │  [Tool: file_read] 读取文件 src/main.rs                     ││
│  │                                                             ││
│  │  工具执行结果...                                             ││
│  │                                                             ││
│  ├─────────────────────────────────────────────────────────────┤│
│  │ User Input                                                  ││
│  │ > 请帮我实现一个函数_                                        ││
│  ├─────────────────────────────────────────────────────────────┤│
│  │ Mode: Free | Tools: 12 | Memory: 45 nodes                   ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

### 核心组件

| 组件 | 说明 |
|------|------|
| `app.rs` | 应用状态管理（会话、模式、UI状态） |
| `event.rs` | 事件轮询（键盘、鼠标、超时） |
| `render.rs` | 布局渲染（标题栏、输出、输入、状态栏） |
| `input_pane.rs` | 多行输入框（支持回车换行） |
| `output_pane.rs` | 输出面板（Markdown 渲染） |
| `markdown.rs` | Markdown 解析和渲染 |
| `scrollbar.rs` | 滚动条控制 |

### Markdown 渲染支持

- **代码块**：语法高亮（支持 100+ 语言）
- **内联代码**：等宽字体
- **链接**：可点击
- **加粗/斜体**：格式化文本
- **列表**：有序/无序列表
- **引用块**：缩进显示

### 事件处理

```rust
enum Event {
    Key(KeyEvent),      // 键盘事件
    Mouse(MouseEvent),  // 鼠标事件
    Tick,               // 定时器（刷新）
}
```

| 事件类型 | 处理 |
|---------|------|
| `Enter` | 发送消息 |
| `Ctrl+C` | 中断 LLM 响应 |
| `↑/↓` | 历史消息导航 |
| `Ctrl+A/E` | 行首/行尾 |
| `MouseWheel` | 滚动输出 |
| `Click(状态栏)` | 切换模式 |

### 流式输出

```
[Assistant] 正在思考_
            ↓
[Assistant] 正在思考...
            ↓
[Assistant] 正在思考...._
            ↓
[Assistant] 我认为这个问题的解决方案是...
```

实时显示 LLM 响应，支持：
- 打字机效果
- 工具调用指示器
- 进度显示

---

## ⚙️ 工作流引擎详解

### 概述

工作流引擎 (WorkflowEngine) 强制执行步骤化的工作流程，确保复杂任务按规范完成。

### 工作流结构

```rust
struct Workflow {
    id: String,
    name: String,
    steps: Vec<WorkflowStep>,
}

struct WorkflowStep {
    id: String,
    name: String,
    description: String,
    requires_user_confirmation: bool,  // 需要用户确认
    allow_tool_execution: bool,         // 允许工具执行
    allow_code_modification: bool,       // 允许代码修改
    step_prompt: String,                 // 步骤提示
    validator_name: Option<String>,      // 验证器名称
}
```

### 三种工作流

#### 1. Free Workflow（自由模式）

- 默认模式
- 1 步完成
- 无约束，工具和代码修改都允许

#### 2. Spec Workflow（规范模式）

6 步结构化流程：

| 步骤 | 名称 | 工具 | 代码 | 说明 |
|------|------|------|------|------|
| 1 | Requirement Analysis | ✅ | ❌ | 分析任务类型 |
| 2 | Generate Spec | ✅ | ❌ | 生成 spec.md |
| 3 | Await Confirmation | ❌ | ❌ | 等待用户确认 |
| 4 | Generate Task | ✅ | ❌ | 生成 task.md |
| 5 | Await Confirmation | ❌ | ❌ | 等待用户确认 |
| 6 | Execute | ✅ | ✅ | 执行并提交 |

#### 3. Council Workflow（议会模式）

- 6 步流程
- 所有步骤禁止代码修改
- 专门用于架构决策和设计评审

### 会话状态管理

```rust
struct SessionState {
    session_id: String,
    current_mode: String,
    current_workflow: String,
    current_step_index: usize,
    awaiting_user_confirmation: bool,
    variables: HashMap<String, String>,
    message_count: usize,
}
```

### 验证器机制

每个步骤可配置验证器：

```rust
// 示例：验证任务是否分类
validator: "check_task_classified"

// 验证器检查
if session.has_variable("task_classified") {
    // 验证通过，进入下一步
}
```

### 中断与干预

| 类型 | 说明 |
|------|------|
| `Interjection` | 用户打断（Ctrl+C） |
| `Interrupt` | 系统中断（外部信号） |
| `Intervention` | 干预请求（确认、警告） |

### 使用方式

```bash
/free                          # 切换到自由模式
/spec on "实现用户认证系统"    # 启动规范模式
/council start "选择数据库"   # 启动议会模式

---

## 🖥️ 运行时环境详解

### 概述

运行时系统检测环境信息并注入系统提示，使 Ox 能够生成正确的系统命令。

### RuntimeEnvironment 结构

```rust
struct RuntimeEnvironment {
    os: Os,                    // Windows / Linux / macOS
    arch: String,              // x86_64, aarch64, etc.
    shell: ShellInfo,          // Shell 信息
    home_dir: PathBuf,         // 用户主目录
    working_dir: PathBuf,      // 当前工作目录
    project_root: Option<PathBuf>,  // Git 项目根目录
    project_id: String,         // 项目标识
    project_language: String,  // 检测到的语言
    ox_home_dir: PathBuf,      // Ox 配置目录 (~/.ox)
    project_ox_dir: Option<PathBuf>, // 项目级 .ox 目录
}
```

### Shell 检测

| OS | Shell | 执行前缀 |
|----|-------|----------|
| Windows | PowerShell | `-Command` |
| Windows | cmd | `/C` |
| Linux/macOS | bash | `-c` |
| Linux/macOS | zsh | `-c` |

### 系统提示注入

```markdown
## Environment
- OS: Windows
- Shell: PowerShell
- Working Directory: F:\rust\Ox
- Project Root: F:\rust\Ox
- Project Type: Rust
- Ox Home: ~/.ox/
```

### 项目检测

自动检测项目类型和语言：

```bash
# 检测逻辑
Cargo.toml → Rust
package.json → JavaScript/TypeScript
go.mod → Go
requirements.txt → Python
pom.xml → Java
```

---

## 🔒 安全系统详解

### 概述

安全系统通过信任管理和危险命令检测保护用户系统和文件。

### TrustManager（信任管理）

会话级工具信任管理：

```rust
struct TrustManager {
    trusted_tools: HashSet<String>,
}

impl TrustManager {
    // Safe 工具：始终允许
    // RequiresConfirmation：需信任或 --all
    // Dangerous：需 --all
    fn can_skip_confirmation(&self, tool: &str, level: SafetyLevel) -> bool
}
```

### 信任级别

| 级别 | 说明 | 绕过条件 |
|------|------|----------|
| **Safe** | 无害操作 | 自动放行 |
| **RequiresConfirmation** | 需确认 | `/trust <tool>` 或 `/trust --all` |
| **Dangerous** | 危险操作 | `/trust --all` |

### 高危命令检测

```rust
fn is_high_risk_command(cmd: &str) -> bool {
    let patterns = [
        "rm -rf",           // 递归删除
        "rm -r /",          // 删除根目录
        "format ",          // 格式化
        "mkfs",             // 创建文件系统
        "dd if=",           // 磁盘写入
        ":(){ :|:& };:",    // Fork 炸弹
        "chmod -R 777",     // 过度权限
        "curl | sh",        // 远程执行
        "> /dev/sda",       // 直接写入设备
    ];
}
```

### 路径限制

防止操作工作目录外的文件：

```rust
fn is_path_within_workdir(path: &Path, workdir: &Path) -> bool {
    // 规范化路径后检查是否在工作目录内
    canonical_path.starts_with(canonical_workdir)
}
```

### 使用方式

```bash
/trust file_write        # 信任文件写入工具
/trust git_commit        # 信任 Git 提交
/trust --all            # 信任所有非危险工具
/untrust               # 撤销所有信任

---

## 💰 成本追踪系统

### 概述

自动追踪 API 调用成本，帮助用户控制费用。

### 追踪数据

```rust
struct CostRecord {
    timestamp: String,
    model: String,
    prompt_tokens: u32,
    completion_tokens: u32,
    cost: f64,
}

struct DailyCost {
    date: String,
    total_cost: f64,
    call_count: u32,
    prompt_tokens: u32,
    completion_tokens: u32,
}
```

### 费用计算

| 模型 | 输入 | 输出 |
|------|------|------|
| GPT-4o | $2.50/1M | $10.00/1M |
| GPT-4o-mini | $0.15/1M | $0.60/1M |
| Claude-3.5-Sonnet | $3.00/1M | $15.00/1M |
| DeepSeek-V3 | $0.27/1M | $1.10/1M |

### 成本限制配置

```toml
[cost]
max_monthly_cost = 5.0      # 月度上限
max_daily_cost = 2.0        # 日度上限
warning_threshold = 0.8     # 警告阈值 (80%)
```

### 使用方式

```bash
/cost    # 查看费用统计
```

### 输出示例

```
📊 成本统计

今日: $0.45 (23 次调用)
本月: $3.20 / $5.00 (64%)
├── GPT-4o: $2.80
├── Claude: $0.40

⚠️ 即将达到月度限制 (64%)

---

## 📁 文件索引系统

### 概述

Git 感知的文件索引系统，用于快速搜索项目文件。

### 索引结构

```rust
struct FileIndexEntry {
    id: i64,
    filename: String,
    full_path: String,
    file_type: Option<String>,
}

struct FileIndexManager {
    conn: Mutex<Connection>,  // SQLite WAL 模式
}
```

### 数据库表

```sql
CREATE TABLE file_index (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    filename TEXT NOT NULL,
    full_path TEXT NOT NULL UNIQUE,
    file_type TEXT
);
CREATE INDEX idx_filename ON file_index(filename);
```

### 功能

| 方法 | 说明 |
|------|------|
| `batch_insert()` | 批量插入文件 |
| `find_by_filename()` | 按文件名查找 |
| `find_by_id()` | 按 ID 精确查找 |
| `list_all_files()` | 列出所有文件 |
| `search_files()` | 模式匹配搜索 |
| `clear()` | 清空索引重建 |

### Git 集成

- 自动扫描 Git 跟踪的文件
- 忽略 .gitignore 中的文件
- 支持增量更新

### 使用方式

```bash
# 工具调用
file_index search "main.rs"
file_list "src/"
```

---

## 🤖 LLM 提供商

### 支持的模型

| 提供商 | 模型 | 上下文窗口 | 说明 |
|--------|------|-----------|------|
| **OpenAI** | gpt-4o | 128K | 默认 |
| | gpt-4o-mini | 128K | 便宜快速 |
| | o1 | 200K | 推理模型 |
| **Anthropic** | claude-3.5-sonnet | 200K | 高质量 |
| | claude-3-opus | 200K | 最强 |
| **DeepSeek** | deepseek-chat | 64K | 性价比 |

### API 配置

```toml
[models]
default = "gpt-4o"

[models.providers.openai]
api_key = "sk-..."
base_url = "https://api.openai.com/v1"

[models.providers.anthropic]
api_key = "sk-ant-..."

[models.providers.deepseek]
api_key = "sk-..."
base_url = "https://api.deepseek.com/v1"
```

### 环境变量

```bash
OX_OPENAI_API_KEY
OX_ANTHROPIC_API_KEY
OX_DEEPSEEK_API_KEY
```

### 模型切换

```bash
/model gpt-4o-mini
/model claude-3.5-sonnet
/model deepseek-chat

---

## 📂 完整配置示例

```toml
# ~/.ox/config.toml

# ============ 模型配置 ============
[models]
default = "gpt-4o"

[models.providers.openai]
api_key = "sk-..."
base_url = "https://api.openai.com/v1"

[models.providers.anthropic]
api_key = "sk-ant-..."

[models.providers.deepseek]
api_key = "sk-..."
base_url = "https://api.deepseek.com/v1"

# ============ 上下文配置 ============
[context]
history_ratio = 0.10        # 历史占上下文比例
memory_ratio = 0.02        # 记忆占上下文比例
system_prompt_ratio = 0.02 # 系统提示比例

# ============ 智能体配置 ============
[agent]
max_iterations = 50        # 单轮最大迭代次数

# ============ 压缩配置 ============
[compression]
enabled = true
model = "bge-small-zh-v1.5"
kadane_threshold = 0.5
kadane_max_segments = 5
keep_recent = 3

# ============ 成本配置 ============
[cost]
max_monthly_cost = 5.0     # 月度上限 (美元)
max_daily_cost = 2.0       # 日度上限
warning_threshold = 0.8    # 警告阈值

# ============ 记忆配置 ============
[memory]
max_nodes = 1000          # 最大记忆节点

# ============ 议会配置 ============
[council]
default_rounds = 2         # 默认辩论轮次

# ============ 安全配置 ============
[safety]
allow_dangerous_tools = false
confirm_shell_commands = true
```

---

## 📂 数据存储

| 路径 | 用途 |
|------|------|
| `~/.ox/config.toml` | 用户配置 |
| `~/.ox/sessions/` | 会话历史（JSONL） |
| `~/.ox/db/memories_overall.db` | 全局记忆 SQLite |
| `~/.ox/db/memories_<id>.db` | 项目记忆 SQLite |
| `~/.ox/models/` | BGE 嵌入模型 |
| `~/.ox/logs/` | 日志文件 |
| `~/.ox/cost_tracking.json` | 成本追踪 |
| `<项目>/.ox/` | 项目级配置 |

---

## ⚙️ 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust (edition 2024) |
| 异步运行时 | Tokio |
| TUI | Ratatui + Crossterm |
| HTTP | Reqwest |
| 代码高亮 | Syntect |
| 数据库 | rusqlite (bundled SQLite) |
| 嵌入模型 | Candle + BGE (ModelScope) |
| 日志 | tracing |
| 解析 | serde_json, toml |

---

## 📜 License

MIT License - Copyright (c) 2026 Jeff Tang

---

## 📚 相关文档

- `docs/Workflow_Engine_完整调用流程.md` - 工作流引擎详解
- `docs/main_rs_analysis.md` - main.rs 重构建议
- `docs/README_模板.md` - 文档模板

---

**Made with ❤️ by Jeff Tang**
```
```
```
```
```

