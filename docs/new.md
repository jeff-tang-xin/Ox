# 终极工程化 AI Agent 架构设计：全工具化与进程级状态机控制

## 1. 核心设计思想与痛点解决

### 1.1 传统 Agent 架构的痛点

在传统的 ReAct 或 LangChain 架构中，大模型通常被允许直接输出纯文本（Text Message）或自主决定下一步行动。这种「放权」在工程落地时会导致以下问题：

- **输出不可控**：大模型容易话痨、格式混乱，前端只能靠正则猜测意图。
- **上下文丢失与恢复成本高**：依赖外部数据库拼装 Checkpoint，唤醒时易出现记忆断层或幻觉。
- **流程易越狱**：模型可能在未完成任务时自作主张给出「最终答案」，导致提前结束或空转死循环。

### 1.2 本架构的核心设计哲学

核心思想是 **「确定性状态机接管概率性模型」**。

我们将大模型从「自由对话者」转变为 **受控的逻辑执行器**：

- 剥夺其直接面向用户的文本输出权（All-Tooling）。
- 利用 **强制工具调用（Forced Tool Choice）** 与 **进程级挂起（Process Suspend）** 构建工程化控制流。
- **控制流是确定的**（必走 tool、必过 gate）；**参数仍由模型填充**，靠 schema 与 gatekeeper 约束。

大模型在架构上是一个 **后端微服务**，不是聊天机器人。

### 1.3 重要澄清：挂起 ≠ 挂住供应商 HTTP

标准 OpenAI 兼容 API（含流式 Chat Completions）的单次调用形态是：

```text
POST /chat/completions → stream → tool_calls + [DONE] → HTTP 结束
```

因此 **ReAct 阻塞 tool_call 的本质，不是保持同一条供应商 HTTP 长连接**，而是：

```text
在 Ox 进程内 hold 住 ReAct 状态机，使 messages[] 停留在合法的
「assistant 已发出 tool_call，尚未写入 tool_result」中间态；
用户反馈作为 tool_result（Observation）写入后，再发起下一次 completions 请求。
```

对 **模型语义** 而言：等价于「一直在等 Observation」；对 **传输层** 而言：是 **多轮短 HTTP + 会话状态在 messages 中延续**。

挂起期间，状态保存在：

- 内存中的 Agent turn（`tool_call_id`、phase、gate 类型）；
- `messages[]` 对话链（真相源）；
- 可选 session 文件（冷启动恢复，见 §7）。

---

## 2. 架构三大核心支柱

### 2.1 绝对剥夺输出权（All-Tooling 模式）

**设计原则**：任务生命周期内，大模型 **禁止** 用 assistant `content` 向用户交付产物（说明、报告、findings、结束宣告等）。

**统一通信协议**：所有意图 **必须且只能** 通过唯一出口工具 `complete_and_check` 表达，包括：

| 类别 | 说明 |
|------|------|
| read | 读文件、搜索、符号查找、recall |
| write / edit | 写文件、编辑、删除 |
| git | status、diff、commit 等 |
| deliver（原 text） | 向用户交付文本产物（findings、plan、报告、澄清问题） |
| finish | 请求结束本轮/任务（不自动 Done） |
| 其他 | shell、web_fetch、load_skill 等 |

**Think / reasoning**：走独立推理通道，仅用于 Think 面板展示，**不计入 context token**，**不得**用 `deliver` 或 assistant prose 代替。

**工程优势**：前端按 JSON action 渲染，零正则解析。

### 2.2 强制工具调用（Forced Tool Choice）

利用 API 的 `tool_choice` 在关键阶段锁死模型选择：

| 阶段 | tool_choice | 说明 |
|------|-------------|------|
| 探索 / 实施 | `{ type: "function", function: { name: "complete_and_check" } }` 或等价约束 | 仍禁止 prose 交付 |
| 交互 / 收尾 | 强制 `complete_and_check` | 物理收网，杜绝越狱 prose |

建议配合 `parallel_tool_calls: false`，每轮 **单 action**，便于挂起与 gate。

### 2.3 进程级挂起（Process Suspend）

当 `complete_and_check` 命中 **需人工介入的 gate** 时：

1. 解析 `tool_call`，**不** 立即写入 `tool_result`；
2. 渲染 UI（交付预览 / 危险操作确认 / finish 摘要）；
3. **await** 用户输入（分钟～小时均可）；
4. 将用户结论 **包装为 tool_result** 写入 `messages`；
5. **同一 Agent turn** 内发起下一次 LLM 请求（新 HTTP），继续 ReAct。

**不是**：销毁 session、spawn 新 turn、或依赖 TurnDone 断轮后再 confirm。

**是**：tool 对话链在程序端闭合前人为延迟，Observation 由人参与生产。

---

## 3. `complete_and_check` 协议

### 3.1 请求形状（LLM → Ox）

```json
{
  "action": "file_read",
  "params": { "path": "src/app.js" }
}
```

```json
{
  "action": "deliver",
  "params": {
    "kind": "findings",
    "content": "...",
    "metadata": { "paths": [] }
  }
}
```

```json
{
  "action": "finish",
  "params": {
    "kind": "turn",
    "summary": "本轮完成审查…",
    "artifacts": ["findings"]
  }
}
```

`action` 枚举覆盖 Ox 现有全部工具能力；内部分发到现有 Tool 实现（UnifiedActionRouter）。

### 3.2 Gate 策略（默认）

| action 类 | 示例 | Gate | 阻塞？ |
|-----------|------|------|--------|
| read | file_read, code_search, recall | none | 否，执行后返回 result |
| git 读 | git_status, git_diff | none | 否 |
| write / edit | edit_file, file_write | safety | **是**，Allow/Deny/TrustAlways 后再执行 |
| shell 等 | shell_exec | safety | **是** |
| git 写 | git_commit | safety | **是**（可配置） |
| deliver | findings, plan, report, message | business | **是**，确认/讨论/拒绝 |
| finish | turn / task | finish | **是**，用户显式结束或继续 |

**原则**：所有行为走 tool 管道；**仅在卡点阻塞**，不是每个 read 都等人点击。

### 3.3 统一 tool_result（Observation）Envelope

Ox 返回给 LLM 的 `tool_result.content` 建议统一为 JSON：

```json
{
  "status": "ok",
  "gate": null,
  "data": { },
  "user": null,
  "error": null
}
```

`status` 取值示例：

| status | 含义 |
|--------|------|
| `ok` | 执行成功（read/write 等） |
| `denied` | safety gate 拒绝 |
| `confirmed` | business gate 用户确认 |
| `discuss` | business gate 用户讨论 |
| `rejected` | business gate 用户拒绝 |
| `user_finished` | finish gate 用户确认结束 |
| `user_continue` | finish gate 用户要求继续 |
| `error` | 执行失败 |

gate 示例：`"gate": "safety" | "business" | "finish"`。

人工介入 **永远是 tool_result 的一种**，不是旁路 UI 事件改写 session。

---

## 4. 系统执行流转

整个生命周期由 `complete_and_check` 驱动：

```text
User input
  → LLM (complete_and_check)
  → [gate?] → execute / await user
  → tool_result → messages
  → LLM …
  → deliver / finish → [BLOCK] → user → tool_result
  → …
  → finish + user_finished → checkpoint → TurnDone（仅此时）
```

### 4.1 中间执行态（探索与修复）

- 模型调用 `read` / 安全类 `git` 等 **gate=none** 的 action。
- Ox 执行本地操作，**立即** 返回 `tool_result(status=ok, data=…)`。
- **同一 turn** 内继续下一次 completions（新 HTTP）。

写操作：**先 safety 阻塞**，用户 Allow 后再执行，再返回 result。

### 4.2 交互阻塞态（人机协同）

- 模型调用 `deliver`（交付 findings、plan、报告、澄清等）。
- Ox 渲染 UI，**挂起** pending `tool_call_id`，**不** TurnDone。
- Session / turn 保持活跃。

### 4.3 恢复执行态

- 用户确认、讨论、拒绝、或安全操作 Allow/Deny。
- Ox 写入对应 `tool_result`，**不** spawn 新 agent。
- 下一次 LLM 请求携带完整 `messages`，模型侧等价于「Observation 到达」。

### 4.4 任务终结态（程序不主动 Done）

- 模型调用 `finish` **仅表示请求结束**，不是程序指令。
- Ox 展示 summary，**阻塞** 等待用户：`user_finished` | `user_continue`。
- **仅** 在 `user_finished`（且可选 gatekeeper 校验通过）后：
  - 写入本轮 round memory；
  - `emit_turn_done` / 释放 turn；
- 程序 **从不** 因模型输出 `## Done` 字符串或 idle 检测而自动结束；`## Done` 若保留，仅作为 `deliver`/`finish` 的可校验字段。

**用户主动触发结束**：UI 确认 finish、`/exit`、新任务等，与 `finish` gate 一致。

---

## 5. 双门禁与统一挂起管道

现有 Ox 概念映射到本架构：

| 旧概念 | 新架构 |
|--------|--------|
| safety_gate | write/edit/shell → 延迟 tool_result |
| business_gate | deliver → 延迟 tool_result |
| finish gate | finish → 延迟 tool_result，用户主权 |
| TurnDone 断轮确认 | **禁止**；确认 = 补发 tool_result |
| prose + findings 捕获 | **禁止**；一律 deliver action |

三者实现同一管道：`await ui_rx → wrap_human_ack() → ToolResult`。

---

## 6. tool_choice 与阶段

| Workflow 阶段 | 允许的 action 子集 | tool_choice |
|---------------|---------------------|-------------|
| 澄清 / 探索 | read, git(读), deliver(message) | forced 或强约束 |
| 审查 | read, deliver(findings) | forced |
| 实施 | read, write, edit, deliver | forced |
| 等待用户 | （挂起中，不调 LLM） | — |

实现上由 `phase → allowed_actions` 过滤 schema，并与 `tool_choice` 联动。

---

## 7. Session 与 Round Memory

### 7.1 运行时真相源

**`messages[]` 中的 tool_call + tool_result 链** 是 LLM 叙事的主记忆；挂起态 = 链上存在未闭合的 tool_call。

### 7.2 持久化（冷启动）

每次 **用户确认的 finish** 或 turn checkpoint 写入：

```json
{
  "round_id": 12,
  "user_intent": "...",
  "actions_summary": ["file_read×3", "deliver/findings"],
  "deliverables": { "findings_ref": "...", "summary": "..." },
  "gate_outcomes": [{ "tool_call_id": "...", "status": "confirmed" }],
  "messages_snapshot": "压缩后的 tool 链或全文"
}
```

**下次启动 session**：

1. 恢复 `messages`（或压缩 replay）；
2. 注入 **round memory 短摘要**（非全文 dump）；
3. UI 面板（findings 等）从 store hydrate；**LLM 以 tool_result 历史为准**。

### 7.3 「零状态损耗」的准确含义

- **运行时**：不依赖 Redis 实时拼装；挂起 = 进程内 await + messages 中间态。
- **仍需要**：session 文件持久化（冷启动）、大结果 offload/recall（防 context 爆炸）。
- **不需要**：与 messages 平行的第二套「叙事真相」（大量 system 注入、prose 捕获、idle 补丁）。

---

## 8. Think 通道

- 推理内容走 `ReasoningDelta` / think 标签分流 → **Think 面板**。
- **不** 写入 assistant `content`，**不** 计入 context token 预算。
- **不** 通过 `deliver` 重复输出思考过程。

---

## 9. 架构优势总结

| 优势 | 说明 |
|------|------|
| 控制流稳定 | 交付、结束、危险操作必经 gate，减少越狱与空转 |
| Human-in-the-loop | 长周期「下达指令 → 离开 → 回来确认」= suspend + tool_result |
| 记忆一致 | 语义在 tool 链；挂起不丢「等 Observation」的叙事 |
| 前端解耦 | action JSON → 组件；无 prose 正则 |
| 与 Ox 演进对齐 | business_gate、safety_gate、同轮 ReAct 可收敛为统一 Tool Runtime |

---

## 10. Ox 落地映射（实现参考）

| 本设计 | Ox 现状 / 目标 |
|--------|----------------|
| `complete_and_check` | `ToolRegistry` → `UnifiedActionRouter` |
| Forced tool_choice | `mod.rs` / `stream_chat` options |
| Process Suspend | `business_gate` + `safety_gate` + finish gate → 统一 await |
| deliver | 替代 findings prose 捕获 |
| finish + user_finished | 替代 gatekeeper 自动 TurnDone |
| Round memory | session + memory_bridge → round store |
| Think UI-only | `think_stream` + `ReasoningChunk` |

建议落地顺序：统一 Gate 管道 → `deliver` action → `UnifiedActionRouter` → 分阶段 forced tool_choice → 瘦 context 注入。

---

## 11. 非目标与约束

- 不假设供应商支持「单条 HTTP 永久挂起」；挂起由 **Ox 进程 + messages** 实现。
- 不消除 session 磁盘持久化；消除的是 **双轨叙事与自动 Done**。
- 大文件仍走 offload / `recall`，避免 tool_result 撑爆 context。
- DeepSeek 等需回传 `reasoning_content` 的提供商：仅在 **当次 API 往返** 需要时携带；**不** 进入长期 messages 预算（见 `think_stream` / `prepare_messages_for_llm`）。
