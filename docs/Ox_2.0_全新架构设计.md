值# Ox 项目全新架构设计文档

**版本**: 2.0  
**日期**: 2026-05-09  
**状态**: 设计中

---

## 📋 目录

1. [架构概述](#架构概述)
2. [核心组件](#核心组件)
3. [数据流设计](#数据流设计)
4. [状态机设计](#状态机设计)
5. [中间件系统](#中间件系统)
6. [工作流引擎](#工作流引擎)
7. [会话管理](#会话管理)
8. [主流程控制](#主流程控制)
9. [实施计划](#实施计划)
10. [迁移策略](#迁移策略)

---

## 架构概述

### 设计理念

Ox 2.0 采用**事件驱动 + 状态机 + 中间件管道**的三层架构：

```
┌─────────────────────────────────────────────────┐
│              UI Event Loop                       │
│         (crossterm/ratatui 事件循环)              │
└──────────────┬──────────────────────────────────┘
               │ 用户输入 / 异步事件
               ↓
┌─────────────────────────────────────────────────┐
│           Event Dispatcher                       │
│      (分发事件到 Orchestrator)                    │
└──────────────┬──────────────────────────────────┘
               │
               ↓
┌─────────────────────────────────────────────────┐
│            Orchestrator                          │
│     (主流程控制器 - Match-Driven)                │
│                                                  │
│  ┌──────────────┐  ┌──────────────────┐        │
│  │ StateMachine │  │   Workflow       │        │
│  │ (状态管理)    │  │  (流程定义)       │        │
│  └──────┬───────┘  └────────┬─────────┘        │
│         │                   │                    │
│         └───────┬───────────┘                    │
│                 ↓                                │
│      ┌────────────────────┐                     │
│      │ MiddlewarePipeline │                     │
│      │   (功能实现)        │                     │
│      └────────┬───────────┘                     │
└───────────────┼────────────────────────────────┘
                │
                ↓
┌─────────────────────────────────────────────────┐
│              Session Store                       │
│         (数据持久化层)                            │
└─────────────────────────────────────────────────┘
```

### 核心原则

1. **事件驱动**: UI 循环只负责接收事件和渲染，业务逻辑由事件处理器异步执行
2. **状态机管理**: 用 `match` 表达式驱动状态转换，类型安全且清晰
3. **中间件可插拔**: 所有功能通过中间件实现，可动态注册/卸载
4. **职责分离**: Session（数据）、StateMachine（状态）、Workflow（流程）、Middleware（功能）各司其职
5. **Match-Driven**: 用 Rust 的模式匹配代替复杂的转换表，编译时检查

---

## 核心组件

### 1. Session（会话）- 数据容器

**职责**: 纯粹的数据存储，不包含任何业务逻辑

**文件**: `crates/ox-core/src/session/mod.rs`

```rust
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// 会话模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    Free,      // 自由模式
    Spec,      // 规格模式
    Council,   // 议会模式
}

/// 会话元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub mode: SessionMode,
    pub project_id: String,
    pub created_at: Instant,
    pub last_active: Instant,
}

/// 会话（纯数据容器）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    pub context: serde_json::Value,  // 附加上下文
}

impl Session {
    pub fn new(id: String, mode: SessionMode, project_id: String) -> Self {
        Self {
            meta: SessionMeta {
                id,
                mode,
                project_id,
                created_at: Instant::now(),
                last_active: Instant::now(),
            },
            messages: Vec::new(),
            context: serde_json::Value::Null,
        }
    }
    
    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.meta.last_active = Instant::now();
    }
}
```

**关键点**:
- ✅ 只存储数据
- ✅ 不提供业务逻辑
- ✅ 可序列化，支持持久化

---

### 2. StateMachine（状态机）- 状态管理

**职责**: 管理系统状态转换，决定"现在应该做什么"

**文件**: `crates/ox-core/src/state_machine/mod.rs`

```rust
use anyhow::Result;

/// 系统状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemState {
    /// 等待用户输入
    WaitingForInput,
    
    /// 正在执行前置中间件
    ProcessingPreLLM {
        user_input: String,
    },
    
    /// 正在调用 LLM
    CallingLLM {
        prompt: String,
    },
    
    /// 正在执行后置中间件
    ProcessingPostLLM {
        llm_response: String,
    },
    
    /// 等待用户确认（Workflow 步骤完成）
    AwaitingConfirmation {
        step_name: String,
        summary: String,
    },
    
    /// 工作流完成
    WorkflowComplete,
    
    /// 错误状态
    Error {
        message: String,
        recoverable: bool,
    },
}

/// 触发事件
#[derive(Debug, Clone)]
pub enum StateEvent {
    /// 用户输入文本
    UserInput(String),
    
    /// 用户确认操作
    UserConfirmation(ConfirmationAction),
    
    /// 前置中间件完成
    PreLLMCompleted {
        prompt: String,
    },
    
    /// LLM 调用完成
    LLMCallCompleted {
        response: String,
    },
    
    /// 后置中间件完成
    PostLLMCompleted,
    
    /// Workflow 步骤完成（需要确认）
    WorkflowStepCompleted {
        step_name: String,
        summary: String,
    },
    
    /// Workflow 步骤未完成（继续）
    WorkflowStepContinued,
    
    /// 发生错误
    ErrorOccurred {
        message: String,
        recoverable: bool,
    },
}

#[derive(Debug, Clone)]
pub enum ConfirmationAction {
    Accept,   // /Y - 接受
    Reject,   // /N - 拒绝
    Override, // /O - 覆盖
}

/// 状态机（使用 match 驱动）
pub struct StateMachine {
    current_state: SystemState,
}

impl StateMachine {
    pub fn new(initial_state: SystemState) -> Self {
        Self {
            current_state: initial_state,
        }
    }
    
    /// 处理事件，返回新状态（✅ Match-Driven）
    pub fn handle_event(&mut self, event: StateEvent) -> Result<SystemState> {
        let old_state = self.current_state.clone();
        
        let new_state = match (&old_state, event) {
            // WaitingForInput → ProcessingPreLLM
            (SystemState::WaitingForInput, StateEvent::UserInput(input)) => {
                SystemState::ProcessingPreLLM { user_input: input }
            }
            
            // ProcessingPreLLM → CallingLLM
            (SystemState::ProcessingPreLLM { .. }, StateEvent::PreLLMCompleted { prompt }) => {
                SystemState::CallingLLM { prompt }
            }
            
            // CallingLLM → ProcessingPostLLM
            (SystemState::CallingLLM { .. }, StateEvent::LLMCallCompleted { response }) => {
                SystemState::ProcessingPostLLM { llm_response: response }
            }
            
            // ProcessingPostLLM → AwaitingConfirmation (需要确认)
            (SystemState::ProcessingPostLLM { .. }, StateEvent::WorkflowStepCompleted { step_name, summary }) => {
                SystemState::AwaitingConfirmation { step_name, summary }
            }
            
            // ProcessingPostLLM → WaitingForInput (无需确认)
            (SystemState::ProcessingPostLLM { .. }, StateEvent::WorkflowStepContinued) => {
                SystemState::WaitingForInput
            }
            
            // AwaitingConfirmation → WaitingForInput
            (SystemState::AwaitingConfirmation { .. }, StateEvent::UserConfirmation(_)) => {
                SystemState::WaitingForInput
            }
            
            // Error (recoverable) → WaitingForInput
            (SystemState::Error { recoverable: true, .. }, StateEvent::UserInput(_)) => {
                SystemState::WaitingForInput
            }
            
            // 无效转换
            (state, event) => {
                return Err(anyhow::anyhow!(
                    "Invalid state transition: {:?} with event {:?}",
                    state,
                    event
                ));
            }
        };
        
        self.current_state = new_state.clone();
        Ok(new_state)
    }
    
    /// 查询方法
    pub fn current_state(&self) -> &SystemState {
        &self.current_state
    }
    
    pub fn can_accept_input(&self) -> bool {
        matches!(
            self.current_state,
            SystemState::WaitingForInput | SystemState::AwaitingConfirmation { .. }
        )
    }
    
    pub fn get_pending_input(&self) -> Option<String> {
        if let SystemState::ProcessingPreLLM { user_input } = &self.current_state {
            Some(user_input.clone())
        } else {
            None
        }
    }
    
    pub fn get_llm_response(&self) -> Option<String> {
        if let SystemState::ProcessingPostLLM { llm_response } = &self.current_state {
            Some(llm_response.clone())
        } else {
            None
        }
    }
}
```

**关键点**:
- ✅ 用 `match` 表达式处理状态转换
- ✅ 编译时检查所有分支
- ✅ 类型安全，携带状态数据
- ✅ 清晰的转换逻辑

---

### 3. Workflow（工作流）- 业务流程

**职责**: 定义业务步骤，管理流程推进

**文件**: `crates/ox-core/src/workflow/mod.rs`

```rust
use serde::{Deserialize, Serialize};

/// 工作流步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    pub description: String,
    pub requires_confirmation: bool,
    pub expected_outputs: Vec<String>,  // 预期的输出标记
}

/// 工作流阶段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowPhase {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

/// 工作流定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    pub phases: Vec<WorkflowPhase>,
}

/// 工作流运行时状态
pub struct Workflow {
    definition: WorkflowDefinition,
    current_phase_index: usize,
    current_step_index: usize,
    completed_steps: Vec<String>,
}

impl Workflow {
    pub fn new(definition: WorkflowDefinition) -> Self {
        Self {
            definition,
            current_phase_index: 0,
            current_step_index: 0,
            completed_steps: Vec::new(),
        }
    }
    
    /// 获取当前步骤
    pub fn current_step(&self) -> Option<&WorkflowStep> {
        self.definition.phases.get(self.current_phase_index)?
            .steps.get(self.current_step_index)
    }
    
    /// 检查当前步骤是否完成
    pub fn is_step_complete(&self, llm_response: &str) -> bool {
        if let Some(step) = self.current_step() {
            step.expected_outputs.iter().any(|marker| {
                llm_response.contains(marker)
            })
        } else {
            false
        }
    }
    
    /// 推进到下一步
    pub fn advance_step(&mut self) -> bool {
        let current_phase = &self.definition.phases[self.current_phase_index];
        
        if self.current_step_index + 1 < current_phase.steps.len() {
            // 同一 Phase 内的下一步
            if let Some(step) = self.current_step() {
                self.completed_steps.push(step.name.clone());
            }
            self.current_step_index += 1;
            true
        } else if self.current_phase_index + 1 < self.definition.phases.len() {
            // 下一个 Phase
            if let Some(step) = self.current_step() {
                self.completed_steps.push(step.name.clone());
            }
            self.current_phase_index += 1;
            self.current_step_index = 0;
            true
        } else {
            // 工作流完成
            if let Some(step) = self.current_step() {
                self.completed_steps.push(step.name.clone());
            }
            false
        }
    }
    
    pub fn is_complete(&self) -> bool {
        self.current_phase_index >= self.definition.phases.len()
    }
}

/// 工作流工厂
pub struct WorkflowFactory;

impl WorkflowFactory {
    pub fn create_spec_workflow() -> Workflow {
        let definition = WorkflowDefinition {
            name: "spec".to_string(),
            phases: vec![
                WorkflowPhase {
                    name: "phase_1_documentation".to_string(),
                    steps: vec![
                        WorkflowStep {
                            name: "generate_requirement_name".to_string(),
                            description: "生成需求名称".to_string(),
                            requires_confirmation: true,
                            expected_outputs: vec!["[STEP_COMPLETE]".to_string()],
                        },
                        WorkflowStep {
                            name: "create_spec".to_string(),
                            description: "创建规格文档".to_string(),
                            requires_confirmation: true,
                            expected_outputs: vec!["[STEP_COMPLETE]".to_string()],
                        },
                    ],
                },
                WorkflowPhase {
                    name: "phase_2_planning".to_string(),
                    steps: vec![
                        WorkflowStep {
                            name: "create_task".to_string(),
                            description: "创建任务计划".to_string(),
                            requires_confirmation: true,
                            expected_outputs: vec!["[STEP_COMPLETE]".to_string()],
                        },
                    ],
                },
            ],
        };
        
        Workflow::new(definition)
    }
    
    pub fn create_free_workflow() -> Option<Workflow> {
        None  // 自由模式没有固定工作流
    }
}
```

**关键点**:
- ✅ 声明式定义流程
- ✅ 支持多阶段、多步骤
- ✅ 可配置是否需要确认
- ✅ 通过标记判断步骤完成

---

### 4. Middleware（中间件）- 功能实现

**职责**: 执行具体功能，可插拔、可组合

**文件**: `crates/ox-core/src/middleware/mod.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// 中间件执行阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiddlewarePhase {
    PreLLM,   // LLM 调用前
    PostLLM,  // LLM 调用后
}

/// 中间件上下文
#[derive(Debug, Clone)]
pub struct MiddlewareContext {
    pub user_input: String,
    pub llm_response: Option<String>,
    pub session_id: String,
    pub project_id: String,
    pub shared_state: Arc<tokio::sync::RwLock<serde_json::Map<String, serde_json::Value>>>,
    pub metadata: MiddlewareMetadata,
}

#[derive(Debug, Clone, Default)]
pub struct MiddlewareMetadata {
    pub executed_middlewares: Vec<ExecutedMiddleware>,
}

#[derive(Debug, Clone)]
pub struct ExecutedMiddleware {
    pub name: String,
    pub phase: MiddlewarePhase,
    pub order: u32,
    pub duration_ms: u64,
    pub success: bool,
}

/// 中间件 trait
#[async_trait]
pub trait Middleware: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str { self.id() }
    fn phase(&self) -> MiddlewarePhase;
    fn order(&self) -> u32;
    
    fn should_execute(&self, _ctx: &MiddlewareContext) -> bool {
        true
    }
    
    async fn execute(&self, ctx: &mut MiddlewareContext) -> Result<()>;
}

/// 中间件管道
pub struct MiddlewarePipeline {
    pre_llm_middlewares: Vec<Arc<dyn Middleware>>,
    post_llm_middlewares: Vec<Arc<dyn Middleware>>,
}

impl MiddlewarePipeline {
    pub fn new() -> Self {
        Self {
            pre_llm_middlewares: Vec::new(),
            post_llm_middlewares: Vec::new(),
        }
    }
    
    /// 注册中间件（自动按 order 排序）
    pub fn register(&mut self, middleware: Arc<dyn Middleware>) {
        match middleware.phase() {
            MiddlewarePhase::PreLLM => {
                self.pre_llm_middlewares.push(middleware);
                self.pre_llm_middlewares.sort_by_key(|m| m.order());
            }
            MiddlewarePhase::PostLLM => {
                self.post_llm_middlewares.push(middleware);
                self.post_llm_middlewares.sort_by_key(|m| m.order());
            }
        }
    }
    
    /// 执行前置管道
    pub async fn execute_pre_llm(&self, ctx: &mut MiddlewareContext) -> Result<()> {
        for middleware in &self.pre_llm_middlewares {
            if middleware.should_execute(ctx) {
                let start = std::time::Instant::now();
                middleware.execute(ctx).await?;
                let duration = start.elapsed().as_millis() as u64;
                
                ctx.metadata.executed_middlewares.push(ExecutedMiddleware {
                    name: middleware.name().to_string(),
                    phase: MiddlewarePhase::PreLLM,
                    order: middleware.order(),
                    duration_ms: duration,
                    success: true,
                });
            }
        }
        Ok(())
    }
    
    /// 执行后置管道
    pub async fn execute_post_llm(&self, ctx: &mut MiddlewareContext) -> Result<()> {
        for middleware in &self.post_llm_middlewares {
            if middleware.should_execute(ctx) {
                let start = std::time::Instant::now();
                middleware.execute(ctx).await?;
                let duration = start.elapsed().as_millis() as u64;
                
                ctx.metadata.executed_middlewares.push(ExecutedMiddleware {
                    name: middleware.name().to_string(),
                    phase: MiddlewarePhase::PostLLM,
                    order: middleware.order(),
                    duration_ms: duration,
                    success: true,
                });
            }
        }
        Ok(())
    }
}
```

#### 中间件示例

```rust
// 记忆检索中间件
pub struct MemoryRetrievalMiddleware {
    memory: Arc<MemoryManager>,
}

#[async_trait]
impl Middleware for MemoryRetrievalMiddleware {
    fn id(&self) -> &str { "memory_retrieval" }
    fn phase(&self) -> MiddlewarePhase { MiddlewarePhase::PreLLM }
    fn order(&self) -> u32 { 10 }
    
    async fn execute(&self, ctx: &mut MiddlewareContext) -> Result<()> {
        let nodes = self.memory.retrieve(&ctx.user_input, &Some(&ctx.project_id), 5);
        let memory_context = self.memory.format_memory_context(&nodes, false);
        
        let mut state = ctx.shared_state.write().await;
        state.insert("memory_context".to_string(), serde_json::Value::String(memory_context));
        
        Ok(())
    }
}

// 关键词提取中间件
pub struct KeywordExtractionMiddleware {
    memory: Arc<MemoryManager>,
}

#[async_trait]
impl Middleware for KeywordExtractionMiddleware {
    fn id(&self) -> &str { "keyword_extraction" }
    fn phase(&self) -> MiddlewarePhase { MiddlewarePhase::PostLLM }
    fn order(&self) -> u32 { 40 }
    
    fn should_execute(&self, ctx: &MiddlewareContext) -> bool {
        ctx.llm_response.is_some()
    }
    
    async fn execute(&self, ctx: &mut MiddlewareContext) -> Result<()> {
        if let Some(ref response) = ctx.llm_response {
            if let Some(extracted) = extract_keywords_from_response(response) {
                self.memory.record_llm_keywords(&ctx.user_input, extracted);
            }
        }
        Ok(())
    }
}
```

**关键点**:
- ✅ 分为 PreLLM 和 PostLLM 两个阶段
- ✅ 每个中间件有 order 优先级
- ✅ 支持条件执行（`should_execute`）
- ✅ 通过 `shared_state` 传递数据

---

### 5. Orchestrator（主流程控制器）- 总指挥

**职责**: 协调所有组件，用 match 驱动主流程

**文件**: `crates/ox-core/src/orchestrator/mod.rs`

```rust
use crate::state_machine::{StateMachine, SystemState, StateEvent};
use crate::session::Session;
use crate::workflow::Workflow;
use crate::middleware::MiddlewarePipeline;
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};
use anyhow::Result;

pub struct Orchestrator {
    session: Arc<RwLock<Session>>,
    workflow: Option<Arc<Mutex<Workflow>>>,
    state_machine: Arc<Mutex<StateMachine>>,
    middleware_pipeline: Arc<MiddlewarePipeline>,
}

impl Orchestrator {
    pub async fn process_event(&self, event: StateEvent) -> Result<()> {
        // 1. 更新状态机
        let new_state = {
            let mut sm = self.state_machine.lock().await;
            sm.handle_event(event)?
        };
        
        // 2. ✅ Match-Driven: 根据新状态执行相应逻辑
        match new_state {
            SystemState::ProcessingPreLLM { user_input } => {
                self.execute_pre_llm(&user_input).await?;
            }
            
            SystemState::CallingLLM { prompt } => {
                self.call_llm(&prompt).await?;
            }
            
            SystemState::ProcessingPostLLM { llm_response } => {
                self.execute_post_llm(&llm_response).await?;
            }
            
            SystemState::AwaitingConfirmation { step_name, summary } => {
                self.show_confirmation_prompt(&step_name, &summary).await?;
            }
            
            SystemState::Error { message, recoverable } => {
                self.handle_error(&message, recoverable).await?;
            }
            
            _ => {
                tracing::debug!("[ORCHESTRATOR] No action needed for state: {:?}", new_state);
            }
        }
        
        Ok(())
    }
    
    async fn execute_pre_llm(&self, user_input: &str) -> Result<()> {
        let mut ctx = self.build_context(user_input).await?;
        self.middleware_pipeline.execute_pre_llm(&mut ctx).await?;
        
        let prompt = ctx.shared_state.read().await
            .get("prompt")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        
        // 触发下一个状态
        self.process_event(StateEvent::PreLLMCompleted { prompt }).await?;
        Ok(())
    }
    
    async fn call_llm(&self, prompt: &str) -> Result<()> {
        let response = self.call_llm_streaming(prompt).await?;
        
        // 保存响应到 Session
        {
            let mut session = self.session.write().await;
            session.add_message(Message::assistant(&response));
        }
        
        // 触发下一个状态
        self.process_event(StateEvent::LLMCallCompleted { response }).await?;
        Ok(())
    }
    
    async fn execute_post_llm(&self, llm_response: &str) -> Result<()> {
        let mut ctx = self.build_context("").await?;
        ctx.llm_response = Some(llm_response.to_string());
        self.middleware_pipeline.execute_post_llm(&mut ctx).await?;
        
        // 检查工作流
        if let Some(ref workflow_arc) = self.workflow {
            let mut workflow = workflow_arc.lock().await;
            
            if workflow.is_step_complete(llm_response) {
                let step_name = workflow.current_step().unwrap().name.clone();
                let summary = self.generate_summary(llm_response);
                
                self.process_event(StateEvent::WorkflowStepCompleted {
                    step_name,
                    summary,
                }).await?;
            } else {
                self.process_event(StateEvent::WorkflowStepContinued).await?;
            }
        } else {
            self.process_event(StateEvent::WorkflowStepContinued).await?;
        }
        
        Ok(())
    }
    
    // ... 其他辅助方法
}
```

**关键点**:
- ✅ 用 `match` 驱动主流程
- ✅ 每个状态对应一个处理方法
- ✅ 递归调用 `process_event` 触发下一个状态
- ✅ 清晰的执行流程

---

## 数据流设计

### 完整的请求-响应流程

```
用户输入 "帮我设计一个订单系统"
    ↓
┌─────────────────────────────────────────┐
│ 1. UI Event Loop                         │
│    event::read() → KeyCode::Enter       │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 2. App.dispatch_user_input()            │
│    tokio::spawn(async {                 │
│        orchestrator.process_event(      │
│            StateEvent::UserInput(...)   │
│        )                                │
│    })                                   │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 3. StateMachine.handle_event()          │
│    match (&state, event) {              │
│        (WaitingForInput, UserInput)     │
│        → ProcessingPreLLM               │
│    }                                    │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 4. Orchestrator (match new_state)       │
│    ProcessingPreLLM → execute_pre_llm() │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 5. MiddlewarePipeline.execute_pre_llm() │
│    ├─ MemoryRetrieval (order=10)        │
│    ├─ PromptBuilder (order=20)          │
│    └─ ...                               │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 6. StateEvent::PreLLMCompleted          │
│    → StateMachine.handle_event()        │
│    → CallingLLM                         │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 7. Orchestrator.call_llm()              │
│    call_llm_streaming(prompt).await     │
│    (流式响应，实时更新 UI)               │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 8. StateEvent::LLMCallCompleted         │
│    → StateMachine.handle_event()        │
│    → ProcessingPostLLM                  │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 9. MiddlewarePipeline.execute_post_llm()│
│    ├─ KeywordExtractor (order=40)       │
│    ├─ ImplicitFeedback (order=50)       │
│    └─ ...                               │
└────────────┬────────────────────────────┘
             ↓
┌─────────────────────────────────────────┐
│ 10. 检查工作流                           │
│     if workflow.is_step_complete() {    │
│         → AwaitingConfirmation          │
│     } else {                            │
│         → WaitingForInput               │
│     }                                   │
└─────────────────────────────────────────┘
```

---

## 状态机设计

### 状态转换图

```
WaitingForInput
  ├─ UserInput → ProcessingPreLLM
  
ProcessingPreLLM
  ├─ PreLLMCompleted → CallingLLM
  └─ ErrorOccurred → Error
  
CallingLLM
  ├─ LLMCallCompleted → ProcessingPostLLM
  └─ ErrorOccurred → Error
  
ProcessingPostLLM
  ├─ WorkflowStepCompleted → AwaitingConfirmation
  ├─ WorkflowStepContinued → WaitingForInput
  └─ ErrorOccurred → Error
  
AwaitingConfirmation
  ├─ UserConfirmation(Accept) → WaitingForInput
  ├─ UserConfirmation(Reject) → WaitingForInput
  └─ UserConfirmation(Override) → WaitingForInput
  
Error (recoverable)
  └─ UserInput → WaitingForInput
  
Error (unrecoverable)
  └─ (任何事件) → Error (保持不变)
```

### Match 表达式示例

```rust
let new_state = match (&old_state, event) {
    (SystemState::WaitingForInput, StateEvent::UserInput(input)) => {
        SystemState::ProcessingPreLLM { user_input: input }
    }
    
    (SystemState::ProcessingPreLLM { .. }, StateEvent::PreLLMCompleted { prompt }) => {
        SystemState::CallingLLM { prompt }
    }
    
    (SystemState::CallingLLM { .. }, StateEvent::LLMCallCompleted { response }) => {
        SystemState::ProcessingPostLLM { llm_response: response }
    }
    
    // ... 更多转换
    
    (state, event) => {
        return Err(anyhow::anyhow!("Invalid transition"));
    }
};
```

---

## 中间件系统

### 中间件分类

#### 前置中间件（PreLLM）

| 中间件 | Order | 功能 |
|--------|-------|------|
| InputValidator | 1 | 验证用户输入 |
| RateLimiter | 5 | 限流控制 |
| MemoryRetrieval | 10 | 检索相关记忆 |
| ContextEnricher | 15 | 注入文件上下文 |
| PromptBuilder | 20 | 构建 System Prompt |
| RequestLogger | 25 | 记录请求日志 |

#### 后置中间件（PostLLM）

| 中间件 | Order | 功能 |
|--------|-------|------|
| ResponseParser | 30 | 解析 LLM 响应 |
| ResponseLogger | 35 | 记录响应日志 |
| KeywordExtractor | 40 | 提取关键词 |
| ImplicitFeedback | 45 | 检测隐式反馈 |
| SatisfactionTracker | 50 | 追踪满意度 |
| MemoryUpdater | 55 | 更新记忆系统 |

### 中间件注册

```rust
let mut pipeline = MiddlewarePipeline::new();

// 前置中间件
pipeline.register(Arc::new(MemoryRetrievalMiddleware::new(memory.clone())));
pipeline.register(Arc::new(PromptBuilderMiddleware));

// 后置中间件
pipeline.register(Arc::new(KeywordExtractionMiddleware::new(memory.clone())));
pipeline.register(Arc::new(ImplicitFeedbackMiddleware::new(detector.clone())));
```

---

## 工作流引擎

### 工作流模式

#### Free Mode（自由模式）
- 无固定工作流
- 直接对话，无步骤限制
- `workflow = None`

#### Spec Mode（规格模式）
- Phase 1: Documentation
  - Step 1: Generate requirement name
  - Step 2: Create spec.md
- Phase 2: Planning
  - Step 1: Create task.md
- 每步完成后需要用户确认

#### Council Mode（议会模式）
- 多专家讨论
- 生成 council_record.md
- 需要用户确认

### 工作流推进

```rust
// 检查步骤是否完成
if workflow.is_step_complete(&llm_response) {
    // 包含 [STEP_COMPLETE] 标记
    workflow.advance_step();
    
    // 触发确认状态
    StateEvent::WorkflowStepCompleted { ... }
} else {
    // 继续当前步骤
    StateEvent::WorkflowStepContinued
}
```

---

## 会话管理

### Session 存储

```rust
// 内存中
let session = Arc::new(RwLock::new(Session::new(...)));

// 持久化到磁盘
session_store.save(&session).await?;

// 从磁盘加载
let session = session_store.load(session_id).await?;
```

### 会话隔离

- **Free Mode**: 共享全局记忆
- **Spec Mode**: 每个需求独立会话
- **Council Mode**: 每个议题独立会话

---

## 主流程控制

### UI 事件循环

```rust
fn main() -> Result<()> {
    let runtime = RuntimeContext::new(...);
    let mut app = App::new(runtime.clone());
    
    loop {
        // 1. 渲染
        terminal.draw(|f| app.render(f))?;
        
        // 2. 等待事件（非阻塞）
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter => {
                        let input = app.get_input();
                        app.clear_input();
                        app.dispatch_user_input(input);
                    }
                    KeyCode::Esc => break,
                    _ => {}
                }
            }
        }
        
        // 3. 处理异步事件
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_async_event(event)?;
        }
    }
    
    Ok(())
}
```

### 事件分发

```rust
impl App {
    pub fn dispatch_user_input(&self, input: String) {
        let orchestrator = self.runtime.orchestrator.clone();
        
        tokio::spawn(async move {
            let event = StateEvent::UserInput(input);
            if let Err(e) = orchestrator.process_event(event).await {
                tracing::error!("Failed to process input: {}", e);
            }
        });
    }
}
```

---

## 实施计划

### Phase 1: 基础设施（3 天）

- [ ] 创建 `state_machine/mod.rs`
- [ ] 创建 `workflow/mod.rs`
- [ ] 创建 `middleware/mod.rs`
- [ ] 创建 `orchestrator/mod.rs`
- [ ] 编写单元测试

### Phase 2: 中间件迁移（5 天）

- [ ] MemoryRetrievalMiddleware
- [ ] PromptBuilderMiddleware
- [ ] KeywordExtractionMiddleware
- [ ] ImplicitFeedbackMiddleware
- [ ] SatisfactionTrackerMiddleware

### Phase 3: 主流程重构（4 天）

- [ ] 修改 `main.rs` 使用新架构
- [ ] 移除硬编码逻辑
- [ ] 集成 UI 事件循环
- [ ] 测试完整流程

### Phase 4: 测试优化（3 天）

- [ ] 集成测试
- [ ] 性能测试
- [ ] 文档完善

**总计**: 约 15 个工作日

---

## 迁移策略

### 渐进式迁移

1. **保留旧代码**: 新功能使用新架构，旧功能暂时保留
2. **双轨运行**: 通过配置开关切换新旧架构
3. **逐步替换**: 逐个模块迁移，确保每一步都可回退
4. **完全切换**: 所有模块迁移完成后，删除旧代码

### 兼容性保证

- API 接口保持不变
- 配置文件向后兼容
- 数据格式不变

---

## 优势总结

### 相比旧架构的优势

1. **职责清晰**: Session、StateMachine、Workflow、Middleware 各司其职
2. **类型安全**: 用 `match` 表达式，编译时检查所有分支
3. **易于扩展**: 新增功能只需添加中间件或状态分支
4. **易于测试**: 每个组件可独立测试
5. **事件驱动**: UI 不阻塞，异步处理 LLM 调用
6. **可插拔**: 中间件可动态注册/卸载
7. **Match-Driven**: 代码即文档，清晰易懂

### 性能优势

- `match` 比 HashMap 查找更快
- 异步处理，UI 响应流畅
- 中间件按需执行，避免浪费

---

## 附录

### 关键文件清单

```
crates/ox-core/src/
├── session/
│   └── mod.rs              # Session 定义
├── state_machine/
│   └── mod.rs              # StateMachine 实现
├── workflow/
│   └── mod.rs              # Workflow 引擎
├── middleware/
│   ├── mod.rs              # Middleware 接口
│   ├── memory_retrieval.rs # 记忆检索中间件
│   ├── prompt_builder.rs   # Prompt 构建中间件
│   ├── keyword_extraction.rs # 关键词提取中间件
│   └── implicit_feedback.rs # 隐式反馈中间件
├── orchestrator/
│   └── mod.rs              # 主流程控制器
└── lib.rs                  # 导出公共 API

crates/ox-cli/src/
├── main.rs                 # UI 事件循环
└── app.rs                  # App 状态和事件处理
```

### 配置示例

```toml
# config.toml

[middleware]
enabled_middlewares = [
    "memory_retrieval",
    "prompt_builder",
    "keyword_extraction",
    "implicit_feedback",
]

[workflow]
default_mode = "free"  # free, spec, council

[state_machine]
stop_on_error = false
max_retries = 3
```

---

**文档结束**
