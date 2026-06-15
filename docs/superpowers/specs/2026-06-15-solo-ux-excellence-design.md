# Solo UX Excellence Design

**Date:** 2026-06-15  
**Status:** Approved — P0 + P1 (routing) implementing  
**Decisions:** simple coding 跳过 Review ✓ | verify 仅注入提示不自动 shell ✓  
**Scope:** 单兵开发者体验优先；企业治理/多租户/API 明确不在本 spec 范围

---

## 1. 目标与成功标准

### 1.1 目标

让 Ox 在**单人、单项目、终端 TUI**场景下，达到「明显比通用 Agent 更可靠、更省 Token、更少重复劳动」的体验。

### 1.2 成功标准（可感知）

| 指标 | 现状痛点 | 目标 |
|------|----------|------|
| Execute 重复读文件 | 第 2 轮 iteration 像重新开始 | 同一步内明确知道「已读/已改/待做」 |
| 计划可执行性 | 计划空、步骤模糊、Review 瞎猜 | Plan 必填字段 + Review 对照探索快照 |
| 简单任务耗时 | 改一行也要走 4 步 | `complexity: simple` 可跳过 Review 或压缩流程 |
| 失败恢复 | build 失败靠模型自觉 | 结构化自愈提示 + 可选自动 `cargo check` |
| 用户心智负担 | 不知道卡在哪一步 | TUI 常驻：Workflow 步骤 + 计划项进度 + iteration |

### 1.3 非目标

- 审计日志、RBAC、团队策略中心
- Headless API / CI 集成
- 新 LLM 提供商

---

## 2. 现状与缺口

### 2.1 已有能力（保留并强化）

- 4 步 Workflow：Intent → Plan → Review → Execute
- `_exploration_snapshot`：Plan 探索落盘 + 大文件 ref
- `[STEP_MEMORY]`：Plan/Execute 每轮 iteration 注入进度（近期已加）
- Enforcer：read-before-edit、plan-before-edit
- `error_recovery`：shell_exec 失败后注入修复指引
- TUI `plan_items` 侧边栏（**仅 UI 层，未与 Execute 引擎联动**）

### 2.2 核心缺口

```
┌─────────────────────────────────────────────────────────┐
│  Plan JSON  ──×──▶  Execute 逐步对照（无结构化追踪）      │
│  exploration_snapshot ──▶ Review（有）Execute（仅预览）   │
│  duplicate file_read  ──▶ 仅 Plan 拦截，Execute 不拦截    │
│  Intent complexity    ──▶ 未用于路由，一律 4 步          │
│  plan_items (TUI)     ──▶ 与 WorkflowEngine 状态脱节    │
└─────────────────────────────────────────────────────────┘
```

---

## 3. 设计原则

1. **引擎状态优先**：计划进度、已探索、已修改 — 存 `WorkflowEngine` session 变量，TUI 只读展示。
2. **每轮 LLM 调用前注入**：延续 `[STEP_MEMORY]` 模式，不依赖模型「记得」session 历史。
3. **能拦则拦**：重复 `file_read`/`file_list` 在 Execute 也硬拦截（与 Plan 一致）。
4. **简单任务快路径**：Intent 分类驱动，不牺牲复杂任务的安全 Review。
5. **小步交付**：P0→P3 可独立上线，每阶段用户可感知。

---

## 4. 架构：Plan Tracker（核心新增）

### 4.1 数据结构

Plan 确认后（或进入 Review 前），解析 `_step1_output` 的 JSON，写入：

```json
// session variable: _plan_tracker
{
  "steps": [
    {
      "index": 1,
      "file": "crates/ox-core/src/foo.rs",
      "action": "modify",
      "target": "handle_key",
      "desc": "...",
      "verify": "cargo check -p ox-core",
      "status": "pending"  // pending | in_progress | done | skipped
    }
  ],
  "current_index": 1
}
```

API（`WorkflowEngine`）：

- `load_plan_tracker_from_output(plan_json: &str)`
- `get_current_plan_step() -> Option<PlanStep>`
- `mark_plan_step_done(index)` — 在对应 `file_write`/`edit_file` 成功后调用
- `plan_progress_summary() -> String` — 供 `[STEP_MEMORY]` 注入

### 4.2 Execute iteration 注入（增强现有 `inject_execute_step_memory`）

每轮追加：

```
【计划进度】2/5 完成
  ✅ 1. modify `handle_key` in app.rs
  ▶ 2. modify `render` in render.rs  ← 当前
  ⏳ 3. add test in app.rs
...
规则：完成当前步骤并验证后，再进入下一步。勿重复已完成项。
```

### 4.3 与 TUI 联动

- `TurnDone` / Plan 确认时：`app.plan_items` 从 `_plan_tracker` 同步（不再仅从 ## Done 反推）
- 侧边栏显示 `2/5` 与当前步骤 desc 摘要

---

## 5. Phase 划分

### P0 — 零失忆执行（最高优先级，~1 周）

| 项 | 说明 |
|----|------|
| Plan Tracker | 解析 plan JSON → `_plan_tracker`；Execute 每轮注入进度 |
| Execute 重复拦截 | `file_read`/`file_list` 同路径重复调用返回错误（复用 `record_explored_path`） |
| 步骤完成检测 | `edit_file`/`file_write` 成功且 path 匹配当前 plan step → `mark_plan_step_done` |
| Review 阻塞 | `safe=false` 或 `complete=false` 时不 advance（已有 JSON 校验，补 UI 提示） |

**验收：** Execute 3 轮 iteration 内不再重复读同一文件；TUI 计划侧边栏与引擎一致。

### P1 — 智能路由（~3–5 天）

Intent JSON 已有 `complexity: simple|complex`：

| complexity | intent | 路由 |
|------------|--------|------|
| simple | chat | Intent 后直接回复，**不**进 Plan（或仅 1 步） |
| simple | coding | Intent → Plan → Execute，**跳过 Review** |
| complex | * | 完整 4 步 |

实现：`advance_on_output` step 0 读 `complexity` + `intent`，`advance_to_step` 跳转。

**验收：** 「解释这段代码」不走 Plan；「改一个 typo」跳过 Review。

### P2 — 计划质量（~3–5 天）

| 项 | 说明 |
|----|------|
| Plan 校验增强 | 每条 step 必须有 `file` + `desc`；`modify`/`delete` 必须有 `target` |
| Review 提示 | 系统提示要求对照 `{EXPLORATION_SNAPSHOT}` 中是否出现过 plan 里的 `file` |
| Plan 修订 | 用户反馈重跑 Plan 时保留 `_exploration_snapshot`，不清空探索 |

**验收：** 空 plan / 无 file 的 step 被拒绝；Review 的 issues 可引用具体路径。

### P3 — 自愈与验证（~1 周）

| 项 | 说明 |
|----|------|
| 编辑后自动验证 | Execute 步：`edit_file`/`file_write` 成功后，若 plan step 含 `verify: cargo check...`，注入「请执行 verify」 |
| 编译错误捕获 | 解析 `cargo check` 输出中的 `error[E]`，注入 `error_recovery` 同款结构化提示 |
| Done 门禁 | 输出 `## Done` 前检查 `_plan_tracker` 是否全部 done；否则要求补完或声明 skipped |

**验收：** 改 Rust 文件后自动提示 cargo check；未完成计划项时不允许 Done。

### P4 — TUI 心智模型（~3 天，可与 P0 并行）

| 项 | 说明 |
|----|------|
| Status bar | `📋 Plan 2/5 · ⚡ Execute iter 3 · embed: …` |
| Workflow 条 | Header 显示 `Intent ✓ Plan ✓ Review ✓ Execute ▶` |
| 探索摘要 | `/plan` 或侧边栏可展开「已探索文件列表 + ref 路径」 |

---

## 6. 关键文件

| 模块 | 路径 |
|------|------|
| Plan Tracker | `crates/ox-core/src/agent/plan_tracker.rs`（新） |
| Engine 集成 | `crates/ox-core/src/agent/engine.rs` |
| Iteration 注入 | `crates/ox-core/src/agent/context_injector.rs` |
| Execute 拦截 | `crates/ox-core/src/agent/mod.rs`（tool 执行前，与 Plan 并列） |
| Intent 路由 | `crates/ox-core/src/agent/engine.rs` `advance_on_output` |
| TUI 同步 | `crates/ox-cli/src/handlers/agent_handler.rs`, `terminal/render.rs` |

---

## 7. 风险与缓解

| 风险 | 缓解 |
|------|------|
| Plan JSON 格式不稳定 | 严格校验 + 失败时展示 raw JSON fallback（已有） |
| 过度拦截导致模型卡住 | Execute 重复读同一文件时，提示读 `.ox/exploration/` ref |
| simple 路由误跳过 Review | 仅 `complexity: simple` **且** plan ≤3 步才跳过 |
| Token 膨胀 | Plan progress 摘要每步一行，不全量重复 plan JSON |

---

## 8. 推荐实施顺序

```
P0 Plan Tracker + Execute 拦截  →  立刻解决「没记忆、重复读」
P1 Intent 路由                   →  简单任务体感提速
P2 Plan 质量                     →  减少烂计划
P3 自愈 + Done 门禁              →  执行可靠
P4 TUI                           →  可观测性
```

**建议从 P0 开始编码**；P1–P4 可在 P0 合并后按优先级迭代。

---

## 9. 待确认

- [ ] 是否同意 `simple` coding 任务跳过 Review？
- [ ] Plan step 的 `verify` 是否强制自动触发 shell_exec，还是仅注入提示？
- [ ] P0 是否立即开始实现？
