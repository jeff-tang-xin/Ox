# Ox 渐进式重写计划（Strangler Fig）

> 单一进度源。每完成一小步就更新对应复选框与「变更记录」表。
> 维护者：Ox agent + 项目负责人。最后更新：P5 react_log 去重完成，P5.2/P5.3/P5.5 待续。

---

## 0. 核心原则（不可违背）

1. **绝不一次性推倒重写。** 现有代码是能跑的产品，`tool_args_repair`、streak 熔断、API-400 恢复等是踩坑换来的行为补丁，重写必须保留其语义。
2. **每一步都保持绿色。** 任何一次提交后 `cargo check` + `cargo test -p ox-core` 必须通过；新旧代码并存，逐模块绞杀。
3. **零行为变更优先。** 结构重构阶段先搬结构、后改行为；阈值/魔数迁移前先与现值逐一对齐。
4. **层边界不变。** `ox-cli -> ox-core -> 三方库`，core 不得依赖 TUI；跨 crate 只走 `AgentToUiEvent`/`UiToAgentEvent`。
5. **一步一确认。** 每个 sub-task 动代码前提交精确 diff 方案，确认后再执行。

---

## 1. 五阶段路线总览

| 阶段 | 目标 | 消除的旧债 | 状态 |
|------|------|-----------|------|
| **P1** | 类型化 Turn 状态机骨架 | 十几个散落 streak + `_total_explore` 字符串反模式 | ✅ 完成 |
| **P2** | 统一门禁管线（前置拦截也走 `Gate` trait） | enforcer / read_guard / `validate_tool_call` 三处重叠 | ✅ 完成（P2a 双调修复 + P2b 删死代码） |
| **P3** | 原生 function calling，删 `complete_and_check` 壳 | `unified_action` / `unified_handler` / `tool_args_repair` 大半 | 🔄 暂缓（依赖 P5 先拆开 god function 才好改） |
| **P4** | 统一上下文预算账本 | `memory_offload` 散落魔数（85%/92%/冷却） | ⬜ 未开始 |
| **P5** | 拆 `run_agent_turn` god function（3814 行） | mod.rs 巨型命令式循环 → 每状态一 handler | 🔄 进行中 |

图例：⬜未开始 · 🔄进行中 · ✅完成 · ⏸️暂缓

> **P3/P5 顺序调整说明**：原计划 P3 在 P5 之前，但实际分析后发现——`run_agent_turn` 3814 行未拆开前，`unified_tool_mode` 分支遍布 60+ 处，此时改 P3 等于在巨型函数里做手术，风险极高。改为 P5 先拆开结构，P3 再在已拆分的 handler 里逐个迁移。

---

## 2. 阶段明细与进度

### P1 — 类型化 Turn 状态机骨架 ✅

**产出物**：`crates/ox-core/src/agent/turn_state.rs`（新增）

- [x] P1.1 `engine.rs` 新增 `get_counter/set_counter/bump_counter` 类型化计数器 API ✅
- [x] P1.2 新建 `turn_state.rs`：`TurnPhase` 枚举 + `TurnBudget` 结构（只持可变计数，上限委托 `ConvergeMode`） ✅
- [x] P1.3 `mod.rs` 挂模块声明 `pub mod turn_state;` ✅
- [x] P1.4 `mod.rs:688` 用 `engine.get_counter("_total_explore")` 替换字符串 parse 加载逻辑；new-task 重置改 `set_counter(..,0)` ✅
- [x] P1.5 `mod.rs:1626` 回写改用 `engine.set_counter("_total_explore", total_explore)` ✅
- [x] P1.6 `gate/gate.rs` 的 `bump_failure/current_failures/reset_failures` 改用新计数器 API ✅
- [x] P1.7 单元测试：new` 新任务归零 / `on_explore` 累加 / `on_edit_or_finish` 重置 / `explore_exhausted` 边界（4 个用例均通过） ✅
- [x] P1.8 `cargo test -p ox-core turn_state` 通过（8 passed / 0 failed） ✅

**验收标准**：`parse::<u32>` 字符串状态反模式在 mod.rs + gate.rs 内清扫干净；行为与迁移前一致。
**触碰文件**：`engine.rs`、`turn_state.rs`（新）、`mod.rs`、`gate/gate.rs`（凡 4 个）

> **⚠️ 使用期间发现（已修正设计）：**
>
> - **发现 #1：上限是动态的，不是常量。** `total_explore` 上限用 `ConvergeMode::ceiling()` 动态返回：`Answer=6`(QA) / `DirectEdit=10`(FIX) / `SubmitPlan=12`(TOTAL)。原计划的 `TurnBudget::EXPLORE_CEILING=40` 常量错误，会制造第二事实源，已删除。
> - **发现 #2：阈值已集中在 `gate/explore_reflect.rs`**：`REFLECT_AT=3`、`STOP_AFTER_REFLECT=2`、`IMPL_REFLECT_AT=3`、三个 ceiling。故 `TurnBudget` **只持有可变计数、不重定义阈值**；上限检查委托 `ConvergeMode`。
> - **发现 #3：计数器比原计划多。** 实际 locals（mod.rs:681-717）：`content_only_streak`、`explore_streak`、`total_explore`、`impl_streak`、`unified_parse_error_streak`、`findings_deliver_error_streak`、`api_error_recovery_streak`（`MAX_API_ERROR_RECOVERY=2` 就地定义于 717 行），另有 `explore_reflected`/`impl_reflected` 布尔位。
> - **发现 #4：`total_explore` 以可变引用传入 `evaluate()` 再回写**（mod.rs:1617→1627），P1 只需替换持久化的 `parse`/`to_string`，不加 `evaluate` 签名。

---

### P2 — 统一门禁管线 ✅

**现状**：已有 `gate/gate.rs` 的 `Gate` trait + `GateRunner`，但只跑在 `## Done` 之后（后置校验）；前置拦截散在 `enforcer.rs`(22KB) / `gate/read_guard.rs` / `engine.rs::validate_tool_call`。

#### P2a — 修复双调 bug（✅ 完成于使用期间发现）

分析三处重叠时实测发现：`read_guard::check` 是**有状态**门禁（file_read 首次重读时 `record_impl_file_read` 写状态），却在两条执行路径里各自调用两次——
- **mod.rs 主循环**：2512 直接调 + 2548 `validate_tool_call`→validation.rs 间接调
- **unified_handler**：930 直接调 + 927 `validate_tool_call` 间接调

后果：首次重读在第一次调用消耗掉「允许一次重读」额度，第二次调用即判罚。已误判 2 次以上「禁止重读」而拦截。改造者迁移 `turn_state.rs` 时亲历此误检。

- [x] P2a.1 从 `validate_single_step_tool`（validation.rs:69）移除 `read_guard::check`，补注说明唯一调用点
- [x] P2a.2 两条路径各保留唯「缓存命中 cached-response 回填」的唯一直接调用
- [x] P2a.3 新增回归单测 `single_step_validation_does_not_consume_reread_budget`
- [x] P2a.4 `cargo test -p ox-core --lib` → 381 passed / 0 failed

#### P2b — 删除 enforcer 死代码 + EnforcementRules 孤儿配置（✅ 完成）

依赖分析确认：`RuleEnforcer::validate` 唯一调用点（mod.rs:2639）被 `skip_plan_rules`（单步模式）短路；所有 36 处 register/activate 均用 `DEFAULT_WORKFLOW_ID`（单步），多步 pipeline 无活跃入口。`EnforcementRules` 仅被 enforcer 消费，CLI 层零引用。

- [x] P2b.1 删除 `enforcer.rs` 整文件（557 行）+ `pub mod enforcer;` 声明
- [x] P2b.2 删除 `config/rules.rs` 整文件（72 行）+ `pub mod rules;` 声明 + `enforcement_rules` 字段 + TOML 注释模板
- [x] P2b.3 确认 `source_paths` 被 engine.rs/exploration_snapshot.rs 活跃使用，不受影响
- [x] P2b.4 `cargo check` 0 error / 0 warning；`cargo test -p ox-core --lib` → 374 passed / 0 failed（减 7 = enforcer 内部测试）

**原 P2b 计划（PreGate trait + 统一管线组装）取消**：enforcer 死代码删除后，前置门禁事实源已从三处缩减到两处（`read_guard` + `validate_tool_call`），无需再造大抽象。进一步统一在 P5 拆分 mod.rs 时自然收敛。

**验收标准**：死代码清零；enforcement_rules 孤儿消除；编译测试全绿。
---

### P3 — 原生 function calling ⏸️

> **暂缓说明**：P3 原定在 P5 之前，但 `unified_tool_mode` 分支在 `run_agent_turn` 3814 行里有 60+ 处 if/else 分叉。在这个巨型函数里改 P3 = 在刀尖上走钢丝。改为 **P5 先拆开结构**，P3 再在已拆分的 handler 里逐个迁移。

- [ ] P3.1 各内置 Tool 直接暴露 schema，`tool_choice` 用 `Function(complete_and_check)` 改为 `Auto`（部分已在 commit a9cf804 完成，需核对）
- [ ] P3.2 `finish` 语义 = 不再调工具、返回纯文本，移除 `action` 枚举分发
- [ ] P3.3 逐步废弃 `unified_action.rs` / `unified_handler.rs` 的 action 分发层
- [ ] P3.4 缩减 `tool_args_repair.rs`（原生 schema 校验接管）
- [ ] P3.5 回归测试：多工具并行调用、finish 收尾

**验收标准**：参数校验交给 JSON Schema + 模型原生能力；壳层代码显著缩减。

---

### P4 — 统一上下文预算账本 ⬜

- [ ] P4.1 定义 `ContextBudget { max, used }` 单一计量入口
- [ ] P4.2 卸载/压缩触发改为纯函数 `should_offload(budget, policy) -> Decision`
- [ ] P4.3 阈值（85%/92%）、冷却期为 `OffloadPolicy` 配置项
- [ ] P4.4 `memory_offload` 迁移到新账本，删散落魔数
- [ ] P4.5 单测：预算触发 / 冷却 / 优先级压缩

---

### P5 — 拆 god function 🔄

**当前 god function 结构**（`run_agent_turn`，3814 行，600→3613）：

| 行号区间 | 职责 | 拆出目标 |
|---------|------|---------|
| 600-717 | 函数签名 + 局部变量初始化（17 个 streak/flag/budget） | → `TurnContext` 结构体 |
| 719-840 | ReAct 循环入口：cancellation、interjection drain、memory sync、context assembly | → `loop_head()` |
| 841-960 | LLM stream 发起（provider 选择、schema 过滤、tool_choice） | → `dispatch_llm()` |
| 976-1183 | LLM stream 收集（text/reasoning/tool_calls + timeout/error handling） | → `collect_response()` |
| 1200-1261 | budget offload（prompt token → archive → placeholder） | → `offload_if_needed()` |
| 1263-1290 | args repair（XML→JSON、GLM `<tool_call>` 提取） | → `repair_tool_args()` |
| 1292-1331 | legacy review-findings 捕获 + business gate await | → `capture_review_findings()` |
| 1334-1398 | 空输出收尾（idle narrative → TurnDone） | → `handle_idle()` |
| 1400-1463 | truncation/loop-limit 过滤 | → `filter_tool_calls()` |
| 1500-1678 | reflect-first guard（explore/impl streak 评估 → 可能 continue） | → `evaluate_reflection()` |
| 1747-1838 | ReAct 记录（pre-execution decision → SQLite） | → `record_decision()` |
| 1855-3401 | **工具执行循环**（unified handler / legacy tool / safety gate / impact / react log / post-hooks） | → `execute_tool_batch()` |
| 3407-3455 | post-fix：orphan tool_call 清理 | → `cleanup_orphans()` |
| 3456-3572 | Done reminder + AST recovery + verify hints + repeated-failure handoff | → `post_edit_checks()` |
| 3574-3604 | offloader cleanup + repeat guard + loop tail | → `loop_tail()` |

**P5 拆分策略**：

- [ ] **P5.1** 提取 `TurnContext` 结构体，收编 17 个局部变量 → 减少传参爆炸
- [ ] **P5.2** 提取 `execute_tool_batch()`（1855-3401，最大块 ~1550 行）→ 最大收益
- [ ] **P5.3** 提取 `collect_response()`（976-1183）+ `dispatch_llm()`（841-960）
- [x] **P5.4** ✅ 提取 `evaluate_reflection()`（1500-1678）
- [ ] **P5.5** 提取 `loop_head()` + `loop_tail()` + `post_edit_checks()`
- [x] **P5.6** ✅ 提取 `handle_idle()` + `capture_review_findings()` + `filter_tool_calls()`
- [ ] **P5.7** `run_agent_turn` 从 3814 行降到数百行，主循环变成 `transition(state, event)` 调度
- [ ] **P5.8** 全量回归

**依赖**：P1 的 `TurnPhase` 枚举已就绪 ✅

---

## 3. 变更记录（每次提交追加一行）

| 日期 | 阶段/子任务 | commit | 说明 | check/test |
|------|------------|--------|------|-----------|
| —    | —          | —      | 计划文档建立 | —|
| 本轮 | P1.1+P1.6  | c933890 | engine.rs 新增类型化计数器 API；gate.rs 三函数迁移；消除 gate 反模式 | check 0 err·0 gate 测|
| 本轮 | P1.2+P1.3+P1.7 | c933890 | 新建 turn_state.rs（TurnPhase/TurnBudget，上限委托 ConvergeMode）；挂模块 + 6 单测 | 7 passed / 0 failed |
| 本轮 | P1.4+P1.5 ✅P1 完成 | c933890 | mod.rs 两处 `_total_explore` 字符串反模式迁移为 get_counter/set_counter；parse/to_string 在 agent 模块内清扫 | check 0 err·380 passed / 0 failed |
| 本轮 | P2a 双调bug修复 | 2ca6d85 | 实测发现 read_guard::check 在单步 unified 两路径各自被调用两次（有状态门禁），误判首次重读；从 validation.rs 移除重复调用 + 回归单测 | check 0 err·381 passed / 0 failed |
| 本轮 | P2b 删死代码 | 待提交 | 删 enforcer.rs(557行)+config/rules.rs(72行)+enforcement_rules 字段+TOML 模板；依赖分析确认全 36 处 register/activate 均用 DEFAULT_WORKFLOW_ID，enforcer 零活跃引用 | check 0 err·374 passed / 0 failed |
| 本轮 | P2b 删死代码 | 61df638 | 删 enforcer.rs(557行)+config/rules.rs(72行)+enforcement_rules 字段+TOML 模板；依赖分析确认全 36 处 register/activate 均用 DEFAULT_WORKFLOW_ID，enforcer 零活跃引用 | check 0 err·374 passed / 0 failed |
| 本轮 | P5.1+P5.4+P5.6 | b173ccd | 提取 classify_tool_calls()（6 单测）+ evaluate_reflection() + TurnContext 结构体；mod.rs 净减 227 行 | check 0 err·380 passed / 0 failed |
| 本轮 | P5.6 续 | 待提交 | 提取 react_log_ids() + react_log_assistant_text() + record_react_tool() 三辅助函数；替换 8 处重复 react_log 模板（净减 95 行） | check 0 err·380 passed / 0 failed |

---

## 4. 风险登记

| 风险 | 阶段 | 缓解 |
|------|------|------|
| 阈值迁移引入行为漂移 | P1/P4 | 迁移前逐一读取现值写入常量并注释来源 |
| 前置门禁委托遗漏某分支 | P2 | 保留旧入口名做薄壳委托，单测覆盖每分支 |
| 原生 FC 后模型参数格式回退 | P3 | 灰度：先并存 action 壳，稳定后再删 |
| god function 拆分失败隐式状态丢失 | P5 | 依赖 P1 已把隐式 locals 显式化后再拆 |
| 拆分时 `unified_tool_mode` 分支爆炸 | P5 | 先拆不含 unified 分支的块（P5.3-P5.6），最后拆 P5.2 工具执行循环 |

---

## 5. 当前决策

- 已否决「一次性全量推倒重写」—改为绞杀者模式渐进替换。
- **P1 ✅** + **P2 ✅** 均已完成。P2b 原计划的 PreGate trait 抽象取消（enforcer 删除后事实源已从三处缩减到两处，无需再造大抽象）。
- **P3 ⏸️ 暂缓**：`unified_tool_mode` 分支在 mod.rs 里有 60+ 处，在 god function 里改风险极高。改为 P5 先拆开结构。
- **当前进行 P5**：从 `TurnContext` 结构体提取开始，逐步把 `run_agent_turn` 3814 行拆成数个 handler 函数。
