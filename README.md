# Ox 🐂

> **终端 AI 编程助手 — 懂项目、有记忆、会反思、能写代码**

Ox 是基于 Rust 构建的终端 AI 编程助手，通过 TUI 界面与 LLM 交互，提供代码读写、搜索、执行能力。具备**四层渐进式记忆**、**上下文精炼**、**自动反思生成 Skill**、**上下文卸载**和**编辑前确认机制**。

---

## ✨ 核心特性

| 特性 | 说明 |
|------|------|
| 🤖 **智能代码助手** | 读取/搜索/编写/补丁代码，执行 Shell 命令，自动检测项目语言 |
| 🧠 **四层渐进记忆** | L0 原始对话 → L1 原子事实 → L2 场景分块 → L3 项目画像（SQLite + Markdown 混合存储） |
| 🔄 **上下文精炼** | 自动将冗长对话提炼为紧凑摘要，移除思考块，保留关键决策 |
| 📦 **上下文卸载** | 长工具输出自动存文件，上下文只保留摘要+引用，Token 节省 60%+ |
| 🪞 **自动反思 & Skill 生成** | 工作流完成后自动分析执行轨迹，提取可复用模式生成 Skill |
| ⚠️ **编辑前确认** | 修改代码前必须征求用户确认（最高优先级规则） |
| 🔧 **工作流引擎** | 引导式任务执行，步骤验证 + 用户确认 + 回退支持 |
| 🛡️ **分层安全防护** | 工具分级确认、风险命令警告、信任模式、连续错误限制 |
| 💬 **交互式反馈** | 随时打断 AI，隐式反馈检测，EMA 趋势追踪 |

---

## 🚀 快速开始

### 1. 安装

```bash
git clone https://github.com/jeff-tang-xin/Ox.git
cd Ox
cargo install --path crates/ox-cli
```

### 2. 配置 API Key

```bash
# Linux/macOS
export OPENAI_API_KEY=sk-...

# Windows PowerShell
$env:OPENAI_API_KEY="sk-..."

# 自定义端点
export OPENAI_BASE_URL=https://api.openai.com/v1
```

### 3. 启动

```bash
ox
```

---

## 💬 基础交互

直接输入即可对话：

```
> 帮我重构 auth 模块的登录逻辑
> 添加用户头像上传功能
> 为什么这个测试会失败？
```

命令行模式：

```bash
# 解释代码
ox "解释这段代码" --file src/auth.rs

# 无 TUI 模式
ox --no-tui "帮我写一个快速排序"
```

---

## ⚡ Slash 命令

### 常用命令

| 命令 | 说明 |
|------|------|
| `/exit` | 退出程序 |
| `/clear` | 清空当前会话 |
| `/debug` | 切换调试模式 |
| `/cost` | 显示 token 消耗 |
| `/reload` | 重新加载配置 |
| `/init` | 初始化新项目 |
| `/cd <path>` | 切换工作目录 |
| `/cancel` | 取消当前操作 |
| `/plan` | 查看当前会话计划 |
| `/free` | 切换到自由模式 |
| `/skill` | 管理 Skill |
| `/trust` | 管理信任模式 |

### 反馈命令

| 命令 | 说明 |
|------|------|
| `/Y` | 确认/同意 |
| `/N` | 拒绝 |
| `/O <text>` | 提供替代方案或反馈 |

### 记忆管理

| 命令 | 说明 |
|------|------|
| `/memory show` | 显示当前记忆 |
| `/memory search <query>` | 搜索记忆 |
| `/memory transform` | 手动触发记忆转换 |

---

## 🔧 配置

### 配置文件位置

- **Linux/macOS**: `~/.config/ox/config.toml`
- **Windows**: `%APPDATA%\ox\config.toml`

### 完整配置示例

```toml
# ── 模型配置 ──────────────────────────────────────
[models]
default = "gpt-4o"
backup = ["claude-sonnet-4", "gpt-4-turbo"]
adaptive_thinking = true
effort_level = "high"

[models.providers.openai]
api_key = "sk-..."  # 或使用环境变量
base_url = "https://api.openai.com/v1"
max_tokens = 4096

[models.providers.anthropic]
api_key = "sk-ant-..."

# ── Agent 配置 ───────────────────────────────────
[agent]
max_iterations = 25
max_per_turn_tokens = 500000

# ── 安全配置 ─────────────────────────────────────
[safety]
enable_sandbox = false
confirm_dangerous_ops = true
high_risk_apis = [
    "Command::new",
    "remove_dir_all",
    "fs::remove_dir_all",
]

# ── 记忆配置 ─────────────────────────────────────
[memory]
max_nodes = 1000
alpha = 0.8
time_decay = 0.01
isolation_application = true
share_session_group = true

# LLM 裁判重排
enable_llm_judge = true
llm_judge_threshold = 7

# ── 行为规则 ─────────────────────────────────────
[behavior_rules]
enforce_safe_code = true
enforce_lint = true
enforce_format = true
enforce_tests = true
enforce_all = true

# ── 成本限制 ─────────────────────────────────────
[cost]
max_monthly_cost = 5.0
max_daily_cost = 2.0
budget_alert_threshold = 0.8
cost_transparency = true

# ── 会话配置 ─────────────────────────────────────
[session]
auto_restore = true
archive_on_exit = true
```

### 环境变量

| 变量 | 说明 |
|------|------|
| `OPENAI_API_KEY` | OpenAI API Key |
| `ANTHROPIC_API_KEY` | Anthropic API Key |
| `OPENAI_BASE_URL` | 自定义 API 端点 |
| `OX_CONFIG_PATH` | 自定义配置文件路径 |

---

## 🏗️ 架构

```
Ox/
├── crates/
│   ├── ox-cli/                    # 命令行界面 (TUI + 终端)
│   │   └── src/
│   │       ├── main.rs            # 入口 & 主循环
│   │       ├── terminal/          # TUI 渲染
│   │       │   ├── app.rs         # 应用状态管理
│   │       │   ├── render.rs      # 渲染逻辑
│   │       │   ├── input_pane.rs  # 输入面板
│   │       │   ├── output_pane.rs # 输出面板
│   │       │   └── markdown.rs    # Markdown 渲染
│   │       ├── slash_commands/    # Slash 命令实现
│   │       ├── middleware/        # 中间件（压缩、反馈）
│   │       └── helpers/           # 辅助函数（上下文构建等）
│   │
│   └── ox-core/                   # 核心逻辑库
│       └── src/
│           ├── agent/             # Agent 引擎
│           │   ├── engine.rs      # 主循环 & 工具执行
│           │   ├── workflow.rs    # 工作流定义
│           │   ├── interrupt.rs   # 中断处理
│           │   ├── interjection.rs# 用户打断
│           │   ├── intervention.rs# 干预机制
│           │   ├── auto_reflect.rs# 🆕 自动反思 → Skill 生成
│           │   ├── context_offloader.rs # 🆕 上下文卸载
│           │   ├── enforcer.rs    # 规则执行器
│           │   ├── session.rs     # 会话管理
│           │   └── progress.rs    # 进度追踪
│           ├── llm/               # LLM 抽象层
│           │   ├── openai.rs      # OpenAI 适配器
│           │   ├── openai_sse.rs  # OpenAI SSE 流式解析
│           │   ├── anthropic.rs   # Anthropic 适配器
│           │   └── sse.rs         # 通用 SSE 解析
│           ├── memory/            # 记忆系统
│           │   ├── store.rs       # SQLite 存储
│           │   ├── semantic.rs    # 语义关联
│           │   ├── layering.rs    # 🆕 四层渐进记忆 (L0-L3)
│           │   ├── hybrid_storage.rs # 🆕 SQLite+Markdown 混合存储
│           │   └── mod.rs         # 记忆管理器
│           ├── context/           # 上下文管理
│           │   ├── system_prompt.rs # 系统提示词构建
│           │   ├── refinement.rs  # 🆕 上下文精炼
│           │   ├── compressed_store.rs # 压缩存储
│           │   └── effort.rs      # 努力度评估
│           ├── tools/             # 工具注册表
│           │   ├── file_read.rs   # 文件读取
│           │   ├── file_write.rs  # 文件写入
│           │   ├── file_patch.rs  # 文件补丁
│           │   ├── file_list.rs   # 文件列表
│           │   ├── file_search.rs # 文件搜索
│           │   ├── code_search.rs # 代码搜索 (ripgrep)
│           │   ├── shell_exec.rs  # Shell 执行
│           │   ├── memory_search.rs # 记忆搜索
│           │   ├── project_detect.rs # 项目检测
│           │   ├── web_fetch.rs   # URL 获取
│           │   └── content_validation.rs # 内容校验
│           ├── skill/             # 🆕 Skill 系统
│           │   ├── mod.rs         # Skill 定义 (System/Global/Project)
│           │   ├── generation.rs  # Skill 自动生成
│           │   └── builtin/       # 内置 Skill
│           ├── safety/            # 安全检查 & 信任管理
│           ├── cost/              # 成本追踪
│           ├── config/            # 配置管理
│           └── feedback/          # 反馈系统
│
└── release/                       # 发布脚本
```

---

## 📖 设计理念

### 1. 四层渐进记忆 🧠

受 TencentDB-Agent-Memory 启发的分层记忆架构：

| 层级 | 名称 | 存储 | 说明 |
|------|------|------|------|
| **L0** | 原始对话 (Raw Conversation) | SQLite | 完整对话记录 |
| **L1** | 原子事实 (Atom Fact) | SQLite | 提炼出的独立事实条目 |
| **L2** | 场景分块 (Scenario Chunk) | Markdown | 按场景组织的知识片段，人类可读 |
| **L3** | 项目画像 (Project Persona) | Markdown | 项目架构、风格、偏好的全局画像 |

**混合存储**：L0-L1 用 SQLite（快速索引查询），L2-L3 用 Markdown（人类可读白盒文件），通过 `node_id` 双向溯源。

**语义关联**：自动建立同义词、共现、层级关系，提升检索召回率。

### 2. 上下文精炼 🔄

自动将冗长对话提炼为紧凑格式：

- 移除 `≸... McKay` 思考块，只保留结论
- 提取每轮关键决策和工具使用记录
- 格式：`User: 消息\nAssistant: 摘要 [工具列表] ✏️`

效果：Token 节省 **25-30%**，语义完整性 **+20%**。

### 3. 上下文卸载 📦

长工具输出自动卸载到 `.ox/refs/` 目录：

- 完整结果保存到 `{session}_{step}.md`
- 上下文只保留摘要 + `node_id` 引用
- 需要 `recall` 时按 `node_id` 回溯完整内容
- Token 节省 **60%+**

### 4. 自动反思 & Skill 生成 🪞

工作流完成后自动触发：

1. 分析执行轨迹，识别可复用模式
2. 生成 Skill（Markdown 格式），存入 `.ox/skills/` 或 `~/.ox/skills/`
3. Skill 分三级作用域：System（内置）、Global（用户级）、Project（项目级）
4. 后续类似任务自动匹配已有 Skill

### 5. 编辑前确认 ⚠️

LLM 在使用编辑工具前必须：
1. 读取文件了解当前状态
2. 提出方案（改什么、为什么）
3. 请求用户确认
4. 获得确认后才执行

适用：`file_write`、`file_patch`、`shell_exec`
无需确认：`file_read`、`file_search`、`memory_search`

### 6. 分层安全防护 🛡️

- 工具分级：低/中/高风险，不同确认策略
- 信任模式：可信项目免确认
- 连续错误限制：3 次失败后暂停
- 高风险 API 监控：`Command::new`、`remove_dir_all` 等

---

## 🛠️ 开发

```bash
cargo build                    # Debug 构建
cargo build --release          # Release 构建
cargo test                     # 运行测试
cargo clippy -- -D warnings    # 静态检查
cargo fmt                      # 格式化代码
```

---

## ⚠️ 已知限制

- 单文件超过 10 万行时需分批处理
- 部分提供商有严格并发限制
- 完全依赖外部 LLM API，无法离线工作

---

## 📄 许可证

MIT License
