# Ox 项目总结

> 🐂 AI 编程助手 — 终端里的智能体，懂项目、记得偏好、能写代码

**技术栈**: Rust (edition 2024) + Tokio + Ratatui TUI + SQLite  
**许可证**: MIT  
**作者**: Jeff Tang

---

## 📌 一句话概述

Ox 是一个基于 Rust 开发的 AI 编程助手，通过精美的 TUI 界面连接 LLM，为开发者提供代码读取、搜索、编写和执行能力，同时具备强大的记忆系统和多模式工作流管理。

---

## 🏗️ 项目架构

```
Ox/
├── crates/
│   ├── ox-cli/           # TUI 应用入口 (~2648 行 main.rs)
│   │   └── terminal/     # UI 组件（app, event, input, output, markdown, render）
│   └── ox-core/          # 核心库
│       ├── agent/         # 智能体循环 + 工作流引擎
│       ├── llm/          # OpenAI / Anthropic 提供商
│       ├── memory/       # SQLite 记忆系统
│       ├── embedding/    # BGE 嵌入 + KadaneDial 压缩
│       ├── council/      # 多模型辩论系统
│       ├── tools/        # 12+ 内置工具
│       └── ...
└── docs/                 # 技术文档
```

---

## ✨ 核心特性矩阵

| 类别 | 特性 | 说明 |
|------|------|------|
| **界面** | Ratatui TUI | Markdown 渲染、语法高亮、鼠标滚轮 |
| **LLM** | 多提供商 | OpenAI / Anthropic，自动降级回显模式 |
| **工具** | 12+ 内置 | 文件操作、代码搜索、Shell、Git、记忆查询 |
| **记忆** | SQLite 持久化 | DEWMA/ACT-R 衰减，Janitor 清理 |
| **压缩** | BGE 嵌入 | KadaneDial 算法，自动压缩长对话 |
| **工作流** | 三模式系统 | Free / Spec / Council 统一管理 |
| **议会** | 多模型辩论 | 提案→评审→反驳→仲裁 |
| **安全** | 信任管理 | 工具确认机制、危险操作保护 |
| **成本** | 实时追踪 | Token 统计、费用上限 |

---

## 🔄 三种工作模式

| 模式 | 用途 | 代码修改 | 工作流步数 |
|------|------|----------|------------|
| **Free** | 日常对话 | ✅ 允许 | 1 步 |
| **Spec** | 任务开发 | ⚠️ 仅最后一步 | 6 步 |
| **Council** | 架构决策 | ❌ 禁止 | 6 步 |

---

## 🛠️ 内置工具集

| 工具 | 功能 |
|------|------|
| `file_read/write/patch/list/search` | 文件操作 |
| `code_search` | 正则/文本代码搜索 |
| `shell_exec` | Shell 命令执行 |
| `git_status/diff/commit` | Git 集成 |
| `memory_search` | 记忆知识查询 |
| `project_detect` | 项目类型检测 |
| `web_fetch` | 网页抓取 |

---

## 🧠 记忆系统

- **类型**: Fact / Architectural / Business / Style / AntiPattern / Council
- **来源**: 用户指令 / 工具观察 / LLM 提取 / 议会结论 / 反馈
- **查询**: LLM 主动调用，60s TTL 缓存，50x 性能提升
- **衰减**: DEWMA (项目) + ACT-R MCM (长期) + Janitor 自动清理

---

## 📁 数据存储

| 路径 | 用途 |
|------|------|
| `~/.ox/config.toml` | 用户配置 |
| `~/.ox/sessions/` | 会话历史 (JSONL) |
| `~/.ox/db/` | SQLite (记忆/压缩/费用) |
| `~/.ox/logs/` | 日志 |
| `<项目>/.ox/` | 项目级配置 |

---

## ⚙️ 技术栈详情

| 组件 | 技术 |
|------|------|
| 语言 | Rust 1.85+ (edition 2024) |
| 运行时 | Tokio |
| TUI | Ratatui + Crossterm |
| HTTP | Reqwest |
| 高亮 | Syntect |
| 数据库 | rusqlite |
| 嵌入模型 | Candle + BGE (ModelScope) |
| 日志 | tracing |

---

## 🚀 快速使用

```bash
# 构建
cargo build --release

# 运行
./target/release/ox

# 或直接运行
cargo run
```

---

## ⌨️ 常用命令

| 命令 | 说明 |
|------|------|
| `/help` | 显示帮助 |
| `/new` | 新会话 |
| `/spec on` | 激活规范模式 |
| `/council start <topic>` | 启动辩论 |
| `/memory` | 记忆统计 |
| `/cost` | 费用统计 |
| `/trust --all` | 信任所有工具 |

---

## 📚 文档资源

- `docs/Workflow_Engine_完整调用流程.md` - 工作流引擎详细文档
- `docs/main_rs_analysis.md` - main.rs 重构建议

---

## 🔮 未来演进方向

1. **主动推理** - LLM 预测用户需求
2. **知识演化** - 从查询频率学习
3. **双层专家** - Global + Project Expert

---

**核心价值**: 懂项目 + 记得偏好 + 安全可控 + 自我进化
