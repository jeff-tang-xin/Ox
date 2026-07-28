# Ox 渐进式重写计划（Strangler Fig）

> 单一进度源。每完成一小步就更新对应复选框与「变更记录」表。
> 维护者：Ox agent + 项目负责人。最后更新：2025 首次建立。

---

## 0. 核心原则（不可违背）

1. **绝不一次性推倒重写。** 现有代码是能跑的产品，`tool_args_repair`、streak 熔断、API-400 恢复等是踩坑换来的行为补丁，重写必须保留其语义。
2. **每一步都保持绿色。** 任何一次提交后 `cargo check` + `cargo test -p ox-core` 必须通过；新旧代码并存，逐模块绞杀。
3. **零行为变更优先。** 结构重构阶段先搬结构、后改行为；阈值/魔数迁移前先与现值逐一对齐。
4. **层边界不变。** `ox-cli → ox-core → 三方库`，core 不得依赖 TUI；跨 crate 只走 `AgentToUiEvent`/`UiToAgentEvent`。
5. **一步一确认。** 每个 sub-task 动代码前提交精确 diff 方案，确认后再执行。

---

## 1. 五阶段路线总览

| 阶段 | 目标 | 消除的旧债 | 状态 |
|------|------|-----------|------|
| **P1** | 类型化 Turn 状态机骨架 | 十几个散落 streak + `_total_explore` 字符串反模式 | 🟡 进行中（P1.1/P1.6 ✅） |
| **P2** | 统一门禁管线（前置拦截也走 `Gate` trait） | enforcer / read_guard / `validate_tool_call` 三处重叠 | ⬜ 未开始 |
| **P3** | 原生 function calling，删 `complete_and_check` 壳 | `unified_action` / `unified_handler` / `tool_args_repair` 大半 | ⬜ 未开始 |
| **P4** | 统一上下文预算账本 | `memory_offload` 散落魔数（85%/92%/冷却） | ⬜ 未开始 |
| **P5** | 拆 `run_agent_turn` god function（3855 行） | mod.rs 巨型命令式循环 → 每状态一 handler | ⬜ 未开始 |

图例：⬜未开始 · 🟡进行中 · ✅完成 · ⏸️暂缓

---

## 2. 阶段明细与进度

### P1 — 类型化 Turn 状态机骨架 ⬜

**产出物**：`crates/ox-core/src/agent/turn_state.rs`（新增）

- [x] P1.1 `engine.rs` 新增 `get_counter/set_counter/bump_counter` 类型化计数器 API ✅
- [x] P1.2 新建 `turn_state.rs`：`TurnPhase` 枚举 + `TurnBudget` 结构（只持可变计数，上限委托 `ConvergeMode`）✅
- [x] P1.3 `mod.rs` 挂模块声明 `pub mod turn_state;` ✅
- [x] P1.4 `mod.rs:688` 用 `engine.get_counter("_total_explore")` 替换字符串 parse 加载逻辑；new-task 重置改 `set_counter(..,0)` ✅
- [x] P1.5 `mod.rs:1626` 回写改用 `engine.set_counter("_total_explore", total_explore)` ✅
- [x] P1.6 `gate/gate.rs` 的 `bump_failure/current_failures/reset_failures` 改用新计数器 API ✅
- [x] P1.7 单元测试：`new` 新任务归零 / `on_explore` 累加 / `on_edit_or_finish` 重置 / `explore_exhausted` 边界（6个用例均通过）✅
- [x] P1.8 `cargo test -p ox-core turn_state` 通过（7 passed / 0 failed）✅

**验收标准**：`parse::<u32>` 字符串状态反模式在 mod.rs + gate.rs 内清零；行为与迁移前一致。
**触碰文件**：`engine.rs`、`turn_state.rs`（新）、`mod.rs`、`gate/gate.rs`（≤4 个）

> **⚠️ 使用期间发现（已修正设计）**
>
> - **发现 #1：上限是动态的，不是常量。** `total_explore` 上限由 `ConvergeMode::ceiling()` 动态返回：`Answer=6`(QA) / `DirectEdit=10`(FIX) / `SubmitPlan=12`(TOTAL)。原计划的 `TurnBudget::EXPLORE_CEILING=40` 常量错误，会制造第二事实源，已删除。
> - **发现 #2：阈值已集中在 `gate/explore_reflect.rs`**：`REFLECT_AT=3`、`STOP_AFTER_REFLECT=2`、`IMPL_REFLECT_AT=3`、三个 ceiling。故 `TurnBudget` **只持有可变计数、不重定义阈值**；上限检查委托 `ConvergeMode`。
> - **发现 #3：计数器比原计划多。** 实际 locals（mod.rs:681-717）：`content_only_streak`、`explore_streak`、`total_explore`、`impl_streak`、`unified_parse_error_streak`、`findings_deliver_error_streak`、`api_error_recovery_streak`（`MAX_API_ERROR_RECOVERY=2` 就地定义于 717 行），另有 `explore_reflected`/`impl_reflected` 布尔位。
> - **发现 #4：`total_explore` 以可变引用传入 `evaluate()` 再回写**（mod.rs:1617→1627），P1 只需替换持久化的 `parse`/`to_string`，不动 `evaluate` 签名。
>
> **P1 缩小范围**：先做 **P1.1（类型化计数器 API）** + **P1.6（gate.rs 迁移）** 这个最小闭环——纯 API 收敛、零行为变更、独立可测。`turn_state.rs`（P1.2）待 API 稳定后再引入。

---

### P2 — 统一门禁管线 ⬜

**现状**：已有 `gate/gate.rs` 的 `Gate` trait + `GateRunner`，但只跑在 `## Done` 之后（后置校验）；前置拦截散在 `enforcer.rs`(22KB) / `gate/read_guard.rs` / `engine.rs::validate_tool_call`。

- [ ] P2.1 定义前置 `PreGate` trait（或复用 `Gate`，输入改为 `ToolCall + TurnCtx`）
- [ ] P2.2 `ReadBeforeEdit` / `PathScope` / `ImpactAnalysis` / `TrustLevel` 各实现为独立 gate
- [ ] P2.3 组装 `pre_pipeline()`，返回 `Allow | Warn | Block | NeedConfirm`
- [ ] P2.4 `enforcer` / `read_guard` / `validate_tool_call` 逐个改为委托新管线（保留旧入口签名，内部转发）
- [ ] P2.5 单测覆盖每个 gate 的 allow/block 分支
- [ ] P2.6 `cargo test -p ox-core` 通过

**验收标准**：「能否执行工具」的判断只有一处事实源；旧三入口沦为薄委托。

---

### P3 — 原生 function calling ⬜

- [ ] P3.1 各内置 Tool 直接暴露 schema，`tool_choice` 由 `Function(complete_and_check)` 改为 `Auto`（部分已在 commit a9cf804 完成，需核对）
- [ ] P3.2 `finish` 语义 = 不再调工具、返回纯文本，移除 `action` 枚举分发
- [ ] P3.3 逐步废弃 `unified_action.rs` / `unified_handler.rs` 的 action 分发层
- [ ] P3.4 缩减 `tool_args_repair.rs`（原生 schema 校验接管）
- [ ] P3.5 回归测试：多工具并行调用、finish 收尾

**验收标准**：参数校验交给 JSON Schema + 模型原生能力；壳层代码显著缩减。

---

### P4 — 统一上下文预算账本 ⬜

- [ ] P4.1 定义 `ContextBudget { max, used }` 单一计量入口
- [ ] P4.2 卸载/压缩触发改为纯函数 `should_offload(budget, policy) -> Decision`
- [ ] P4.3 阈值（85%/92%）、冷却抽为 `OffloadPolicy` 配置项
- [ ] P4.4 `memory_offload` 迁移到新账本，删散落魔数
- [ ] P4.5 单测：预算触发/冷却/优先级压缩

---

### P5 — 拆 god function ⬜

- [ ] P5.1 依据 P1 的 `TurnPhase`，每个 phase 抽出独立 handler 函数
- [ ] P5.2 主循环变为 `transition(state, event) -> state` 调度
- [ ] P5.3 `run_agent_turn` 从 3855 行降到数百行
- [ ] P5.4 全量回归

**依赖**：必须在 P1 完成后进行。

---

## 3. 变更记录（每次提交追加一行）

| 日期 | 阶段/子任务 | commit | 说明 | check/test |
|------|------------|--------|------|-----------|
| —    | —          | —      | 计划文档建立 | — |
| 本次 | P1.1+P1.6  | 待提交 | engine.rs 新增类型化计数器 API；gate.rs 三函数迁移；消除 gate 反模式 | check 0 err。60 gate 测|
| 本次 | P1.2+P1.3+P1.7 | 待提交 | 新建 turn_state.rs（TurnPhase/TurnBudget，上限委托 ConvergeMode）+ 挂模块 + 6 单测 | 7 passed / 0 failed |
| 本次 | P1.4+P1.5 ✅P1 完成 | 待提交 | mod.rs 两处 `_total_explore` 字符串反模式迁移为 get_counter/set_counter；parse/to_string 在 agent 模块内清零 | check 0 err，380 passed / 0 failed |

---

## 4. 风险登记

| 风险 | 阶段 | 缓解 |
|------|------|------|
| 阈值迁移引入行为漂移 | P1/P4 | 迁移前逐一读取现值写入常量并注释来源 |
| 前置门禁委托遗漏某分支 | P2 | 保留旧入口签名做薄委托，单测覆盖每分支 |
| 原生 FC 后模型参数格式回退 | P3 | 灰度：先并存 action 壳，稳定后再删 |
| god function 拆分丢失隐式状态 | P5 | 依赖 P1 已把隐式 locals 显式化后再拆 |

---

## 5. 当前决策

- 已否决「一次性全量推倒重写」——改为绞杀者模式渐进替换。
- **下一步**：等确认后执行 **P1.1 + P1.2**（类型化计数器 API + `turn_state.rs` 骨架），最小闭环、独立可测。
