/// Workflow step definition
#[derive(Debug, Clone)]
pub struct WorkflowStep {
    /// Step identifier
    pub id: String,
    /// Display name
    pub name: String,
    /// Description of what to do in this step
    pub description: String,
    /// Whether user confirmation is required before proceeding
    pub requires_user_confirmation: bool,
    /// Whether tool execution is allowed in this step
    pub allow_tool_execution: bool,
    /// Whether code file modification is allowed (only applies when allow_tool_execution=true)
    pub allow_code_modification: bool,
    /// System prompt fragment for this step (injected into context)
    pub step_prompt: String,
    /// Optional validation function name (registered in StateRegistry)
    pub validator_name: Option<String>,
    /// Allowed tools whitelist (empty = all tools allowed if allow_tool_execution=true)
    pub allowed_tools: Vec<String>,
    /// Memory layers to inject for this step (empty = default retrieval)
    /// Uses EntityKind values: "WorkingMemory", "AtomicMemory", "EpisodicMemory", "SemanticMemory", "CodeSymbol"
    pub memory_layers: Vec<String>,
    /// Short user-facing status label (e.g., "🤔 Thinking", "📋 Planning", "⚡ Action")
    pub display_status: String,
}

impl WorkflowStep {
    pub fn new(id: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            requires_user_confirmation: false,
            allow_tool_execution: true,
            allow_code_modification: true, // Default to allowing code modification
            step_prompt: String::new(),
            validator_name: None,
            allowed_tools: Vec::new(), // Empty means all tools allowed
            memory_layers: Vec::new(), // Empty means default retrieval
            display_status: String::new(), // Will be set by with_display_status()
        }
    }

    pub fn with_display_status(mut self, status: &str) -> Self {
        self.display_status = status.to_string();
        self
    }

    pub fn require_confirmation(mut self) -> Self {
        self.requires_user_confirmation = true;
        self
    }

    pub fn disallow_tools(mut self) -> Self {
        self.allow_tool_execution = false;
        self.allow_code_modification = false;
        self
    }

    pub fn allow_tools_disallow_code(mut self) -> Self {
        self.allow_tool_execution = true;
        self.allow_code_modification = false;
        self
    }

    pub fn with_prompt(mut self, prompt: &str) -> Self {
        self.step_prompt = prompt.to_string();
        self
    }

    pub fn with_validator(mut self, validator_name: &str) -> Self {
        self.validator_name = Some(validator_name.to_string());
        self
    }

    pub fn with_allowed_tools(mut self, tools: &[&str]) -> Self {
        self.allowed_tools = tools.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_memory_layers(mut self, layers: &[&str]) -> Self {
        self.memory_layers = layers.iter().map(|s| s.to_string()).collect();
        self
    }
}

/// Workflow definition - ordered sequence of steps
#[derive(Debug, Clone)]
pub struct Workflow {
    /// Unique workflow identifier
    pub id: String,
    /// Display name
    pub name: String,
    /// Ordered list of steps
    pub steps: Vec<WorkflowStep>,
}

impl Workflow {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            steps: Vec::new(),
        }
    }

    pub fn add_step(&mut self, step: WorkflowStep) {
        self.steps.push(step);
    }

    pub fn get_step(&self, index: usize) -> Option<&WorkflowStep> {
        self.steps.get(index)
    }

    pub fn total_steps(&self) -> usize {
        self.steps.len()
    }
}

/// Default workflow identifier (4 steps: Intent → Plan → Review → Execute).
pub const DEFAULT_WORKFLOW_ID: &str = "four_step_pipeline";

/// Legacy alias kept for sessions persisted before the rename.
pub const LEGACY_WORKFLOW_ID: &str = "five_step_pipeline";

/// Create the default 4-step pipeline workflow — used for ALL user interactions.
///
/// Step 1 (Intent):   L0 WorkingMemory — recent context
/// Step 2 (Plan):     L2 EpisodicMemory + L3 SemanticMemory — history + patterns + exploration
/// Step 3 (Review):   L1 AtomicMemory — rules, anti-patterns, safety review of the plan
/// Step 4 (Execute):  L0 WorkingMemory + L1 AtomicMemory — state + tooling experience
///
/// This pipeline is generic: it works for coding, exploring, debugging, and chat tasks.
/// Only Step 2 and Step 4 allow tool calls; Step 1 and Step 3 are JSON-only.
pub fn create_default_workflow() -> Workflow {
    let mut workflow = Workflow::new(DEFAULT_WORKFLOW_ID, "4-Step Pipeline");

    // ── Step 0: Intent Classification ──
    workflow.add_step(
        WorkflowStep::new("step0_intent", "Intent", "Classify user intent")
            .with_prompt("\
【任务】分析用户意图，判定走哪条工作流路径，并说明理由。

【用户原话】{USER_REQUEST}
{ROUTING_HINT}

【路由表 — pipeline 由 intent + complexity 唯一决定，必须严格匹配】
| intent     | complexity | pipeline   | 实际步骤 |
|------------|------------|------------|----------|
| chat       | 任意       | chat       | 直接自然语言回复，不走工作流 |
| exploring  | 任意       | fast       | 意图 → **人工确认** → 只读执行（跳过规划/审阅） |
| ops        | 任意       | ops-fast   | 运维/发布 → **系统 Preflight** → **人工确认** → 执行（跳过规划/审阅） |
| coding     | simple     | fast       | 意图 → **人工确认** → 执行（跳过规划/审阅） |
| coding     | complex    | standard   | 意图 → 规划 → 审阅 → **人工确认** → 执行 |

【重要】跳过规划/审阅 ≠ 自动执行。fast 路径在意图分析后必须先经用户确认，才会进入执行步骤。

【intent 判定】
- exploring：只读 — 检查/审查/分析代码、理解架构、找问题、给建议，**不修改文件**
- ops：运维/发布 — git tag、push、release、发版、部署、changelog 等（不改源码）
- coding：会改代码 — 新增/修改/删除/重构/修 bug/实现功能
- chat：闲聊、概念问答、与当前项目无关

【complexity 判定（仅 coding）】
- simple：单文件或 ≤2 文件、改动行数少、逻辑直观（改文案、修 typo、加一行）
- complex：多文件、架构级、重构、不确定影响面、需先探索再改

【需求澄清 — needs_clarification】
在路由前判断用户原话是否**足以开工**。**有疑问必须澄清，禁止猜测或假设用户未说明的对象、范围、约束。**
以下情况设 needs_clarification=true 并列出 1–3 个**具体、可回答**的问题：
- 模糊动词无对象（如「加个验证」「优化一下」）
- 多种合理解释且选择会显著改变方案（改 A 文件 vs 改 B 模块）
- 缺少关键约束（语言/框架/范围/验收标准）且原文未给出
- 任何你不确定、需要用户确认才能开工的情况
以下情况 needs_clarification=false（须原文已明确，不得凭常识补全）：
- 意图、范围、目标已在用户原话中写清；exploring 全景检查；chat 问答
- 用户已给出文件路径 + 具体改动描述

【输出格式】只输出 JSON，不要调用工具：
{
  \"intent\": \"coding\"|\"exploring\"|\"ops\"|\"chat\",
  \"complexity\": \"simple\"|\"complex\",
  \"files\": [\"用户提到的文件路径\"],
  \"topic\": \"一句话主题\",
  \"pipeline\": \"fast\"|\"ops-fast\"|\"standard\"|\"chat\",
  \"routing_reason\": \"≥15字：说明为何选此 intent/complexity/pipeline；若 pipeline=fast 须写明跳过规划/审阅但仍需人工确认后执行\",
  \"needs_clarification\": true|false,
  \"clarification_questions\": [\"需用户回答的具体问题（needs_clarification=false 时为空数组）\"]
}

【示例】
用户：「检查下整个项目的代码」
→ exploring + complex + pipeline=fast（只读检查走快速路径，不生成修改计划）
用户：「把 main.rs 里的 typo 改掉」
→ coding + simple + pipeline=fast
用户：「重构 agent 模块并拆分 engine.rs」
→ coding + complex + pipeline=standard（必须走规划+审阅）
用户：「给当前 commit 打 v1.2.0 tag 并 push」
→ ops + simple + pipeline=ops-fast（系统 Preflight 探测 tag/状态后人工确认再执行）
用户：「Rust 的所有权是什么」
→ chat + pipeline=chat

【规则】
- pipeline 必须与上表一致，填错会被要求重试
- routing_reason 必须写明「跳过规划/审阅」或「需要规划+审阅」的原因；fast 路径须注明「待人工确认后执行」
- needs_clarification=true 时 routing 字段**仅反映用户原话已明确的信息**；files 不得虚构；routing_reason 须列出「待澄清项」，**禁止最佳推测**
- 输出 JSON 后立即结束")
            .disallow_tools()
            .with_memory_layers(&["WorkingMemory"])
            .with_display_status("🤔 分析意图")
    );

    // ── Step 1: Explore + Plan ──
    workflow.add_step(
        WorkflowStep::new("step1_plan", "Plan", "Explore and make a detailed plan")
            .with_prompt("\
【任务】边探索边起草计划：每轮工具调用后更新认知，探索充分后输出 plan JSON。

【上一步意图】{PREVIOUS_OUTPUT}

【审阅回退意见（如有）】{REVIEW_FEEDBACK}

【用户中途补充（采纳后继续当前 Plan，勿回 Intent）】{USER_GUIDANCE}

【探索阶段查阅内容】{EXPLORATION_SNAPSHOT}

【并行模式 — 探索与起草同步】
- 探索未完成时：每完成 1-2 次工具调用后，可在回复开头写 `## 计划草稿`（≤8 行 Markdown，非最终 JSON）
- 草稿记录：已确认路径、拟改文件、步骤概要
- 探索门禁满足后：将草稿整理为正式 plan JSON 一次输出，不要再调工具

【探索门禁 — 全部满足后才能输出 plan JSON（适用任何语言/构建系统）】
1. project_detect — 了解项目类型与构建方式（第一步）
2. 目录探索 — 满足以下任一组合：
   • 分层布局：file_list 根目录 + file_list 至少一个子目录 + file_read ≥1
   • 扁平/小项目：file_list ≥1 + file_read ≥2
   • 搜索确认：file_list ≥1 + file_read ≥1 + find_symbol/code_search/file_search ≥1
3. 不要假设固定目录名（src/、crates/、packages/ 等）— 以 file_list 实际结果为准
4. 计划中的每个 file 路径，必须已通过 file_list（父目录）或 file_read 确认存在

【工具使用规则】
- project_detect — 只调一次
- file_list <dir> — 【只列单层】当前目录的直接子项；要看子目录内容必须再调 file_list(\"子目录路径\")，逐层向下
- file_search <glob> — 按文件名递归搜索（如 *.rs），不是 file_list
- file_read — 大文件默认只读 200 行，用 offset/limit 续读
- file_read — 读入口、配置、或计划将修改的文件
- find_symbol / code_search / file_search — 确认符号/模块存在
- load_skill — 加载【按需】skill（内置/全局/项目扩展）；项目规范与业务指导已自动注入正文，无需再 load

【制定计划】
- **必须先遵守**上方【项目 Skill — 必读】中的规范与业务指导，再起草 plan
- structure_summary：写明检测到的项目类型、实际目录布局、入口文件位置
- 每个 plan 步骤的 file 必须是探索中已确认的路径
- desc / verify 写具体、可执行

【输出格式】探索完成后只输出 JSON：
{
  \"structure_summary\": \"≥40字：项目类型、实际目录布局、入口/关键文件\",
  \"plan\": [
    {
      \"step\": 1,
      \"file\": \"已探索确认存在的文件路径\",
      \"action\": \"add|modify|delete|create|explain\",
      \"target\": \"函数/类/模块名\",
      \"desc\": \"具体做什么、怎么做的描述（≥15字）\",
      \"verify\": \"验证方法\"
    }
  ],
  \"skills\": [\"需要的 skill 名\"],
  \"key_files\": [\"涉及的关键文件路径\"]
}

【规则】
- 探索不足时继续调用工具，不要猜测路径
- 输出 JSON 后立即结束

{WORKFLOW_PHASE}")
            .with_allowed_tools(&["file_read", "file_list", "file_search", "code_search",
                                  "find_symbol", "project_detect", "load_skill",
                                  "memory_search", "recall"])
            .allow_tools_disallow_code()
            .with_memory_layers(&["CodeSymbol", "EpisodicMemory", "SemanticMemory"])
            .with_display_status("📋 探索+规划")
    );

    // ── Step 2: Review (Safety + Completeness) ──
    workflow.add_step(
        WorkflowStep::new("step2_review", "Review", "Review plan for safety and completeness")
            .with_prompt("\
【任务】审阅上一步生成的计划，检查安全性和完整性。

【上一步计划】{PREVIOUS_OUTPUT}

【探索阶段查阅内容】{EXPLORATION_SNAPSHOT}

【检查项】
1. 安全性：是否有删除文件、危险命令、跨项目操作？
2. 完整性：计划是否覆盖了用户意图的全部？
3. 可行性：每个步骤的文件和函数是否已在探索中确认存在？
4. 依赖：步骤顺序是否正确？有无遗漏的前置步骤？

【输出格式】只输出 JSON：
{\"safe\": true|false,
 \"complete\": true|false,
 \"issues\": [\"发现的问题（safe/complete 为 false 时必填）\"],
 \"warnings\": [\"需要注意但可继续的事项（可选）\"]}

【规则】
- 输出 JSON 后立即结束，不要调用工具
- 仅依据【上一步计划】和【探索阶段查阅内容】评估，不要猜测
- 不要使用 file_read / file_list — 探索内容已在 EXPLORATION_SNAPSHOT 中
- safe=false → 必须列出具体原因
- complete=false → 必须列出缺少什么

{WORKFLOW_PHASE}")
            .disallow_tools()
            .with_validator("safety_check")
            .with_memory_layers(&["AtomicMemory"])
            .with_display_status("🛡️ 审阅计划")
    );

    // ── Step 3: Execute ──
    workflow.add_step(
        WorkflowStep::new("step3_execute", "Execute", "Execute the plan")
            .with_prompt("\
【任务】按照计划逐步执行。

{WORKFLOW_PHASE}

【执行交接包 — 用户已确认】
{EXECUTE_HANDOFF}

【用户中途补充（采纳后继续 Execute，勿回 Intent/Plan）】{USER_GUIDANCE}

【全部前序输出】{ALL_PREVIOUS_OUTPUTS}

【探索 / Preflight 快照】{EXPLORATION_SNAPSHOT}

【规则 — 按当前阶段执行】
1. **必须先遵守**上方【项目 Skill — 必读】中的项目规范与业务指导
2. **感知阶段**：只读探索；结束时输出 findings JSON + 审查报告 + `## Done`
3. **执行阶段**：消费【计划进度】清单；禁止退回探索；读后即改
4. 按照计划中的步骤顺序执行（`action: shell` 用 shell_exec）
5. 全部完成后输出 ## Done

{FINDINGS_SCHEMA}")
            .with_allowed_tools(&[
                "file_read",
                "file_list",
                "file_search",
                "code_search",
                "find_symbol",
                "file_write",
                "edit_file",
                "delete_range",
                "shell_exec",
                "git_status",
                "git_diff",
                "load_skill",
            ])
            .with_memory_layers(&["WorkingMemory", "AtomicMemory"])
            .with_display_status("⚡ 执行")
    );

    workflow
}
