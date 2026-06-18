/// Workflow step definition
#[derive(Debug, Clone)]
pub struct WorkflowStep {
    pub id: String,
    pub name: String,
    pub description: String,
    pub requires_user_confirmation: bool,
    pub allow_tool_execution: bool,
    pub allow_code_modification: bool,
    pub step_prompt: String,
    pub validator_name: Option<String>,
    pub allowed_tools: Vec<String>,
    pub memory_layers: Vec<String>,
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
            allow_code_modification: true,
            step_prompt: String::new(),
            validator_name: None,
            allowed_tools: Vec::new(),
            memory_layers: Vec::new(),
            display_status: String::new(),
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

#[derive(Debug, Clone)]
pub struct Workflow {
    pub id: String,
    pub name: String,
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

pub const DEFAULT_WORKFLOW_ID: &str = "single_step";
pub const LEGACY_WORKFLOW_ID: &str = "four_step_pipeline";

const TASK_PROMPT: &str = "\
【任务】完成用户请求。每轮二选一：**调工具** 或 **交产物**。

【用户请求】{USER_REQUEST}
【用户补充】{USER_GUIDANCE}

【输出纪律】已注入 system prompt；每轮 LLM 前会刷新摘要，遵守 ox-output-discipline。

【计划性 — 复杂/多文件任务】
先输出 `## Plan`，用 checkbox 列出步骤，逐项推进；完成的标 `- [x]`：
## Plan
- [ ] 步骤一
- [ ] 步骤二
最终 ## Done 时所有项必须是 `- [x]`（门禁校验，未完成会被打回）。

【审查任务 — 输出 findings】
检查/审查代码时，除文字报告外，附独立 findings JSON 块；之后修复时 ## Done 须带 completion_receipt 对应每条 finding：
```json
{
  \"findings_summary\": \"≥30字结论\",
  \"findings\": [
    {\"index\":1,\"severity\":\"high|medium|low\",\"file\":\"路径\",\"target\":\"类/方法\",\"issue\":\"问题\",\"recommendation\":\"建议\"}
  ]
}
```

{COMPLETION_RECEIPT_SCHEMA}

【工具】file_read, file_list, file_search, code_search, find_symbol, project_detect, \
file_write, edit_file, delete_range, shell_exec, git_status, git_diff, load_skill

【完成格式】
## Done
<做了什么 + 验证结果，1–3 行>
（有计划则附最终勾选状态；有 findings 则附 completion_receipt JSON）
";

/// Single-step agent workflow — no Intent/Plan/Review pipeline.
pub fn create_default_workflow() -> Workflow {
    let mut workflow = Workflow::new(DEFAULT_WORKFLOW_ID, "Agent");
    workflow.add_step(
        WorkflowStep::new("task", "Task", "Complete the user's request")
            .with_prompt(TASK_PROMPT)
            // allowed_tools left empty → registry exposes all built-in tools (no whitelist filter)
            .with_memory_layers(&[
                "WorkingMemory",
                "AtomicMemory",
                "EpisodicMemory",
                "SemanticMemory",
                "CodeSymbol",
            ])
            .with_display_status("⚡ Agent"),
    );
    workflow
}

/// Legacy stubs — single-step model has no execute modes.
pub fn execute_mode_banner(_engine: &crate::agent::engine::WorkflowEngine) -> &'static str {
    "【Agent】"
}

pub fn execute_mode_rules(_engine: &crate::agent::engine::WorkflowEngine) -> &'static str {
    ""
}
