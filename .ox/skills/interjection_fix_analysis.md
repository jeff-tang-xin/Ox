# 用户介入功能问题分析与修复

## 📋 问题描述

用户反馈："现在用着感觉介入还是不友好"

## 🔍 根本原因分析

### 设计目标（技术文档要求）

根据 `docs/Ox-CLI-技术设计文档.md` Section 5.9，用户介入机制应该实现：

1. **Split-View 终端布局** - 输入区与输出区物理分离
2. **实时双向通信** - UI ↔ Agent 通过 `ui_to_agent_tx/rx` 通道
3. **自然边界注入** - 在 LLM 调用前和 tool 执行前检查用户消息
4. **紧急介入 (`!`)** - 以 `!` 开头的输入标记为 Urgent，立即处理

### 实际实现的问题

#### ❌ 问题 1：消息没有实时发送给 Agent

**原代码 (main.rs:1300-1307):**
```rust
if app.agent_running {
    interjection_buf.push(text.clone(), InterjectionPriority::Normal);
    app.output.push_line(OutputLine::System(format!(
        "(queued while agent running) {}",
        text.trim()
    )));
}
```

**问题分析：**
- 用户输入只放入本地 `interjection_buf`
- **没有通过 `ui_to_agent_tx` 通道发送给正在运行的 Agent**
- Agent 根本不知道用户输入了新消息
- 用户看到 "(queued while agent running)" 但实际上消息只是被丢弃了

#### ❌ 问题 2：只在 turn 结束后才处理

**原代码 (main.rs:940-950):**
```rust
let interjections_vec: Vec<String> = interjection_buf.drain();
if !interjections_vec.is_empty() {
    for inj_text in &interjections_vec {
        app.output.push_line(OutputLine::User(format!("(queued) {}", inj_text)));
    }
    // 只把最后一条作为新对话开始
    if let Some(last) = interjections_vec.last() {
        let user_msg = Message::user(last);
        session.append_message(user_msg)...
    }
}
```

**问题分析：**
- 用户介入要等 **整个 Agent Turn 完全结束** 才会被看到
- 如果 Agent 在执行耗时任务（编译、下载、大文件操作），用户介入毫无意义
- 违背了 "实时介入" 的设计初衷

#### ❌ 问题 3：Urgent 优先级未使用

**问题：**
- 代码中定义了 `InterjectionPriority::Urgent`
- 但用户输入时**全部标记为 Normal**
- 没有检测 `!` 前缀来区分普通和紧急介入
- Urgent 机制完全闲置

## ✅ 修复方案

### 修复 1：实时发送消息到 Agent

**新代码 (main.rs):**
```rust
if app.agent_running {
    // Send interjection to agent immediately via channel
    let priority = if text.starts_with('!') {
        InterjectionPriority::Urgent
    } else {
        InterjectionPriority::Normal
    };
    let content = text.trim_start_matches('!').to_string();
    
    if let Some(tx) = &app.ui_to_agent_tx {
        let _ = tx.send(UiToAgentEvent::Interjection(content.clone()));
    }
    
    // Also buffer locally for fallback display
    interjection_buf.push(content.clone(), priority);
    
    let prefix = if priority == InterjectionPriority::Urgent {
        "(urgent!)"
    } else {
        "(queued)"
    };
    app.output.push_line(OutputLine::System(format!(
        "{} {}", prefix, content.trim()
    )));
}
```

**改进点：**
- ✅ 立即通过 `ui_to_agent_tx` 发送消息给 Agent
- ✅ 支持 `!` 前缀标记紧急介入
- ✅ 更清晰的 UI 提示（`(urgent!)` vs `(queued)`）

### 修复 2：优化 Agent 端显示

**新代码 (agent/mod.rs):**
```rust
// Before LLM call
while let Ok(ev) = ui_rx.try_recv() {
    if let ui_event::UiToAgentEvent::Interjection(text) = ev {
        messages.push(Message::user(&text));
        let _ = ui_tx.send(AgentToUiEvent::Status(
            format!("💬 User: {}", text.trim())
        ));
    }
}

// Before tool execution
while let Ok(ev) = ui_rx.try_recv() {
    if let ui_event::UiToAgentEvent::Interjection(text) = ev {
        messages.push(Message::user(&text));
        let _ = ui_tx.send(AgentToUiEvent::Status(
            format!("💬 User (before tool): {}", text.trim())
        ));
    }
}
```

**改进点：**
- ✅ 更友好的显示格式（`💬 User:` 代替 `(interjection injected:)`）
- ✅ 清晰标识介入时机（LLM 调用前 vs tool 执行前）

## 📊 修复前后对比

| 场景 | 修复前 | 修复后 |
|------|--------|--------|
| 用户输入普通消息 | 显示 "(queued while agent running)"，消息丢失 | 立即发送给 Agent，显示 "(queued)" |
| 用户输入 `!停` | 和普通消息一样，无特殊处理 | 识别为 Urgent，显示 "(urgent!)" |
| Agent 收到介入 | 不显示或显示 "(interjection injected: ...)" | 显示 "💬 User: ..." |
| 介入生效时机 | 等整个 turn 结束 | 下一个自然边界（LLM 调用前/tool 执行前） |

## 🎯 使用示例

### 场景 1：Agent 正在执行耗时操作

```
Ox: Running tool: shell_exec (cargo build --release)
[编译进行中...]

用户输入: 等一下，先别编译，我还有个文件要改
UI 显示: (queued) 等一下，先别编译，我还有个文件要改

Agent 状态: 💬 User: 等一下，先别编译，我还有个文件要改
[当前 tool 完成后，LLM 会看到用户的消息并调整计划]
```

### 场景 2：紧急介入

```
Ox: Running tool: file_write (src/main.rs)
[写入进行中...]

用户输入: !停，不要删 migration 文件
UI 显示: (urgent!) 停，不要删 migration 文件

Agent 状态: 💬 User (before tool): 停，不要删 migration 文件
[当前 tool 完成后立即停止，LLM 重新规划]
```

## ⚠️ 已知限制

1. **不是真正的"立即中断"**
   - 介入消息在"自然边界"注入（LLM 调用前、tool 执行前）
   - 不会强制终止正在执行的 tool（避免数据不一致）
   - 这是设计决策，符合技术文档要求

2. **Urgent 优先级尚未完全实现**
   - 虽然可以标记 Urgent，但 Agent 端还没有特殊的 Urgent 处理逻辑
   - 目前 Urgent 和 Normal 的注入时机相同
   - 未来可以在 tool 执行中增加 Urgent 检查点

3. **Split-View 布局已存在但未充分利用**
   - ratatui 已有 input/output pane 分离
   - 但输入时仍然需要按 Enter 提交
   - 可以实现真正的并行输入（无需等待 Agent 完成）

## 🔧 后续改进建议

1. **实现真正的 Urgent 中断**
   - 在 tool 执行循环中定期检查 Urgent 消息
   - 如果是 Urgent，提前终止当前 tool（安全的方式）

2. **增强 UI 反馈**
   - 显示 "Agent 已收到您的介入" 确认
   - 高亮显示用户介入消息
   - 显示介入消息的影响（"Agent 正在根据您的指示调整..."）

3. **支持部分命令在 Agent 运行时执行**
   - `/plan`, `/cost`, `/help` 等只读命令
   - `/trust` 运行时生效

4. **可视化 Split-View**
   - 明确区分输入区和输出区
   - 输入区始终可用，不受 Agent 状态影响
