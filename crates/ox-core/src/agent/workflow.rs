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
        }
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

/// Create Spec mode workflow
pub fn create_spec_workflow() -> Workflow {
    let mut workflow = Workflow::new("spec_workflow", "Spec Mode Workflow");
    
    // Step 1: Requirement Analysis - Tools allowed, no code modification
    workflow.add_step(
        WorkflowStep::new(
            "requirement_analysis",
            "Requirement Analysis",
            "Analyze user request and classify task type (Complex/Simple/Exploratory)"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 1: Analyze the task type. You can use file_read, file_search, code_search to understand the codebase. Do NOT modify any code files yet. When you complete the analysis, respond with [STEP_COMPLETE] to proceed.")
        .with_validator("check_task_classified")
    );
    
    // Step 2: Generate spec.md - Tools allowed, can create docs but not modify code
    workflow.add_step(
        WorkflowStep::new(
            "generate_spec",
            "Generate Specification",
            "Create spec.md with goal, constraints, output files, verification criteria"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 2: For Complex tasks:\n1. Generate a short, descriptive name for this requirement (e.g., 'user-auth', 'payment-integration')\n2. Create directory `.ox/{requirement_name}/`\n3. Use file_write tool to create `.ox/{requirement_name}/spec.md` with:\n   - Goal (one sentence)\n   - Constraints (technical, style, testing)\n   - Output Files (list of files to modify/create)\n   - Verification Criteria (how to verify success)\n\nYou CAN create documentation files but CANNOT modify source code files (.rs, .py, .js, etc). After creating spec.md, respond with [STEP_COMPLETE].")
        .with_validator("check_spec_file_exists")
    );
    
    // Step 3: User confirmation on spec - No tools, wait for user
    workflow.add_step(
        WorkflowStep::new(
            "await_spec_confirmation",
            "Await Spec Confirmation",
            "Wait for user to review and confirm the specification"
        )
        .require_confirmation()
        .disallow_tools()
        .with_prompt("STEP 3: Present spec.md to user and WAIT for confirmation. Do NOT call any tools. Do NOT proceed without explicit user approval.")
    );
    
    // Step 4: Generate task.md - Tools allowed, can create docs but not modify code
    workflow.add_step(
        WorkflowStep::new(
            "generate_task",
            "Generate Task Plan",
            "Create task.md with step-by-step execution plan"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 4: Use the SAME requirement_name from Step 2. Use file_write tool to create `.ox/{requirement_name}/task.md` with:\n- Steps (numbered list)\n- Verification for each step\n- Status tracking (- [ ] / - [x])\n\nYou CAN create documentation files but CANNOT modify source code files. After creating task.md, respond with [STEP_COMPLETE].")
        .with_validator("check_task_file_exists")
    );
    
    // Step 5: Final confirmation before execution - No tools, wait for user
    workflow.add_step(
        WorkflowStep::new(
            "await_task_confirmation",
            "Await Task Confirmation",
            "Wait for final user approval before code execution"
        )
        .require_confirmation()
        .disallow_tools()
        .with_prompt("STEP 5: Present task.md to user and WAIT for final confirmation. Do NOT call any tools. Do NOT execute any code yet.")
    );
    
    // Step 6: Execute code - Full tool access including code modification
    workflow.add_step(
        WorkflowStep::new(
            "execute_code",
            "Execute Code",
            "Perform actual code modifications according to task plan"
        )
        .with_prompt("STEP 6: NOW you can modify source code files. Execute the task plan. Update task.md progress as you work. Mark completed steps with - [x].")
    );
    
    workflow
}

/// Create Council mode workflow
pub fn create_council_workflow() -> Workflow {
    let mut workflow = Workflow::new("council_workflow", "Council Mode Workflow");
    
    // Step 1: Topic Definition - Tools allowed for research, no code modification
    workflow.add_step(
        WorkflowStep::new(
            "topic_definition",
            "Topic Definition",
            "Define the discussion topic and generate meeting name"
        )
        .require_confirmation()
        .allow_tools_disallow_code()
        .with_prompt("STEP 1: Generate a short, descriptive name for this meeting (e.g., 'architecture-review', 'tech-stack-selection'). Use file_read/file_search to understand context if needed. Wait for user confirmation.")
    );
    
    // Step 2-5: Debate phases - Tools allowed for research, no code modification
    workflow.add_step(
        WorkflowStep::new(
            "proposal_phase",
            "Proposal Phase",
            "Multiple agents submit their proposals"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 2: Agents submit proposals. You can use tools for research but CANNOT modify source code files.")
    );
    
    workflow.add_step(
        WorkflowStep::new(
            "review_phase",
            "Review Phase",
            "Agents critique and review proposals"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 3: Agents review and critique proposals. You can use tools for research but CANNOT modify source code files.")
    );
    
    workflow.add_step(
        WorkflowStep::new(
            "rebuttal_phase",
            "Rebuttal Phase",
            "Agents defend their proposals against criticism"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 4: Agents defend their proposals. You can use tools for research but CANNOT modify source code files.")
    );
    
    workflow.add_step(
        WorkflowStep::new(
            "arbitration",
            "Arbitration",
            "Final decision and synthesis of best ideas"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 5: Synthesize best ideas and make final decision. You can use tools for research but CANNOT modify source code files.")
    );
    
    // Step 6: Conclusion - Save meeting record, still no code modification
    workflow.add_step(
        WorkflowStep::new(
            "conclusion",
            "Conclusion",
            "Save meeting record and summarize conclusions"
        )
        .require_confirmation()
        .allow_tools_disallow_code()
        .with_prompt("STEP 6: Use the SAME meeting_name from Step 1. Use file_write tool to create `.ox/{meeting_name}/council_record.md` with:\n- Meeting Topic\n- Proposals Submitted\n- Key Arguments\n- Final Decision\n- Action Items\n\nYou CAN create documentation files but CANNOT modify source code files. Wait for user confirmation.")
    );
    
    workflow
}

/// Create Free mode workflow (single step, no restrictions)
pub fn create_free_workflow() -> Workflow {
    let mut workflow = Workflow::new("free_workflow", "Free Exploration Workflow");
    
    workflow.add_step(
        WorkflowStep::new(
            "free_interaction",
            "Free Interaction",
            "Open-ended conversation and coding assistance"
        )
    );
    
    workflow
}
