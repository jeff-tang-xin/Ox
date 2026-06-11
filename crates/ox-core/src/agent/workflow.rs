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

/// Create the default 5-step pipeline workflow — used for ALL user interactions.
///
/// Step 1 (Intent Classify): L0 WorkingMemory — recent turns
/// Step 2 (Task Planning): L2 EpisodicMemory + L3 SemanticMemory — history + patterns
/// Step 3 (Parameter Extract): L0 WorkingMemory + CodeSymbol — context completion
/// Step 4 (Safety Check): L1 AtomicMemory — rules, anti-patterns
/// Step 5 (Execution): L0 WorkingMemory + L1 AtomicMemory — state + tooling experience
pub fn create_default_workflow() -> Workflow {
    let mut workflow = Workflow::new("five_step_pipeline", "5-Step Pipeline");

    // ── Step 1: Intent Classification ──
    workflow.add_step(
        WorkflowStep::new("step1_intent", "Intent Classification", "Analyze user intent")
            .with_prompt("\
【任务】你是意图分类器。分析用户的消息，输出 JSON 分类结果。

【输出格式】只输出一个 JSON 对象：
{\"intent\": \"coding\"|\"exploring\"|\"chat\",
 \"complexity\": \"simple\"|\"complex\",
 \"files\": [\"提到的文件路径\"],
 \"topic\": \"主题关键词\"}

【规则】
- 输出 JSON 后立即结束，不要输出其他内容，不要调用工具
- coding: 需要读/写/改代码
- exploring: 探索项目结构、解释架构
- chat: 闲聊、问候、一般问题")
            .disallow_tools()
            .with_memory_layers(&["WorkingMemory"])
            .with_display_status("🤔 分析意图")
    );

    // ── Step 2: Explore + Task Planning ──
    workflow.add_step(
        WorkflowStep::new("step2_plan", "Task Planning", "Explore codebase then plan the task")
            .with_prompt("\
【任务】先探索项目，再制定计划。

【上一步分析】{PREVIOUS_OUTPUT}

【步骤】
1. 用 file_read / file_list / find_symbol / code_search 了解相关代码
2. 加载需要的 skill
3. 制定具体执行计划
4. 输出 JSON：
{\"plan\": [\"步骤1\", \"步骤2\"],
 \"skills\": [\"必须加载的skill名\"],
 \"estimated_steps\": 步骤数,
 \"key_files\": [\"涉及的关键文件路径\"]}

【规则】
- 先探索再规划：不了解代码就制定的计划毫无价值
- 最多 3 轮探索（file_read/file_list/project_detect），然后必须输出 JSON
- 已经看过的文件不要再重复看
- 输出 JSON 后立即结束，不要继续探索")
            .with_allowed_tools(&["file_read", "file_list", "file_search", "code_search",
                                  "find_symbol", "project_detect", "load_skill",
                                  "memory_search", "recall"])
            .allow_tools_disallow_code()
            .with_memory_layers(&["EpisodicMemory", "SemanticMemory"])
            .with_display_status("📋 探索+规划")
    );

    // ── Step 3: Parameter Extraction ──
    workflow.add_step(
        WorkflowStep::new("step3_params", "Parameter Extraction", "Extract structured parameters from context")
            .with_prompt("\
【任务】你是参数提取器。从用户消息和上下文提取结构化的执行参数。

【上一步规划】{PREVIOUS_OUTPUT}

【输出格式】只输出 JSON：
{\"target_file\": \"文件路径\",
 \"target_symbol\": \"函数/类名\",
 \"action\": \"add|modify|delete|explain\",
 \"description\": \"变更描述\"}

【规则】
- 输出 JSON 后立即结束，不要调用工具
- 利用上一步的规划和\"最近上下文\"来补全参数
- 如果参数不足，在 missing 字段列出需要用户补充的信息
- 不要编造值 — 不确定就标为 null")
            .disallow_tools()
            .with_memory_layers(&["WorkingMemory", "CodeSymbol"])
            .with_display_status("🔎 提取参数")
    );

    // ── Step 4: Safety Check ──
    workflow.add_step(
        WorkflowStep::new("step4_safety", "Safety Check", "Verify operation safety before execution")
            .with_prompt("\
【任务】你是安全审计器。评估要执行的操作是否安全。

【上一步参数】{PREVIOUS_OUTPUT}

【输出格式】只输出 JSON：
{\"safe\": true|false,
 \"reason\": \"不安全的原因（safe=true 时为空）\",
 \"warnings\": [\"需要注意的事项（可选）\"]}

【规则】
- 输出 JSON 后立即结束，不要调用工具
- 基于上一步的参数来评估安全性
- 参考下面的「用户规则」和「已知反模式」
- 涉及删除文件、破坏性命令、跨项目操作 → 标记 unsafe")
            .disallow_tools()
            .with_validator("safety_check")
            .with_memory_layers(&["AtomicMemory"])
            .with_display_status("🛡️ 安全检查")
    );

    // ── Step 5: Execution ──
    workflow.add_step(
        WorkflowStep::new("step5_execute", "Execution", "Execute the planned task")
            .with_prompt("\
【任务】执行计划。探索已在 Step 2 完成，现在直接执行代码修改。

【前序步骤全部输出】{ALL_PREVIOUS_OUTPUTS}

【规则】
1. 需要时用 file_read 查看具体代码行
2. 用 file_write / edit_file / delete_range 执行修改
3. 改后验证（读回或 cargo check / cargo test）
4. 完成全部修改后输出 ## Done

不要重复探索。Step 2 已经完成了探索。直接执行。")
            .with_memory_layers(&["WorkingMemory", "AtomicMemory"])
            .with_display_status("⚡ 执行")
    );

    workflow
}
