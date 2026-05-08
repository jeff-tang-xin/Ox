# Workflow 3-Phase 简化方案实施说明

## 📋 概述

本次更新将 Spec Mode 的 6 步工作流简化为 **3 个明确阶段**，并在代码层面强制按顺序执行，防止 LLM 跳过或合并步骤。

同时优化了 **Council Mode** 和 **Free Mode** 的工作流设计，确保每种模式符合其定位：
- **Spec Mode**: 严格分阶段，需要用户确认
- **Council Mode**: 关键节点确认，辩论过程自动化
- **Free Mode**: 完全自由，无强制确认

---

## 🎯 核心改进

### 1. **简化的 3 阶段工作流**

```
Phase 1: 需求分析 + 文档生成 (Steps 1-3)
├─ Step 1: 分析需求并生成需求名
├─ Step 2: 创建 spec.md  
└─ Step 3: 创建 task.md + 等待用户确认 (/Y /N /O)

Phase 2: 代码执行 + 验证 (Steps 4-5)
├─ Step 4: 执行代码
└─ Step 5: 运行测试 + 等待用户确认 (/Y /N /O)

Phase 3: 总结归档 (Step 6)
└─ Step 6: 生成 summary.md
```

### 2. **强制步骤隔离机制**

#### A. 工具 Schema 过滤（代码层面）
- **位置**: `crates/ox-core/src/agent/mod.rs:180-207`
- **功能**: 在调用 LLM 前，根据当前步骤的白名单过滤可用的工具
- **效果**: LLM **根本看不到**不允许使用的工具，无法尝试调用

```rust
// 示例：Step 2 只允许 file_write
let allowed_tools = engine.get_allowed_tools(); // ["file_write"]
// LLM 只能看到 file_write 的 schema，其他工具完全隐藏
```

#### B. 工具执行验证（运行时）
- **位置**: `crates/ox-core/src/agent/mod.rs:444-473`
- **功能**: 即使 LLM 尝试调用未授权工具，也会被拦截
- **效果**: 双重保护，确保步骤隔离

#### C. 提示词强化
- **位置**: `crates/ox-core/src/agent/workflow.rs`
- **功能**: 每个步骤的提示词都明确标注：
  - 当前是第几步/共几步
  - 可以做什么（✅）
  - 不能做什么（❌）
  - 完成后如何响应

### 3. **用户确认命令 `/Y`、`/N`、`/O`**

#### `/Y` - Approve（同意并继续）
```
用户输入: /Y
系统行为: 
  - 检查是否有待确认的步骤
  - 推进到下一步
  - 显示新步骤信息
```

#### `/N` - Reject（拒绝并终止）
```
用户输入: /N
系统行为:
  - 检查工作流是否激活
  - 停用工作流
  - 切换到 Free Mode
```

#### `/O` - Revise（提供反馈修改）
```
用户输入: /O
系统行为:
  - 设置 pending_revision_feedback 标志
  - 提示用户输入反馈内容
  
用户下一条消息: "需要调整数据库设计"
系统行为:
  - 回退到上一个可修改的步骤
  - 保存用户的反馈为消息
  - 不触发 agent，等待用户再次确认
```

---

## 🔧 技术实现细节

### 文件修改清单

#### 1. `crates/ox-core/src/agent/workflow.rs`
**修改内容**: 重新定义 `create_spec_workflow()` 函数

**关键变化**:
- 从 6 步简化为 6 步（但组织为 3 个 Phase）
- 每个步骤添加明确的 Phase 标识（如 "Phase 1 - Step 1/3"）
- 需要确认的步骤明确要求 LLM 输出特定格式
- 强调 "CRITICAL: After outputting this message, DO NOT call any more tools"

**示例提示词**:
```
## PHASE 1 - STEP 3/3: Create Task Plan

**After creating task.md:**
Respond with EXACTLY this message:
```
✅ Phase 1 Complete!

Files created:
- .ox/{requirement_name}/spec.md
- .ox/{requirement_name}/task.md

Please review the documents and confirm:
/Y - Approve and proceed to Phase 2 (Code Execution)
/N - Reject and abort workflow
/O - Provide feedback for revision
```

**CRITICAL:** After outputting this message, DO NOT call any more tools. Wait for user's /Y, /N, or /O command.
```

#### 2. `crates/ox-core/src/agent/engine.rs`
**新增方法**:
- `get_allowed_tools()` - 获取当前步骤允许的工具列表
- `get_current_step_index()` - 获取当前步骤索引
- `is_workflow_active()` - 检查工作流是否激活
- `is_current_step_waiting_confirmation()` - 检查是否在等待用户确认
- `get_current_step_info()` - 获取当前步骤显示信息

**用途**: 为主流程提供工作流状态查询和控制的 API

#### 3. `crates/ox-core/src/agent/mod.rs`
**关键修改**:
- **工具 Schema 过滤** (行 180-207): 根据当前步骤过滤 LLM 可见的工具
- **步骤推进检测** (行 284-316): 检测 `[STEP_COMPLETE]` 标记并自动推进

**代码示例**:
```rust
// Filter tool schemas based on current workflow step
let schemas = if planning_mode && iteration == 0 {
    vec![]
} else if let Some(ref engine_arc) = workflow_engine {
    let engine = engine_arc.lock().await;
    let allowed_tools = engine.get_allowed_tools();
    
    if allowed_tools.is_empty() {
        tool_schemas.clone()
    } else {
        // Filter to only include allowed tools
        tool_schemas.iter()
            .filter(|schema| {
                if let Some(name) = schema.get("name").and_then(|v| v.as_str()) {
                    allowed_tools.contains(&name.to_string())
                } else {
                    false
                }
            })
            .cloned()
            .collect()
    }
} else {
    tool_schemas.clone()
};
```

#### 4. `crates/ox-cli/src/slash/mod.rs`
**新增命令**:
- `SlashCommand::Approve` - 对应 `/Y`
- `SlashCommand::Reject` - 对应 `/N`
- `SlashCommand::Revise` - 对应 `/O`

**解析逻辑**:
```rust
"y" | "Y" => SlashCommand::Approve,
"n" | "N" => SlashCommand::Reject,
"o" | "O" => SlashCommand::Revise,
```

#### 5. `crates/ox-cli/src/main.rs`
**关键修改**:

A. **处理 `/Y`、`/N`、`/O` 命令** (行 2772-2835)
```rust
SlashCommand::Approve => {
    if engine.is_workflow_active() && engine.is_current_step_waiting_confirmation() {
        engine.advance_step()?;
        app.output.push_system("✅ Approved! Proceeding to next phase...");
    }
}
```

B. **处理用户反馈回退** (行 1783-1835)
```rust
if app.pending_revision_feedback {
    app.pending_revision_feedback = false;
    
    // Rewind workflow to previous step
    let target_step_idx = match current_step_idx {
        2 => Some(1), // From create_task back to create_spec
        4 => Some(3), // From verify_results back to execute_code
        _ => None
    };
    
    if let Some(target_idx) = target_step_idx {
        engine.go_to_step(target_idx)?;
    }
    
    continue; // Skip normal agent flow
}
```

C. **Token 使用详情显示** (行 1078-1100)
```rust
app.output.push_line(OutputLine::System(format!(
    "\n💰 Token Usage: {} prompt + {} completion = {} total | Cost: ${:.4}{}",
    usage.prompt_tokens,
    usage.completion_tokens,
    total_tokens,
    cost_this_turn,
    context_info
)));
```

D. **压缩完成通知** (行 1439-1460)
```rust
AgentToUiEvent::CompressionComplete { ... } => {
    app.output.push_line(OutputLine::System(format!(
        "\n🗜️ Context Compressed: {} messages → {} messages (saved ~{} msgs)",
        source_msg_count,
        compressed_messages.len(),
        source_msg_count.saturating_sub(compressed_messages.len())
    )));
}
```

#### 6. `crates/ox-cli/src/terminal/app.rs`
**新增字段**:
- `pending_revision_feedback: bool` - 标记用户请求修订反馈

#### 7. `crates/ox-core/src/cost/mod.rs`
**公开函数**:
- `pub fn estimate_cost()` - 允许外部调用计算成本

---

## 📊 完整工作流程示例

### 场景：用户要求实现用户认证功能

#### Phase 1: 需求分析 + 文档生成

**Step 1: 分析需求**
```
用户: "帮我实现用户认证功能"

LLM:
Requirement Name: user-auth
Task Type: Complex
Analysis: 需要实现 JWT 认证、密码加密、登录接口
[PHASE1_STEP1_COMPLETE]

系统: ✅ Step completed. Moving to: Phase 1 - Step 2/3: Create Specification
```

**Step 2: 创建 spec.md**
```
LLM: （调用 file_write 创建 .ox/user-auth/spec.md）

系统: 💰 Token Usage: 2345 prompt + 678 completion = 3023 total | Cost: $0.0126 | Context: 3 msgs (no compression)
系统: ✅ Step completed. Moving to: Phase 1 - Step 3/3: Create Task Plan
```

**Step 3: 创建 task.md**
```
LLM: （调用 file_write 创建 .ox/user-auth/task.md）

LLM:
✅ Phase 1 Complete!

Files created:
- .ox/user-auth/spec.md
- .ox/user-auth/task.md

Please review the documents and confirm:
/Y - Approve and proceed to Phase 2 (Code Execution)
/N - Reject and abort workflow
/O - Provide feedback for revision

系统: ⏸️ 等待用户确认...
```

**用户确认选项**:

**选项 A: 同意继续 (`/Y`)**
```
用户: /Y

系统: ✅ Approved! Proceeding to next phase...
系统: 📍 Now at: Phase 2 - Step 1/2: Execute Code (4/6)

LLM: 🎉 User approved Phase 1! Now execute the task plan.
     （开始执行代码...）
```

**选项 B: 拒绝终止 (`/N`)**
```
用户: /N

系统: ❌ Workflow aborted by user.
系统: 已切换到 Free mode.
```

**选项 C: 提供反馈 (`/O`)**
```
用户: /O

系统: 📝 Please provide your feedback below:
系统: (Your next message will be sent as revision feedback)

用户: "需要添加 OAuth2 支持"

系统: 📝 Feedback received. Returning to previous step for revision...
系统: 📍 Now at: Phase 1 - Step 2/3: Create Specification (2/6)

LLM: （重新读取 spec.md，添加 OAuth2 相关内容后重新写入）
```

#### Phase 2: 代码执行 + 验证

**Step 4: 执行代码**
```
LLM: （按 task.md 逐步执行）
  - 创建 auth.rs
  - 修改 main.rs 添加路由
  - 更新 Cargo.toml 添加依赖
  - 更新 task.md 进度

LLM:
✅ Phase 2 Complete!

All tasks executed and verified.

Please confirm to proceed to Phase 3 (Summary & Archival):
/Y - Approve and generate summary
/N - Reject (report issues)
/O - Request changes
```

**Step 5: 运行测试**
```
LLM: （运行 cargo test、cargo clippy 等）

LLM:
✅ All verifications passed!
Ready for Phase 3.

用户: /Y

系统: ✅ Approved! Proceeding to next phase...
系统: 📍 Now at: Phase 3: Summary & Archival (6/6)
```

#### Phase 3: 总结归档

**Step 6: 生成 summary.md**
```
LLM: （创建 .ox/user-auth/summary.md）

LLM:
🎊 Workflow Complete!

All phases completed successfully.
Documents archived in: .ox/user-auth/
- spec.md
- task.md
- summary.md

Thank you for using Ox Spec Mode!
```

---

## 🎨 UI 显示增强

### 1. Token 使用详情
每次对话后显示：
```
💰 Token Usage: 2345 prompt + 678 completion = 3023 total | Cost: $0.0126 | Context: 3 compressed + 2 recent = 5 total msgs
```

### 2. 压缩通知
异步压缩完成后显示：
```
🗜️ Context Compressed: 45 messages → 3 messages (saved ~42 msgs)
```

### 3. 步骤状态显示
状态栏实时显示当前步骤：
```
gpt-4o │ /home/user/project │ ⏳ Phase 1 - Step 2/3 | 5 msgs | $1.23/mo · 5tk today
```

---

## ✅ 解决的问题

### 问题 1: 用户确认机制缺失
**之前**: LLM 自行判断是否完成，没有强制的用户确认  
**现在**: 
- 每阶段完成后必须等待用户 `/Y`、`/N`、`/O` 命令
- 代码层面检查 `requires_user_confirmation` 标志
- 用户在确认前无法进入下一步

### 问题 2: LLM 跳过或合并步骤
**之前**: LLM 可能在第一步就做完所有事  
**现在**:
- **工具 Schema 过滤**: LLM 只能看到当前步骤允许的工具
- **工具执行验证**: 即使尝试调用未授权工具也会被拦截
- **提示词强化**: 明确告知当前步骤和限制

### 问题 3: 文件路径缺少需求名
**之前**: 直接创建 `.ox/spec.md`  
**现在**:
- Step 1 要求生成需求名（如 `user-auth`）
- 后续步骤使用 `.ox/{requirement_name}/` 路径
- 提示词中明确标注路径格式

---

## 🔍 关键技术点

### 1. 工具 Schema 过滤原理
```rust
// LLM 调用前
let allowed_tools = engine.get_allowed_tools(); // ["file_write"]

// 过滤 tool_schemas
let filtered_schemas = tool_schemas.iter()
    .filter(|schema| allowed_tools.contains(schema.name))
    .collect();

// LLM 只能看到 file_write 的 schema
provider.stream_chat(&msgs, &filtered_schemas, ...)
```

**效果**: LLM **根本不知道**有其他工具存在，无法尝试调用

### 2. 反馈回退机制
```rust
// 用户输入 /O
app.pending_revision_feedback = true;

// 用户下一条消息
if app.pending_revision_feedback {
    // 回退到上一步
    engine.go_to_step(current_step - 1)?;
    
    // 保存反馈
    session.append_message(Message::user(&feedback_text));
    
    // 不触发 agent，等待用户再次确认
    continue;
}
```

**效果**: 用户可以基于 LLM 的输出提供反馈，系统自动回退到合适的步骤让 LLM 重新执行

---

## 🚀 使用方法

### 启动 Spec Mode
```bash
/spec 实现用户认证功能
```

### 工作流中的命令
- `/Y` - 同意并继续
- `/N` - 拒绝并终止
- `/O` - 提供反馈修改

### 查看帮助
```bash
/help
```

---

## 📝 注意事项

1. **需求名规范**: Step 1 生成的需求名必须符合规范（小写字母、数字、连字符，不超过 20 字符）

2. **文件格式**: spec.md 和 task.md 必须严格遵循提示词中的模板格式

3. **确认时机**: 只有在 LLM 明确输出确认提示后，才能使用 `/Y`、`/N`、`/O` 命令

4. **反馈回退**: 使用 `/O` 后，下一条消息会被视为反馈内容，不会触发 agent 执行

---

## 🎯 总结

本次更新通过以下三个层面的改进，实现了**强制的、可控的、透明的**工作流执行：

1. **代码层面**: 工具 Schema 过滤 + 运行时验证
2. **交互层面**: `/Y`、`/N`、`/O` 用户确认命令
3. **提示词层面**: 明确的步骤标识和约束说明

这样的设计确保了：
- ✅ LLM 无法跳过或合并步骤
- ✅ 用户完全控制流程推进
- ✅ 文件路径规范化（包含需求名）
- ✅ Token 使用和上下文压缩完全透明

---

## 🔄 三种模式对比

### Spec Mode（规格模式）
**定位**: 结构化、高风险、需要审核

**工作流**:
```
Phase 1: 需求分析 + 文档生成 (Steps 1-3)
  ├─ Step 1: 分析需求并生成需求名
  ├─ Step 2: 创建 spec.md  
  └─ Step 3: 创建 task.md + ⏸️ 等待 /Y /N /O

Phase 2: 代码执行 + 验证 (Steps 4-5)
  ├─ Step 4: 执行代码
  └─ Step 5: 运行测试 + ⏸️ 等待 /Y /N /O

Phase 3: 总结归档 (Step 6)
  └─ Step 6: 生成 summary.md
```

**特点**:
- ✅ **严格分阶段**: 每步完成后必须等待用户确认
- ✅ **工具白名单**: 根据步骤限制可用工具
- ✅ **强制确认**: `/Y`、`/N`、`/O` 控制流程
- ✅ **文档驱动**: 先写 spec/task，再执行代码

**适用场景**:
- 复杂功能开发
- 架构设计
- 需要审核的代码修改

---

### Council Mode（会议模式）
**定位**: 多模型辩论、决策支持

**工作流**:
```
Step 1: 定义主题 + ⏸️ 等待 /Y /N /O
Step 2: 提案阶段 （自动推进）
Step 3: 评审阶段 （自动推进）
Step 4: 反驳阶段 （自动推进）
Step 5: 仲裁阶段 （自动推进）
Step 6: 保存记录 + ⏸️ 等待 /Y /N /O
```

**特点**:
- ⚠️ **关键节点确认**: 只在开始（Step 1）和结束（Step 6）需要确认
- ✅ **辩论自动化**: Step 2-5 自动推进，无需用户干预
- ✅ **工具限制**: 全程禁止代码修改，只允许研究工具
- ✅ **文档输出**: 生成 council_record.md 记录讨论结果

**适用场景**:
- 技术方案选型
- 架构评审
- 多角度分析问题

**为什么这样设计**:
- Step 1 需要确认：确保讨论的主题正确
- Step 2-5 不确认：辩论是 AI 之间的对话，用户只需看结果
- Step 6 需要确认：让用户决定是否保存讨论结果

---

### Free Mode（自由模式）
**定位**: 灵活交互、快速迭代

**工作流**:
```
单步骤: Free Interaction （无限制）
```

**特点**:
- ❌ **无强制确认**: 不需要 `/Y`、`/N`、`/O`
- ✅ **自然对话**: 直接说"帮我做 X"
- ✅ **随时打断**: Agent 运行时输入新消息（interjection）
- ✅ **Ctrl+C**: 中断当前操作
- ✅ **工具信任**: `/trust` 减少确认弹窗
- ✅ **完全自由**: 所有工具可用，无步骤限制

**适用场景**:
- 简单问题解答
- 代码片段生成
- 快速原型开发
- 日常编程辅助

**为什么不需要确认**:
1. ✅ 用户已经通过对话自然控制节奏
2. ✅ 可以随时打断（interjection 机制）
3. ✅ 本来就是"聊天式"交互
4. ❌ 强制确认会破坏流畅性

---

### 模式选择指南

| 场景 | 推荐模式 | 原因 |
|------|---------|------|
| 实现新功能 | Spec Mode | 需要设计和审核 |
| 修复 Bug | Free Mode | 快速直接 |
| 技术选型 | Council Mode | 需要多角度分析 |
| 重构代码 | Spec Mode | 风险高，需要规划 |
| 学习新技术 | Free Mode | 灵活探索 |
| 架构评审 | Council Mode | 需要多方意见 |
| 小优化 | Free Mode | 简单直接 |
| 大型项目 | Spec Mode | 需要详细规划 |
