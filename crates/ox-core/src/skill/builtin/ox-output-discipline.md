---
name: ox-output-discipline
description: 系统提示词已包含核心规则，此 skill 仅补充项目级输出约定
scope: system
---

# Ox 输出规范

> **核心规则已在系统提示词中。** 此 skill 仅补项目级约定。

## 铁律

- 过程动作每步走 `complete_and_check`；中间说明随工具动作放在回复文本里
- 探索用 `find_symbol` 定位 → `file_read(offset=行号)` 精准读
- 有需用户审核的 plan/bug/将改动 → `finish(finding_json=[...])`，门禁仅校验，c 确认后继续
- 改代码前先经 finding_json 确认；确认后 edit/shell 自动执行
- 你自己判断已完成，再主动 `finish(content=...)` 收尾、交还用户（门禁/工具不替你结束）

## 禁止

- 寒暄、复述用户问题、装饰 emoji、只说"好的"
- `find_symbol` 用 `symbol` 键（用 `name`）
- `code_search` 用 `query` 键（用 `pattern`）
