---
name: ReAct LLM-driven termination
overview: 结束权完全归 LLM。finish 是 LLM 主动调用的"显式结束动作"——结束本轮、交还用户，但绝不锁死会话。门禁(finding_json)与工具只做执行/校验，永不主动结束、永不禁止后续工具。修掉 handle_finish 的"假完成"bug 与 Complete 禁工具僵局。
todos:
  - id: finish-explicit-end
    content: unified_handler.rs handle_finish 无 finding_json 路径：用 complete_workflow() 正确收尾(推进 step→is_workflow_complete=true，下一轮可复位)，替换只设 phase=Complete 的 on_done_gate_passed；保留 WorkflowCompleted + TurnDone
    status: completed
  - id: unlock-complete
    content: 拆掉 Complete 禁工具硬门禁：engine.rs:175 删除 Complete→Err；unified_action.rs 路由 Complete 由 blocked=[*] 改为只读+finish；tool_graph.rs Complete 同步软化
    status: completed
  - id: prompts
    content: 文案澄清：finish=主动收尾(交还用户、不锁后续)；中间想说明就随工具一起放文本，勿用 finish；finding_json 仅门禁校验、确认后由 LLM 自己 finish 收尾(system_prompt.rs / build_unified_route / ox-unified-tooling.md / ox-output-discipline.md)
    status: completed
  - id: tests
    content: 新增/调整：finish 收尾后 is_workflow_complete 为真且 phase 非 Complete 锁定；下一轮 begin_user_round 正确复位；Complete 阶段不再硬拦工具
    status: completed
  - id: verify-build
    content: cargo build + 相关 cargo test 通过
    status: completed
isProject: false
---

## 核心原则（方案A）

- 结束本轮 = LLM **主动调用** `finish`（可带最终 `content`）。这是 LLM 深思后的显式收尾动作。
- `finish` 结束后只是**交还本轮给用户**；绝不把会话锁进"禁止调用工具"的死状态，下一条用户输入自然续接。
- 中间想说明/分析但还要继续 → 直接把文字放进 assistant 文本，**随下一个工具动作一起输出**；不要用 `finish` 投递中间内容。
- `finding_json` → 仅触发门禁做校验/确认；确认后解锁写权限并**回到循环**，由 LLM 自己继续 file_read/edit/验证，最后**自己** `finish` 收尾。
- 门禁与工具是辅助/校验，**永不主动结束、永不禁止后续工具**。

## 根因

`handle_finish` 无 finding_json 路径（[unified_handler.rs:153‑166](F:\rust\Ox\crates\ox-core\src\agent\unified_handler.rs)）只调用了 `phase::on_done_gate_passed`（把 phase 设成 `Complete`），**却没有** `complete_workflow()`。结果：

- `is_workflow_complete()` 仍为 `false`（step index 未推进）→ 下一轮 `begin_user_round`（[user_round.rs:191‑213](F:\rust\Ox\crates\ox-core\src\agent\user_round.rs)）不会走"已完成→复位"分支，phase 卡在 `Complete`。
- phase=`Complete` 触发禁工具硬门禁（[engine.rs:175](F:\rust\Ox\crates\ox-core\src\agent\engine.rs)、[unified_action.rs:226‑231](F:\rust\Ox\crates\ox-core\src\agent\unified_action.rs)）→ 任何后续 action 都被 `✅ 任务已完成 — 禁止调用工具。` 拦死 = 截图死循环。

## 改动点

### 1. finish = 正确收尾（核心修复）
`crates/ox-core/src/agent/unified_handler.rs` `handle_finish` 无 finding_json 分支：
- 用 `mut` 锁，调用 `engine.complete_workflow()` 取代 `phase::on_done_gate_passed(&engine, true)`。
- `complete_workflow()` 会 finalize 本轮（归档 round + 推进 step index 到末尾，`is_workflow_complete()=true`），但**不**改 phase 为 `Complete`，因此不会触发禁工具门禁；下一轮 `begin_user_round` 走"已完成→reopen/reset + on_round_started"正常复位。
- 保留 `WorkflowCompleted` 事件（CLI 反思/状态）与 `return TurnDone`（finish 就是 LLM 的显式结束）。

### 2. 拆掉 Complete 禁工具硬门禁（防 stranding，贴合"门禁不锁死"）
- `crates/ox-core/src/agent/engine.rs` [engine.rs:174‑176](F:\rust\Ox\crates\ox-core\src\agent\engine.rs)：删除 `phase==Complete → Err("任务已完成 — 禁止调用工具")` 早退。
- `crates/ox-core/src/agent/unified_action.rs` [unified_action.rs:226‑231](F:\rust\Ox\crates\ox-core\src\agent\unified_action.rs)：`Complete` 分支由 `blocked=["*"]`/`allowed=[]` 改为只读探索 + `finish`（与 Review/Receive 只读分支一致），避免 schema 工具集为空导致无工具可用而卡死。
- `crates/ox-core/src/agent/tool_graph.rs` [tool_graph.rs:26‑31](F:\rust\Ox\crates\ox-core\src\agent\tool_graph.rs)：同步软化（legacy 路由）。

### 3. 文案澄清（语义本就接近，仅补"不锁后续 + 中间文本"）
现有提示词已写 `finish(content) → 结束`，与方案A一致。仅补充：
- `crates/ox-core/src/context/system_prompt.rs`（[system_prompt.rs:209‑216, 241‑251](F:\rust\Ox\crates\ox-core\src\context\system_prompt.rs)）：明确 finish 结束后会交还用户、不锁后续；中间说明随工具一起放文本，勿用 finish 投递中间内容；finding_json 仅门禁校验、确认后由你自己 finish 收尾。
- `crates/ox-core/src/agent/unified_action.rs` `build_unified_route`（[unified_action.rs:345‑353](F:\rust\Ox\crates\ox-core\src\agent\unified_action.rs)）：同步。
- `crates/ox-core/src/skill/builtin/ox-unified-tooling.md`、`ox-output-discipline.md`：同步小节。

## 防回环
- 依赖既有上限 `MAX_ITERATIONS_PER_TURN=12`、`MAX_SAME_TOOL_CALLS=5`（`complete_and_check:finish` 连续 5 次触发 loop break）。

## 测试
- `phase.rs` / `user_round.rs`：新增——finish 收尾后 `is_workflow_complete()` 为真且 phase 不处于会锁工具的 Complete 状态；随后 `begin_user_round("新任务")` 复位到 Receive/对应 intent。
- `engine.rs`：`validate_tool_call`/`validate_single_step_tool` 在收尾后不再硬拦只读/ finish。
- 现有 `unified_action` 测试（解析层不变）保持通过。
