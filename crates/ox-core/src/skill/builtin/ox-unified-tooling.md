---
name: ox-unified-tooling
description: 系统提示词已包含完整工具规则，此 skill 仅补充参数细节
scope: system
---

# complete_and_check 工具参考

> **系统提示词是权威来源。** 此 skill 仅提供参数细节。

## 常用 action 及精确键名

| action | params | 说明 |
|--------|--------|------|
| `find_symbol` | `{"name":"..."}` | ❌ 不要用 symbol/query 键 |
| `file_read` | `{"path":"...","offset":0,"limit":50}` | 先 find_symbol 定位再精准读 |
| `code_search` | `{"pattern":"..."}` | ❌ 不要用 query 键 |
| `edit_file` | `{"path":"...","old_string":"...","new_string":"..."}` | old_string 逐字匹配原文 |
| `file_write` | `{"path":"...","content":"..."}` | 整文件写 |
| `shell_exec` | `{"command":"..."}` | 构建/测试 |
| `code_graph` | `{"op":"...", ...}` | 代码知识图谱(GitNexus)；改前查关系与影响面，见下表 |
| `finish` | `{"content":"..."}` | 你主动收尾：纯分析/回答/已完成 → 结束本轮、交还用户 |
| `finish` | `{"finding_json":{"findings_summary":"…","findings":[{"index":1,"severity":"high","file":"…","issue":"…","recommendation":"…"}]}}` | 需用户审核的 plan/bug/将改动 → 门禁**仅校验**，等 c 确认后继续 |
| `load_skill` | `{"name":"..."}` | 加载项目 skill |

## code_graph — 代码知识图谱（改前先建关系模型）

GitNexus 用图存储符号与关系（调用/导入/继承/实现/字段读写），比 grep 更懂代码怎么连在一起。
**动手改/重构前**用它核对影响面；拿不准谁会受影响时别盲改。`op` 选能力，其余字段按该 op 透传。
图谱不可用时自动降级（提示用 file_read/find_symbol/code_search），不阻塞。

| op | params | 用途 |
|----|--------|------|
| `query` | `{"query":"概念/关键词", "limit?":5}` | 概念 → 相关执行流(调用链)、符号、定义 |
| `context` | `{"name":"符号"}` 或 `{"uid":"..."}` | 单符号 360°：谁调它/它调谁、字段读写、参与的流程 |
| `impact` | `{"target":"符号", "direction":"upstream\|downstream", "maxDepth?":3}` | 改动爆炸半径：哪些会断(d=1)/受影响(d=2) + 风险等级 |
| `detect_changes` | `{"scope?":"unstaged\|staged\|all"}` | 未提交 git 改动 → 受影响的执行流(提交前自查) |
| `api_impact` | `{"route":"/api/x"}` 或 `{"file":"..."}` | 改 API 路由前：消费者、响应字段、中间件、风险 |
| `route_map` / `tool_map` / `shape_check` | `{"route?\|tool?":"..."}` | API 路由/工具映射、响应结构与消费方不匹配检测 |
| `cypher` | `{"query":"MATCH ..."}` | 复杂结构化图查询(先了解 schema) |
| `rename` | `{"symbol_name":"旧","new_name":"新","dry_run?":true}` | 跨文件协调重命名(默认仅预览，不改文件) |
| `list_repos` / `group_list` / `group_sync` | `{}` / `{"name":"..."}` | 已索引仓库、多仓 group 管理 |

典型顺序：探索时 `query`/`context` 建模 → 改前 `impact`/`api_impact` 看影响面 → file_read 核证 → edit_file → 提交前 `detect_changes` 复查。

## 结束本轮（finish 是你**主动**收尾的动作）

- `finish` 由你深思后主动调用：结束本轮、把控制权交还用户。结束后下一条用户输入会**自然继续**，不会被锁。
- 门禁(finding_json)与工具只执行/校验，**永不替你结束**：
  - **有 finding_json** → 弹门禁，用户 `c` 确认后写权限解锁、自动实施；改完代码仍需**你自己** `finish(content)` 收尾。
  - **无 finding_json** → `content` 展示到 chat，本轮收尾，等用户新输入。
- 中间想说明但还要继续 → 文字放进本次回复随下一个工具动作一起；**不要**用 finish 投递中间内容。
- 分析/解释只是文本，放 `content`；不要塞进 finding_json。

## 上下文

以系统提示词和 [TURN_CONTEXT] 为准。工具调用结果在上下文中可见。
