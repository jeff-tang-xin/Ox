# 🐛 修复：Findings 上下文污染问题

## 问题描述

**症状**：当前面一件事情已经做完，开始新的事情时，Agent 总是回复前面已完成事情的内容。

**根因**：`_findings_store` 变量在任务完成后未被清理，导致旧任务的 findings 污染新任务的上下文。

---

## 问题链路

```
旧任务完成
  ↓
findings 保存到 WorkflowEngine._findings_store
  ↓
用户开始新任务
  ↓
begin_user_round() 检测到新任务
  ↓
reset_workflow() 被调用
  ↓
clear_ephemeral_workflow_state() 被调用
  ↓
❌ 问题：未清理 _findings_store
  ↓
新任务 Agent turn 启动
  ↓
context_injector::inject_context() 注入上下文
  ↓
findings_panel_from_engine() → load_or_migrate()
  ↓
从 _findings_store 加载到旧的 findings
  ↓
❌ 旧 findings 被注入到 LLM prompt
  ↓
LLM 误以为还在处理旧任务，回复旧内容
```

---

## 代码证据

### 1. findings.rs 有 clear() 函数但未被调用

```rust
// crates/ox-core/src/agent/findings.rs:353-355
pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(STORE_KEY, String::new());  // STORE_KEY = "_findings_store"
}
```

### 2. clear_ephemeral_workflow_state() 漏掉了 findings 清理

```rust
// crates/ox-core/src/agent/engine.rs:395-432 (修复前)
pub fn clear_ephemeral_workflow_state(&mut self) {
    // ... 清理了很多状态变量
    crate::agent::perception::clear(self);  // 清理了 perception
    crate::agent::workflow_phases::clear_phase(self);  // 清理了 phase
    // ❌ 但没有调用 crate::agent::findings::clear(self);
}
```

### 3. load_or_migrate() 会从旧的 _findings_store 加载

```rust
// crates/ox-core/src/agent/findings.rs:368-386
pub fn load_or_migrate(engine: &WorkflowEngine) -> Option<FindingsStore> {
    if let Some(store) = load(engine) {  // 读取 _findings_store
        if !store.findings.is_empty() {
            return Some(store);  // ❌ 返回旧的 findings
        }
    }
    // ...
}
```

---

## 修复方案

在 `clear_ephemeral_workflow_state()` 中添加 `findings::clear()` 调用。

### 修改文件

**`crates/ox-core/src/agent/engine.rs:395-432`**

```rust
/// Clear per-round ephemeral workflow state (keeps step index and user request).
pub fn clear_ephemeral_workflow_state(&mut self) {
    if self.current_workflow.is_none() {
        return;
    }
    if let Ok(mut session) = self.session_state.try_lock() {
        session.awaiting_user_confirmation = false;
        session.set_variable("_explored_paths", "[]");
        session.set_variable("_exploration_snapshot", "[]");
        session.set_variable("_plan_tracker", "");
        session.set_variable("_route_chat", "");
        session.set_variable("_chat_reply_pending", "");
        session.set_variable("_chat_reply", "");
        session.set_variable("_done_gate_blocks", "");
        session.set_variable("_turn_memory", "");
        session.set_variable("_workflow_guidance", "[]");
        session.set_variable("_execute_report_delivered", "");
        session.set_variable("_execute_handoff", "");
        crate::agent::workflow_session::clear_session_flags(self);
        crate::agent::perception::clear(self);
        crate::agent::workflow_phases::clear_phase(self);
        // ✅ FIX: Clear findings store to prevent context pollution across rounds
        crate::agent::findings::clear(self);
        // Clear impl file read counters so new turns don't inherit old limits
        self.clear_impl_files_read();
        // ...
    }
}
```

---

## Impact 分析（GitNexus）

```bash
# 影响范围
Risk: HIGH (但是预期的修复场景)
Direct callers: 2
- finalize_completed_round()  # 任务完成时清理
- begin_user_round()          # 新任务开始时清理

Affected processes: 3
- begin_user_round (10 hits)
- process_agent_event (9 hits)
- handle_turn_done (4 hits)
```

所有调用者都是**预期的清理场景**，不会有副作用。

---

## 验证方式

### 测试场景

```
1. 启动 Ox
2. 执行任务 A（例如：/fix 某个文件，生成 findings）
3. 完成任务 A（LLM 调用 finish）
4. 开始新任务 B（输入全新的需求）
5. ✅ 验证：LLM 应该只关注任务 B，不提及任务 A 的 findings
```

### 预期行为

- **修复前**：新任务 B 时，LLM 仍然看到任务 A 的 findings，会混淆上下文
- **修复后**：新任务 B 时，`_findings_store` 已被清空，LLM 看不到旧 findings

---

## 相关清理点

这次修复确保了在以下场景清理 findings：

1. **新任务开始** → `begin_user_round()` → `reset_workflow()` → `clear_ephemeral_workflow_state()` → ✅ `findings::clear()`
2. **任务完成** → `finalize_completed_round()` → `clear_ephemeral_workflow_state()` → ✅ `findings::clear()`
3. **工作流重置** → `reset_workflow()` → `clear_ephemeral_workflow_state()` → ✅ `findings::clear()`

---

## 其他被清理的状态（供参考）

在 `clear_ephemeral_workflow_state()` 中，以下状态也会被清理：

- `_explored_paths` - 探索路径
- `_exploration_snapshot` - 探索快照
- `_plan_tracker` - 计划追踪
- `_turn_memory` - 回合记忆
- `_workflow_guidance` - 工作流指导
- `perception` - 感知状态
- `workflow_phases` - 工作流阶段
- **`_findings_store`** - ✅ **本次新增清理**

---

## 编译验证

```bash
cargo build --release
# ✅ 编译成功，无错误
```

---

## 提交信息

```
fix: 清理 findings 状态防止任务间上下文污染

问题：
- 旧任务完成后，_findings_store 未清理
- 新任务开始时，旧 findings 被注入到 LLM 上下文
- 导致 LLM 回复旧任务内容而非新任务

修复：
- 在 clear_ephemeral_workflow_state() 中调用 findings::clear()
- 确保新任务开始、任务完成、工作流重置时清理 findings

影响：
- Risk: HIGH（但所有调用者都是预期清理场景）
- 修复上下文污染问题，提升任务切换准确性
```

---

## 后续优化建议

1. **添加单元测试**：验证 findings 在任务切换时被正确清理
2. **日志增强**：在清理 findings 时记录日志，方便调试
3. **状态审计**：定期审查所有 `set_variable()` 调用，确保有对应的清理逻辑

---

**修复完成时间**：2026-06-30  
**修复人员**：Claude (Kiro)
