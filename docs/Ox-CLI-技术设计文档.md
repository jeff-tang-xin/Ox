# Ox CLI 技术设计文档

> **版本**: v2.1 (REPL Agent 架构 - 完整版)
> **日期**: 2026-04-22
> **状态**: 设计阶段
>
> 整合自:
> - AI智能体需求整合 (Final v1.0)
> - 多语言人格与记忆优化设计 (v1.1 ~ v1.3)
> - Ox CLI混合记忆系统设计

---

## 目录

1. [项目概述与设计目标](#1-项目概述与设计目标)
2. [AI 助手准则](#2-ai-助手准则)
3. [系统架构](#3-系统架构)
4. [核心数据结构](#4-核心数据结构)
5. [REPL 交互引擎](#5-repl-交互引擎)
6. [消息协议与会话管理](#6-消息协议与会话管理)
7. [Tool 系统](#7-tool-系统)
8. [上下文窗口与 Token 管理](#8-上下文窗口与-token-管理)
9. [LLM 调用层](#9-llm-调用层)
10. [多 AI 议会系统 (Council)](#10-多-ai-议会系统-council)
11. [记忆系统](#11-记忆系统)
12. [人格系统](#12-人格系统)
13. [自演化系统 (DGM)](#13-自演化系统-dgm)
14. [安全模块](#14-安全模块)
15. [用户反馈系统](#15-用户反馈系统)
16. [多语言支持](#16-多语言支持)
17. [插件与扩展系统](#17-插件与扩展系统)
18. [Slash 命令参考](#18-slash-命令参考)
19. [配置文件规范](#19-配置文件规范)
20. [验收清单与测试计划](#20-验收清单与测试计划)
21. [路线图](#21-路线图)
22. [附录: 设计决策记录](#22-附录-设计决策记录)

---

## 1. 项目概述与设计目标

### 1.1 项目定位

Ox 是一个**以多轮对话为核心交互方式的 AI 编程智能体**。用户在终端启动 `ox` 后进入持续对话环境，可以用自然语言指挥 Ox 完成代码编写、调试、文件操作、命令执行等编程任务。

```
Ox 不是一组 CLI 子命令的集合。
Ox 是一个驻留在终端的 AI 编程搭档，
它能理解你的项目、记住你的偏好、主动使用工具完成任务。
当遇到复杂问题时，它会召集多个 AI 模型展开辩论，给你最优解。
```

**Ox 的八字定位:**

```
会遗忘 · 会学习 · 有风格 · 懂节制 · 可信赖 · 有底线 · 守准则 · 能进化
```

| 定位 | 含义 | 对应机制 |
|------|------|----------|
| **会遗忘** | 记忆有衰减，不是无限堆积；遗忘是智能的一部分 | DEWMA / ACT-R 衰减 + Janitor 清理 |
| **会学习** | 从每次交互中积累知识，跨项目迁移经验 | 记忆系统 + 跨项目知识转化 (MetaSkill) |
| **有风格** | 行为和表达有个性，适配用户偏好和语言习惯 | PersonaVector + 多语言人格差异化 |
| **懂节制** | Token 成本可控，不盲目消耗，量入为出 | Token 压缩策略 + 预算管理 + Effort 分级 |
| **可信赖** | 输出可靠，行为可预期，复杂问题多模型交叉验证 | 多 AI 议会辩论 + Tool 安全拦截 |
| **有底线** | 绝不执行危险操作，安全字段不可被演化篡改 | 6 层安全 + `refuses_unsafe_code` 锁定 |
| **守准则** | 严格遵循 AI 助手行为规则，意图理解有章可循 | AI 助手准则 (Ch.2) + 行为规则引擎 |
| **能进化** | 自我改进但需要证据，盲目调参不如不变 | DGM + MetaController + 评估函数验证 |

### 1.2 核心能力

**核心壁垒 (产品护城河):**
- **混合记忆系统**: 项目记忆 + 长期记忆，支持衰减、清理、隔离、跨项目知识转化。用过 3 个月的 Ox 记住你的项目架构、编码偏好和踩过的坑 — 这是最大的迁移成本和差异化来源
- **自适应人格**: PersonaVector 追踪用户偏好，支持多语言人格差异化
- **自我演化 (DGM)**: 基于显式反馈 + 隐式行为信号的自我改进机制

**基础能力:**
- **多轮对话**: 持续 REPL 对话环境，项目绑定会话，自动恢复
- **完整 Tool Use**: 自主读写文件、执行 shell、搜索代码、运行构建/测试命令等
- **远程 LLM 驱动**: 所有推理通过远程 LLM 完成 (OpenAI / Anthropic / DeepSeek)
- **安全优先**: 危险操作拦截、沙箱执行、敏感数据脱敏

**高阶能力 (低频高价值):**
- **多 AI 议会**: 复杂任务启动多模型辩论，交叉评审后综合最优方案。日常编码不需要，但架构决策、技术选型等关键时刻提供多模型交叉验证的可信答案

### 1.3 设计目标

**四层架构目标:**

| 层 | 名称 | 目标 |
|----|------|------|
| L0 | 基础功能层 | 意图理解、行为规则、安全底线 |
| L1 | 远程模型层 | 多模型调度、Token 成本管理、Embedding/Reranker |
| L2 | 记忆图谱层 | Memory Graph、混合记忆、衰减引擎、Janitor 清理 |
| L3 | 人格 + 自演化层 | PersonaVector、DGM、MetaController |

**核心设计原则:**

1. **对话即界面**: REPL 是唯一主界面，所有功能通过对话或 /slash 命令触达
2. **Agent 自主性**: Ox 可自主决定调用哪些工具，不需要用户逐步指令
3. **安全不可妥协**: `refuses_unsafe_code` 等安全字段不可被自动演化修改
4. **渐进式智能**: 从简单到复杂，记忆深度随使用自然增长
5. **隐私保护**: 敏感数据在发送到远程 LLM 前自动脱敏，本地存储加密
6. **成本可控**: Token 用量透明，预算可配置，超限自动告警

### 1.4 核心价值观 (KEY TAKEAWAY)

```
"会遗忘，所以珍贵"
  → 记忆不等于存储，遗忘不等于丢失。衰减和清理是智能的一部分

"会学习，所以成长"
  → 每一次交互都是学习机会，知识能跨项目迁移，经验不断积累

"有风格，所以独特"
  → PersonaVector 是基于数据的行为倾向，不是预设的角色脚本

"懂节制，所以可持续"
  → Token 有预算，复杂度有分级，不盲目消耗算力

"可信赖，所以敢用"
  → 复杂问题多模型辩论交叉验证，不是单一模型的一家之言

"有底线，所以安全"
  → refuses_unsafe_code 永不可被自动修改，安全是底线而非特性

"守准则，所以一致"
  → 行为规则引擎保证输出稳定可预期，不随性发挥

"能进化，所以持久"
  → DGM 的每一次演化都需要评估函数验证，不是盲目调参
```

---

## 2. AI 助手准则

### 2.1 底层执行准则 (Coding Principles)

**以下四项准则是 Ox 一切代码行为的基石。在生成、修改、审查任何代码前，Ox 必须自动遵循这四条准则。这些准则同时作为 Ox 自身的 AI 执行准则 -- 不仅约束 Ox 为用户写的代码，也约束 Ox 自身的决策过程。**

#### P1: 先思考，再编码 (Think Before Coding)

**不要假设。不要隐藏困惑。显式暴露权衡。**

- 行动前显式陈述假设。如果不确定，先问用户
- 如果存在多种合理解读，全部列出 -- 不要静默选择
- 如果存在更简单的方案，主动说出来。必要时应当推回用户的方案
- 如果有任何不清楚的地方，停下来。指出具体哪里令人困惑。然后问

**Ox 自身执行规则**: 在调用任何 Tool 之前，Ox 必须先在内部评估：这次操作的前提假设是什么？有没有更简单的替代方案？如果答案不明确，先用 `file_read` / `code_search` 了解情况，或者直接问用户。

#### P2: 简洁优先 (Simplicity First)

**用最少的代码解决问题。不做投机性编码。**

- 不添加未被要求的功能
- 不为一次性代码创建抽象
- 不添加未被要求的 "灵活性" 或 "可配置性"
- 不为不可能发生的场景编写错误处理
- 如果写了 200 行但 50 行能解决，重写

**自检**: "一个高级工程师会说这过于复杂吗？" 如果是，简化。

**Ox 自身执行规则**: Ox 生成的代码必须是解决当前问题的最小集合。不主动添加文档注释、类型标注、日志打印等用户没要求的内容。

#### P3: 精准修改 (Surgical Changes)

**只动必须动的部分。只清理自己造成的混乱。**

编辑已有代码时:
- 不 "顺便改进" 相邻的代码、注释或格式
- 不重构没有问题的代码
- 匹配已有风格，即使你会做得不同
- 如果发现无关的死代码，提及但不删除

当你的修改产生了孤儿引用时:
- 移除因你的修改而变为未使用的 import/变量/函数
- 不移除修改前就已存在的死代码 (除非用户要求)

**验证**: 每一个被修改的行都应当能直接追溯到用户的请求。

**Ox 自身执行规则**: Ox 在 `file_write` 时只修改与用户请求直接相关的部分。如果发现相邻代码有问题，以建议的形式告知用户，不擅自修改。

#### P4: 目标驱动执行 (Goal-Driven Execution)

**定义成功标准。循环直到验证通过。**

将任务转化为可验证的目标:
- "添加校验" → 先写无效输入的测试，再让测试通过
- "修复 bug" → 先写复现 bug 的测试，再让测试通过
- "重构 X" → 确保重构前后测试都通过

多步任务时，陈述简要计划:

```
1. [步骤] → 验证: [检查方式]
2. [步骤] → 验证: [检查方式]
3. [步骤] → 验证: [检查方式]
```

**Ox 自身执行规则**: Ox 在执行多步任务时，必须先制定计划并告知用户，每一步完成后验证结果，而不是盲目执行到底。

### 2.2 意图理解准则

Ox 作为 AI 编程助手，必须先理解用户真正想做什么，再行动:

1. **代码生成** -- "帮我写一个排序函数"
   - 优先使用标准库 (而不是手写 200 行 sort)
   - 变量名/函数名遵循当前项目语言的命名规范
2. **代码补全** -- "补全这个函数" + 上下文
   - 需读取上下文 (前 200 行后 50 行) 再补全
3. **调试** -- "这段代码有 bug"
   - 先读代码，不要盲目猜测 (遵循 P1)
4. **解释** -- "解释一下 API 概念"
   - 根据 `expertise_level` 调整解释深度
5. **执行命令** -- "curl -X POST http://localhost:8080/api/test"
   - 执行前进行安全检查

### 2.3 行为准则

1. **操作前先说明** -- 调用工具前先告诉用户要做什么 (遵循 P1)
2. **不猜测文件内容** -- 不确定时先用 file_read 或 code_search 了解情况 (遵循 P1)
3. **代码质量** -- 生成的代码应当能直接通过编译/解释 (遵循 P2)
4. **最小修改** -- 只改该改的，不做无关 "改进" (遵循 P3)
5. **主动提醒** -- 对用户代码中的潜在问题主动提醒 (而非沉默)
6. **回复风格** -- 根据 PersonaVector 的 `prefers_conciseness` 调整详细程度
7. **禁止表达** -- 不使用 `forbidden_phrases` 中的表达
8. **验证结果** -- 多步任务每步执行后验证结果 (遵循 P4)

### 2.4 安全底线

1. **不执行**用户未确认的危险操作
2. **不生成**包含已知安全漏洞的代码
3. **主动警告**高危 API (如: 递归删除目录、执行外部进程、eval/exec 等)
4. **不存储**敏感信息 (密码、API Key、身份证号) 到记忆系统
5. 连续 3 次忽略安全警告 → 强制阻止

### 2.5 行为规则配置

```toml
[behavior_rules]
enforce_safe_code = true      # 强制安全代码检查
enforce_lint = true           # 强制 lint 规则 (自动适配项目语言: clippy/eslint/ruff/...)
enforce_format = true         # 强制代码格式化 (自动适配: rustfmt/prettier/black/gofmt/...)
enforce_tests = true          # 建议编写测试
enforce_all = true            # 全局开关
```

```
/config set behavior_rules.enforce_lint=false     # 关闭单条规则
/config validate --report=true                    # 验证规则一致性
```

---

## 3. 系统架构

### 3.1 架构总览

```
┌────────────────────────────────────────────────────────────┐
│                      用户终端                               │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  REPL 引擎                                           │  │
│  │  输入编辑器 | 流式输出渲染 | /slash 命令路由           │  │
│  └──────────────┬───────────────────────────┬───────────┘  │
│                 │                           │              │
│                 ▼                           ▼              │
│  ┌─────────────────────┐    ┌────────────────────────┐    │
│  │  上下文窗口构建器     │    │  Slash 命令处理器       │    │
│  │                     │    │  (不经过 LLM)           │    │
│  │  System Prompt      │    │  /discuss → 议会调度器   │    │
│  │  + 记忆上下文        │    │  /verbose → 输出控制    │    │
│  │  + 前 N 轮对话       │    └────────────────────────┘    │
│  │  + 当前用户输入      │                                  │
│  │  + Tool 定义         │                                  │
│  └──────────┬──────────┘                                  │
│             │                                              │
│             ▼                                              │
│  ┌─────────────────────────────────────────────────────┐  │
│  │  LLM 调用层 (L1)                                     │  │
│  │  远程 API (OpenAI / Anthropic / DeepSeek)            │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐  │  │
│  │  │ 流式输出  │  │ Tool Call │  │ Token 成本追踪    │  │  │
│  │  └──────────┘  └────┬─────┘  └──────────────────┘  │  │
│  ├──────────────────────┼──────────────────────────────┤  │
│  │  多 AI 议会 (Council)│      用户通过 /discuss 触发   │  │
│  │  ┌──────────┐ ┌──────┴─────┐ ┌──────────────────┐  │  │
│  │  │ 独立提案  │ │ 交叉评审    │ │ 仲裁综合          │  │  │
│  │  │ Phase 1  │ │ Phase 2    │ │ Phase 4          │  │  │
│  │  └──────────┘ └────────────┘ └──────────────────┘  │  │
│  └──────────────────────┼──────────────────────────────┘  │
│                         │                                  │
│                         ▼                                  │
│  ┌─────────────────────────────────────────────────────┐  │
│  │  Tool 执行引擎                                       │  │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐ │  │
│  │  │ 文件读写  │ │ Shell    │ │ 代码搜索  │ │ 构建   │ │  │
│  │  └──────────┘ └──────────┘ └──────────┘ └────────┘ │  │
│  │  安全拦截层 (危险操作确认 / 沙箱 / 重命名检测)         │  │
│  └─────────────────────────────────────────────────────┘  │
│                                                            │
│  ┌──────────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │  记忆系统 (L2)    │  │  人格系统(L3) │  │  DGM 演化   │  │
│  │  项目记忆         │  │  PersonaVec  │  │  MetaCtrl   │  │
│  │  + 长期记忆       │  │  + Self-Pmt  │  │  + EvLog    │  │
│  │  + Janitor       │  │  + 多语言     │  │            │  │
│  │  + OxyGent 钩子   │  │              │  │            │  │
│  └──────────────────┘  └──────────────┘  └────────────┘  │
│                                                            │
│  ┌─────────────────────────────────────────────────────┐  │
│  │  持久化层                                            │  │
│  │  SQLite (WAL) | 会话历史 (JSONL) | 配置 (TOML)       │  │
│  └─────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

### 3.2 单轮对话数据流

```
用户输入 (自然语言 或 /discuss 命令)
  │
  ▼
[1] 路由判断
    ├─ /discuss 命令 → 进入议会模式 [A]
    └─ 普通对话     → 进入标准模式 [B]

═══ [B] 标准模式 (单模型) ═══════════════════════════

[2] 记忆检索 ─── 项目记忆 + 长期记忆，按语言和项目过滤
  │
  ▼
[3] 上下文组装
    ├─ System Prompt (人格注入 + 行为规则 + Tool 定义)
    ├─ 记忆上下文 (<relevant_memories>)
    ├─ 前 N 轮对话历史
    └─ 当前用户输入
  │
  ▼
[4] 远程 LLM 调用 (流式)
  │
  ├─ LLM 返回文本 ────────→ 流式输出到终端
  │
  └─ LLM 请求 Tool Call ──→ [5] 安全检查
                              ├─ 通过 → 执行工具 → 结果回传 LLM → 回到 [4]
                              └─ 拒绝 → 告知 LLM "操作被拒绝" → 回到 [4]
  │
  ▼
[6] 后处理 → 跳到 [C]

═══ [A] 议会模式 (多模型辩论) ═══════════════════════

[2'] 记忆检索 (同 [2])
  │
  ▼
[3'] Council Orchestrator 调度
  │
  ├─ Phase 1: 各模型独立提案 (并行调用 N 个 LLM)
  │
  ├─ Phase 2: 交叉评审 (每个模型评审其他提案)
  │
  ├─ Phase 3: 反驳与修正 (可选，根据 rounds 配置)
  │
  └─ Phase 4: 仲裁模型综合最终方案
  │
  ▼
[4'] 输出控制
    ├─ 默认: 仲裁结论 + 关键分歧摘要
    └─ --verbose: 全部讨论过程
  │
  ▼
[5'] 后处理 → 跳到 [C]

═══ [C] 公共后处理 ══════════════════════════════════

[7] 后处理
  ├─ 记忆更新 (新增/强化相关记忆节点; 议会结论享有低衰减)
  ├─ 人格微调 (根据交互模式调整 PersonaVector)
  ├─ Token 计费 (议会模式分模型统计)
  └─ 会话持久化
```

---

## 4. 核心数据结构

### 4.1 Message (消息)

```rust
enum Message {
    User { content: String, timestamp: u64 },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
        token_usage: TokenUsage,
        timestamp: u64,
    },
    System { content: String },
    ToolCall { id: String, tool_name: String, arguments: Value },
    ToolResult { tool_call_id: String, output: String, is_error: bool },
}

struct TokenUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    estimated_cost_usd: f32,
}
```

### 4.2 Session (会话)

```rust
struct Session {
    id: String,
    project_id: String,
    messages: Vec<Message>,
    created_at: u64,
    updated_at: u64,
    metadata: SessionMetadata,
}

struct SessionMetadata {
    total_tokens_used: u64,
    total_cost_usd: f32,
    total_turns: u32,
    model_used: String,
    working_directory: String,
}
```

### 4.3 MemoryNode (记忆节点)

> **设计决策 D1**: 合并三份文档的不同定义。项目字段用 `Option` 包装。

```rust
struct MemoryNode {
    // ---- 基础字段 ----
    id: String,
    content: Value,
    created_at: u64,
    last_accessed: u64,
    depth: u32,                         // 强化深度
    decay_score: f32,                   // 衰减分数 [0.0, 1.0]
    tags: Vec<String>,
    aliases: Vec<String>,

    // ---- 类型与来源 ----
    node_type: MemoryNodeType,
    source: MemorySource,

    // ---- 多语言支持 ----
    language: String,                   // "rust", "python"
    traces: [f32; 5],                   // 多时间尺度记忆痕迹 (ACT-R MCM)
    language_weight: f32,               // 语言权重系数

    // ---- 项目记忆字段 ----
    project_id: Option<String>,         // None = 长期记忆
    is_project_critical: bool,
    project_retention_period: Option<u32>,
    access_permissions: Option<MemoryPermissions>,

    // ---- 清理辅助 ----
    last_reinforce_attempt: Option<u64>,
}

enum MemoryNodeType { Normal, MetaSkill, AntiPattern, Project }
enum MemorySource { User, System, RemoteModel, ProjectLock }
struct MemoryPermissions {
    owner: String,
    shared_teams: Vec<(String, Permission)>,
}
enum Permission { Read, Write, Admin }
```

### 4.4 PersonaVector (人格向量)

```rust
struct PersonaVector {
    // ---- 行为偏好 (可演化, 变化 <= max_trait_change) ----
    prefers_conciseness: f32,           // [0.0, 1.0]
    favors_safety_over_speed: f32,
    proactive_debugging: f32,
    avoids_boilerplate: f32,
    expertise_level: f32,

    // ---- 安全字段 (不可自动演化) ----
    refuses_unsafe_code: bool,
    enforces_clippy: bool,
    enforces_format: bool,

    // ---- 多语言人格 ----
    language: String,
    attachment_style: String,
    moral_priorities: Vec<String>,      // "安全性", "性能"
    forbidden_phrases: Vec<String>,     // "大概可能"

    // ---- 状态 ----
    frozen: bool,
}
```

---

## 5. REPL 交互引擎

### 5.1 启动流程

```
$ ox
  │
  ├─ [1] 加载配置 (~/.config/ox/config.toml)
  ├─ [2] 检测运行时环境 (OS/Shell/cwd/项目根 → RuntimeEnvironment, Section 5.7)
  ├─ [3] 初始化记忆系统 (连接 SQLite, WAL checkpoint, 异常恢复检查, Section 11.2b)
  ├─ [4] 检测项目 (Cargo.toml/package.json/pyproject.toml/go.mod/... → 识别项目语言)
  ├─ [5] 恢复会话 (自动恢复上次会话 + 用最近对话检索相关记忆, Section 11.2b)
  ├─ [6] Janitor 清理 (20% 概率)
  └─ [7] 初始化 Split-View 终端 (ratatui, Section 5.9) → 进入 REPL

  ╭─ Ox v2.1 ──────────────────────────────╮
  │ 项目: my-app (Python)                  │
  │ 系统: Windows (pwsh) | 目录: ~/my-app  │
  │ 会话已恢复 (上次: 2 小时前, 12 轮对话)  │
  │ 模型: gpt-4o | Token 余额: $4.23       │
  ╰────────────────────────────────────────╯
  ox>
```

### 5.2 REPL 主循环

```rust
async fn repl_loop(session: &mut Session, ctx: &mut AppContext) {
    loop {
        let input = editor.read_line("ox> ").await;
        match input {
            Input::Exit => break,
            Input::SlashCommand(cmd, args) => {
                handle_slash_command(cmd, args, ctx).await; // 不经过 LLM
            }
            Input::Text(text) => {
                session.messages.push(Message::User { content: text.clone(), timestamp: now() });
                run_agent_turn(session, ctx, &text).await; // 完整 Agent 循环
            }
        }
    }
}
```

### 5.3 Agent Turn

```rust
async fn run_agent_turn(session: &mut Session, ctx: &mut AppContext, input: &str) {
    let memory_ctx = ctx.memory.retrieve(input, &ctx.project_id).await;
    let mut messages = ctx.context_builder.build(&ctx.persona, &memory_ctx, &session.messages, input);
    let mut total_usage = TokenUsage::default();

    loop {
        let response = ctx.llm.stream_chat(&messages).await;
        total_usage += &response.usage;

        match response.content {
            LlmResponse::Text(text) => {
                terminal.stream_print(&text).await;
                session.messages.push(Message::assistant(text));
                break;
            }
            LlmResponse::ToolCalls(calls) => {
                // 将 assistant 的 tool_call 消息加入上下文，保持 call→result 对应关系
                messages.push(Message::assistant_tool_calls(&calls));

                for call in &calls {
                    if ctx.safety.requires_confirmation(&call)
                        && !terminal.confirm(&format!("允许 {}?", call.tool_name)).await {
                        messages.push(Message::tool_result(call.id, "用户拒绝", true));
                        continue;
                    }
                    let result = ctx.tools.execute(&call).await;
                    messages.push(Message::tool_result(call.id, &result.output, result.is_error));
                }
                // tool results 已在 messages 中，下次循环 stream_chat 将看到完整上下文
            }
        }
    }
    // 后处理：将 tool 交互同步到 session 持久化记录
    session.messages.extend(messages.tool_exchange_messages());
    ctx.memory.update_from_turn(session).await;
    ctx.persona.adjust_from_turn(session).await;
    ctx.cost_tracker.record(&total_usage).await;
    session.save().await;
}
```

**Agent Turn 设计要点：**
- `messages` 为可变引用，tool results 直接追加，下次循环 `stream_chat` 可见完整上下文
- `total_usage` 累计所有轮次的 Token 消耗（多轮 tool-use 场景下可能调用 LLM 多次）
- assistant 的 `tool_calls` 消息**必须**在 tool results 之前加入 `messages`，否则 LLM 无法理解 tool_result 的来源
- 循环结束后，将 tool 交互消息同步到 `session.messages` 以便持久化和记忆提取

### 5.4 输入编辑器

基于 `rustyline` 或 `reedline`:
- 多行输入 (`\` 换行 / 未闭合括号自动续行)
- 历史记录 (上/下箭头, Ctrl+R 搜索)
- 自动补全 (/slash 命令, 文件路径)
- 快捷键 (Ctrl+C 取消, Ctrl+D 退出)

### 5.5 流式输出渲染

```rust
struct StreamRenderer {
    markdown_renderer: TerminalMarkdown,
    in_code_block: bool,
    code_language: Option<String>,
}
// 代码块内使用 syntect 语法高亮
```

### 5.6 执行中断机制

Ox 的 Agent Turn 可能涉及长时 LLM 流式输出、耗时 tool 执行、多轮 tool-use 循环、以及 Council 会议。用户必须能在任意阶段中断执行。

**中断层级：**

| 信号 | 触发 | 行为 |
|------|------|------|
| 单次 Ctrl+C | 用户按一次 | **优雅中断** — 停止当前操作，保留已完成的部分结果 |
| 双次 Ctrl+C (1s内) | 用户快速按两次 | **强制中止** — 立即终止所有异步任务，丢弃未完成结果 |

**各阶段中断行为：**

```rust
enum InterruptiblePhase {
    LlmStreaming,      // 停止接收 stream tokens，已接收内容作为 partial response 保留
    ToolExecution,     // 向 tool 子进程发送 SIGTERM/TerminateProcess，记录 "用户中断" 作为 tool_result
    ToolUseLoop,       // 退出 agent turn loop，已完成的 tool results 保留在 session 中
    CouncilDebate,     // 终止议会会议，已完成的 phase 结论保留，返回 partial arbitration
}

struct InterruptController {
    cancel_token: CancellationToken,      // tokio_util::sync::CancellationToken
    last_ctrl_c: Option<Instant>,         // 用于检测双击
    force_threshold: Duration,            // 默认 1s
}

impl InterruptController {
    fn on_ctrl_c(&mut self) {
        if self.last_ctrl_c.map_or(false, |t| t.elapsed() < self.force_threshold) {
            self.cancel_token.cancel();   // 强制中止：取消所有关联的 async 任务
            terminal.print_warning("⚠ 强制中止");
        } else {
            self.last_ctrl_c = Some(Instant::now());
            self.cancel_token.cancel();   // 优雅中断：当前操作停止
            terminal.print_info("中断信号已发送，等待当前操作结束...");
        }
    }
}
```

**与 Agent Turn 集成：**
- `stream_chat` 接受 `CancellationToken`，流式传输随时可中断
- `tools.execute` 将 `CancellationToken` 传递给子进程管理器，中断时终止子进程
- Agent Turn loop 在每次迭代开始时检查 `cancel_token.is_cancelled()`
- 中断后，已产生的 assistant 消息和 tool results 正常写入 `session.messages`，但附加 `interrupted: true` 标记

**用户体验：**
- 中断后 Ox 回到 REPL 提示符，用户可继续输入新指令或用 `/retry` 重试被中断的操作
- 中断的 turn 在 session.jsonl 中保留，标记 `"interrupted": true`，不影响后续上下文构建
- 如果中断发生在 tool 写文件过程中，Ox 检查并报告文件一致性状态

### 5.7 运行时环境感知

Ox 启动时自动检测运行时环境，结果注入 System Prompt 和 tool 执行上下文，无需用户配置。

```rust
struct RuntimeEnvironment {
    os: Os,
    arch: String,              // std::env::consts::ARCH → "x86_64" / "aarch64"
    shell: ShellInfo,
    home_dir: PathBuf,
    working_dir: PathBuf,      // 启动时的 cwd
    project_root: Option<PathBuf>,  // 向上搜索 .git / Cargo.toml / package.json 等
    project_id: String,        // 基于 project_root 的 blake3 hash，无 project_root 则用 cwd hash
}

enum Os { Windows, Linux, MacOS, Other(String) }

struct ShellInfo {
    path: PathBuf,     // Windows: pwsh.exe 优先 → cmd.exe 回退; Unix: $SHELL → /bin/sh 回退
    name: String,      // "pwsh" / "cmd" / "bash" / "zsh" / "fish"
    exec_prefix: Vec<String>,  // pwsh: ["-Command"], cmd: ["/C"], sh: ["-c"]
}
```

**检测流程 (Section 5.1 启动流程 Step [2] 之前):**

```rust
fn detect_runtime() -> RuntimeEnvironment {
    let os = match std::env::consts::OS {
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        "macos" => Os::MacOS,
        other => Os::Other(other.to_string()),
    };
    
    let shell = detect_shell(&os);
    let working_dir = std::env::current_dir().unwrap();
    let project_root = find_project_root(&working_dir); // 向上查找 .git, Cargo.toml, package.json, go.mod, pyproject.toml
    let project_id = compute_project_id(&project_root, &working_dir);
    
    RuntimeEnvironment { os, arch: ARCH.into(), shell, home_dir: dirs::home_dir().unwrap(), working_dir, project_root, project_id }
}

fn detect_shell(os: &Os) -> ShellInfo {
    match os {
        Os::Windows => {
            // 优先 PowerShell Core (pwsh)，回退到 Windows PowerShell，最后 cmd
            if which("pwsh").is_ok() {
                ShellInfo { path: "pwsh.exe".into(), name: "pwsh".into(), exec_prefix: vec!["-Command".into()] }
            } else if which("powershell").is_ok() {
                ShellInfo { path: "powershell.exe".into(), name: "powershell".into(), exec_prefix: vec!["-Command".into()] }
            } else {
                ShellInfo { path: "cmd.exe".into(), name: "cmd".into(), exec_prefix: vec!["/C".into()] }
            }
        }
        _ => {
            let shell_path = std::env::var("SHELL").unwrap_or("/bin/sh".into());
            let name = Path::new(&shell_path).file_name().unwrap().to_string_lossy().into();
            ShellInfo { path: shell_path.into(), name, exec_prefix: vec!["-c".into()] }
        }
    }
}
```

**System Prompt 注入 (Section 5.3 context_builder.build 时):**

```
## 环境
- OS: {os} ({arch})
- Shell: {shell_name} ({shell_path})
- 工作目录: {working_dir}
- 项目根: {project_root}
- 路径分隔符: {path_separator}
```

LLM 通过这段上下文自动选择正确的命令 (`ls` vs `dir`、`/` vs `\`、`rm` vs `del`)，`shell_exec` 使用 `ShellInfo.exec_prefix` 构造子进程命令。

### 5.8 目录切换与上下文热切换

用户可通过 `/cd` 命令在 session 中切换工作目录。切换时 Ox 自动检测项目边界，决定是否需要切换项目上下文。

**项目边界检测:**

```rust
enum DirectoryChangeResult {
    /// 同一项目内移动 (同 git repo)，只更新 cwd
    SameProject { new_cwd: PathBuf },
    /// 跨项目切换 (不同 git repo / 不同项目根)
    DifferentProject {
        old_project_id: String,
        new_project_id: String,
        new_project_root: PathBuf,
    },
    /// 切换到无项目标识的目录
    NoProject { new_cwd: PathBuf },
}

fn detect_project_boundary(runtime: &RuntimeEnvironment, new_cwd: &Path) -> DirectoryChangeResult {
    let new_root = find_project_root(new_cwd);
    match (&runtime.project_root, &new_root) {
        (Some(old), Some(new)) if old == new => {
            SameProject { new_cwd: new_cwd.to_path_buf() }
        }
        (_, Some(new_root)) => {
            DifferentProject {
                old_project_id: runtime.project_id.clone(),
                new_project_id: compute_project_id(&Some(new_root.clone()), new_cwd),
                new_project_root: new_root.clone(),
            }
        }
        (_, None) => NoProject { new_cwd: new_cwd.to_path_buf() },
    }
}
```

**切换策略:**

| 情况 | cwd | project_id | 记忆检索 | TaskPlan | project_context.md | Session |
|------|-----|------------|----------|----------|--------------------|---------|
| SameProject | 更新 | 不变 | 不变 | 不变 | 不变 | 继续 |
| DifferentProject | 更新 | 切换 | 按新 project_id 过滤 | 保存旧 → 加载新 | 保存旧 → 加载新 | 继续 (记录 ProjectSwitched 事件) |
| NoProject | 更新 | 清空 | 仅全局记忆 | 保存旧 → 无新 plan | 无 | 继续 |

**DifferentProject 切换流程:**

```rust
async fn handle_project_switch(
    ctx: &mut AppContext,
    session: &mut Session,
    old_id: &str,
    new_id: &str,
    new_root: &Path,
) {
    // 1. 保存当前项目状态
    ctx.task_plan.save_to(&old_project_ox_dir(old_id)).await;
    ctx.project_context.flush(&old_project_ox_dir(old_id)).await;
    
    // 2. 切换 RuntimeEnvironment
    ctx.runtime.project_id = new_id.to_string();
    ctx.runtime.project_root = Some(new_root.to_path_buf());
    ctx.runtime.working_dir = new_root.to_path_buf();
    
    // 3. 加载新项目状态
    let new_ox_dir = new_root.join(".ox");
    ctx.task_plan = TaskPlan::load_or_default(&new_ox_dir).await;
    ctx.project_context = ProjectContext::load_or_default(&new_ox_dir).await;
    
    // 4. 重建 System Prompt (新的环境信息 + 新的项目上下文 + 新的记忆检索)
    ctx.context_builder.invalidate_cache();
    
    // 5. 记录切换事件到 session
    session.messages.push(Message::System {
        content: format!("[项目切换] {} → {}", old_id, new_id),
        metadata: json!({ "event": "ProjectSwitched", "old_project": old_id, "new_project": new_id }),
    });
    
    // 6. 运行 project_detect 更新项目语言/框架信息
    let project_info = ctx.tools.execute_internal("project_detect", &new_root).await;
    terminal.print_info(&format!(
        "项目切换: {} ({}) | 任务计划: {} 项 | 上下文: {}",
        new_root.display(), project_info.language,
        ctx.task_plan.items.len(),
        if ctx.project_context.is_loaded() { "已加载" } else { "空" }
    ));
}
```

**设计考量:**
- Session 不强制中断 — 用户经常需要 "参照 A 项目来改 B 项目"，对话上下文应连续
- `ProjectSwitched` 事件写入 session.messages，LLM 能感知到项目已切换
- `/cd` 支持相对路径和绝对路径，`~` 展开为 home_dir

### 5.9 用户介入机制 (Split-View Terminal)

> **设计决策**: Agent 工作期间用户必须能实时注入想法，而非只能中断-重启。Ox 采用 Split-View 终端布局 (基于 `ratatui` + `crossterm`)，输入区与输出区物理分离，实现真正的并行交互。

**终端布局:**

```
┌─────────────────────────────────────────────────────────────┐
│ ╭─ Agent Output ───────────────────────────────────────────╮│
│ │ [tool] file_read src/main.rs                             ││
│ │ [llm]  分析了 main.rs 的结构，发现以下几个问题...         ││
│ │ [tool] file_patch src/main.rs (3 edits)                  ││
│ │ [llm]  接下来修改 config.rs 中的配置解析...               ││
│ │ █ (streaming...)                                         ││
│ ╰──────────────────────────────────────────────────────────╯│
│ ╭─ Input ─────────────────── [Tab: 切换焦点] [Ctrl+C: 中断]╮│
│ │ ox> 对了，config.rs 里还要处理环境变量覆盖的情况_         ││
│ ╰──────────────────────────────────────────────────────────╯│
└─────────────────────────────────────────────────────────────┘
```

**核心架构:**

```rust
/// Split-View 终端管理器
struct TerminalUI {
    output_pane: OutputPane,      // 上方: Agent 输出 (滚动区)
    input_pane: InputPane,        // 下方: 用户输入 (固定 2-3 行)
    layout_ratio: (u16, u16),     // 默认 (85, 15) — 输出区占 85%
}

/// 用户输入缓冲 — Agent Turn 运行时接收用户消息
struct InputBuffer {
    tx: mpsc::UnboundedSender<UserInterjection>,
    rx: mpsc::UnboundedReceiver<UserInterjection>,
}

struct UserInterjection {
    content: String,
    timestamp: Instant,
    priority: InterjectionPriority,
}

enum InterjectionPriority {
    /// 常规补充 — 在下一个自然边界注入
    Normal,
    /// 紧急修正 (以 `!` 开头) — 立即中断当前 tool 执行并注入
    Urgent,
}
```

**消息注入流程 — 自然边界检查:**

Agent Turn 在每次 LLM 调用前检查 InputBuffer，将待处理的用户消息注入到 `messages`:

```rust
async fn run_agent_turn(session: &mut Session, ctx: &mut AppContext, input: &str) {
    let memory_ctx = ctx.memory.retrieve(input, &ctx.project_id).await;
    let mut messages = ctx.context_builder.build(&ctx.persona, &memory_ctx, &session.messages, input);
    let mut total_usage = TokenUsage::default();

    loop {
        // ★ 自然边界: 每次调 LLM 前检查用户介入
        let interjections = ctx.input_buffer.drain();
        if !interjections.is_empty() {
            let combined = interjections.iter()
                .map(|i| i.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            messages.push(Message::UserInterjection {
                content: combined.clone(),
                injected_at_turn: messages.len(),
            });
            terminal.output_pane.print_system(&format!("[已注入用户补充] {}", &combined));
            // 同步到 session 持久化
            session.messages.push(Message::UserInterjection {
                content: combined,
                injected_at_turn: session.messages.len(),
            });
        }

        let response = ctx.llm.stream_chat(&messages).await;
        total_usage += &response.usage;

        match response.content {
            LlmResponse::Text(text) => {
                terminal.output_pane.stream_print(&text).await;
                session.messages.push(Message::assistant(text));
                break;
            }
            LlmResponse::ToolCalls(calls) => {
                messages.push(Message::assistant_tool_calls(&calls));
                for call in &calls {
                    // ★ tool 执行前也检查紧急介入
                    if let Some(urgent) = ctx.input_buffer.try_recv_urgent() {
                        messages.push(Message::UserInterjection {
                            content: urgent.content,
                            injected_at_turn: messages.len(),
                        });
                        // 不执行当前 tool，让 LLM 重新评估
                        messages.push(Message::tool_result(
                            call.id, "[跳过] 用户紧急介入，请根据用户最新指示重新规划", true
                        ));
                        continue;
                    }

                    if ctx.safety.requires_confirmation(&call)
                        && !terminal.confirm(&format!("允许 {}?", call.tool_name)).await {
                        messages.push(Message::tool_result(call.id, "用户拒绝", true));
                        continue;
                    }
                    let result = ctx.tools.execute(&call).await;
                    messages.push(Message::tool_result(call.id, &result.output, result.is_error));
                }
            }
        }
    }
    session.messages.extend(messages.tool_exchange_messages());
}
```

**自然边界定义:**

| 边界点 | 时机 | 处理 |
|--------|------|------|
| LLM 调用前 | `stream_chat()` 之前 | drain 所有 Normal + Urgent 消息，合并为一条 UserInterjection 注入 |
| Tool 执行前 | 每个 `tools.execute()` 之前 | 仅检查 Urgent 消息，如有则跳过当前 tool 调用，让 LLM 重新规划 |
| Tool 批次间 | 多个 tool_calls 之间 | Urgent 检查同上 |

**紧急介入 (`!` 前缀):**

```
ox> !停，不要删 migration 文件，那个还在用
```

用户以 `!` 开头的输入标记为 `Urgent`，触发行为:
1. 如果当前在 LLM streaming → 取消 streaming，将 partial response 保留，立即注入用户消息到 messages
2. 如果当前在 tool 执行 → 等当前 tool 完成 (不强制终止，避免文件不一致)，然后注入
3. 注入后 LLM 在下次调用时能看到用户的紧急修正

**与 Ctrl+C 中断的区别:**

| | Ctrl+C (Section 5.6) | 用户介入 (Normal) | 紧急介入 (`!` Urgent) |
|------|------|------|------|
| 意图 | 停止一切 | 补充想法 | 紧急修正方向 |
| 当前操作 | 取消/终止 | 不影响，下个边界注入 | 当前 tool 完成后立即注入 |
| 后续 | 回到 REPL 提示符 | Agent 继续工作 (带上补充) | Agent 重新规划 (带上修正) |
| session 记录 | `interrupted: true` | `UserInterjection` | `UserInterjection { urgent: true }` |

**IO 并行架构:**

```rust
async fn run_terminal(ctx: AppContext) {
    let terminal = TerminalUI::new()?;  // ratatui + crossterm 初始化
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let input_buffer = InputBuffer::new(input_tx.clone(), input_rx);
    
    // 输入线程: 始终监听用户按键，独立于 Agent 输出
    let input_handle = tokio::spawn({
        let tx = input_tx.clone();
        let input_pane = terminal.input_pane.clone();
        async move {
            loop {
                match input_pane.read_line().await {
                    InputEvent::Line(text) => {
                        let priority = if text.starts_with('!') {
                            InterjectionPriority::Urgent
                        } else {
                            InterjectionPriority::Normal
                        };
                        let content = text.trim_start_matches('!').to_string();
                        tx.send(UserInterjection { content, timestamp: Instant::now(), priority }).ok();
                    }
                    InputEvent::CtrlC => { /* 走 InterruptController 逻辑 */ }
                    InputEvent::SlashCommand(cmd, args) => { /* 即使 Agent 工作中也可执行部分 slash 命令 */ }
                }
            }
        }
    });
    
    // REPL 主循环
    repl_loop_with_split_view(&mut session, &mut ctx, &input_buffer, &terminal).await;
}
```

**Agent 工作期间可执行的 Slash 命令:**

部分 slash 命令是只读的，不需要等 Agent Turn 结束:

| 命令 | Agent 工作中可用 | 说明 |
|------|:---:|------|
| `/plan` | Yes | 查看当前任务进度 (只读) |
| `/cost` | Yes | 查看当前 token 消耗 (只读) |
| `/help` | Yes | 帮助信息 (只读) |
| `/cd` | No | 切换目录影响 Agent 上下文，必须等 Agent Turn 结束 |
| `/trust` | Yes | 运行时生效，下个 tool 确认时即可跳过 |
| `/model` | No | 切换模型影响 Agent Turn，必须等结束 |

### 5.10 Graceful Shutdown (退出流程)

用户退出 Ox (`/exit`、Ctrl+D、终端关闭) 时，必须保证数据完整性。

**退出信号捕获:**

```rust
// 三种退出路径统一汇聚到 shutdown()
enum ExitTrigger {
    UserCommand,       // /exit 或 Ctrl+D
    SignalCaught,      // SIGTERM / SIGINT (终端关闭、系统关机)
    Panic,             // 程序 panic (通过 panic hook 捕获)
}
```

**Shutdown 流程:**

```rust
async fn shutdown(ctx: &mut AppContext, session: &mut Session, trigger: ExitTrigger) {
    // 1. 如果 Agent Turn 正在运行，先优雅中断
    if ctx.agent_running {
        ctx.interrupt_controller.cancel();
        // 等待最多 3 秒让当前操作完成
        tokio::time::timeout(Duration::from_secs(3), ctx.agent_handle.take()).await.ok();
    }
    
    // 2. flush 记忆写缓冲 — 确保 pending 的低优先级记忆写入磁盘
    ctx.memory.write_buffer.flush();
    
    // 3. 更新 project_context (如果本次会话有结构性变更)
    if ctx.session_has_structural_changes {
        update_project_context(session, ctx).await;
    }
    
    // 4. 保存 TaskPlan 当前状态
    ctx.task_plan.save_to(&ctx.ox_dir()).await;
    
    // 5. flush session.jsonl — 确保最后几轮对话已写入
    session.flush_to_disk().await;
    
    // 6. SQLite WAL checkpoint — 将 WAL 日志合并回主数据库
    ctx.memory.long_term_db.execute("PRAGMA wal_checkpoint(TRUNCATE)").await;
    if let Some(ref db) = ctx.memory.project_db {
        db.execute("PRAGMA wal_checkpoint(TRUNCATE)").await;
    }
    
    // 7. 写入 clean_shutdown 标记 (下次启动用于判断是否需要完整性检查)
    fs::write(ctx.ox_dir().join("clean_shutdown"), "").await.ok();
    
    // 8. 关闭数据库连接
    ctx.memory.close().await;
    
    // 9. 恢复终端 (ratatui cleanup)
    ctx.terminal.restore().ok();
    
    match trigger {
        ExitTrigger::UserCommand => println!("Ox 已退出。会话已保存。"),
        ExitTrigger::SignalCaught => println!("Ox 收到终止信号，数据已安全保存。"),
        ExitTrigger::Panic => eprintln!("Ox 异常退出，已尽力保存数据。"),
    }
}
```

**信号处理注册 (程序启动时):**

```rust
fn register_shutdown_hooks(ctx: Arc<Mutex<AppContext>>, session: Arc<Mutex<Session>>) {
    // Unix: SIGTERM, SIGHUP
    // Windows: CTRL_CLOSE_EVENT, CTRL_SHUTDOWN_EVENT
    ctrlc::set_handler(move || {
        // 注意: 这里与 Section 5.6 InterruptController 共享 Ctrl+C
        // 如果 Agent 未在运行 (REPL 提示符状态)，Ctrl+C 触发 shutdown
        // 如果 Agent 在运行，第一次 Ctrl+C 走中断逻辑，第三次才 shutdown
    });
    
    // panic hook: 尽力保存
    std::panic::set_hook(Box::new(move |info| {
        // 同步 flush (不能用 async，panic 时 runtime 可能已损坏)
        ctx.memory.write_buffer.flush_sync();
        session.flush_to_disk_sync();
        fs::write(ox_dir.join("dirty_shutdown"), format!("{}", info)).ok();
    }));
}
```

**异常退出恢复 (下次启动时):**

| 标记文件 | 含义 | 启动行为 |
|----------|------|----------|
| `clean_shutdown` 存在 | 上次正常退出 | 正常启动，删除标记 |
| `clean_shutdown` 不存在 | 上次异常退出 | WAL checkpoint + integrity_check |
| `dirty_shutdown` 存在 | 上次 panic | 显示警告 + 完整性检查 + 尝试恢复 |

**数据丢失风险评估:**

| 退出方式 | 记忆安全 | session 安全 | TaskPlan 安全 |
|----------|:---:|:---:|:---:|
| `/exit` / Ctrl+D | 完全安全 | 完全安全 | 完全安全 |
| 终端关闭 (SIGTERM) | 安全 (3s 缓冲) | 安全 | 安全 |
| 断电 / kill -9 | 高优先级安全，低优先级缓冲区可能丢失 ≤10 条 Fact | WAL 保护，最多丢 1 轮 | 最多丢最后一次更新 |
| panic | 尽力保存 (sync flush) | 尽力保存 | 可能丢失 |

> **设计原则**: 高价值记忆 (depth>=2) 立即提交，永远不在缓冲区中。断电最多丢失少量 Fact 级记忆 — 这些本身 depth=1，被 Janitor 清理的概率也最高，可接受。

---

## 6. 消息协议与会话管理

### 6.1 会话存储

```
.ox/
├── session.jsonl              # 当前活跃会话 (JSON Lines)
├── sessions/                  # 历史归档
│   ├── 2026-04-20_abc123.jsonl
│   └── 2026-04-21_def456.jsonl
└── project.toml               # 项目配置
```

**JSON Lines 格式:**
```jsonl
{"type":"user","content":"帮我实现 HTTP 服务器","timestamp":1714000000}
{"type":"assistant","content":"好的...","tool_calls":[...],"token_usage":{...},"timestamp":1714000005}
{"type":"tool_result","tool_call_id":"tc_1","output":"文件已写入","is_error":false}
```

### 6.2 会话控制

```
/new              归档当前会话，开始新会话
/sessions         列出历史会话
/resume <id>      恢复历史会话
/clear            清空当前上下文
```

### 6.3 任务计划持久化 (TaskPlan)

大型任务（重构 40 个文件、从零搭建项目）需要一个跨轮次、跨会话的计划追踪机制。Ox 通过 P4 生成的计划不应仅存在于对话历史中 — 当历史被压缩后计划会丢失。

**数据结构：**

```rust
/// 附着在 Session 上的任务计划
struct TaskPlan {
    id: Uuid,
    title: String,                    // "Django 函数视图 → CBV 重构"
    items: Vec<TaskItem>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

struct TaskItem {
    description: String,              // "重构 users/views.py"
    status: TaskStatus,
    notes: Option<String>,            // 执行后的备注 (如 "发现 3 个额外依赖")
}

enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Skipped { reason: String },
}
```

**与 Session 的关系：**

```
.ox/
├── session.jsonl                 # 对话历史 (可被压缩)
├── task_plan.json                # 当前任务计划 (不被压缩，独立持久化)
└── sessions/
```

**使用流程：**

```
ox> 帮我把 40 个视图函数重构为 CBV

Ox: (遵循 P4) 制定计划：
  1. [ ] users/views.py (12 个函数)
  2. [ ] products/views.py (8 个函数)
  ...
  已保存任务计划 "Django 函数视图 → CBV 重构"

ox> /plan                         # 查看当前计划
  Django 函数视图 → CBV 重构
  ✓ 1. users/views.py             (完成)
  → 2. products/views.py          (进行中)
    3. orders/views.py             (待处理)
  ...

ox> /plan skip 5 --reason "已废弃"  # 跳过某项
```

**关键设计：**
- TaskPlan 存储在 `task_plan.json` 中，**不受对话历史压缩影响**
- Ox 在每轮 Agent Turn 完成后自动更新 TaskItem 状态（完成一个文件 → 标记 Done）
- 新会话启动时，如果存在未完成的 TaskPlan，自动在 System Prompt 中注入摘要
- `/plan` 命令查看进度，`/plan clear` 清除已完成的计划

---

## 7. Tool 系统

### 7.1 Tool Trait

```rust
#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, args: Value) -> ToolOutput;
    fn safety_level(&self) -> SafetyLevel { SafetyLevel::Safe }
}

enum SafetyLevel { Safe, RequiresConfirmation, Dangerous }
struct ToolOutput { output: String, is_error: bool }
```

### 7.2 内置工具清单

| 工具名 | 说明 | 安全级别 |
|--------|------|---------|
| `file_read` | 读取文件 | Safe |
| `file_write` | 写入/创建文件 | RequiresConfirmation |
| `file_patch` | 局部修改文件 (搜索替换/行号编辑) | RequiresConfirmation |
| `file_list` | 列出目录 | Safe |
| `file_search` | Glob 搜索文件 | Safe |
| `code_search` | Grep 搜索代码 | Safe |
| `shell_exec` | 执行 shell 命令 (构建/测试/运行等)，支持流式输出 | RequiresConfirmation |
| `project_detect` | 自动检测项目类型和语言 | Safe |
| `git_status` | 查看 git 状态 | Safe |
| `git_diff` | 查看 git diff | Safe |
| `git_commit` | 创建 commit | RequiresConfirmation |
| `web_fetch` | 获取网页内容 | Safe |

### 7.3 Tool 注册与 LLM 映射

```rust
struct ToolRegistry { tools: HashMap<String, Box<dyn Tool>> }

impl ToolRegistry {
    fn to_llm_tools_schema(&self) -> Vec<Value> {
        self.tools.values().map(|t| json!({
            "type": "function",
            "function": { "name": t.name(), "description": t.description(), "parameters": t.parameters_schema() }
        })).collect()
    }
}
```

### 7.3a file_patch — 局部修改工具

`file_write` 是全文覆写，修改一个 500 行文件中的 10 行需要 LLM 重新生成全部 500 行，Token 浪费严重。`file_patch` 通过搜索替换实现精准局部编辑：

```rust
/// file_patch 参数
struct FilePatchArgs {
    path: String,
    /// 修改操作列表 (按顺序执行)
    edits: Vec<PatchEdit>,
}

enum PatchEdit {
    /// 搜索替换: 找到 old_string 并替换为 new_string
    SearchReplace {
        old_string: String,      // 要查找的原始内容 (必须在文件中唯一匹配)
        new_string: String,      // 替换后的内容
    },
    /// 在指定行号后插入
    InsertAfterLine {
        line: u32,
        content: String,
    },
    /// 删除行范围
    DeleteLines {
        start_line: u32,
        end_line: u32,
    },
}
```

**Token 节约估算:**
- 全文覆写 500 行文件改 10 行: ~500 行 output tokens
- file_patch 改 10 行: ~30 行 output tokens (old_string + new_string)
- **节约 ~94%** output tokens

**LLM Function Calling schema:**
```json
{
  "name": "file_patch",
  "description": "对文件进行局部修改。优先使用 search_replace 模式（需要 old_string 在文件中唯一）。仅当 file_write 是创建新文件或重写超过 50% 内容时才使用 file_write。",
  "parameters": {
    "path": { "type": "string" },
    "edits": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "type": { "enum": ["search_replace", "insert_after_line", "delete_lines"] },
          "old_string": { "type": "string" },
          "new_string": { "type": "string" },
          "line": { "type": "integer" },
          "start_line": { "type": "integer" },
          "end_line": { "type": "integer" }
        }
      }
    }
  }
}
```

**冲突检测:** 如果 `old_string` 在文件中匹配 0 次或多于 1 次，返回 `is_error: true`，提示 LLM 提供更多上下文以唯一定位。

### 7.3b shell_exec 流式输出

数据迁移、构建、测试等长时任务可能执行数分钟到数小时。默认的"等待完成后返回全部输出"模式会导致用户在等待期间无任何反馈。

**双模式执行：**

```rust
struct ShellExecArgs {
    command: String,
    working_dir: Option<String>,
    /// 流式模式: 实时将 stdout/stderr 输出到终端
    /// 默认 true — 除非 LLM 显式请求静默模式 (如只需要 exit code)
    stream: bool,
    /// 超时 (秒)，默认 300s，长任务可由 LLM 设置更长
    timeout: Option<u64>,
}
```

**流式模式行为：**

```
ox> 运行全量数据迁移

Ox: 即将执行: python scripts/migrate.py
    允许? [y/n] y

  ┌─ shell_exec (streaming) ──────────────────────┐
  │ [00:00] 连接数据库...                           │
  │ [00:02] 开始迁移 users 表                       │
  │ [00:15] 已迁移 10,000 / 500,000 条 (2%)        │
  │ [00:30] 已迁移 20,000 / 500,000 条 (4%)        │
  │ ...                                            │
  │ [15:32] 迁移完成。成功: 498,231 | 跳过: 1,769   │
  └────────────────────────────────────────────────┘

  执行完成 (exit code: 0, 耗时: 15m32s)
```

**LLM 收到的 tool_result：** 流式输出的内容最终会被截断后作为 tool_result 返回给 LLM（取最后 N 行，默认 50 行），LLM 基于此决定下一步操作。完整输出保存在 session 日志中，用户可通过 `/log last` 查看。

**与中断机制集成：** 流式模式下用户可随时 Ctrl+C 中断子进程（Section 5.6 InterruptController），已输出的内容保留。

### 7.4 Tool 安全拦截

- Safe → 自动执行
- RequiresConfirmation → 显示确认
- Dangerous → 默认拒绝
- `shell_exec` 额外检查 `high_risk_apis` + 重命名检测
- 连续 3 次忽略 → 强制阻止

### 7.5 批量确认模式

批量操作场景（如重构 40 个文件）中，每次 `file_write` 都要确认会导致**确认疲劳**。提供临时信任机制：

```
/trust file_write           # 当前会话内 file_write 跳过确认
/trust file_write file_patch # 同时信任多个工具
/trust --all                # 信任所有 RequiresConfirmation 工具 (Dangerous 除外)
/untrust                    # 撤销所有临时信任，恢复确认
```

**安全约束：**

```rust
struct TrustManager {
    trusted_tools: HashSet<String>,   // 当前会话临时信任的工具
    session_scoped: bool,             // 始终为 true — 信任仅在当前会话有效
}

impl TrustManager {
    fn can_skip_confirmation(&self, tool_name: &str, safety: SafetyLevel) -> bool {
        match safety {
            SafetyLevel::Safe => true,                              // 本来就不需要确认
            SafetyLevel::RequiresConfirmation =>
                self.trusted_tools.contains(tool_name)
                || self.trusted_tools.contains("__all__"),
            SafetyLevel::Dangerous => false,                        // Dangerous 永不跳过
        }
    }
}
```

**设计原则：**
- `/trust` 仅限当前会话，退出 REPL 自动失效，不持久化到配置
- `Dangerous` 级别的工具（如 `rm -rf`）永远不跳过确认，即使 `/trust --all`
- 信任状态在 REPL 提示符中显示提示：`ox [trusted: file_write]>`
- `/trust` 启用时，每次操作仍在终端静默记录日志（文件路径 + 操作类型），便于事后审计

---

## 8. 上下文窗口与 Token 管理

### 8.1 上下文构成

Token 预算**不是固定值**，而是根据当前模型的上下文窗口动态计算:

```
总 Token 预算 = 当前模型的 context_window_size (如 GPT-4o: 128K, DeepSeek: 64K, Claude: 200K)

预算分配比例 (按总窗口百分比):
  ├─ System Prompt (固定)             ~2%   (人格 + P1-P4准则 + 行为规则 + Tool 定义)
  ├─ 记忆上下文 (动态)                ~2%   (检索结果)
  ├─ 前 N 轮对话 (动态)               ~36%  (最近 N 轮完整对话)
  ├─ 当前用户输入                     ~1%
  └─ 预留给 LLM 回复                  ~59%

示例:
  GPT-4o (128K):   System 2,560 | 记忆 2,560 | 历史 46,080 | 回复 75,520
  DeepSeek (64K):  System 1,280 | 记忆 1,280 | 历史 23,040 | 回复 37,760
  Claude (200K):   System 4,000 | 记忆 4,000 | 历史 72,000 | 回复 118,000
```

### 8.2 Token 计数

不同 Provider 的 tokenizer 不同，Ox 需要适配:

```rust
trait Tokenizer: Send + Sync {
    fn count_tokens(&self, text: &str) -> u32;
    fn truncate_to_tokens(&self, text: &str, max_tokens: u32) -> String;
}

/// 根据 Provider 选择 tokenizer
fn get_tokenizer(provider: &str) -> Box<dyn Tokenizer> {
    match provider {
        "openai"    => Box::new(TiktokenTokenizer::new("cl100k_base")),
        "anthropic" => Box::new(AnthropicTokenizer::new()),
        "deepseek"  => Box::new(TiktokenTokenizer::new("cl100k_base")), // 兼容
        _           => Box::new(WhitespaceEstimator::new()),  // 按 4 chars/token 估算
    }
}
```

### 8.3 上下文构建器

```rust
struct ContextBuilder {
    /// 预算按比例从模型窗口计算，不硬编码
    system_prompt_ratio: f32,    // 0.02
    memory_ratio: f32,           // 0.02
    history_ratio: f32,          // 0.36
    reply_reserve_ratio: f32,    // 0.59
}

impl ContextBuilder {
    /// 根据当前模型窗口动态计算各项预算
    fn budgets(&self, model: &dyn LlmProvider) -> Budgets {
        let total = model.context_window_size();
        Budgets {
            system_prompt: (total as f32 * self.system_prompt_ratio) as u32,
            memory:        (total as f32 * self.memory_ratio) as u32,
            history:       (total as f32 * self.history_ratio) as u32,
            reply_reserve: (total as f32 * self.reply_reserve_ratio) as u32,
        }
    }

    fn build(&self, model: &dyn LlmProvider, tokenizer: &dyn Tokenizer,
             persona: &PersonaVector, memory_ctx: &str,
             history: &[Message], current_input: &str) -> Vec<Message> {
        let budgets = self.budgets(model);
        let mut msgs = vec![];

        // 1. System Prompt (含 P1-P4 底层准则)
        msgs.push(Message::System { content: self.build_system_prompt(persona) });

        // 2. 记忆上下文
        if !memory_ctx.is_empty() {
            msgs.push(Message::System {
                content: format!("<relevant_memories>\n{}\n</relevant_memories>",
                    tokenizer.truncate_to_tokens(memory_ctx, budgets.memory)),
            });
        }

        // 3. 历史对话 (从最近开始填充)
        let mut budget = 0;
        let mut hist = vec![];
        for msg in history.iter().rev() {
            let tokens = tokenizer.count_tokens(&msg.content());
            if budget + tokens > budgets.history { break; }
            budget += tokens;
            hist.push(msg.clone());
        }
        hist.reverse();
        msgs.extend(hist);

        // 4. 当前输入
        msgs.push(Message::User { content: current_input.into(), timestamp: now() });
        msgs
    }
}
```

### 8.3 System Prompt 模板

```
你是 Ox，一个 AI 编程助手，运行在用户的终端中。

## 核心执行准则 (不可违反)
P1: 先思考再编码 — 不假设、不隐藏困惑、显式暴露权衡。不确定时先问。
P2: 简洁优先 — 最少代码解决问题。不添加未要求的功能、抽象、错误处理。
P3: 精准修改 — 只动必须动的。不改相邻代码。匹配已有风格。
P4: 目标驱动 — 定义成功标准，多步任务先说计划，每步验证结果。

## 人格
- 简洁偏好: {conciseness} (0=详细, 1=极简)
- 安全优先度: {safety}
- 专业水平: {expertise}
- 价值观: {priorities}
- 禁止表达: {forbidden_phrases}

## 行为规则
- 操作前先说明要做什么，再调用工具 (P1)
- 不确定时先读取文件了解情况 (P1)
- 不要猜测文件内容 (P1)
- 只改该改的，不做无关改进 (P3)
- 安全代码: {enforce_safe} | Lint: {enforce_lint}

## 当前项目
- 工作目录: {cwd}
- 项目类型: {project_type}
- 项目语言: {project_language}
```

### 8.5 Token 压缩策略

三个阶段**按剩余预算自动切换**:

```
                   历史 Token 占比
                   ┌──────────────────────────────────────┐
  阶段 1 (< 70%)   │ 完整保留最近 N 轮                      │  正常区
                   ├──────────────────────────────────────┤
  阶段 2 (70~90%)  │ 截断 + 摘要替代 + 依赖记忆补充          │  压缩区
                   ├──────────────────────────────────────┤
  阶段 3 (> 90%)   │ 大任务自动拆分子任务                    │  降级区
                   └──────────────────────────────────────┘
```

**切换条件 (基于 history_budget 占用率):**

| 阶段 | 触发条件 | 行为 |
|------|----------|------|
| 阶段 1: 正常 | 历史 Token < history_budget × 70% | 保留最近 N 轮完整对话 |
| 阶段 2: 压缩 | 历史 Token 达到 history_budget × 70% | 更早的对话: 移除 ToolCall/ToolResult 详情，用摘要替代。超出预算的彻底丢弃，依赖记忆系统在下次检索时补充 |
| 阶段 3: 降级 | 单次任务预估 > reply_reserve × 50% | 自动拆分为子任务，每个子任务独立执行，减少单次 Token 消耗 |

### 8.6 Effort Level 判定

Effort Level 由**本地启发式规则**判定（不消耗 Token）:

```rust
fn estimate_effort(input: &str, history_len: usize) -> EffortLevel {
    let input_tokens = tokenizer.count_tokens(input);
    let has_code_block = input.contains("```");
    let is_question = input.ends_with('?') || input.ends_with('？');

    match () {
        // 简单问答 / 解释
        _ if is_question && input_tokens < 50 && !has_code_block
            => EffortLevel::Low,

        // 代码补全 / 轻度修改
        _ if input_tokens < 200
            => EffortLevel::Medium,

        // 标准代码生成 / 重构
        _ if input_tokens < 500
            => EffortLevel::Standard,

        // 复杂任务: 长输入 / 含大段代码 / 多文件操作
        _ => EffortLevel::High,
    }
}
```

**规则特征 (不调用 LLM，零 Token 成本):**
- 基于输入长度、是否包含代码块、是否为提问句式
- 用户可通过 `/effort <level>` 手动覆盖
- DGM 可优化阈值参数 (input_tokens 的分界值)

### 8.7 Token 成本公式

```
Token_cost = Base_tokens * (1 + Effort_level * 系数)
```

| Effort Level | 系数 | 适用场景 |
|-------------|------|---------|
| low | 0.2 | 简单解释、格式化 |
| medium | 0.5 | 代码补全、轻度调试 |
| standard | 1.0 | 代码生成、重构 |
| high | 1.5 | 复杂架构设计、深度调试 |

### 8.8 成本追踪

```rust
struct CostTracker {
    // 分类追踪
    conversation_cost: f32,       // 普通对话消耗
    council_cost: f32,            // 议会辩论消耗
    background_cost: f32,         // 后台任务消耗 (记忆转化、DGM 评估等)
    // 汇总
    daily_cost: f32,              // = conversation + council + background
    monthly_cost: f32,
    // 限额
    daily_limit: f32,
    monthly_limit: f32,
    alert_threshold: f32,         // 0.8
}
```

**统一预算**: 所有 Token 消耗（对话、议会、记忆转化、DGM 评估）都归入同一预算池:

- 月度达 80% → 告警
- 日/月达 100% → 阻止远程调用 (包括议会和记忆转化)
- `/cost` 输出分类明细 (对话 / 议会 / 后台)

**后台任务预算上限**: 记忆转化和 DGM 评估等后台 LLM 调用，每日不超过总预算的 10%。超限则推迟到次日。

```
/cost                       查看成本
/cost limit --monthly=5.0   设置预算
/cost simulate "任务描述"    模拟成本
/stats                      详细统计
```

---

## 9. LLM 调用层

### 9.1 支持的模型

| 提供商 | 模型 | 特点 |
|--------|------|------|
| OpenAI | gpt-4o, gpt-4-turbo | Function Calling 支持好 |
| Anthropic | Claude 3.5 Sonnet, Claude Opus 4.6 | Tool Use, 长上下文 |
| DeepSeek | deepseek-coder | 代码专精，成本低 |

### 9.2 LLM Provider Trait

```rust
#[async_trait]
trait LlmProvider: Send + Sync {
    async fn stream_chat(&self, messages: Vec<Message>, tools: &[Value]) -> Result<LlmResponseStream>;
    fn model_name(&self) -> &str;
    fn context_window_size(&self) -> u32;
    fn cost_per_input_token(&self) -> f32;
    fn cost_per_output_token(&self) -> f32;
}

enum LlmStreamEvent {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgumentsDelta(String),
    ToolCallEnd,
    Done(TokenUsage),
    Error(String),
}
```

### 9.3 模型选择与任务路由

```
1. 使用 default 模型
2. API 错误 → fallback 到 backup 列表
3. /model <name> 临时切换
4. Token 预算超限 → 降级到便宜模型
5. 按任务复杂度路由:
   预估 < 500 tokens   → 轻量模型 + low effort
   预估 < 1000 tokens  → 标准模型 + medium effort
   预估 < 2000 tokens  → 默认模型 + standard effort
   预估 > 2000 tokens  → 高级模型 + high effort → 自动拆分子任务
```

### 9.4 Embedding / Reranker

使用 Bi/Cross Encoder (Sentence-Transformers):
- 记忆检索语义相似度
- 记忆去重 (> 0.85 视为重复)
- 跨项目知识关联

---

## 10. 多 AI 议会系统 (Council)

### 10.1 设计理念

```
单一模型有盲区，多模型辩论出真知。
Ox 不依赖某一个 LLM 的判断。
在关键问题上，它会召集一个「议会」—— 多个模型各抒己见、互相质疑，
最终由仲裁者综合各方观点，给出经过交叉验证的最优方案。
```

**产品定位 — 低频高价值:**

议会是一个**关键时刻功能**，不是日常功能。日常编码（写函数、修 bug、加测试）不需要多模型辩论。但在架构决策、技术选型、复杂 debug 等关键时刻，议会提供的多模型交叉验证能大幅降低决策风险。

预期使用频率：每个项目每周 1-3 次。产品宣传应定位为"关键时刻你会庆幸有它"的后盾能力，而非核心卖点。Ox 的核心护城河是**记忆系统**（用户的项目知识积累 → 迁移成本高），议会是锦上添花。

**与「可信赖」定位的关系**: 多 AI 议会是 Ox "可信赖" 特质的核心技术支撑。单模型可能产生幻觉或偏见，而多模型辩论通过交叉验证大幅降低错误率。

### 10.2 触发机制

多 AI 议会默认**不启用**，由用户主动控制：

| 触发方式 | 说明 |
|----------|------|
| `/discuss` | 对当前问题启动一次议会辩论 |
| `/discuss <prompt>` | 对指定问题启动议会辩论 |
| `/discuss --rounds 3` | 指定辩论轮次 (默认 2 轮) |

**不触发议会时**: 使用默认单模型调用 (Section 9 的标准流程)，保持低延迟和低成本。

### 10.3 辩论架构

```
用户输入: "/discuss 这个微服务应该用 gRPC 还是 REST？"
  │
  ▼
┌─────────────────────────────────────────────────────────────────┐
│  Council Orchestrator (议会调度器)                                │
│                                                                  │
│  [Phase 1] 独立提案 (Proposal)                                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  Model A     │  │  Model B     │  │  Model C     │             │
│  │  (GPT-4o)    │  │  (Claude)    │  │  (DeepSeek)  │             │
│  │              │  │              │  │              │             │
│  │  提案: gRPC  │  │  提案: REST  │  │  提案: 混合   │             │
│  │  理由: ...   │  │  理由: ...   │  │  理由: ...   │             │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         │                │                │                      │
│         └────────────────┼────────────────┘                      │
│                          ▼                                       │
│  [Phase 2] 交叉评审 (Cross-Review)                               │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  每个模型评审其他模型的提案:                                 │   │
│  │  Model A → 评审 B 和 C: "REST 在微服务间效率低..."          │   │
│  │  Model B → 评审 A 和 C: "gRPC 的浏览器兼容性差..."          │   │
│  │  Model C → 评审 A 和 B: "纯方案都有缺陷..."                │   │
│  └──────────────────────────────────────────────────────────┘   │
│                          │                                       │
│                          ▼                                       │
│  [Phase 3] 反驳与修正 (Rebuttal)  — 可选，按 rounds 配置重复     │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  模型针对评审意见进行反驳或修正自己的提案:                     │   │
│  │  Model A: "接受浏览器兼容性问题，建议加 gRPC-Web 网关"       │   │
│  │  Model B: "承认效率问题，但强调团队学习成本"                  │   │
│  │  Model C: "细化混合方案：内部 gRPC + 外部 REST gateway"      │   │
│  └──────────────────────────────────────────────────────────┘   │
│                          │                                       │
│                          ▼                                       │
│  [Phase 4] 仲裁综合 (Arbitration)                                │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Arbiter (仲裁模型，默认使用 default_model):                  │   │
│  │  综合所有提案、评审、反驳，输出:                               │   │
│  │  1. 最终推荐方案                                             │   │
│  │  2. 关键分歧点摘要                                           │   │
│  │  3. 各方案优劣对比                                           │   │
│  │  4. 置信度评分                                               │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
  │
  ▼
输出给用户 (默认: 仲裁结论 + 分歧摘要)
```

### 10.4 核心数据结构

```rust
/// 议会辩论会话
struct CouncilSession {
    id: Uuid,
    question: String,             // 用户问题
    participants: Vec<Participant>,
    rounds: u8,                   // 辩论轮次
    phases: Vec<DebatePhase>,
    arbitration: Option<Arbitration>,
    token_usage: CouncilTokenUsage,
    created_at: DateTime<Utc>,
}

/// 参与模型
struct Participant {
    role: ParticipantRole,
    provider: String,             // "openai" | "anthropic" | "deepseek"
    model: String,                // "gpt-4o" | "claude-sonnet-4-20250514" | ...
}

enum ParticipantRole {
    Proposer,     // 提案者
    Reviewer,     // 评审者 (同一模型在不同阶段可切换角色)
    Arbiter,      // 仲裁者
}

/// 辩论阶段
enum DebatePhase {
    Proposal(Vec<Proposal>),
    CrossReview(Vec<Review>),
    Rebuttal(Vec<Rebuttal>),
}

struct Proposal {
    participant_idx: usize,
    content: String,
    reasoning: String,
}

struct Review {
    reviewer_idx: usize,          // 谁在评审
    target_idx: usize,            // 评审谁的提案
    critique: String,             // 质疑/意见
    score: f32,                   // 0.0 ~ 1.0
}

struct Rebuttal {
    participant_idx: usize,
    original_proposal: String,    // 原始提案摘要
    response_to_critiques: String,
    revised_proposal: Option<String>,  // 修正后的提案 (如果有)
}

/// 仲裁结果
struct Arbitration {
    arbiter_idx: usize,
    final_recommendation: String, // 最终推荐方案
    primary_source_idx: usize,    // 主要采纳的提案来源 (用于模型能力学习 §10.9)
    reasoning: String,            // 仲裁推理过程 (用于追踪评审引用)
    key_disagreements: Vec<String>,  // 关键分歧点
    comparison_table: String,     // 各方案对比
    confidence: f32,              // 置信度 0.0 ~ 1.0
}

/// Token 用量追踪
struct CouncilTokenUsage {
    per_participant: Vec<(usize, TokenUsage)>,
    arbitration: TokenUsage,
    total: TokenUsage,
    estimated_cost: f64,
}
```

### 10.5 议会调度器 (Council Orchestrator)

```rust
#[async_trait]
trait CouncilOrchestrator {
    /// 启动议会辩论
    async fn convene(
        &self,
        question: &str,
        context: &ContextWindow,
        config: &CouncilConfig,
    ) -> Result<CouncilSession>;

    /// 执行单个阶段
    async fn run_phase(
        &self,
        session: &mut CouncilSession,
        phase: DebatePhaseType,
    ) -> Result<()>;

    /// 仲裁综合
    async fn arbitrate(
        &self,
        session: &mut CouncilSession,
    ) -> Result<Arbitration>;
}
```

**调度流程:**

```
1. 解析 /discuss 命令参数 (rounds, participants 等)
2. 从已配置的 LlmProvider 中选择参与模型 (至少 2 个不同提供商)
3. 构建 Phase 1 prompt → 并行调用所有参与模型 → 收集提案
4. 构建 Phase 2 prompt (附带其他模型的提案) → 并行交叉评审
5. 如 rounds > 1: 构建 Phase 3 prompt → 并行反驳/修正
6. 重复 Phase 2-3 直到达到指定轮次
7. 构建仲裁 prompt (包含全部讨论记录) → 仲裁模型综合
8. 输出格式化结果
```

### 10.6 输出可见性控制

| 模式 | 触发 | 显示内容 |
|------|------|----------|
| **默认** | `/discuss` | 仲裁结论 + 关键分歧点摘要 |
| **详细** | `/discuss --verbose` 或 `/verbose` (辩论进行中) | 全部阶段的完整讨论过程 |
| **回顾** | `/council last` | 查看上一次议会的完整记录 |

**默认输出格式:**

```
╔══════════════════════════════════════════════════╗
║  🏛  议会结论                                     ║
╠══════════════════════════════════════════════════╣
║                                                  ║
║  推荐方案: 内部 gRPC + 外部 REST Gateway          ║
║  置信度: 0.85                                     ║
║                                                  ║
║  关键分歧:                                        ║
║  • gRPC 浏览器兼容性 vs 性能优势                    ║
║  • 团队学习成本是否可接受                           ║
║                                                  ║
║  参与模型: GPT-4o / Claude / DeepSeek             ║
║  辩论轮次: 2 | Token 消耗: 12,340                  ║
╚══════════════════════════════════════════════════╝
```

**详细模式输出:**

```
──── Phase 1: 独立提案 ────
[GPT-4o] 提案: gRPC
  理由: 微服务间通信性能优先，Protobuf 编解码效率高...

[Claude] 提案: REST
  理由: 团队已有 REST 经验，OpenAPI 生态成熟...

[DeepSeek] 提案: 混合架构
  理由: 不必二选一，内外部需求不同...

──── Phase 2: 交叉评审 ────
[GPT-4o → Claude] gRPC 的浏览器兼容性确实是问题，但可用 gRPC-Web...
[Claude → GPT-4o] 性能差距在多数场景下并不显著...
...

──── Phase 3: 反驳 ────
...

──── 仲裁结论 ────
(同默认输出)
```

### 10.7 成本控制

议会辩论会消耗较多 Token。以下是控制策略:

| 策略 | 说明 |
|------|------|
| **用户主动触发** | 默认不启用，避免意外高消耗 |
| **参与者上限** | 最多 4 个模型参与 (默认 3 个) |
| **轮次上限** | 最多 3 轮辩论 (默认 2 轮) |
| **预算检查** | 启动前估算成本，超出当日预算则警告 |
| **提前收敛** | 如果 Phase 2 中所有评审分数 > 0.8，跳过 Phase 3 直接仲裁 |
| **Token 压缩** | 仲裁阶段对讨论历史做摘要压缩，而非传入完整原文 |

**成本估算公式:**

```
Council_cost ≈ (N_models × avg_proposal_tokens)          // Phase 1
             + (N_models × (N_models-1) × avg_review_tokens)  // Phase 2
             + (rounds - 1) × (N_models × avg_rebuttal_tokens) // Phase 3
             + arbiter_tokens                               // Phase 4

// 典型场景 (3 模型, 2 轮):
// ≈ 3×800 + 3×2×500 + 1×3×600 + 2000 ≈ 9,200 tokens
```

### 10.8 记忆整合

议会辩论的结论可被记忆系统保存:

```rust
// 辩论结束后，如果结论有价值:
MemoryNode {
    content: arbitration.final_recommendation,
    source: MemorySource::Council,        // 新增来源类型
    tags: ["architecture", "decision"],
    depth: 3,                              // 议会结论初始深度较高
    metadata: {
        "council_id": session.id,
        "confidence": arbitration.confidence,
        "participants": ["gpt-4o", "claude", "deepseek"],
        "key_disagreements": arbitration.key_disagreements,
    }
}
```

议会来源的记忆在衰减时享有较低的衰减速率 (乘以 0.7 系数)，因为它们经过了多模型验证，可信度更高。

### 10.9 模型能力学习

议会系统应能从历史辩论中学习各模型的优势领域，而非每次都平等对待所有参与者。

**学习目标：** 跟踪每个模型在不同问题类别上的表现，用于优化未来的议会组成和权重分配。

**能力评分数据结构：**

```rust
/// 模型在特定领域的表现记录
struct ModelCapabilityScore {
    provider: String,              // "openai" | "anthropic" | "deepseek"
    model: String,                 // "gpt-4o" | "claude-sonnet-4-20250514" | ...
    topic_scores: HashMap<TopicCategory, TopicScore>,
}

/// 话题分类 (基于用户问题的关键词 + LLM 分类，本地推断不消耗 Token)
enum TopicCategory {
    Architecture,       // 架构设计、系统设计
    Algorithm,          // 算法、数据结构
    Debugging,          // 调试、错误排查
    CodeReview,         // 代码审查、质量
    DevOps,             // 部署、CI/CD、基础设施
    Frontend,           // UI/UX、前端框架
    Database,           // 数据库设计、查询优化
    Security,           // 安全、认证、加密
    General,            // 无法分类的通用问题
}

struct TopicScore {
    // 基于 EMA 的能力评分，与 DGM 保持一致的计算方式
    proposal_adopted_rate: f32,    // 提案被仲裁采纳的比率 (EMA, α=0.3)
    review_quality: f32,           // 评审意见被引用的比率 (EMA, α=0.3)
    session_count: u32,            // 该领域参与的辩论次数 (至少 3 次才纳入权重调整)
    last_updated: DateTime<Utc>,
}
```

**评分更新规则（每次辩论结束后自动执行）：**

```rust
fn update_model_scores(session: &CouncilSession, scores: &mut Vec<ModelCapabilityScore>) {
    let topic = classify_topic(&session.question);  // 本地关键词匹配，零 Token 消耗
    let arbitration = session.arbitration.as_ref().unwrap();

    for (idx, participant) in session.participants.iter().enumerate() {
        let score = get_or_create_score(scores, &participant.provider, &participant.model);
        let topic_score = score.topic_scores.entry(topic).or_default();

        // 1. 提案采纳率：检查仲裁结论与哪个提案最接近
        let proposal_adopted = arbitration.primary_source_idx == idx;  // 仲裁者标记主要采纳来源
        topic_score.proposal_adopted_rate =
            ema(topic_score.proposal_adopted_rate, if proposal_adopted { 1.0 } else { 0.0 }, 0.3);

        // 2. 评审质量：该模型的评审意见是否在仲裁中被引用
        let reviews_cited = count_cited_reviews(&arbitration.reasoning, idx);
        let review_ratio = reviews_cited as f32 / max(total_reviews_by(idx), 1) as f32;
        topic_score.review_quality = ema(topic_score.review_quality, review_ratio, 0.3);

        topic_score.session_count += 1;
        topic_score.last_updated = Utc::now();
    }
}
```

**能力评分的应用场景：**

| 场景 | 应用方式 |
|------|----------|
| **议会组成** | `/discuss` 时，优先选择在相关话题上得分较高的模型参与 |
| **仲裁权重** | 仲裁 prompt 中注入各模型的历史表现提示，如 "Model A 在架构类问题上采纳率 72%" |
| **fallback 选择** | 当某模型不可用时，选择在该领域表现最接近的替代模型 |
| **用户提示** | `/council stats` 展示各模型的领域能力雷达图 |

**最低数据要求：** 某模型在某领域参与 < 3 次辩论时，不进行权重调整（样本不足），使用默认平等权重。

**存储位置：** 模型能力评分存储在 `.ox/council_scores.json`，跨会话持久化，随项目迁移。

**与 DGM 的关系：** 模型能力学习是 DGM (Directed Growth Model) 在议会维度的具体应用。两者共享 EMA 计算方式和"最小样本量才调整"的保守原则。

---

## 11. 记忆系统

### 11.1 混合记忆架构

```
┌─────────────────────────────────────┐
│           混合记忆系统               │
├──────────────┬──────────────────────┤
│  项目记忆     │  长期记忆             │
│  (Project)   │  (Overall/Long-term) │
│              │                      │
│ - 项目特定    │ - 跨项目通用          │
│ - 架构决策    │ - 最佳实践            │
│ - 业务逻辑    │ - 元技能 (MetaSkill)  │
│ - 代码风格    │ - 反模式 (AntiPattern)│
│ - DEWMA 衰减  │ - ACT-R MCM 衰减     │
└──────────────┴──────────────────────┘
```

### 11.2 存储方案

SQLite + WAL，按团队和项目隔离:

```
~/.ox/memories/
├── teams/
│   ├── team_a/
│   │   ├── project_123.db
│   │   └── project_456.db
│   └── team_b/
│       └── project_789.db
└── long_term.db
```

```rust
fn setup_database(conn: &mut SqliteConnection) {
    conn.execute("PRAGMA busy_timeout=5000");
    conn.execute("PRAGMA journal_mode=WAL");
    conn.execute("BEGIN IMMEDIATE");
}
```

### 11.2b 记忆系统启动与持久化生命周期

记忆存储在 SQLite 文件中，天然跨重启持久化。但启动/退出时需要显式的连接管理和数据一致性保障。

**启动加载流程 (Section 5.1 Step [2] 之后，Step [4] 之前):**

```rust
/// 记忆系统初始化 — 在 RuntimeEnvironment 检测完成后、会话恢复前执行
async fn init_memory_system(runtime: &RuntimeEnvironment, config: &MemoryConfig) -> MemoryManager {
    // 1. 连接数据库
    let long_term_db = open_sqlite(&home_dir().join(".ox/memories/long_term.db")).await;
    let project_db = if let Some(ref project_id) = runtime.project_id {
        Some(open_sqlite(&memory_db_path(project_id)).await)
    } else {
        None
    };
    
    // 2. WAL checkpoint — 将 WAL 日志合并回主数据库文件
    //    上次退出可能是异常关闭 (断电/kill)，WAL 中可能有未合并的数据
    long_term_db.execute("PRAGMA wal_checkpoint(PASSIVE)").await;
    if let Some(ref db) = project_db {
        db.execute("PRAGMA wal_checkpoint(PASSIVE)").await;
    }
    
    // 3. 完整性检查 (仅异常退出后执行，通过 .ox/clean_shutdown 标记判断)
    if !runtime.working_dir.join(".ox/clean_shutdown").exists() {
        log::warn!("上次未正常退出，执行数据库完整性检查...");
        long_term_db.execute("PRAGMA integrity_check").await;
        // 如果检查失败，尝试从 WAL 恢复或提示用户
    }
    
    MemoryManager {
        long_term_db,
        project_db,
        write_buffer: WriteBuffer::new(config.flush_interval),
    }
}
```

**记忆写入策略 — 即时提交 + 批量优化:**

```rust
struct WriteBuffer {
    pending: Vec<MemoryNode>,
    flush_interval: Duration,     // 默认 5s
    last_flush: Instant,
}

impl WriteBuffer {
    /// 低优先级写入 (Fact 类型, depth=1) — 缓冲后批量提交
    fn buffer(&mut self, node: MemoryNode) {
        self.pending.push(node);
        if self.pending.len() >= 10 || self.last_flush.elapsed() > self.flush_interval {
            self.flush();
        }
    }
    
    /// 高优先级写入 (Style/Architectural/AntiPattern, depth>=2) — 立即提交
    fn write_immediate(&self, node: &MemoryNode, db: &SqliteConnection) {
        db.execute_in_transaction(|tx| {
            tx.insert_memory_node(node);
        });
    }
    
    /// 将缓冲区全部写入磁盘
    fn flush(&mut self) {
        if self.pending.is_empty() { return; }
        db.execute_in_transaction(|tx| {
            for node in self.pending.drain(..) {
                tx.insert_memory_node(&node);
            }
        });
        self.last_flush = Instant::now();
    }
}
```

**写入优先级规则:**

| 记忆类型 | depth | 写入方式 | 理由 |
|----------|-------|----------|------|
| Style (用户偏好) | 3 | **立即提交** | 用户明确表达的偏好不能丢 |
| Architectural | 2 | **立即提交** | 架构决策价值高 |
| AntiPattern | 2 | **立即提交** | 错误模式学习不能丢 |
| Council 结论 | 3 | **立即提交** | 议会共识成本高 |
| Fact (工具操作) | 1 | **缓冲批量** | 低价值，丢失可接受 |

**会话恢复时的记忆检索:**

```rust
/// 恢复会话后，用历史上下文重新检索相关记忆
async fn retrieve_memory_on_resume(session: &Session, ctx: &AppContext) -> String {
    // 用最后 3 轮对话的摘要作为检索 query
    let recent_summary = session.messages.iter()
        .rev()
        .filter(|m| m.is_user() || m.is_assistant())
        .take(6)  // 3 轮 = 6 条消息 (user + assistant)
        .map(|m| truncate(&m.content, 100))
        .collect::<Vec<_>>()
        .join(" ");
    
    // 加上 TaskPlan 的当前进行项作为辅助 query
    let task_context = ctx.task_plan.current_in_progress()
        .map(|t| t.description.clone())
        .unwrap_or_default();
    
    let query = format!("{} {}", recent_summary, task_context);
    retrieve_memory(&query, &ctx.runtime.project_id).await
}
```

**关键设计: `decay_score` 不预存，检索时实时计算:**

```rust
/// decay_score 是 last_accessed + 当前时间的函数，不需要存储或更新
/// 数据库中只存 last_accessed 和 depth，检索时实时计算
fn retrieve_from_project_db(query: &str, project_id: &str, types: &[&str], limit: usize) -> Vec<MemoryNode> {
    let rows = db.query("SELECT * FROM memories WHERE project_id = ? AND node_type IN (?)", 
                        &[project_id, types]);
    
    rows.into_iter()
        .map(|row| {
            let mut node = MemoryNode::from_row(&row);
            // ★ 实时计算 decay_score，而非读取存储值
            node.decay_score = calculate_project_decay(&node, config.base_half_life);
            node
        })
        .filter(|n| n.decay_score > 0.3 || n.is_project_critical)
        .sorted_by(|a, b| composite_score(b).partial_cmp(&composite_score(a)).unwrap())
        .take(limit)
        .collect()
}
```

> **为什么不存 `decay_score`?**
> - 衰减是时间的连续函数 — 存储的那一刻就过时了
> - 用户可能关机 3 天才重启，预存的分数完全不反映真实衰减
> - 实时计算只需 `last_accessed` + 当前时间 + `depth`，计算成本可忽略 (纳秒级)
> - 唯一存储的衰减相关字段: `last_accessed`, `depth`, `traces[5]`, `language_weight`

### 11.3 记忆检索流程

```rust
async fn retrieve_memory(query: &str, project_id: &Option<String>) -> String {
    let mut ctx = vec![];

    // 1. 项目记忆 (decay_score > 0.3 或 is_project_critical)
    if let Some(pid) = project_id {
        let mems = retrieve_from_project_db(query, pid, &["architectural", "business", "style"], 5);
        ctx.extend(mems.into_iter().filter(|m| m.decay_score > 0.3 || m.is_project_critical));
    }

    // 2. 长期记忆
    let overall = retrieve_from_overall_db(query, &["bestPractice", "pattern", "expertise"], 5);

    // 3. 排序: depth*0.5 + decay_score*0.3 + recency*0.2
    //    权重依据:
    //    - depth (0.5): 被多次强化的记忆价值最高，最重要的信号
    //    - decay_score (0.3): 衰减分低的记忆可能已过时
    //    - recency (0.2): 近期访问的有一定加成，但不应压过质量
    //    注意: 这是初始值，DGM 可通过 adjustable_fields 优化这三个权重
    let sorted = overall.sorted_by(|a, b| composite_score(b).cmp(&composite_score(a))).take(5);
    ctx.extend(sorted);

    // 4. 去重 (相似度 > 0.85)
    remove_duplicates(ctx, 0.85).join("\n")
}
```

### 11.4 衰减策略

> **设计决策 D2**: 项目记忆和长期记忆采用不同衰减策略。

**计算时机: `decay_score` 检索时实时计算，不预存**

数据库只存储 `last_accessed`、`depth`、`traces[5]`、`language_weight` 等原始字段。`decay_score` 是这些字段 + 当前时间的纯函数，每次检索时实时计算。这意味着:
- 关机 3 天后重启 → 检索时自动反映 3 天的衰减，无需批量更新
- Janitor 清理判断 → 也基于实时计算的 decay_score
- 不存在"启动时刷新全部 decay_score"的步骤 — 数据库记录数可能上千，批量更新没有必要

详见 Section 11.2b `retrieve_from_project_db` 中的实时计算逻辑。

#### 衰减概念模型 (设计指南，非实现公式)

以下公式是**所有衰减算法的设计约束**，不是直接实现。每个具体算法（DEWMA、ACT-R MCM）在实现时必须满足这个基本规律:

```
F ∝ (Q * k) / r²

F = 记忆强度 (最终 decay_score)
Q = 初始记忆质量 (depth, 来源质量)
k = 常数 (语言/领域相关的 language_weight)
r = 距离 (时间衰减，距最后访问的时间)
```

**设计约束** (所有具体衰减算法必须满足):
1. F 与 Q 正相关 → 高 depth 的记忆衰减更慢
2. F 与 k 正相关 → 语言权重高的记忆衰减更慢
3. F 与 r² 负相关 → 时间越久衰减越快，且是加速衰减

**与具体实现的关系**:
- DEWMA 通过 `exp(-age/half_life)` 满足约束 3 (指数衰减比 1/r² 更激进)
- ACT-R MCM 通过多时间尺度 traces 满足约束 1+2 (`language_weight` + `depth` 加成)
- 幂律衰减 `t^(-beta)` 是最接近 1/r² 原始形式的备用方案

#### 项目记忆: DEWMA (双指数加权移动平均)

```rust
fn calculate_project_decay(node: &MemoryNode, base_half_life: u64) -> f32 {
    let age = now() - node.last_accessed;
    if node.is_project_critical { return 1.0; } // 关键记忆永不衰减

    let short_term = (-age as f32 / (base_half_life as f32 * 0.3)).exp(); // 快速衰减
    let long_term = (-age as f32 / (base_half_life as f32 * 5.0)).exp();  // 缓慢衰减
    0.7 * short_term + 0.3 * long_term
}
```

#### 长期记忆: ACT-R MCM + 多时间尺度 traces + 语言权重

```rust
fn calculate_overall_decay(node: &MemoryNode) -> f32 {
    let t = (now() - node.last_accessed) as f32;
    let config = get_language_config(&node.language);

    // 多时间尺度 traces
    let traces_sum: f32 = node.traces.iter()
        .zip(config.traces.iter())
        .map(|(trace, tau)| trace * (-t / tau).exp())
        .sum();
    let base_decay = traces_sum / config.traces.len() as f32;

    // 语言权重 + 深度加成
    base_decay * node.language_weight + (node.depth as f32) * 0.5
}
```

**语言特定配置:**
```toml
[memory.language_config.rust]
lambda = 0.02
max_retention_days = 30
traces = [0.1, 0.2, 0.3, 0.4, 0.5]

[memory.language_config.python]
lambda = 0.01
max_retention_days = 90
traces = [0.05, 0.15, 0.25, 0.35, 0.5]
```

#### 幂律衰减 (长期记忆备用)

```rust
fn power_law_decay(node: &MemoryNode, beta: f32) -> f32 {
    (now() - node.last_accessed).powf(-beta)
}
```

### 11.5 Janitor 清理器

**清理规则:**

| 条件 | 操作 |
|------|------|
| `depth == 0~1` + 超过 `max_retention_days` | 直接删除 |
| `depth == 2` + 超过 `max_retention_days × 2` + `decay_score < 0.3` | 标记候选删除 |
| `depth >= 3` + `decay_score < 0.1` | 标记候选删除 (高价值也需超低分才清理) |
| `is_project_critical` 或 `pin` | 永不清理 |

> **depth=2 管理策略**: depth=2 节点处于"观察区" -- 保留时间是 depth<2 的两倍。如果在观察期内被强化 (depth++)，将进入转化候选。如果一直不被强化且衰减到 0.3 以下，才会被清理。这消除了 depth=2 的管理盲区。

```rust
fn should_cleanup(node: &MemoryNode) -> bool {
    let days = (now() - node.last_accessed) as f32 / 86400.0;
    days > get_retention_threshold(&node.language) as f32
        && node.depth < 4
        && !node.is_project_critical
        && node.last_reinforce_attempt.map(|t| now() - t > 86400 * 2).unwrap_or(true)
}
```

**触发:** 启动 20% 概率 / max_nodes 80% 强制 / `/memory janitor` 手动
**限制:** 每次最多清理 10% 低价值节点

### 11.6 记忆提取规则

**这是记忆系统最关键的环节**: 决定哪些对话内容会被转化为记忆节点。

提取发生在每轮对话结束的后处理阶段 (`ctx.memory.update_from_turn`)：

```rust
async fn extract_memories_from_turn(session: &Session, project_id: &Option<String>) {
    let last_turn = session.get_last_turn(); // (user_msg, assistant_msg, tool_results)

    // 规则 1: Tool 产出 → 自动提取
    // 如果本轮使用了 file_write / shell_exec 等修改性工具，记录操作和结果
    for tool_result in &last_turn.tool_results {
        if tool_result.tool_name.is_write_operation() && !tool_result.is_error {
            store_memory(MemoryNode {
                content: format!("{}: {}", tool_result.tool_name, summarize(&tool_result.output, 200)),
                node_type: MemoryNodeType::Fact,
                tags: extract_tags(&tool_result),
                depth: 1,
                source: MemorySource::ToolOutput,
                ..default_for_project(project_id)
            });
        }
    }

    // 规则 2: 错误修复 → 自动提取 (反模式学习)
    // 如果用户描述了 bug 且 Ox 成功修复，记录错误模式
    if last_turn.detected_pattern == Pattern::BugFix && last_turn.user_satisfied() {
        store_memory(MemoryNode {
            content: format!("错误: {} | 修复: {}", last_turn.bug_description, last_turn.fix_summary),
            node_type: MemoryNodeType::AntiPattern,
            depth: 2,
            ..default_for_project(project_id)
        });
    }

    // 规则 3: 架构/设计决策 → 自动提取
    // 如果对话涉及架构选择 (通过关键词检测: "应该用", "设计", "架构", "选型")
    if last_turn.contains_decision_keywords() {
        store_memory(MemoryNode {
            content: summarize(&last_turn.assistant_response, 500),
            node_type: MemoryNodeType::Architectural,
            depth: 2,
            ..default_for_project(project_id)
        });
    }

    // 规则 4: 用户显式指令 → 偏好提取
    // "以后都用 tabs" / "不要用分号" 等用户偏好指令
    if let Some(preference) = detect_user_preference(&last_turn.user_message) {
        store_memory(MemoryNode {
            content: preference,
            node_type: MemoryNodeType::Style,
            depth: 3, // 用户显式偏好初始深度较高
            ..default_for_project(project_id)
        });
    }

    // 规则 5: 议会结论 → 自动提取 (见 Section 10.8)
    // 由 Council 系统在仲裁完成后直接写入

    // 规则 6: 不提取的内容
    // - 纯问答 (用户问"什么是 REST" → 不提取，这是常识)
    // - 失败的操作 (tool_result.is_error = true，除非是学习反模式)
    // - 敏感内容 (DataSanitizer 检测到密码/key → 不提取)
}
```

**提取决策总表:**

| 对话类型 | 是否提取 | 提取为 | 初始 depth |
|----------|----------|--------|-----------|
| 工具修改操作 (写文件/执行命令) | 是 | Fact | 1 |
| Bug 修复成功 | 是 | AntiPattern | 2 |
| 架构/设计讨论 | 是 | Architectural | 2 |
| 用户偏好指令 | 是 | Style | 3 |
| 议会结论 | 是 | Council | 3 |
| 纯知识问答 | 否 | - | - |
| 失败操作 | 否 | - | - |
| 含敏感信息 | 否 (脱敏后才可) | - | - |

### 11.7 记忆强化

```
相似度 > 0.85 → depth++
同一错误模式复现 → 创建 AntiPattern
跨领域标签重复 ("concurrency") → 提升权重
```

### 11.7b 记忆冲突检测与解决

当新记忆与已有记忆产生语义矛盾时（如"应该用方案 A" vs "方案 A 有问题"），需要检测和解决:

```rust
async fn check_memory_conflict(new_node: &MemoryNode, existing: &[MemoryNode]) -> ConflictResult {
    // 1. 语义相似度检测: 找到高相似的已有记忆
    let similar = existing.iter()
        .filter(|m| semantic_similarity(&new_node.content, &m.content) > 0.7)
        .collect::<Vec<_>>();

    if similar.is_empty() { return ConflictResult::NoConflict; }

    // 2. 矛盾检测: 高相似但情感/结论相反
    for old in &similar {
        if is_contradictory(&new_node.content, &old.content) {
            return ConflictResult::Conflict {
                new_node: new_node.clone(),
                old_node: old.clone(),
            };
        }
    }

    ConflictResult::NoConflict
}
```

**冲突解决策略:**

| 场景 | 策略 | 说明 |
|------|------|------|
| 新记忆 depth > 旧记忆 depth | **新覆旧** | 新记忆更可信 (被强化更多次) |
| 新记忆 depth <= 旧记忆 depth | **共存标记** | 两条都保留，标记 `has_conflict = true`，检索时同时呈现让 LLM 判断 |
| 新记忆来源为 Council | **新覆旧** | 议会结论经过多模型验证，可信度更高 |
| 用户通过 `/feedback` 明确否定旧记忆 | **弃旧存新** | 旧记忆 depth 降为 0，等待 Janitor 清理 |

**检索时冲突处理**: 如果检索到 `has_conflict = true` 的记忆对，在注入上下文时加标注:
```
<conflicting_memories>
  <memory_a>[A 的内容]</memory_a>
  <memory_b>[B 的内容] (与 A 矛盾)</memory_b>
  请根据当前上下文判断哪个更适用。
</conflicting_memories>
```

### 11.8 四级隔离模型

| 级别 | 范围 | 说明 |
|------|------|------|
| Application | 全局 | 所有项目共享的基础知识 |
| SessionGroup | 会话组 | 相关会话间共享 |
| Request | 单次请求 | 仅当前请求可见 |
| Node | 节点级 | 通过 shared_domains 控制 (如 "code_snippets", "design_concepts") |

```toml
[memory]
isolation_application = true
share_session_group = true
share_request = true
# Node 级: shared_domains = ["code_snippets", "design_concepts"]
```

**团队共享:**
```toml
# ~/.config/ox/teams.toml
[teams]
team_a = { shared_project_tags = ["backend", "style"], allowed_teams = ["team_b"], default_retention_period = 30 }
team_b = { shared_project_tags = ["frontend", "design"], allowed_teams = ["team_a"], default_retention_period = 60 }
```

### 11.9 记忆转化 (项目 → 长期)

高价值项目记忆 (depth >= 3, criticality >= 0.8) → 远程模型抽象 → 脱敏 → 存入长期 MetaSkill

**成本风险**: 每条记忆转化约消耗 ~500 tokens (内容 + 抽象 prompt + 回复)。如果 max_nodes=100，单次转化批量可达 ~50K tokens，接近半天的对话成本。因此必须严格控制频率。

**频率限制:**

| 限制 | 默认值 | 说明 |
|------|--------|------|
| `transform_interval` | 7 天 | 两次转化之间的最短间隔 |
| `transform_batch_size` | 20 | 单次转化最多处理的记忆条数 |
| `transform_daily_token_cap` | 10,000 | 单日转化 Token 上限 (受 CostTracker 10% 后台预算约束) |
| `transform_trigger` | 手动优先 | 默认 `/transform` 手动触发；可配置为 `auto` (定期自动执行) |

```rust
async fn memory_transform_process(config: &TransformConfig) {
    // 频率检查
    if last_transform_time().elapsed() < config.transform_interval {
        return; // 未到转化周期
    }

    let mut token_used = 0u32;
    let high_value = retrieve_high_value_memories(
        min_depth: 3,
        max_nodes: config.transform_batch_size,  // 单批上限 (默认 20)
    );

    for mem in high_value {
        // Token 预算检查
        let estimated_cost = estimate_transform_tokens(&mem);
        if token_used + estimated_cost > config.transform_daily_token_cap {
            log_info("转化 Token 预算用尽，剩余记忆下次处理");
            break;
        }

        let abstracted = call_remote_model("抽象为通用最佳实践:\n{}", mem.content).await;
        let sanitized = DataSanitizer::sanitize(&abstracted);
        store_in_overall_database(&MemoryNode {
            content: sanitized, project_id: None,
            node_type: MemoryNodeType::MetaSkill,
            source: MemorySource::RemoteModel, ..Default::default()
        });
        update_project_database_weight(&db, mem.id, 0.7);

        token_used += actual_tokens_used;
    }

    record_transform_time(Utc::now());
}
```

**配置:**
```toml
[memory.transform]
interval_days = 7           # 转化周期 (天)
batch_size = 20             # 单批上限
daily_token_cap = 10000     # 日 Token 上限
trigger = "manual"          # "manual" | "auto"
```

### 11.9b 项目上下文摘要 (Project Context Summary)

记忆系统存储的是**碎片化的知识节点**（单条事实/决策/偏好），但长周期项目中用户需要 Ox 具有**结构化的项目全景**。例如用户第 15 天说"给用户表加 avatar 字段"，Ox 需要同时理解数据库 schema、后端 model、前端组件位置、API 路由模式。

**项目上下文摘要**是记忆之上的结构化聚合层，自动维护、不被对话压缩影响：

**存储位置与格式：**

```
.ox/
├── project_context.md       # 自动生成的项目上下文摘要
├── session.jsonl
└── task_plan.json
```

**摘要内容结构：**

```markdown
# Project Context: my-saas-app
> Auto-generated by Ox. Last updated: 2026-04-22T15:30:00Z

## Tech Stack
- Backend: Rust Axum (backend/)
- Frontend: React TypeScript (frontend/)
- Database: PostgreSQL
- Build: cargo build / npm run build

## Database Schema (key tables)
- users: id, email, name, avatar, created_at
- projects: id, owner_id (FK users), title, status, created_at
- tasks: id, project_id (FK projects), assignee_id, title, status

## API Routes (backend/src/routes/)
- POST /api/auth/login
- GET/POST /api/projects
- GET/PUT/DELETE /api/projects/:id
- GET/POST /api/projects/:id/tasks

## Key Architecture Decisions
- JWT auth with refresh tokens
- Service layer pattern (routes → services → models)
- React Query for data fetching

## Current State
- [x] Database schema + migrations
- [x] Auth system (login/register/refresh)
- [x] Project CRUD
- [ ] Task management
- [ ] Frontend dashboard
```

**自动维护机制：**

```rust
async fn update_project_context(session: &Session, ctx: &AppContext) {
    // 触发条件: 每次会话结束时 (/new 或 /exit)
    // 只在发生了结构性变更时才更新 (新文件创建、schema 变化、路由增删)

    let structural_changes = detect_structural_changes(session);
    if structural_changes.is_empty() { return; }

    let current_context = read_project_context();  // 可能为空 (首次)
    let update_prompt = format!(
        "基于以下变更更新项目上下文摘要 (保持简洁，只记录结构信息):\n\
         当前摘要:\n{}\n\n本次变更:\n{}",
        current_context,
        structural_changes.summary()
    );

    // 消耗少量 Token 生成更新 (~500 tokens)
    let updated = call_remote_model(&update_prompt).await;
    write_project_context(&updated);
}

fn detect_structural_changes(session: &Session) -> Vec<Change> {
    // 从 session 的 tool_results 中检测:
    // - file_write 创建了新文件
    // - shell_exec 运行了 migration
    // - file_patch 修改了路由/schema 相关文件
    // 不触发: 普通代码修改、bug 修复、样式调整
}
```

**与 System Prompt 的集成：**

会话启动时，如果 `.ox/project_context.md` 存在，其内容被注入到 System Prompt 的记忆上下文区域（优先级高于普通记忆节点）：

```
System Prompt:
  ...
  <project_context>
    [project_context.md 的内容]
  </project_context>
  <relevant_memories>
    [检索到的记忆节点]
  </relevant_memories>
```

**Token 成本：** project_context.md 通常 200-500 tokens，占 memory_budget (~2%) 的一部分。如果超出，截断最早的 "Current State" 条目。

### 11.10 OxyGent 记忆生命周期钩子

记忆系统在处理链路中提供钩子，插件和扩展可在每个阶段介入。**分阶段交付，避免过早抽象：**

**Phase 1-2 核心钩子 (必须实现):**

| # | 钩子 | 阶段 | 说明 |
|---|------|------|------|
| 1 | `before_llm_call` | 输入 | 发送到 LLM 前注入记忆上下文、格式化输入 |
| 2 | `after_llm_call` | 输出 | 执行后更新记忆、衰减更新、Token 计费、持久化 |

> Phase 1-2 只需这两个钩子。所有输入预处理（格式化、记忆注入、约束检查）合并到 `before_llm_call`；所有后处理（记忆更新、衰减、日志、持久化、计费）合并到 `after_llm_call`。两个钩子足以覆盖全部核心逻辑，且调用链路简单、debug 容易。

**Phase 3+ 扩展钩子 (按需拆分):**

当插件生态出现后，如果 `before_llm_call` / `after_llm_call` 内部职责过重，再按需拆分为细粒度钩子：

| # | 钩子 | 拆分自 | 说明 |
|---|------|--------|------|
| 1a | `_format_input` | before_llm_call | 格式化输入，准备记忆查询 |
| 1b | `_pre_send_message` | before_llm_call | 注入记忆上下文 |
| 1c | `_before_execute` | before_llm_call | 执行前检查记忆约束 |
| 2a | `_after_execute` | after_llm_call | 执行后更新记忆 |
| 2b | `_post_process` | after_llm_call | 衰减更新等 |
| 2c | `_post_log` | after_llm_call | 记录日志 |
| 2d | `_post_save_data` | after_llm_call | 持久化到 SQLite |
| 2e | `_format_output` | after_llm_call | 格式化输出 |
| 2f | `_post_send_message` | after_llm_call | Token 计费等 |

**拆分原则：** 只在确实有两个以上不同的插件需要在同一阶段的不同时机介入时才拆分。不要为了"设计完整性"提前暴露不需要的钩子。

---

## 12. 人格系统

### 12.1 演化规则

**核心约束 (KEY RULE):** 任何 trait 单次变化 <= `max_trait_change` (默认 0.1)。

```
连续 3 次 "bad" → prefers_conciseness += 0.05
"unsafe" 反馈   → refuses_unsafe_code 保持 true (不可变)
expertise_level → 仅随 depth 自然增长
trait < 0.2 变化视为稳定
```

### 12.2 多语言人格差异

不同语言上下文使用不同的 PersonaVector 数值配置。每个维度都直接参与 System Prompt 生成和行为决策，没有装饰性标签：

| PersonaVector 维度 | Rust 上下文 | Python 上下文 | 影响 |
|---|---|---|---|
| `favors_safety_over_speed` | 0.9 | 0.6 | 安全检查严格程度、是否主动提醒 unsafe 用法 |
| `prefers_conciseness` | 0.8 | 0.7 | 回复详细程度、是否展开解释 |
| `forbidden_phrases` | ["大概可能"] | [] | 过滤不确定性表达 |
| `moral_priorities` | ["安全性", "性能"] | ["可读性", "简洁"] | 代码审查和建议的侧重点 |
| `code_style_strictness` | 0.9 | 0.6 | enforce_lint/enforce_format 的默认阈值 |

> 早期版本使用 MBTI (INFJ/ENFP) 标记不同语言人格，但 MBTI 在工程实践中缺乏可操作性 — `favors_safety_over_speed: 0.9` 比 `INFJ` 更精确、更可调、更可演化。PersonaVector 的每个数值维度都直接映射到具体行为决策，不需要中间解释层。

### 12.3 Self-Prompting

根据 PersonaVector + 语言上下文动态生成 System Prompt:

```rust
fn generate_self_prompt(lang: &str) -> String {
    let p = load_persona_by_language(lang);
    format!(r#"You are Ox in {lang} context with:
- Safety priority: {safety} | Conciseness: {concise}
- Forbidden phrases: {forbidden}
- Value priorities: {values}
- Recent successful patterns: {patterns}"#,
        lang = lang,
        safety = p.favors_safety_over_speed,
        concise = p.prefers_conciseness,
        forbidden = p.forbidden_phrases.join(", "),
        values = p.moral_priorities.join(", "),
        patterns = format!("{:?}", get_recent_successful_patterns()),
    )
}
```

### 12.4 冻结

```
/persona freeze [--lang rust]   冻结 (停止自动演化)
/persona unfreeze               解冻
```

---

## 13. 自演化系统 (DGM)

### 13.1 五阶段 (含回滚)

```
感知 → 收集反馈、记忆统计、Token 成本
提议 → 提出演化方案
验证 → 评估函数验证 (score > 0 才通过)
应用 → 写入配置，保存快照
观察 → N 轮交互后复评，低于基线则回滚
```

> 与旧版四阶段的区别: 新增**观察阶段**。应用后不立即生效为永久配置，而是进入观察期（默认 20 轮对话）。观察期结束后自动复评，如果指标下降则回滚到快照。

### 13.2 评估函数

```rust
fn evaluate(proposal: &EvolutionProposal, stats: &UsageStats) -> f32 {
    // 项目维度: 可自动计算的硬指标
    let project_score = {
        // recall_rate: 用户问了记忆中有的内容，是否成功检索到 (命中率)
        let recall_delta = stats.memory_hit_rate - stats.baseline_hit_rate;
        // retrieval_time: 记忆检索平均耗时 (越短越好)
        let speed_delta = stats.baseline_retrieval_ms - stats.retrieval_ms;
        recall_delta * 0.6 + (speed_delta as f32 / 1000.0) * 0.4
    };

    // 用户维度: 显式反馈 + 隐式信号
    let user_score = {
        // 显式: /feedback good 的比例 (近 50 轮)
        let good_rate = stats.recent_good_count as f32 / stats.recent_feedback_count.max(1) as f32;
        // 显式: 工具调用成功率 (非 is_error 的比例)
        let tool_rate = stats.tool_success_count as f32 / stats.tool_total_count.max(1) as f32;

        // 隐式: 代码采纳率 (Section 15.4)
        // Ox 写入的文件在检测窗口内未被大幅改动的比例
        let code_accept_rate = stats.code_accepted_count as f32
            / stats.code_written_count.max(1) as f32;

        // 权重分配: 隐式信号覆盖面广但噪声较大，给较低权重
        // 显式反馈精确但稀疏，给较高单位权重
        if stats.recent_feedback_count >= 5 {
            // 有足够显式反馈时: 显式为主，隐式为辅
            good_rate * 0.3 + tool_rate * 0.3 + code_accept_rate * 0.4
        } else {
            // 显式反馈不足时: 隐式信号权重提升
            good_rate * 0.15 + tool_rate * 0.25 + code_accept_rate * 0.6
        }
    };

    project_score * 0.5 + user_score * 0.5
}
```

**指标定义 (均可自动计算，无需人工标注):**

| 指标 | 如何自动计算 | 说明 |
|------|-------------|------|
| `memory_hit_rate` | 检索结果非空 / 总检索次数 | 记忆召回率 |
| `retrieval_ms` | 检索耗时 moving average | 检索效率 |
| `good_rate` | `/feedback good` 次数 / 近 50 轮总反馈数 | 用户满意度 (显式) |
| `tool_success_rate` | 非 error 工具调用 / 总工具调用 | 行为准确度 (显式) |
| `code_accept_rate` | 写入后未被大幅改动的文件数 / 总写入文件数 | 代码采纳率 (隐式，见 §15.4) |

> **隐式信号的价值**: 50 轮对话中，显式 `/feedback` 可能只有 3-5 次，但 `file_write` 可能发生 20-30 次。`code_accept_rate` 覆盖面远大于显式反馈，解决了反馈稀疏导致 DGM 评估不稳定的问题。

### 13.3 MetaController

管理可演化参数，使用**指数移动平均 (EMA)**追踪趋势（替代 LSTM，适合小数据量）:

> **#13 修正**: 原设计使用 LSTM 预测趋势，但个人 CLI 工具的 evolution_log 数据量 (通常 < 200 条) 不足以训练 LSTM。改用 EMA，只需最近 10-20 个数据点即可稳定工作。

```rust
struct MetaController {
    /// 可调参数白名单
    adjustable: HashMap<String, AdjustableParam>,
    /// 不可调参数 (安全锁定)
    fixed: HashSet<String>,
    /// EMA 追踪器 (替代 LSTM，适合小样本)
    trend_tracker: HashMap<String, ExponentialMovingAverage>,
}

struct AdjustableParam {
    current_value: f64,
    min_value: f64,
    max_value: f64,
    step_size: f64,      // 单次最大调整幅度
}

struct ExponentialMovingAverage {
    alpha: f64,          // 平滑因子 (默认 0.3)
    current: f64,
    trend: f64,          // 正=上升趋势, 负=下降趋势
}
```

```json
{
    "memory": {
        "adjustable_fields": ["decay_score_weight", "merge_threshold", "retrieval_weights"],
        "forbidden_fields": ["max_nodes", "safety_rules"]
    },
    "persona": {
        "adjustable_fields": ["prefers_conciseness", "favors_safety_over_speed"],
        "fixed_fields": ["refuses_unsafe_code"]
    }
}
```

### 13.4 回滚机制

```rust
struct EvolutionSnapshot {
    id: Uuid,
    timestamp: DateTime<Utc>,
    config_backup: HashMap<String, Value>,  // 演化前的配置快照
    baseline_metrics: UsageStats,           // 演化前的基线指标
    observation_rounds: u32,                // 观察期轮次 (默认 20)
    rounds_completed: u32,                  // 已观察轮次
    status: SnapshotStatus,
}

enum SnapshotStatus {
    Observing,       // 观察中
    Confirmed,       // 观察通过，确认生效
    RolledBack,      // 指标下降，已回滚
}

async fn check_evolution_observation(snapshot: &mut EvolutionSnapshot, current_stats: &UsageStats) {
    snapshot.rounds_completed += 1;

    if snapshot.rounds_completed >= snapshot.observation_rounds {
        let current_score = evaluate_current(current_stats);
        let baseline_score = evaluate_baseline(&snapshot.baseline_metrics);

        if current_score >= baseline_score * 0.95 {
            // 没有显著下降 (5% 容忍) → 确认
            snapshot.status = SnapshotStatus::Confirmed;
            log_evolution("confirmed", &snapshot);
        } else {
            // 显著下降 → 回滚
            restore_config(&snapshot.config_backup);
            snapshot.status = SnapshotStatus::RolledBack;
            log_evolution("rolled_back", &snapshot);
        }
    }
}
```

### 13.5 演化日志 (evolution_log)

记录: meta_skill (学到的元技能)、anti_pattern (反模式)、参数对比、评估得分、**回滚记录**

### 13.6 触发条件

- max_nodes 80% → 记忆参数演化
- 连续 N 次 negative 反馈 → 分类后路由 (见 Section 15)
- EMA trend 连续 3 次同向 → 自动微调参数
- `/evolve [full|memory|persona]` 手动触发

### 13.7 自动参数优化

```rust
fn optimize_with_ema(param_name: &str, current_score: f64) {
    let ema = &mut self.trend_tracker[param_name];
    ema.update(current_score);

    // 只在趋势明确时调整 (避免震荡)
    if ema.trend.abs() > 0.05 {
        let param = &mut self.adjustable[param_name];
        let delta = ema.trend.signum() * param.step_size;
        param.current_value = (param.current_value + delta)
            .clamp(param.min_value, param.max_value);

        // 创建快照用于回滚
        create_snapshot(param_name, param.current_value);
    }
}
```

---

## 14. 安全模块

### 14.1 分层安全

```
Layer 1: Tool 安全级别 (Safe / RequiresConfirmation / Dangerous)
Layer 2: 命令黑名单
Layer 3: high_risk_apis 检测
Layer 4: 重命名检测 (如 use remove_dir_all as clean)
Layer 5: 用户确认
Layer 6: 连续忽略阻止 (3次 → 强制)
```

### 14.2 配置

```toml
[safety]
enable_sandbox = false
confirm_dangerous_ops = true
high_risk_apis = [
    "Command::new", "remove_dir_all", "fs::remove_dir_all",
    "os.remove", "os.rmdir",
]
custom_rules = []
```

### 14.3 DataSanitizer

记忆存储和远程调用前自动脱敏:

```rust
impl DataSanitizer {
    const SENSITIVE_PATTERNS: &[(&str, &str)] = &[
        ("phone", r"1[3-9]\d{9}"),           // 138****1234
        ("email", r"[\w.-]+@[\w.-]+\.\w+"),  // ab***@example.com
        ("id_card", r"\d{17}[\dXx]|\d{15}"), // 1234****5678
        ("bank_card", r"\d{16,19}"),          // 1234****5678
        ("password", r"[密码:]\s*\S+"),       // ****
    ];
    fn sanitize(text: &str) -> String { /* 脱敏 */ }
    fn hash_identifier(id: &str) -> String { /* SHA-256 前 16 位 */ }
}
```

**脱敏触发点 (3 处):**

| 触发点 | 说明 | 配置 |
|--------|------|------|
| 记忆存储 | MemoryNode 写入 SQLite 前 | 始终启用，不可关闭 |
| 记忆转化 | 记忆发送到远程模型做抽象前 | 始终启用，不可关闭 |
| **tool_result 回传 LLM** | tool 执行结果发送到远程 LLM 前 | `sanitize_tool_output` 配置，默认 `true` |

> **风险场景**: 数据迁移时用户说"查一下 phone 为空的记录"，`shell_exec` 执行 SQL 查询后 tool_result 包含真实用户数据（手机号、邮箱等）。如果直接发送到远程 LLM，构成隐私泄露。

**tool_result 脱敏实现:**

```rust
// 在 Agent Turn 中，tool_result 加入 messages 前脱敏
let result = ctx.tools.execute(&call).await;
let sanitized_output = if ctx.config.safety.sanitize_tool_output {
    DataSanitizer::sanitize(&result.output)
} else {
    result.output.clone()
};
messages.push(Message::tool_result(call.id, &sanitized_output, result.is_error));

// 原始未脱敏输出保留在 session 日志中 (本地存储，不发送远程)
session.raw_tool_outputs.push(RawToolOutput {
    call_id: call.id,
    original: result.output,   // 未脱敏，用于本地调试
});
```

**配置：**
```toml
[safety]
sanitize_tool_output = true    # tool_result 回传 LLM 前脱敏 (默认 true)
# 设为 false: 当用户确认不涉及敏感数据 (如纯代码项目)
```

### 14.4 沙箱

```
/sandbox on|off    Docker 容器执行
--no-remote        完全本地，不调远程 API
```

---

## 15. 用户反馈系统

### 15.1 反馈命令

```
/feedback good     → depth++ (强化最近记忆)
/feedback bad      → depth-- (降低记忆权重)
/feedback unsafe   → 标记安全违规，触发审查
```

### 15.2 反馈 → 演化映射 (分类路由)

> **#16 修正**: 原设计将所有 "bad" 反馈直接路由到人格演化，过于粗暴。用户给 "bad" 可能是因为代码逻辑错误（记忆/工具问题），不一定是人格问题。

**反馈分类**: `/feedback bad` 后，Ox 通过**本地启发式规则**自动分类原因 (不消耗 Token):

```rust
fn classify_negative_feedback(last_turn: &Turn) -> FeedbackCategory {
    // 1. 工具失败 → 工具策略问题
    if last_turn.tool_results.iter().any(|r| r.is_error) {
        return FeedbackCategory::ToolFailure;
    }

    // 2. 记忆相关 → 记忆质量问题
    if last_turn.used_memories && !last_turn.memory_was_relevant {
        return FeedbackCategory::MemoryIrrelevant;
    }

    // 3. 代码不正确 → 代码质量问题
    if last_turn.generated_code && last_turn.code_had_errors {
        return FeedbackCategory::CodeQuality;
    }

    // 4. 无法归类 → 默认为风格/人格问题
    FeedbackCategory::StyleMismatch
}
```

**分类路由表:**

| 反馈类型 | 分类 | 路由动作 |
|----------|------|----------|
| `good` | - | 记忆 depth++，可能触发 MetaSkill 转化 |
| `bad` → ToolFailure | 工具策略 | 记忆相关工具调用的 depth--，**不触发人格演化** |
| `bad` → MemoryIrrelevant | 记忆质量 | 检索权重参数微调 (DGM adjustable)，**不触发人格演化** |
| `bad` → CodeQuality | 代码质量 | 创建 AntiPattern，记录错误模式 |
| `bad` → StyleMismatch | 风格/人格 | 连续 5 次 (非 3 次) 才触发人格演化 |
| `unsafe` | 安全 | 标记安全违规，refuses_unsafe_code 不可变，记录安全日志 |

### 15.3 交互式确认

高影响操作 (文件写入/删除、Shell 执行、危险 API) → `允许? [y/n]`

### 15.4 隐式反馈信号 (零 Token 成本)

显式反馈 (`/feedback good/bad`) 依赖用户主动操作，实际使用中反馈率很低（50 轮对话可能只有 3-5 次反馈）。隐式信号通过观察用户行为自动采集，无需用户干预。

**信号 1: 代码改动检测 (Code Override Detection)**

Ox 通过 `file_write` 生成代码后，监测用户是否在短时间内对同一文件做了大幅修改：

```rust
struct CodeOverrideDetector {
    /// Ox 最近写入的文件及其内容哈希
    recent_writes: HashMap<PathBuf, WriteRecord>,
    /// 检测窗口 (默认 5 分钟)
    detection_window: Duration,
}

struct WriteRecord {
    content_hash: u64,       // xxHash 快速哈希
    line_count: usize,
    timestamp: Instant,
}

/// 在每次用户输入前检查 (在 REPL loop 中调用)
fn detect_overrides(&self) -> Vec<OverrideSignal> {
    let mut signals = vec![];
    for (path, record) in &self.recent_writes {
        if record.timestamp.elapsed() > self.detection_window { continue; }

        // 比较文件当前内容与 Ox 写入时的内容
        let current_hash = hash_file(path);
        if current_hash == record.content_hash { continue; }  // 未改动

        let current_lines = count_lines(path);
        let change_ratio = diff_ratio(record.content_hash, current_hash);

        signals.push(OverrideSignal {
            path: path.clone(),
            change_ratio,   // 0.0~1.0，改动比例
            time_elapsed: record.timestamp.elapsed(),
        });
    }
    signals
}
```

**改动比例 → 隐式反馈映射:**

| 改动比例 | 解读 | DGM 信号 |
|----------|------|----------|
| < 5% | 微调（格式、变量名） | 忽略，不视为负反馈 |
| 5% ~ 30% | 部分修正 | 弱负信号 (weight: 0.3)，记录但不立即影响 |
| > 30% | 大幅重写 | 强负信号 (weight: 0.8)，等效于 `/feedback bad` |
| 文件被删除 | 完全拒绝 | 强负信号 (weight: 1.0)，创建 AntiPattern 记忆 |

**信号 2: 对话放弃检测 (Conversation Abandon)**

用户在 Ox 回复后直接开始新话题（无关联），或长时间无响应后 `/new` 开始新会话：

```rust
fn detect_abandon(current_input: &str, last_assistant_output: &str) -> bool {
    // 语义相关性极低 + 无 /feedback → 可能是放弃
    let similarity = local_keyword_overlap(current_input, last_assistant_output);
    similarity < 0.1 && !current_input.starts_with("/feedback")
}
```

这个信号较弱 (weight: 0.1)，只在连续出现 3 次以上时纳入 DGM 评估。

**隐私保护:** 隐式反馈只记录文件路径、改动比例和时间戳，**不记录文件内容**。检测使用快速哈希比较，不读取文件全文。

---

## 16. 多语言支持

### 16.1 语言感知

Ox 根据当前语言上下文自动调整:

1. **记忆检索** → 优先当前语言节点 (language_weight 加权)
2. **人格切换** → 加载对应语言 PersonaVector
3. **System Prompt** → 注入语言特定规则和禁止表达
4. **衰减参数** → 使用语言特定 traces, lambda, max_retention_days

### 16.2 语言检测

```
1. 用户显式指定 (/lang python)
2. 项目配置文件检测:
   - Cargo.toml → Rust
   - package.json → JavaScript/TypeScript
   - pyproject.toml / requirements.txt → Python
   - go.mod → Go
   - pom.xml / build.gradle → Java
   - *.sln / *.csproj → C#
   - ...可扩展
3. 对话中提到的语言
4. 最近编辑文件扩展名
```

### 16.3 命令

```
/memory list --lang python
/persona show --lang rust
/lang rust                       手动切换
```

---

## 17. 插件与扩展系统

### 17.1 核心 Trait

```rust
trait MemoryBackend {
    fn store(&self, node: &MemoryNode) -> Result<()>;
    fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryNode>>;
    fn delete(&self, id: &str) -> Result<()>;
    fn update(&self, node: &MemoryNode) -> Result<()>;
    fn list(&self, filter: &MemoryFilter) -> Result<Vec<MemoryNode>>;
}

#[async_trait]
trait LlmProvider: Send + Sync {
    async fn stream_chat(&self, messages: Vec<Message>, tools: &[Value]) -> Result<LlmResponseStream>;
    fn model_name(&self) -> &str;
    fn context_window_size(&self) -> u32;
    fn cost_per_input_token(&self) -> f32;
    fn cost_per_output_token(&self) -> f32;
}

#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, args: Value) -> ToolOutput;
    fn safety_level(&self) -> SafetyLevel { SafetyLevel::Safe }
}
```

### 17.2 事件系统

| 事件 | 触发时机 | 数据 |
|------|---------|------|
| `SkillUsed` | 工具调用 | tool_name, args, result |
| `PersonaUpdated` | 人格更新 | old, new, reason |
| `MemoryAdded` | 新增记忆 | node |
| `MemoryRemoved` | 删除记忆 | node_id, reason |
| `EvolutionProposal` | 演化提案 | proposal, score |
| `SessionStarted` | 会话开始 | session_id, project_id |
| `SessionEnded` | 会话结束 | session_id, summary |
| `CostAlert` | 成本告警 | current_cost, limit |

---

## 18. Slash 命令参考

> **设计决策 D7**: 所有控制命令统一为 /slash 命令。

### 会话
```
/new  /sessions  /resume <id>  /clear  /exit
```

### 任务计划
```
/plan                         # 查看当前任务计划进度
/plan skip <N> [--reason R]   # 跳过第 N 项
/plan clear                   # 清除已完成的计划
```

### 工具信任
```
/trust <tool_name>            # 当前会话内临时信任该工具 (跳过确认)
/trust --all                  # 信任所有 RequiresConfirmation 工具 (Dangerous 除外)
/untrust                      # 撤销所有临时信任
```

### 模型
```
/model [name]  /model list  /effort <level>
/cost  /cost limit --monthly=5.0  /cost simulate "描述"  /stats
```

### 记忆
```
/memory  /memory list [--tag T --type T --lang L --project P]
/memory search <q>  /memory pin <id>  /memory unpin <id>
/memory remove <id>  /memory merge  /memory janitor
/memory export  /memory import <file>
/memory lock --project P <id>
/memory share --project P --team T --tags "a,b"
/memory transform --project P --type meta_skill --depth-threshold 3
/memory config show --lang rust
/memory config set --lang python --trace-tau 2 0.25
```

### 人格
```
/persona  /persona show [--lang L]  /persona set <trait>=<val>
/persona freeze [--lang L]  /persona unfreeze
/persona reset [--trait T]  /persona compare
/persona export  /persona import --lang en --file F
```

### 安全
```
/safety rules  /safety rules add|remove <pattern>
/safety scan [--file F --dir D]  /safety report
/sandbox on|off
```

### 演化
```
/evolve [full|memory|persona]  /evolve log
```

### 项目
```
/project  /project init  /project list  /project status
/cd <path>                    # 切换工作目录 (自动检测项目边界，触发上下文热切换)
/cd                           # 显示当前工作目录和项目信息
```

### 配置
```
/config  /config show <section>  /config set <k>=<v>
/config validate --report=true
/config baseline --export|--diff|--import=F
/config reset [--section S]
```

### 反馈 & 语言
```
/feedback good|bad|unsafe
/lang [language]
/help [cmd]  /debug
```

### 议会 (Council)
```
/discuss [prompt]              # 对当前/指定问题启动多模型辩论
/discuss --rounds <N>          # 指定辩论轮次 (默认 2)
/discuss --models "a,b,c"      # 指定参与模型 (默认使用所有已配置模型)
/verbose                       # 切换当前议会为详细输出模式
/council last                  # 查看上一次议会完整记录
/council history               # 列出所有议会记录
/council stats                 # 展示各模型的领域能力评分
```

---

## 19. 配置文件规范

**路径:** `~/.config/ox/config.toml`

```toml
[general]
version = "2.1"
debug_mode = false
verbose = false
lang = "en"

[repl]
history_file = "~/.ox/history"
max_history_entries = 10000
multiline_enabled = true
stream_output = true
syntax_highlight = true

[terminal]
split_view = true                              # 启用 Split-View 布局 (输出区 + 输入区)
output_ratio = 85                              # 输出区占终端高度百分比 (输入区 = 100 - output_ratio)
urgent_prefix = "!"                            # 紧急介入前缀字符
input_during_agent = true                      # Agent 工作期间允许用户输入 (关闭则退化为传统阻塞模式)

[session]
auto_restore = true
max_archived_sessions = 50
session_dir = ".ox"

[context]
max_history_turns = 20
memory_budget_tokens = 2000
history_budget_tokens = 50000
reply_reserve_tokens = 73000

[tools]
auto_confirm_safe = true
confirm_writes = true
confirm_shell = true
shell_timeout_ms = 30000
max_output_chars = 10000

[models]
default = "gpt-4o"
backup = ["Claude-Opus-4.6", "gpt-4-turbo"]
openai_api_key = ""
anthropic_api_key = ""
deepseek_api_key = ""
adaptive_thinking = true
effort_level = "high"

[council]
default_rounds = 2                        # 默认辩论轮次
max_rounds = 3                            # 最大辩论轮次
max_participants = 4                      # 最大参与模型数
participants = ["gpt-4o", "claude-sonnet-4-20250514", "deepseek-coder"]  # 默认参与模型
arbiter_model = "default"                 # 仲裁模型 ("default" 使用 models.default)
early_convergence_threshold = 0.8         # 评审分数 > 此值时跳过反驳阶段
verbose_by_default = false                # 默认是否显示完整讨论过程
budget_warning = true                     # 启动前是否预估成本并警告
council_memory_decay_factor = 0.7         # 议会结论记忆的衰减系数 (低于1 = 衰减更慢)

[memory]
max_nodes = 1000
alpha = 0.8
time_decay = 0.01
isolation_application = true
share_session_group = true
share_request = true
export_format = "json"
janitor_run_on_startup_prob = 0.2

[memory.project_decay]
base_half_life = 30
critical_threshold = 0.3

[memory.overall_decay]
beta = 0.015

[memory.language_config.rust]
lambda = 0.02
max_retention_days = 30
traces = [0.1, 0.2, 0.3, 0.4, 0.5]

[memory.language_config.python]
lambda = 0.01
max_retention_days = 90
traces = [0.05, 0.15, 0.25, 0.35, 0.5]

[memory.transform]
interval_days = 7                          # 两次转化之间的最短间隔 (天)
batch_size = 20                            # 单次转化最多处理的记忆条数
daily_token_cap = 10000                    # 单日转化 Token 上限
trigger = "manual"                         # "manual" (需 /memory transform) | "auto" (定期自动)

[persona]
auto_evolve = true
max_trait_change = 0.1
frozen = false
export_format = "json"

[behavior_rules]
enforce_safe_code = true
enforce_lint = true           # 自动适配: clippy/eslint/ruff/golint/...
enforce_format = true         # 自动适配: rustfmt/prettier/black/gofmt/...
enforce_tests = true
enforce_all = true

[safety]
enable_sandbox = false
confirm_dangerous_ops = true
high_risk_apis = ["Command::new", "remove_dir_all", "fs::remove_dir_all", "os.remove", "os.rmdir"]
custom_rules = []

[cost]
max_monthly_cost = 5.0
max_daily_cost = 2.0
budget_alert_threshold = 0.8
cost_transparency = true
```

---

## 20. 验收清单与测试计划

### 20.1 REPL 核心 (1-6)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 1 | REPL 启动 | `ox` 显示 `ox>` 提示符 |
| 2 | 多轮对话 | 连续 5 轮上下文连贯 |
| 3 | 会话恢复 | 退出重启后恢复并可继续 |
| 4 | /slash 命令 | `/help`、`/memory`、`/persona` 正确执行 |
| 5 | 流式输出 | 逐字输出 |
| 6 | 代码高亮 | 代码块语法高亮 |

### 20.2 Tool Use (7-12)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 7 | 文件读取 | "读取 src/main.py" → file_read |
| 8 | 文件写入 | "创建 hello world" → file_write + 确认 |
| 9 | 代码搜索 | "找到所有 TODO" → code_search |
| 10 | Shell 执行 | "运行测试" → shell_exec (自动识别 pytest/npm test/cargo test 等) + 确认 |
| 11 | 危险拦截 | "删除 /tmp" → 拦截 |
| 12 | 连续 Tool Call | 复杂任务多工具连续调用 |

### 20.3 上下文与 Token (13-16)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 13 | API Key 安全 | Key 不出现在日志和会话文件 |
| 14 | Token 追踪 | `/cost` 准确统计，误差 < 50% |
| 15 | 历史截断 | 超 N 轮截断，对话仍连贯 |
| 16 | 预算告警 | 月度 80% 时告警 |

### 20.4 记忆系统 (17-25)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 17 | 记忆注入 | 对话引用到项目知识 |
| 18 | 深度增长 | depth >= 2 |
| 19 | Janitor 清理 | 10% 低价值清理，max_nodes < 80% |
| 20 | 项目隔离 | A 记忆不泄漏到 B |
| 21 | 多语言隔离 | Python/Rust 独立 |
| 22 | DEWMA 衰减 | 30/90 天后按配置衰减 |
| 23 | 记忆转化 | depth >= 3 → MetaSkill |
| 24 | 团队共享 | 授权团队可见 |
| 25 | 关键记忆 | is_project_critical 99% 保留 |

### 20.5 人格与安全 (26-30)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 26 | 人格不突变 | 变化后仍在合理范围 |
| 27 | 人格冻结 | freeze 后不变 |
| 28 | 安全字段不可变 | refuses_unsafe_code=false 被拒绝 |
| 29 | --no-remote | 本地模式正常 |
| 30 | 敏感数据脱敏 | 手机号/邮箱被脱敏 |

### 20.6 反馈与演化 (31-34)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 31 | 反馈生效 | /feedback bad 降低权重 |
| 32 | DGM 有效性 | 演化后泛化提升 |
| 33 | max_trait_change | 单次 <= 0.1 |
| 34 | Janitor 衰减率 | 低深度 80% 清理率 |

### 20.7 议会系统 (35-40)

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 35 | /discuss 启动议会 | 至少 2 个模型参与辩论 |
| 36 | 辩论完整流程 | Phase 1→2→3→4 全部执行 |
| 37 | 提前收敛 | 评审分数 > 0.8 时跳过反驳 |
| 38 | 成本预估 | 启动前显示预估 Token 消耗 |
| 39 | /verbose 切换 | 可查看完整讨论过程 |
| 40 | 议会记忆 | 结论保存为 MemorySource::Council，衰减系数 0.7 |
| 41 | 模型能力学习 | 3 次同领域辩论后，`/council stats` 显示差异化评分 |
| 42 | 能力驱动选择 | 高评分模型被优先选入同领域议会（session_count >= 3 后生效） |
| 43 | 执行中断 | Ctrl+C 中断流式输出/tool 执行后回到 REPL 提示符，部分结果保留 |
| 44 | 隐式反馈-代码改动 | Ox 写入文件后用户大幅修改 (>30%)，DGM 统计 code_accept_rate 下降 |
| 45 | 隐式反馈-权重切换 | 显式反馈 < 5 次时，code_accept_rate 权重自动提升至 0.6 |
| 46 | 记忆转化频率 | 两次转化间隔 < 7 天时跳过，单批 <= 20 条，日 Token <= 10K |
| 47 | 钩子分阶段 | Phase 1-2 只有 before_llm_call + after_llm_call 两个钩子 |
| 48 | file_patch 局部修改 | search_replace 模式精准替换，old_string 不唯一时报错 |
| 49 | 批量确认 /trust | `/trust file_write` 后 file_write 跳过确认，Dangerous 永不跳过 |
| 50 | shell_exec 流式输出 | 长时命令实时显示 stdout/stderr，最终截断后回传 LLM |
| 51 | TaskPlan 持久化 | `/plan` 查看进度，会话压缩后计划不丢失，新会话自动注入 |
| 52 | 项目上下文摘要 | `.ox/project_context.md` 自动维护，新会话注入 System Prompt |
| 53 | tool_result 脱敏 | shell_exec 输出含手机号/邮箱时，发送 LLM 前自动脱敏 |
| 54 | 运行时环境感知 | Windows/Linux/macOS 正确检测 shell 类型，System Prompt 注入 OS/Shell/cwd 信息 |
| 55 | 目录切换与上下文热切换 | `/cd` 同项目子目录只更新 cwd，跨项目切换自动加载新 TaskPlan、project_context、记忆过滤 |
| 56 | 用户介入 (Split-View) | Agent 工作中用户输入在下次 LLM 调用前注入，`!` 前缀紧急介入跳过当前 tool 调用 |
| 57 | 记忆持久化与重启恢复 | 退出重启后记忆完整，高优先级记忆立即提交，decay_score 实时计算无需刷新 |
| 58 | Graceful Shutdown | `/exit` 正常退出全部安全，断电最多丢 ≤10 条 Fact 级缓冲，clean_shutdown 标记异常检测 |

### 20.8 测试用例

```bash
# 安全字段不可变
/evolve --mode persona   # 尝试 refuses_unsafe_code=false → 拒绝
/persona set refuses_unsafe_code=false  # → 拒绝

# 记忆衰减
# 添加 Rust 记忆 depth=1, 30天前 → 应被清理

# trait 上限
# 触发多次演化 → 单次变化 <= 0.1

# Token 预算
# /config set cost.max_monthly_cost=0.01 → 请求被阻止

# 会话恢复
# /exit → ox → 自动恢复

# 议会系统
/discuss 这个函数应该用递归还是迭代？
# → 至少 2 个模型参与辩论，输出仲裁结论
/discuss --rounds 3 --verbose 如何设计缓存策略？
# → 3 轮辩论，全过程可见
/council last
# → 可回顾上次议会完整记录
```

---

## 21. 路线图

### 阶段一: REPL 骨架
- [ ] Cargo workspace
- [ ] REPL 引擎, 消息协议, 会话管理
- [ ] LLM 调用层 (OpenAI streaming)
- [ ] Tool 系统 (file_read/write, shell_exec, code_search)
- [ ] 上下文构建器, /slash 命令, 配置系统
- [ ] 流式输出 + Markdown 渲染, Token 追踪

**里程碑**: 启动 ox，对话，读写文件，执行命令

### 阶段二: 记忆与人格
- [ ] MemoryNode + SQLite, 记忆检索
- [ ] DEWMA / ACT-R MCM 衰减, Janitor
- [ ] PersonaVector, Self-Prompting
- [ ] 用户反馈, DataSanitizer
- [ ] OxyGent 钩子, 四级隔离, 行为规则

**里程碑**: 记住项目上下文，调整人格

### 阶段三: 议会与混合记忆
- [ ] 多 AI 议会系统 (Council Orchestrator)
- [ ] 辩论协议 (提案/评审/反驳/仲裁)
- [ ] 议会成本控制 + 提前收敛
- [ ] 混合记忆, 记忆转化
- [ ] 多模型路由, 多语言人格
- [ ] Embedding/Reranker

**里程碑**: /discuss 可用，多模型辩论验证，记忆跨项目迁移

### 阶段四: 自演化
- [ ] DGM, MetaController, 自动调优
- [ ] 团队协作, 共享记忆
- [ ] 更多 Tool, 沙箱
- [ ] 插件系统, 事件系统

**里程碑**: 完整自演化 Agent

### 阶段五: 生态
- [ ] 社区插件市场
- [ ] 企业安全审计, 分布式存储
- [ ] 多 Agent 协作 (基于议会模式扩展)

---

## 22. 附录: 设计决策记录

| ID | 问题 | 决策 | 理由 |
|----|------|------|------|
| D1 | 三份文档 MemoryNode 不同 | 合并，项目字段用 Option | 避免多结构体 |
| D2 | 三种衰减公式 | 项目→DEWMA, 长期→ACT-R MCM, 幂律备用。F=Qk/r² 作概念基础 | 不同场景不同需求 |
| D3 | DGM 可能改安全 | 安全字段 fixed_fields | 安全是底线 |
| D4 | 版本混乱 | 统一 v2.1, Git 管理 | Single Source of Truth |
| D5 | 界面形式 | REPL 唯一主界面, /slash 命令 | Ox 是交互智能体 |
| D6 | 是否需本地意图分类 | 全部远程 LLM | Function Calling 已含意图理解 |
| D7 | REPL 内命令形式 | /slash 命令 | REPL 最自然的控制方式 |
| D8 | 会话是否持久 | 项目绑定会话，自动恢复 | 上下文连续性 |
| D9 | 上下文管理 | 当前轮 + 记忆 + 前N轮 | Token 效率与连贯性平衡 |
| D10 | 多模型使用方式 | 用户可控辩论式议会，非自动触发 | 成本可控 + 用户信任 |
| D11 | 议会讨论模式 | 辩论式 (提案→评审→反驳→仲裁) | 交叉验证降低单模型偏见 |
| D12 | 议会输出可见性 | 默认结论摘要，可切换详细 | 平衡信息量与简洁性 |
| D13 | 底层编码准则 | Coding Principles (P1-P4) 作为 AI 执行底层 | 约束 Ox 代码行为和决策过程 |
| D14 | 工具语言无关 | 移除 cargo_run 等语言特定工具，统一用 shell_exec + project_detect | Ox 面向多语言项目 |
| D15 | Agent Turn 消息累积 | tool_call 消息必须先于 tool_result 加入 messages，不使用 clone() | LLM 需要完整的 call→result 对应关系才能正确推理 |
| D16 | 执行中断机制 | CancellationToken 分层中断 (单击优雅/双击强制)，所有异步阶段可中断 | 长时 tool 执行和议会辩论必须可被用户打断 |
| D17 | 议会模型能力学习 | EMA 跟踪各模型在不同领域的采纳率/评审质量，≥3 次后生效 | 避免每次议会平等对待所有模型，但也不在数据不足时过早偏向 |
| D18 | OxyGent 钩子分阶段 | Phase 1-2 只暴露 before_llm_call + after_llm_call，Phase 3+ 按需拆分 | 避免过早抽象，10 个钩子的中间件管道在无插件生态时是维护负担 |
| D19 | 移除 MBTI 标签 | 人格差异纯用 PersonaVector 数值维度表达，MBTI 无工程意义 | 数值维度直接参与行为决策和 DGM 演化，MBTI 是无法计算的装饰层 |
| D20 | 记忆转化频率限制 | 默认 7 天周期 + 单批 20 条 + 日 10K Token 上限 | 每条转化 ~500 tokens，无限制可达 50K/批，接近半天对话成本 |
| D21 | 隐式反馈信号 | 通过文件改动检测 + 对话放弃检测采集隐式信号，弥补显式反馈稀疏 | 50 轮对话 file_write 20-30 次 vs /feedback 3-5 次，覆盖面差 10 倍 |
| D22 | 产品核心叙事 | 记忆系统是核心护城河（高迁移成本），议会是低频高价值的后盾能力 | 阶段二记忆系统 > 阶段三议会系统的产品优先级 |
| D23 | file_patch 局部编辑 | search-replace 局部修改代替全文件输出，节省 ~94% 输出 tokens | 2000 行文件修改 3 行：file_write 输出 2000 行 vs file_patch 输出 ~120 tokens |
| D24 | 批量确认模式 | /trust 会话级信任 + Dangerous 级别永不跳过，Session 结束自动清空 | 50 次 tool 调用场景下 20+ 次确认严重打断心流，但危险操作必须保底 |
| D25 | shell_exec 流式输出 | 实时 stdout/stderr 显示 + 截断最后 50 行作为 tool_result | 长时构建用户需要即时反馈，但完整日志传给 LLM 浪费 tokens |
| D26 | TaskPlan 持久化 | 任务计划存 `.ox/task_plan.json`，免疫对话压缩，Session 恢复时注入 System Prompt | 100+ 轮对话中 context 压缩会丢失任务全貌，独立持久化保证跨压缩可追踪 |
| D27 | 项目上下文摘要 | 自动维护 `.ox/project_context.md`，结构化聚合高于记忆节点的项目级知识 | 记忆节点是细粒度片段，新 Session 需要项目级全貌才能快速进入状态 |
| D28 | tool_result 脱敏 | DataSanitizer 扩展到 3 个触发点 (存储/转化/回传)，原始输出本地保留 | tool 输出可能包含环境变量、路径、密钥，回传远程 LLM 前必须脱敏 |
| D29 | 运行时环境感知 | 启动时自动检测 OS/Shell/项目根，注入 System Prompt，shell_exec 用 ShellInfo 构造命令 | LLM 需要知道用户系统才能生成正确的 shell 命令，手动配置不现实 |
| D30 | 目录切换上下文热切换 | `/cd` 检测项目边界，同项目只更新 cwd，跨项目自动切换 TaskPlan/记忆/project_context | Session 不强制中断 — 用户经常需要跨项目参照，对话上下文应连续 |
| D31 | Split-View 用户介入 | ratatui 双区布局 + InputBuffer + 自然边界注入 + `!` 紧急介入 | 传统 CLI 阻塞模式下用户只能 Ctrl+C 中断再重新描述，50+ 轮任务中断成本极高 |
| D32 | decay_score 不预存 | 衰减分数检索时实时计算，数据库只存 last_accessed/depth/traces | 衰减是时间连续函数，存储的值瞬间过时；实时计算成本可忽略 (纳秒级) |
| D33 | 记忆写入分级 | depth>=2 立即提交，depth=1 缓冲批量写入 (≤10 条或 5s) | 高价值记忆不能丢，低价值 Fact 批量写入减少 SQLite 事务开销 |
| D34 | Graceful Shutdown | 退出时 flush buffer → save TaskPlan → WAL checkpoint → clean_shutdown 标记 | 异常退出通过 WAL + 标记文件 实现下次启动自动恢复 |
