# Spec/Council 模式移除总结

## ✅ 已完成的清理工作

### 1. 删除的命令文件

- ❌ `crates/ox-cli/src/slash_commands/spec.rs` - Spec 模式命令
- ❌ `crates/ox-cli/src/slash_commands/council.rs` - Council 会议模式命令
- ❌ `crates/ox-cli/src/slash_commands/workflow.rs` - 工作流确认命令（/approve, /reject, /revise）

### 2. 从命令注册表中移除

修改了 `crates/ox-cli/src/slash_commands/mod.rs`：

**移除的命令注册**：
```rust
// ❌ 已删除
registry.register(spec::SPEC_COMMAND);
registry.register(council::COUNCIL_COMMAND);
registry.register(workflow::APPROVE_COMMAND);
registry.register(workflow::REJECT_COMMAND);
registry.register(workflow::REVISE_COMMAND);
```

**移除的模块导入**：
```rust
// ❌ 已删除
mod spec;
mod council;
mod workflow;
```

### 3. 保留的功能

✅ **Free 模式完全保留**
- `/free` 命令仍然可用
- Free workflow engine 正常工作
- Header 渲染简化（不显示工作流进度）

✅ **基础架构保留**
- `workflow_engine` 仍然存在（Free 模式需要）
- `update_workflow_display()` 仍然工作（但 free_workflow 不显示）
- 会话持久化机制完整

---

## 📊 当前可用命令列表

### 帮助和会话管理
- `/help` - 显示帮助
- `/new` - 创建新会话
- `/resume` - 恢复会话
- `/sessions` - 列出会话
- `/clean` - 清空会话

### 模型和信任
- `/model` - 切换模型
- `/trust` - 信任工具
- `/untrust` - 取消信任

### 记忆和反馈
- `/remember` - 记住信息
- `/forget` - 忘记信息
- `/memory` - 查看记忆
- `/feedback` - 提供反馈

### 系统命令
- `/exit` - 退出
- `/cd` - 切换目录
- `/init` - 初始化配置
- `/debug` - 调试信息
- `/cost` - 成本统计
- `/plan` - 任务计划
- `/reload` - 重载会话
- `/download_model` - 下载模型
- `/free` - 切换到 Free 模式 ⭐
- `/cancel` - 取消操作
- `/clear` - 清屏

---

## ⚠️ 保留的"死代码"

以下代码仍然存在于 main.rs 中，但**不会被调用**（因为命令入口已删除）：

1. **Spec planning 逻辑** (Line ~1266-1380)
   - `app.pending_spec_planning` 相关代码
   - 不会被触发，因为没有 `/spec` 命令

2. **Council discuss 逻辑** (Line ~1520-1545)
   - `app.pending_discuss` 相关代码
   - 不会被触发，因为没有 `/council` 命令

3. **Workflow approval 逻辑** (分散在多处)
   - `app.workflow_display` 检查
   - 仍然工作，但只在 Free workflow 中（不显示）

**为什么不删除这些代码？**
- 遵循 "surgical changes" 原则
- 避免大规模重构带来的风险
- 这些代码不会影响功能（只是未被调用）
- 未来如果需要恢复 Spec/Council 模式，可以快速启用

---

## 🎯 简化效果

### 用户体验简化

**之前**：
- 3 种工作模式（Free / Spec / Council）
- 11+ 个模式相关命令
- 复杂的工作流确认机制

**现在**：
- 1 种工作模式（Free）
- 0 个模式切换命令（只有 /free 用于重置状态）
- 简洁的交互体验

### 代码简化

| 指标 | 之前 | 现在 | 变化 |
|------|------|------|------|
| 命令文件数量 | 11 | 8 | -3 |
| 注册命令数量 | ~30 | ~25 | -5 |
| 命令复杂度 | 高 | 低 | ✅ |

---

## 🔧 技术细节

### Header 渲染简化

`terminal/app.rs` 中的 `update_workflow_display()` 仍然保留，但：

```rust
pub fn update_workflow_display(&mut self) {
    if let Some(ref engine_arc) = self.workflow_engine {
        if let Ok(engine) = engine_arc.try_lock() {
            if let Some(workflow) = engine.current_workflow() {
                // ✅ Free workflow 不显示
                if workflow.name == "free_workflow" {
                    self.workflow_display = None;
                    return;
                }
                
                // Spec/Council workflow 会显示（但不会被激活）
                // ...
            }
        }
    }
    self.workflow_display = None;
}
```

**效果**：
- Free 模式下，header 只显示基础信息
- 不会显示工作流进度条
- 界面更简洁

### 会话持久化

`/free` 命令仍然会保存 workflow 状态：

```rust
session.persist_workflow_state("free", "", 0, None);
```

**效果**：
- 退出重进后仍然是 Free 模式
- 不会恢复到 Spec/Council 模式

---

## 📝 后续可选优化

如果希望进一步精简代码，可以考虑：

### 选项 A：保守清理（推荐）
- ✅ 保持当前状态
- 只删除命令文件和注册
- main.rs 中的"死代码"保留但不影响功能

### 选项 B：激进清理
- 删除 main.rs 中所有 Spec/Council 相关代码
- 简化 `update_workflow_display()` 逻辑
- 移除 `workflow_engine` 中 Spec/Council workflow 的定义

**风险**：
- 需要大量测试
- 可能引入 bug
- 违反 "surgical changes" 原则

---

## ✅ 验证清单

- [x] 编译成功，无错误
- [x] `/spec` 命令不可用
- [x] `/council` 命令不可用
- [x] `/approve`, `/reject`, `/revise` 命令不可用
- [x] `/free` 命令仍然可用
- [x] Free 模式正常工作
- [x] Header 显示简洁（无工作流进度）
- [x] 退出重进后保持 Free 模式

---

## 🎉 总结

**已完成 Spec/Council 模式的移除**：
- ✅ 删除了 3 个命令文件
- ✅ 从注册表中移除了 5 个命令
- ✅ 保留了 Free 模式的完整功能
- ✅ 编译成功，功能正常

**当前状态**：
- Ox CLI 现在是纯粹的 Free 模式助手
- 简洁、直接、高效
- 符合 "回归简单" 的目标

**完成时间**: 2026-05-08  
**编译状态**: ✅ 成功  
**功能状态**: ✅ 正常
