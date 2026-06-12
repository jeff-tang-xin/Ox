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
    let mut workflow = Workflow::new("five_step_pipeline", "4-Step Pipeline");

    // ── Step 0: Intent Classification ──
    workflow.add_step(
        WorkflowStep::new("step0_intent", "Intent", "Classify user intent")
            .with_prompt("\
【任务】分析用户意图，输出分类结果。

【输出格式】只输出 JSON：
{\"intent\": \"coding\"|\"exploring\"|\"chat\",
 \"complexity\": \"simple\"|\"complex\",
 \"files\": [\"提到的文件\"],
 \"topic\": \"主题关键词\"}

【规则】
- 输出 JSON 后立即结束，不要调用工具
- coding: 需要读写改代码
- exploring: 探索项目结构、理解架构
- chat: 闲聊、问答、一般帮助")
            .disallow_tools()
            .with_memory_layers(&["WorkingMemory"])
            .with_display_status("🤔 分析意图")
    );

    // ── Step 1: Explore + Plan ──
    workflow.add_step(
        WorkflowStep::new("step1_plan", "Plan", "Explore and make a detailed plan")
            .with_prompt("\
【任务】探索项目，制定详细的执行计划。

【上一步意图】{PREVIOUS_OUTPUT}

【工具使用规则】
- project_detect — 只调一次，结果不变
- file_list <dir> — 列出目录内容，可多次用于不同子目录
- file_read <file> — 读文件内容
- find_symbol <name> — 按函数/结构体/方法名查找定义位置（如 find_symbol handle_key）
- code_search <pattern> — 按文本模式搜索（如搜索 TODO、Ctrl+C 等字符串）
- load_skill <name> — 加载项目 skill
- 已读过的文件不要重复读

【步骤】
1. 探索项目结构和相关代码
2. 加载需要的 skill
3. 制定详细执行计划
4. 输出 JSON（禁止输出其他内容）

【输出格式】
{
  \"plan\": [
    {{
      \"step\": 1,
      \"file\": \"文件路径\",
      \"action\": \"add|modify|delete|create|explain\",
      \"target\": \"函数/结构体/模块名\",
      \"desc\": \"具体做什么、怎么做的描述\",
      \"verify\": \"验证方法（cargo check / test / 读回文件）\"
    }}
  ],
  \"skills\": [\"需要的 skill 名\"],
  \"key_files\": [\"涉及的关键文件路径\"]
}

【规则】
- desc 必须具体！\"修改函数\"不合格，\"在 handle_key 的 match 前添加 Ctrl+S 判断\"合格
- 每个 step 必须有 target
- 先读代码再写计划
- 输出 JSON 后立即结束")
            .with_allowed_tools(&["file_read", "file_list", "file_search", "code_search",
                                  "find_symbol", "project_detect", "load_skill",
                                  "memory_search", "recall"])
            .allow_tools_disallow_code()
            .with_memory_layers(&["EpisodicMemory", "SemanticMemory"])
            .with_display_status("📋 探索+规划")
    );

    // ── Step 2: Review (Safety + Completeness) ──
    workflow.add_step(
        WorkflowStep::new("step2_review", "Review", "Review plan for safety and completeness")
            .with_prompt("\
【任务】审阅上一步生成的计划，检查安全性和完整性。

【上一步计划】{PREVIOUS_OUTPUT}

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
- 基于上一步探索结果来评估，不要猜测
- safe=false → 必须列出具体原因
- complete=false → 必须列出缺少什么")
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

【全部前序输出】{ALL_PREVIOUS_OUTPUTS}

【规则】
1. 按照计划中的步骤顺序执行
2. 修改前用 file_read / find_symbol 确认当前代码
3. 用 file_write / edit_file / delete_range 执行修改
4. 每步修改后按要求验证（cargo check / test / file_read）
5. 全部完成后输出 ## Done

不要重复探索。探索已在计划阶段完成。直接执行。")
            .with_memory_layers(&["WorkingMemory", "AtomicMemory"])
            .with_display_status("⚡ 执行")
    );

    workflow
}
