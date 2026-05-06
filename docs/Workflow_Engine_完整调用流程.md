# Workflow Engine 完整调用流程梳理

## 📋 目录
1. [系统初始化](#1-系统初始化)
2. [工作流注册与激活](#2-工作流注册与激活)
3. [模式切换命令](#3-模式切换命令)
4. [主循环中的使用](#4-主循环中的使用)
5. [Agent 执行中的验证](#5-agent-执行中的验证)
6. [UI 显示更新](#6-ui-显示更新)
7. [数据流总结](#7-数据流总结)

---

## 1. 系统初始化

### 1.1 启动流程

```
main.rs:600+ (主函数入口)
    ↓
创建 Session
    ↓
Line 641: app.init_workflow_engine(&session.meta.id)
    ↓
app.rs:362-381: init_workflow_engine()
    ↓
创建 SessionState (Arc<Mutex>)
    ↓
创建 WorkflowEngine
    ↓
注册 3 个默认工作流:
  - free_workflow
  - spec_workflow  
  - council_workflow
    ↓
根据初始状态激活工作流:
  - spec_active == true → activate "spec_workflow"
  - spec_active == false → activate "free_workflow" (默认)
    ↓
存储到 app.workflow_engine: Option<Arc<Mutex<WorkflowEngine>>>
```

**关键代码位置**:
- [main.rs:641](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L641) - 初始化调用点
- [app.rs:362-381](file:///F:/rust/Ox/crates/ox-cli/src/terminal/app.rs#L362-L381) - 初始化实现
- [engine.rs:19-33](file:///F:/rust/Ox/crates/ox-core/src/agent/engine.rs#L19-L33) - WorkflowEngine::new()

---

## 2. 工作流注册与激活

### 2.1 工作流定义

在 `workflow.rs` 中定义了三个工作流工厂函数：

```rust
create_free_workflow()     // 单步，无限制
create_spec_workflow()      // 6步，代码修改受限
create_council_workflow()   // 6步，禁止代码修改
```

### 2.2 激活流程

```
engine.activate_workflow(workflow_id)
    ↓
从 workflows HashMap 中克隆工作流
    ↓
设置 current_workflow = Some(workflow)
    ↓
try_lock session_state (异步安全)
    ↓
更新 SessionState:
  - current_workflow = workflow_id
  - current_step_index = 0
  - awaiting_user_confirmation = false
    ↓
返回 Result<(), String>
```

**关键方法**:
- [engine.rs:42-57](file:///F:/rust/Ox/crates/ox-core/src/agent/engine.rs#L42-L57) - activate_workflow()
- [engine.rs:60-62](file:///F:/rust/Ox/crates/ox-core/src/agent/engine.rs#L60-L62) - current_workflow()
- [engine.rs:65-73](file:///F:/rust/Ox/crates/ox-core/src/agent/engine.rs#L65-L73) - current_step()

---

## 3. 模式切换命令

所有模式切换命令都通过 `/` slash commands 触发，在 `handle_slash_command()` 中处理。

### 3.1 `/spec on` - 激活 Spec 模式

**两种路径**:

#### A. 从文件加载 (Line ~2354-2392)
```
/spec on
    ↓
context::load_spec() 读取文件
    ↓
设置 app.spec_content 和 app.spec_active
    ↓
try_lock workflow_engine
    ↓
activate_workflow("spec_workflow")
    ↓
显示成功消息
```

#### B. Inline 内容 (Line ~2456-2501)
```
/spec <content>
    ↓
验证内容长度 >= 10
    ↓
设置 app.spec_content 和 app.spec_active
    ↓
try_lock workflow_engine
    ↓
activate_workflow("spec_workflow")
    ↓
保存到文件 (如果有 project_root)
    ↓
显示成功消息
```

**代码位置**: 
- [main.rs:2350-2392](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2350-L2392) - 文件加载
- [main.rs:2456-2501](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2456-L2501) - inline 内容

---

### 3.2 `/spec off` - 停用 Spec 模式

```
/spec off
    ↓
设置 app.spec_active = false
    ↓
检测 previous_mode (Spec/Council/None)
    ↓
设置 workflow_state = WorkflowState::Free
    ↓
try_lock workflow_engine
    ↓
activate_workflow("free_workflow")  ← 切换到自由工作流
    ↓
显示切换消息
```

**代码位置**: [main.rs:2393-2426](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2393-L2426)

---

### 3.3 `/council start` / `/council <topic>` - 激活 Council 模式

```
/council start <topic> 或 /council <topic>
    ↓
设置 workflow_state = WorkflowState::Council { step, topic }
    ↓
try_lock workflow_engine
    ↓
activate_workflow("council_workflow")
    ↓
显示激活消息
```

**代码位置**:
- [main.rs:2170-2198](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2170-L2198) - start 命令
- [main.rs:2238-2258](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2238-L2258) - inline topic

---

### 3.4 `/council stop` / `/council off` - 停用 Council 模式

```
/council stop
    ↓
检测 was_active
    ↓
设置 workflow_state = WorkflowState::Free
    ↓
try_lock workflow_engine
    ↓
activate_workflow("free_workflow")
    ↓
显示切换消息
```

**代码位置**: [main.rs:2216-2237](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2216-L2237)

---

### 3.5 `/free` - 切换到自由模式

```
/free
    ↓
检测 previous_mode (Spec/Council)
    ↓
如果已在 Free 模式，提前返回
    ↓
设置 workflow_state = WorkflowState::Free
    ↓
try_lock workflow_engine
    ↓
activate_workflow("free_workflow")
    ↓
显示切换消息
```

**代码位置**: [main.rs:2499-2522](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L2499-L2522)

---

## 4. 主循环中的使用

### 4.1 UI 显示更新

在主事件循环的 Tick 事件中定期更新工作流显示缓存：

```
Event::Tick (Line ~952)
    ↓
tick_count++
    ↓
app.update_workflow_display()  ← Line 957
    ↓
try_lock workflow_engine
    ↓
获取 current_workflow, current_step, get_progress
    ↓
更新 app.workflow_display:
  - workflow_name
  - step_num / total_steps
  - step_name
  - step_prompt
  - allows_code_modification
    ↓
渲染时使用缓存数据（避免在 render 中加锁）
```

**代码位置**:
- [main.rs:957](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L957) - 调用点
- [app.rs:389-412](file:///F:/rust/Ox/crates/ox-cli/src/terminal/app.rs#L389-L412) - 实现

---

### 4.2 Agent 执行时传递

当启动 Agent turn 时，将 workflow_engine 传递给异步任务：

```
tokio::spawn(async move {
    let workflow_engine_clone = app.workflow_engine.clone();  ← Line 765/1204/1653
        ↓
    agent::run_agent_turn(
        ...,
        workflow_engine_clone,  ← 传递给 agent
    )
})
```

**代码位置**:
- [main.rs:765](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L765) - 压缩后执行
- [main.rs:1204](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L1204) - 正常执行
- [main.rs:1653](file:///F:/rust/Ox/crates/ox-cli/src/main.rs#L1653) - 其他场景

---

## 5. Agent 执行中的验证

### 5.1 工具调用验证

在 `run_agent_turn()` 中，执行每个工具调用前进行工作流验证：

```
for tc in &tool_calls {
    ↓
if let Some(ref engine_arc) = workflow_engine {
    ↓
    lock().await workflow_engine  ← 异步锁
        ↓
    parse tool arguments
        ↓
    engine.validate_tool_call(&tc.name, &args_value)
        ↓
    检查当前步骤是否允许工具执行
        ↓
    如果不允许代码修改且是代码工具:
        - 检查文件扩展名 (.rs, .py, .js, etc.)
        - 如果是源代码文件，返回错误
        ↓
    if Err(e):
        - 记录警告日志
        - 返回 ToolResult 错误给 LLM
        - continue 跳过此工具
}
    ↓
继续执行工具...
}
```

**代码位置**: [mod.rs:364-392](file:///F:/rust/Ox/crates/ox-core/src/agent/mod.rs#L364-L392)

### 5.2 验证逻辑详情

```rust
validate_tool_call(tool_name, args):
    ↓
获取 current_step()
    ↓
检查 step.allow_tool_execution
  - false → 拒绝所有工具
    ↓
检查 step.allow_code_modification
  - false → 检查是否为代码工具
    ↓
如果是 file_write 或 file_patch:
    ↓
    提取 path 参数
        ↓
    is_source_code_file(path):
        - 检查扩展名: .rs, .py, .js, .ts, .java, .cpp, .go, etc.
            ↓
        - 是源代码 → 返回错误
        - 不是源代码 → 允许（如 .md, .txt）
```

**代码位置**: [engine.rs:232-277](file:///F:/rust/Ox/crates/ox-core/src/agent/engine.rs#L232-L277)

---

## 6. UI 显示更新

### 6.1 渲染时使用缓存

在 `render.rs` 中，直接使用 `app.workflow_display` 缓存，不在渲染时加锁：

```rust
if let Some(ref wf_info) = app.workflow_display {
    // 显示工作流进度条
    // Step X/Y [████████░░] XX%
    // 显示当前步骤名称
    // 显示是否允许代码修改
}
```

**优势**:
- 避免在渲染循环中加锁
- 提高渲染性能
- 每帧 Tick 时更新缓存即可

---

## 7. 数据流总结

### 7.1 核心数据结构

```rust
// App 结构 (app.rs)
pub struct App {
    workflow_engine: Option<Arc<tokio::sync::Mutex<WorkflowEngine>>>,
    workflow_state: WorkflowState,  // Spec/Council/Free
    workflow_display: Option<WorkflowDisplayInfo>,  // 缓存用于渲染
    spec_active: bool,  // deprecated，保留向后兼容
    // ...
}

// WorkflowEngine 结构 (engine.rs)
pub struct WorkflowEngine {
    workflows: HashMap<String, Workflow>,
    current_workflow: Option<Workflow>,
    session_state: Arc<tokio::sync::Mutex<SessionState>>,
}

// SessionState 结构 (session.rs)
pub struct SessionState {
    pub current_workflow: String,
    pub current_step_index: usize,
    pub awaiting_user_confirmation: bool,
    pub current_mode: String,
    variables: HashMap<String, String>,
}
```

### 7.2 状态流转图

```
[系统启动]
    ↓
init_workflow_engine()
    ↓
┌─────────────────────────────────────┐
│                                     │
│  Free Mode (default)                │
│  workflow: free_workflow            │
│  step: 1/1                          │
│  allow: all tools, code mod         │
│                                     │
└─────────────────────────────────────┘
    ↓ /spec on
┌─────────────────────────────────────┐
│                                     │
│  Spec Mode                          │
│  workflow: spec_workflow            │
│  steps: 1-6 (planning → execution)  │
│  allow: restricted code mod         │
│                                     │
└─────────────────────────────────────┘
    ↓ /spec off 或 /free
┌─────────────────────────────────────┐
│                                     │
│  Free Mode                          │
│  workflow: free_workflow            │
│  step: 1/1                          │
│  allow: all tools, code mod         │
│                                     │
└─────────────────────────────────────┘
    ↓ /council start
┌─────────────────────────────────────┐
│                                     │
│  Council Mode                       │
│  workflow: council_workflow         │
│  steps: 1-6 (debate phases)         │
│  allow: no code modification        │
│                                     │
└─────────────────────────────────────┘
    ↓ /council stop 或 /free
┌─────────────────────────────────────┐
│                                     │
│  Free Mode                          │
│  workflow: free_workflow            │
│  step: 1/1                          │
│  allow: all tools, code mod         │
│                                     │
└─────────────────────────────────────┘
```

### 7.3 关键 API 调用链

```
用户输入 /spec on
    ↓
handle_slash_command() [main.rs]
    ↓
app.workflow_engine.try_lock()
    ↓
engine.activate_workflow("spec_workflow")
    ↓
SessionState.current_workflow = "spec_workflow"
SessionState.current_step_index = 0
    ↓
下次 Tick 事件
    ↓
app.update_workflow_display()
    ↓
渲染时显示 "Step 1/6: Planning Phase"
    ↓
用户发送消息
    ↓
run_agent_turn(workflow_engine)
    ↓
LLM 返回工具调用
    ↓
engine.validate_tool_call("file_write", args)
    ↓
检查 current_step().allow_code_modification
    ↓
允许/拒绝执行
```

---

## 🔑 关键技术点

### 1. 异步锁策略

**问题**: 在 tokio 异步上下文中使用 `blocking_lock()` 会导致 panic

**解决**: 全部改为 `try_lock()`，失败时优雅降级

```rust
// ❌ 旧代码
let mut engine = engine_arc.blocking_lock();

// ✅ 新代码
if let Ok(mut engine) = engine_arc.try_lock() {
    // 操作 engine
} else {
    tracing::warn!("Failed to acquire lock");
}
```

### 2. 缓存优化

**问题**: 渲染时每帧加锁影响性能

**解决**: 在 Tick 事件中更新缓存，渲染时直接读取

```rust
// Tick 事件中更新
app.update_workflow_display();

// 渲染时使用
if let Some(ref wf_info) = app.workflow_display {
    // 直接使用，无需加锁
}
```

### 3. 向后兼容

保留 deprecated 字段 `spec_active`, `spec_content` 等，确保旧代码仍能工作，同时逐步迁移到新的 `workflow_state` 系统。

---

## 📊 统计信息

- **工作流数量**: 3 (free, spec, council)
- **命令处理点**: 7 (/spec on×2, /spec off, /council start×2, /council stop, /free)
- **验证调用点**: 1 (工具执行前)
- **显示更新频率**: 每 Tick 事件 (~16ms)
- **锁类型**: tokio::sync::Mutex (异步安全)
- **总修复 blocking_lock 数量**: 16 处 (main.rs: 7, engine.rs: 9)

---

## 🎯 总结

Workflow Engine 是整个多模式智能体系统的核心协调组件：

1. **统一管理**: 三种模式都通过 workflow 引擎管理，架构一致
2. **权限控制**: 基于当前步骤动态限制工具使用和代码修改
3. **异步安全**: 全部使用 try_lock()，避免阻塞异步运行时
4. **性能优化**: 缓存显示数据，避免渲染时加锁
5. **可扩展性**: 易于添加新的工作流模式

整个系统设计清晰，职责分离明确，为后续的功能扩展打下了良好基础。
