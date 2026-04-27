# Ox CLI Phase 2: 记忆与人格

## Context

阶段一 (REPL 骨架) 已交付：可启动、对话、流式输出、12 个工具、会话持久化、安全确认、Token 追踪、VS Code 深色主题、目录体系 (`~/.ox/` 系统级 + `<project_root>/.ox/` 项目级)。

阶段二目标：**让 Ox 记住项目上下文、调整人格、采集反馈**。这是产品核心护城河 (D22)。

设计文档：`docs/Ox-CLI-技术设计文档.md` Section 11-13, 15。

## 阶段二总览

```
┌─────────────────────────────────────────────────────────────┐
│  Phase 2: 记忆与人格                                         │
├──────────┬──────────┬──────────┬──────────┬────────────────┤
│  M1      │  M2      │  M3      │  M4      │  M5            │
│  记忆存储 │  记忆检索 │  记忆衰减 │  人格系统 │  反馈+行为规则  │
│  SQLite  │  检索+注入│  DEWMA   │  Vector  │  /feedback     │
│  +提取   │  +四级隔离│  +Janitor│  +演化   │  +DataSanitizer│
└──────────┴──────────┴──────────┴──────────┴────────────────┘

里程碑: 记住项目上下文，调整人格
```

## 新增依赖

```toml
# Workspace Cargo.toml 新增
rusqlite = { version = "0.32", features = ["bundled"] }  # SQLite + WAL
```

---

## M1: 记忆存储 + 提取

**Goal**: MemoryNode 写入 SQLite，每轮对话后自动提取记忆。

### 1.1 数据结构

**文件**: `crates/ox-core/src/memory/mod.rs`

```rust
/// 记忆节点 — 记忆系统的核心数据单元
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,                          // UUID v4
    pub content: String,                     // 记忆内容 (≤500 字)
    pub node_type: MemoryNodeType,           // 类型
    pub depth: u8,                           // 强化深度 (0=新, 最大10)
    pub project_id: Option<String>,          // None = 长期记忆
    pub language: String,                    // 关联编程语言
    pub source: MemorySource,                // 来源
    pub created_at: i64,                     // Unix timestamp
    pub last_accessed: i64,                  // 最后访问时间
    pub is_project_critical: bool,           // 关键记忆 (永不衰减)
    pub traces: [f32; 5],                    // ACT-R 多时间尺度 trace
    pub language_weight: f64,                // 语言权重 (0.0~1.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemoryNodeType {
    Fact,               // 事实 (depth 1, 批量写入)
    Style,              // 用户偏好 (depth 3, 立即写入)
    Architectural,      // 架构决策 (depth 2, 立即写入)
    AntiPattern,        // 反模式 (depth 2, 立即写入)
    Business,           // 业务逻辑 (depth 2)
    BestPractice,       // 最佳实践 (长期记忆)
    Pattern,            // 模式 (长期记忆)
    MetaSkill,          // 元技能 (长期记忆)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemorySource {
    UserExplicit,       // /remember 命令
    ToolObservation,    // 工具操作观察
    LlmExtraction,      // LLM 后处理提取
    CouncilConclusion,  // 议会结论 (Phase 3)
    Feedback,           // 用户反馈
}
```

### 1.2 SQLite 存储层

**文件**: `crates/ox-core/src/memory/store.rs`

- `MemoryStore::open(path)` — 打开 SQLite, PRAGMA WAL + busy_timeout=5000
- `MemoryStore::init_schema()` — 建表 `memories` + 索引
- `MemoryStore::insert(&self, node: &MemoryNode)` — 插入单条
- `MemoryStore::insert_batch(&self, nodes: &[MemoryNode])` — 事务批量插入
- `MemoryStore::query_by_project(project_id, types, limit)` — 项目记忆查询
- `MemoryStore::query_overall(types, limit)` — 长期记忆查询
- `MemoryStore::update_depth(id, new_depth)` — 强化/弱化
- `MemoryStore::update_last_accessed(id)` — 访问时更新
- `MemoryStore::delete_expired(threshold)` — Janitor 删除

**Schema**:

```sql
CREATE TABLE IF NOT EXISTS memories (
    id              TEXT PRIMARY KEY,
    content         TEXT NOT NULL,
    node_type       TEXT NOT NULL,
    depth           INTEGER NOT NULL DEFAULT 0,
    project_id      TEXT,           -- NULL = 长期记忆
    language        TEXT NOT NULL DEFAULT '',
    source          TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    last_accessed   INTEGER NOT NULL,
    is_project_critical INTEGER NOT NULL DEFAULT 0,
    trace_0         REAL NOT NULL DEFAULT 0.0,
    trace_1         REAL NOT NULL DEFAULT 0.0,
    trace_2         REAL NOT NULL DEFAULT 0.0,
    trace_3         REAL NOT NULL DEFAULT 0.0,
    trace_4         REAL NOT NULL DEFAULT 0.0,
    language_weight REAL NOT NULL DEFAULT 0.5
);

CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);
CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(node_type);
CREATE INDEX IF NOT EXISTS idx_memories_accessed ON memories(last_accessed);
```

### 1.3 写入策略 (WriteBuffer)

**文件**: `crates/ox-core/src/memory/write_buffer.rs`

- `WriteBuffer::buffer(node)` — 低优先级 (Fact) 缓冲，满 10 条或 5s 超时刷新
- `WriteBuffer::write_immediate(node)` — 高优先级 (Style/Architectural/AntiPattern) 立即提交
- `WriteBuffer::flush()` — 批量事务写入

### 1.4 记忆提取

**文件**: `crates/ox-core/src/memory/extractor.rs`

每轮对话结束后 (`AgentToUiEvent::TurnDone`)，从 `new_messages` 中提取记忆节点：

```rust
fn extract_from_turn(messages: &[Message], project_id: &str) -> Vec<MemoryNode>
```

**提取规则** (本地启发式，零 Token):

| 触发条件 | 提取类型 | depth | 写入方式 |
|----------|---------|-------|---------|
| `file_write` 工具调用且用户未大幅修改 | Fact (文件创建/修改) | 1 | 批量 |
| `file_patch` 修改特定模式 (unsafe, TODO) | AntiPattern | 2 | 立即 |
| 连续 3 次相同代码风格选择 | Style (用户偏好) | 3 | 立即 |
| 架构相关关键词 (module, struct, trait, interface) | Architectural | 2 | 立即 |
| 业务逻辑关键词 (API, endpoint, model, schema) | Business | 2 | 立即 |

**去重**: content 相似度 > 0.85 (编辑距离 / max_len) 的不重复插入。

### 1.5 MemoryManager (门面)

**文件**: `crates/ox-core/src/memory/manager.rs`

```rust
pub struct MemoryManager {
    project_store: Option<MemoryStore>,   // ~/.ox/db/memories_<project_id>.db
    overall_store: MemoryStore,           // ~/.ox/db/memories_overall.db
    write_buffer: WriteBuffer,
    config: MemoryConfig,
}

impl MemoryManager {
    pub async fn init(runtime: &RuntimeEnvironment, config: &MemoryConfig) -> anyhow::Result<Self>;
    pub fn store(&self, node: MemoryNode);           // 根据 depth 选写入方式
    pub fn retrieve(&self, query: &str, project_id: &Option<String>, limit: usize) -> Vec<MemoryNode>;
    pub fn update_from_turn(&self, messages: &[Message], project_id: &str);  // 提取+存储
    pub fn flush(&mut self);                         // 刷新写缓冲
    pub fn run_janitor(&self);                       // 清理过期记忆
}
```

**数据库路径**:
- 项目记忆: `~/.ox/db/memories_<project_id>.db`
- 长期记忆: `~/.ox/db/memories_overall.db`

### 1.6 集成到 main.rs

- `run_app()` 中初始化 `MemoryManager`
- `AgentToUiEvent::TurnDone` 后调用 `memory.update_from_turn()`
- 退出时 `memory.flush()`

**Verify**:
- `/remember Rust project uses tokio` → 存入 SQLite → 重启 → 记忆仍在
- 对话 3 轮后 → `memories_<project_id>.db` 中有提取的 Fact 节点
- `cargo test -p ox-core memory::` 全部通过

---

## M2: 记忆检索 + 上下文注入 + 四级隔离

**Goal**: 每轮 LLM 调用前检索相关记忆，注入 ContextBuilder。

### 2.1 记忆检索

**文件**: `crates/ox-core/src/memory/retrieval.rs`

```rust
fn retrieve_memory(
    query: &str,
    project_id: &Option<String>,
    config: &MemoryConfig,
    manager: &MemoryManager,
) -> Vec<MemoryNode>
```

**流程**:
1. 项目记忆: 查询 project_store, type IN (architectural, business, style), decay_score > 0.3, limit 5
2. 长期记忆: 查询 overall_store, type IN (bestPractice, pattern, metaSkill), limit 5
3. 复合排序: `depth*0.5 + decay_score*0.3 + recency*0.2`
4. 去重 (相似度 > 0.85)

### 2.2 ContextBuilder 注入记忆

**修改**: `crates/ox-core/src/context/mod.rs`

- `build()` 新增参数 `memory_context: &str`
- 在 system prompt 之后、history 之前插入 memory 消息
- 记忆占 token budget 的 `memory_ratio` (2%)

**格式**:

```
## Memory Context
- [Architectural] Rust project uses tokio for async runtime (depth: 3)
- [Style] User prefers descriptive variable names (depth: 2)
- [Fact] Created src/api/handler.rs (depth: 1)
```

### 2.3 四级隔离模型

**文件**: `crates/ox-core/src/memory/isolation.rs`

| 级别 | 范围 | 项目隔离 | 存储位置 |
|------|------|---------|---------|
| 1 | 单会话 | 是 | 不持久化 (对话上下文自带) |
| 2 | 单项目 | 是 | `memories_<project_id>.db` |
| 3 | 团队共享 | 部分 | 共享项目 ID 的数据库 |
| 4 | 全局长期 | 否 | `memories_overall.db` |

Phase 2 只实现级别 2 + 4。级别 1 自带，级别 3 留 Phase 3。

### 2.4 会话恢复时记忆检索

**文件**: `crates/ox-core/src/memory/manager.rs`

```rust
fn retrieve_on_resume(session: &Session, project_id: &str) -> Vec<MemoryNode>
```

- 用最后 3 轮对话摘要作为 query
- 加上 TaskPlan 当前进行项
- 一次检索，注入 system prompt

### 2.5 Slash 命令

- `/remember <content>` — 手动添加 Style 记忆 (depth=3, 立即写入)
- `/forget <keyword>` — 搜索并删除匹配的记忆
- `/memory` — 显示当前项目记忆统计

**Verify**:
- 对话中提到"本项目用 React" → 下轮 LLM 调用时 Memory Context 包含此信息
- `/remember Always use TypeScript strict mode` → 下轮 System Prompt 注入
- `/memory` → 显示记忆数量和 top 记忆

---

## M3: 记忆衰减 + Janitor

**Goal**: 记忆随时间自动衰减，过期记忆被清理。

### 3.1 衰减策略

**文件**: `crates/ox-core/src/memory/decay.rs`

**核心原则**: `decay_score` 不预存，检索时实时计算 (Section 11.4)。

#### 项目记忆: DEWMA

```rust
fn calculate_project_decay(node: &MemoryNode, base_half_life: u64) -> f32 {
    let age_secs = (now() - node.last_accessed as i64).max(0);
    let age_days = age_secs as f32 / 86400.0;
    if node.is_project_critical { return 1.0; }

    let short_term = (-age_days / (base_half_life as f32 * 0.3)).exp();
    let long_term  = (-age_days / (base_half_life as f32 * 5.0)).exp();
    (0.7 * short_term + 0.3 * long_term).clamp(0.0, 1.0)
}
```

#### 长期记忆: ACT-R MCM

```rust
fn calculate_overall_decay(node: &MemoryNode, config: &LanguageDecayConfig) -> f32 {
    let t = ((now() - node.last_accessed as i64).max(0) as f32) / 86400.0;
    let traces_sum: f32 = node.traces.iter()
        .zip(config.traces.iter())
        .map(|(trace, tau)| trace * (-t / tau).exp())
        .sum();
    let base = traces_sum / config.traces.len() as f32;
    (base * node.language_weight as f32 + node.depth as f32 * 0.5).clamp(0.0, 1.0)
}
```

#### 幂律衰减 (备用)

```rust
fn power_law_decay(node: &MemoryNode, beta: f32) -> f32 {
    let age_days = ((now() - node.last_accessed as i64).max(1) as f32) / 86400.0;
    age_days.powf(-beta)
}
```

### 3.2 Janitor 清理器

**文件**: `crates/ox-core/src/memory/janitor.rs`

**运行时机**: 启动时以 `janitor_run_on_startup_prob` (默认 0.2) 概率运行。

```rust
fn run_janitor(store: &MemoryStore, config: &MemoryConfig) {
    // 1. 删除 decay_score < critical_threshold (默认 0.3) 的非关键记忆
    // 2. 限制项目记忆总数 ≤ max_nodes (默认 1000)
    // 3. 归档超限记忆到 long_term.db (记忆转化，见 M2 集成)
}
```

### 3.3 记忆强化

每次记忆被检索并出现在 Memory Context 中 → `depth += 1` (最大 10) + `last_accessed = now()`。

```rust
fn reinforce(node: &mut MemoryNode) {
    node.depth = (node.depth + 1).min(10);
    node.last_accessed = chrono::Utc::now().timestamp();
}
```

### 3.4 记忆冲突检测

**文件**: `crates/ox-core/src/memory/conflict.rs`

新记忆与已有记忆 content 相似度 > 0.85 时:
- 旧记忆 depth 更高 → 保留旧的，丢弃新的
- 新记忆 depth 更高 → 替换旧的
- depth 相同 → 保留更新的

**Verify**:
- 插入记忆 → 等待 1 天 (模拟) → 检索 → decay_score 降低
- `is_project_critical = true` → decay_score 始终 1.0
- Janitor 运行 → decay < 0.3 的被删除
- 被检索到的记忆 → depth 增长

---

## M4: 人格系统

**Goal**: PersonaVector 数值维度 + 基于反馈的演化 + Self-Prompting。

### 4.1 PersonaVector

**文件**: `crates/ox-core/src/persona/mod.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaVector {
    pub favors_safety_over_speed: f64,   // 0.0~1.0, Rust 上下文默认 0.9
    pub prefers_conciseness: f64,        // 0.0~1.0, 默认 0.8
    pub code_style_strictness: f64,     // 0.0~1.0, Rust 上下文默认 0.9
    pub forbidden_phrases: Vec<String>,  // 如 ["大概可能", "也许"]
    pub moral_priorities: Vec<String>,   // 如 ["安全性", "性能"]
    pub language: String,                // 当前语言上下文
    pub frozen: bool,                    // 冻结状态
}
```

**存储**: `~/.ox/db/persona_<language>.json`

### 4.2 语言感知默认值

**文件**: `crates/ox-core/src/persona/defaults.rs`

| 维度 | Rust | Python | Go | 默认 |
|------|------|--------|----|------|
| `favors_safety_over_speed` | 0.9 | 0.6 | 0.7 | 0.7 |
| `prefers_conciseness` | 0.8 | 0.7 | 0.8 | 0.7 |
| `code_style_strictness` | 0.9 | 0.6 | 0.8 | 0.7 |
| `forbidden_phrases` | ["大概可能"] | [] | [] | [] |
| `moral_priorities` | ["安全性","性能"] | ["可读性","简洁"] | ["简洁","性能"] | ["实用性"] |

### 4.3 Self-Prompting

**修改**: `crates/ox-core/src/context/system_prompt.rs`

在 system prompt 模板中注入 PersonaVector:

```rust
fn generate_persona_block(persona: &PersonaVector) -> String {
    format!(
        "## Persona\n\
         - Safety priority: {safety:.1} | Conciseness: {concise:.1} | Style strictness: {style:.1}\n\
         - Forbidden phrases: {forbidden}\n\
         - Value priorities: {values}",
        safety = persona.favors_safety_over_speed,
        concise = persona.prefers_conciseness,
        style = persona.code_style_strictness,
        forbidden = persona.forbidden_phrases.join(", "),
        values = persona.moral_priorities.join(", "),
    )
}
```

### 4.4 人格演化

**文件**: `crates/ox-core/src/persona/evolve.rs`

**约束**: 单次变化 ≤ `max_trait_change` (默认 0.1)

```rust
fn evolve_persona(persona: &mut PersonaVector, feedback: &FeedbackCategory) {
    if persona.frozen { return; }
    match feedback {
        FeedbackCategory::StyleMismatch => {
            // 连续 5 次 StyleMismatch 才触发
            if consecutive_style_mismatches >= 5 {
                // prefers_conciseness 微调 (±0.05)
                persona.prefers_conciseness = adjust(
                    persona.prefers_conciseness,
                    0.05,
                    persona.max_trait_change,
                );
            }
        }
        FeedbackCategory::CodeQuality => {
            // code_style_strictness 微调
        }
        // ToolFailure / MemoryIrrelevant → 不触发人格演化
        _ => {}
    }
}

fn adjust(current: f64, delta: f64, max_change: f64) -> f64 {
    let change = delta.min(max_change);
    (current + change).clamp(0.0, 1.0)
}
```

### 4.5 Slash 命令

- `/persona` — 显示当前人格向量
- `/persona freeze [--lang rust]` — 冻结 (停止演化)
- `/persona unfreeze` — 解冻
- `/persona set <key> <value>` — 手动设置维度 (如 `/persona set prefers_conciseness 0.9`)

**Verify**:
- Rust 项目 → system prompt 包含 `Safety priority: 0.9`
- `/persona` → 显示所有维度
- `/persona freeze` → 演化停止 → 反馈不改维度
- `/persona set prefers_conciseness 0.9` → 下轮生效

---

## M5: 用户反馈 + 行为规则 + DataSanitizer

**Goal**: `/feedback` 命令、反馈分类路由、行为规则执行、脱敏。

### 5.1 反馈命令

**修改**: `crates/ox-core/src/slash/mod.rs`

新增 `SlashCommand::Feedback { category: String }`:
- `/feedback good` — 记忆 depth++，可能触发 MetaSkill 转化
- `/feedback bad` — 自动分类路由 (Section 15.2)
- `/feedback unsafe` — 标记安全违规，refuses_unsafe_code 不可变

### 5.2 反馈分类路由

**文件**: `crates/ox-core/src/feedback/mod.rs`

```rust
enum FeedbackCategory {
    ToolFailure,       // 工具调用失败 → 记忆 depth--
    MemoryIrrelevant,  // 记忆不相关 → 检索权重微调
    CodeQuality,       // 代码错误 → 创建 AntiPattern
    StyleMismatch,     // 风格不符 → 人格演化 (连续 5 次触发)
    UnsafeViolation,   // 安全违规 → 安全日志
}

fn classify_negative_feedback(last_turn: &TurnSummary) -> FeedbackCategory {
    if last_turn.has_tool_errors { return ToolFailure; }
    if last_turn.used_irrelevant_memory { return MemoryIrrelevant; }
    if last_turn.code_had_errors { return CodeQuality; }
    StyleMismatch
}
```

### 5.3 隐式反馈信号

**文件**: `crates/ox-core/src/feedback/implicit.rs`

**信号 1: 代码改动检测**
- Ox `file_write` 后 30s 内用户大幅修改同一文件 → AntiPattern (depth=2)
- 检测方式: 记录 `file_write` 的时间戳和文件路径，下次 `file_read` 时对比

**信号 2: 对话放弃检测**
- 用户 Ctrl+C 中断 LLM 响应 → 标记该轮为"不满意"
- 连续 3 次中断同一类型任务 → 创建 AntiPattern

### 5.4 行为规则

**文件**: `crates/ox-core/src/behavior/mod.rs`

根据 `BehaviorRulesConfig` 和 `PersonaVector` 在 LLM 调用前注入规则:

```
## Behavior Rules
- enforce_safe_code: true → "Never suggest code that bypasses safety checks"
- enforce_lint: true → "Always run lint before declaring code complete"
- enforce_format: true → "Always format code before writing files"
- enforce_tests: true → "Always write tests for new functions"
```

**Persona 影响**:
- `code_style_strictness > 0.8` → 追加 "Strictly follow project's existing code style"
- `favors_safety_over_speed > 0.8` → 追加 "Prefer safe alternatives (e.g., checked arithmetic)"

### 5.5 DataSanitizer

**文件**: `crates/ox-core/src/safety/sanitizer.rs`

脱敏 regex 模式:

| 模式 | Regex | 替换 |
|------|-------|------|
| 手机号 | `\d{11}` | `[PHONE]` |
| 邮箱 | `[\w.+-]+@[\w-]+\.[\w.]+` | `[EMAIL]` |
| 身份证 | `\d{17}[\dXx]` | `[ID_CARD]` |
| 银行卡 | `\d{16,19}` | `[BANK_CARD]` |
| 密码变量 | `(password|passwd|pwd|secret|token|api_key)\s*[:=]\s*\S+` | `$1=[REDACTED]` |

**触发点**:
1. 记忆存储前 → content 脱敏
2. 记忆转化时 → content 脱敏
3. LLM tool_result 回传前 → 原始输出本地保留，回传内容脱敏

```rust
pub struct DataSanitizer {
    patterns: Vec<(regex::Regex, String)>,
}

impl DataSanitizer {
    pub fn new() -> Self;
    pub fn sanitize(&self, text: &str) -> String;
    pub fn should_sanitize(&self, text: &str) -> bool;
}
```

### 5.6 集成

- `main.rs` 初始化 `DataSanitizer` + `FeedbackTracker`
- `AgentToUiEvent::TurnDone` 后:
  1. `memory.update_from_turn()` — 提取记忆 (M1)
  2. `feedback.detect_implicit()` — 隐式反馈检测
  3. `sanitizer.sanitize()` — tool_result 脱敏
- `/feedback` 命令 → `feedback.classify()` → 路由到记忆/人格/安全

**Verify**:
- `/feedback good` → 最近记忆 depth++
- `/feedback bad` (工具失败) → 记忆 depth--，人格不变
- `/feedback bad` (风格不符 ×5) → 人格维度微调
- tool_result 包含 `api_key=sk-xxx` → LLM 收到 `api_key=[REDACTED]`
- 手机号在记忆中 → 存储为 `[PHONE]`

---

## 目录布局变更

Phase 2 新增文件:

```
crates/ox-core/src/
├── memory/
│   ├── mod.rs           # MemoryNode, MemoryNodeType, MemorySource
│   ├── store.rs         # MemoryStore (SQLite CRUD)
│   ├── write_buffer.rs  # WriteBuffer (批量/立即写入)
│   ├── extractor.rs     # 记忆提取 (启发式规则)
│   ├── manager.rs       # MemoryManager (门面)
│   ├── retrieval.rs     # 记忆检索 + 复合排序
│   ├── decay.rs         # DEWMA + ACT-R MCM + 幂律衰减
│   ├── janitor.rs       # Janitor 清理器
│   ├── conflict.rs      # 冲突检测与解决
│   └── isolation.rs     # 四级隔离模型
├── persona/
│   ├── mod.rs           # PersonaVector
│   ├── defaults.rs      # 语言感知默认值
│   ├── evolve.rs        # 人格演化
│   └── self_prompt.rs   # Self-Prompting 生成
├── feedback/
│   ├── mod.rs           # FeedbackCategory, classify_negative_feedback
│   └── implicit.rs      # 隐式反馈信号检测
├── behavior/
│   └── mod.rs           # 行为规则注入
└── safety/
    ├── mod.rs           # (已有) TrustManager
    └── sanitizer.rs     # DataSanitizer

~/.ox/db/
├── memories_<project_id>.db    # 项目记忆 (新增)
├── memories_overall.db         # 长期记忆 (新增)
└── cost_tracking.json          # (已有)
```

## Phase 2 接口变更

| 文件 | 变更 |
|------|------|
| `context/mod.rs` | `build()` 新增 `memory_context: &str` 参数 |
| `context/system_prompt.rs` | 注入 PersonaVector block + Behavior Rules block |
| `agent/mod.rs` | `run_agent_turn()` 后调用 `memory.update_from_turn()` |
| `config/mod.rs` | `[memory]`/`[persona]`/`[behavior_rules]` 配置开始消费 |
| `main.rs` | 初始化 MemoryManager, PersonaVector, DataSanitizer, FeedbackTracker |

## 验证计划

M1 完成后:
1. `cargo test -p ox-core memory::` — 存储层、提取器、写入策略
2. `/remember Rust uses tokio` → SQLite 中可查 → 重启后仍在

M2 完成后:
3. 3 轮对话后 LLM system prompt 中出现 `## Memory Context`
4. `/memory` 显示统计
5. `/forget tokio` → 记忆删除

M3 完成后:
6. 模拟时间流逝 → decay_score 降低 → 检索结果变化
7. Janitor 运行 → 过期记忆被清理

M4 完成后:
8. Rust 项目启动 → system prompt 包含 `Safety priority: 0.9`
9. `/persona` → 显示维度 → `/persona freeze` → 演化停止

M5 完成后:
10. `/feedback good` → 记忆 depth 增长
11. `/feedback bad` ×5 → 人格维度微调
12. tool_result 含密钥 → LLM 收到 `[REDACTED]`

全部完成后:
13. `cargo build` — 零错误
14. `cargo test` — 全部通过
15. 完整 E2E: 对话 5 轮 → `/memory` 有内容 → `/feedback good` → 下轮记忆更强 → `/persona` 显示演化
