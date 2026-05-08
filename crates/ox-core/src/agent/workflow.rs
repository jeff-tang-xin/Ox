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

    pub fn with_allowed_tools(mut self, tools: &[&str]) -> Self {
        self.allowed_tools = tools.iter().map(|s| s.to_string()).collect();
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

/// Create Spec mode workflow (Simplified 3-Phase)
pub fn create_spec_workflow() -> Workflow {
    let mut workflow = Workflow::new("spec_workflow", "Spec Mode Workflow (3-Phase)");

    // ═══════════════════════════════════════════════════════════
    // PHASE 1: Requirement Analysis & Documentation
    // ═══════════════════════════════════════════════════════════

    workflow.add_step(
        WorkflowStep::new(
            "phase_1_documentation",
            "Phase 1: Requirements & Documentation",
            "Analyze requirements, generate requirement name, create spec.md and task.md"
        )
        .allow_tools_disallow_code()
        .with_allowed_tools(&["file_read", "file_search", "code_search", "project_detect", "file_write"])
        .with_prompt("## 🚨 CRITICAL: FILE PATH REQUIREMENT - READ CAREFULLY 🚨\n\n**YOU MUST FOLLOW THIS RULE OR THE WORKFLOW WILL FAIL:**\n\nThe directory `.ox/{REQUIREMENT_NAME}/` has ALREADY been created for you.\nYou ONLY need to create the CONTENT of spec.md and task.md files.\n\n❌ WRONG: `.ox/spec.md` (MISSING requirement name!)\n❌ WRONG: `.ox/main-rs-refactor/spec.md` (WRONG format!)\n✅ CORRECT: `.ox/order-optimization/spec.md`\n✅ CORRECT: `.ox/user-auth/task.md`\n\n**STEP-BY-STEP PROCESS:**\n\n1️⃣ **Use the EXISTING requirement name**\n   - The directory `.ox/{REQUIREMENT_NAME}/` already exists\n   - DO NOT try to create the directory yourself\n   - Just use this path when writing files\n\n2️⃣ **Create file CONTENT using file_write tool**\n   - spec.md → `.ox/{YOUR_NAME}/spec.md`\n   - task.md → `.ox/{YOUR_NAME}/task.md`\n   - Example: If name is 'order-optimization':\n     * `{\"path\": \".ox/order-optimization/spec.md\", \"content\": \"...\"}`\n     * `{\"path\": \".ox/order-optimization/task.md\", \"content\": \"...\"}`\n\n⚠️ **WARNING:** Use EXACTLY the same requirement name that was used to create the directory. Do NOT invent a different name.\n\n---\n\n**Your Task:** Complete Phase 1 documentation:\n\n### Step 1: Identify Requirement Name\n- The requirement name has already been generated by the system\n- Check which `.ox/spec/<name>/` directory exists\n- Use THAT exact name in your file paths\n\n### Step 2: Create spec.md Content\nFile: `.ox/{IDENTIFIED_NAME}/spec.md`\n\nFormat:\n```markdown\n# {Title}\n\n## Goal\n{One sentence}\n\n## Constraints\n- MUST: {Critical}\n- SHOULD: {Important}\n- MAY: {Optional}\n\n## Output Files\n- `path/to/file` - Description\n\n## Verification Criteria\n- [ ] Criterion 1\n```\n\n### Step 3: Create task.md Content\nFile: `.ox/{IDENTIFIED_NAME}/task.md`\n\nFormat:\n```markdown\n# Task Plan: {Title}\n\n## Prerequisites\n- [ ] Precondition\n\n## Steps\n### Step 1: Title\n**Action**: What to do\n**Files**: Which files\n**Verify**: How to verify\n```\n\n---\n\n**After completing BOTH files:**\nRespond EXACTLY:\n```\n✅ Phase 1 Complete!\n\nFiles created:\n- .ox/{YOUR_NAME}/spec.md\n- .ox/{YOUR_NAME}/task.md\n\nPlease review and confirm:\n/Y - Approve and proceed to Phase 2 (Code Execution)\n/N - Reject and abort workflow\n/O - Provide feedback for revision\n```\n\n**CRITICAL:** After outputting this message, STOP calling tools. Wait for /Y, /N, or /O.")
        .require_confirmation()
    );

    // ═══════════════════════════════════════════════════════════
    // PHASE 2: Code Execution & Verification
    // ═══════════════════════════════════════════════════════════

    workflow.add_step(
        WorkflowStep::new(
            "phase_2_execution",
            "Phase 2: Code Execution & Verification",
            "Execute the task plan, modify source code, run tests, and verify results"
        )
        .with_prompt("## PHASE 2: Code Execution & Verification\n\n🎉 User approved Phase 1! Now execute the implementation.\n\n**Your Task:**\n1. Read `.ox/{requirement_name}/task.md`\n2. Execute each step in order\n3. Update task.md progress: change `- [ ]` to `- [x]` after each step\n4. Verify each step before moving to next\n5. Run tests and verification commands\n\n**You CAN now:**\n- ✅ Modify source code files (.rs, .py, .js, etc.)\n- ✅ Use ALL tools (file_write, file_patch, shell_exec, etc.)\n- ✅ Run tests: `cargo test`, `pytest`, etc.\n- ✅ Run linting: `cargo clippy`, `eslint`, etc.\n\n**After completing all tasks and verification:**\nRespond with EXACTLY:\n```\n✅ Phase 2 Complete!\n\nAll tasks executed and verified.\nTest Results: {summary}\n\nPlease confirm to proceed to Phase 3 (Summary):\n/Y - Approve and generate summary\n/N - Reject (report issues)\n/O - Request changes\n```\n\nIf issues found, report them clearly:\n```\n❌ Issues found:\n- {Issue 1}\n- {Issue 2}\n\nPlease use /O to provide feedback or /Y to proceed anyway.\n```\n\n**CRITICAL:** After outputting this message, DO NOT call any more tools. Wait for user's /Y, /N, or /O command.")
        .require_confirmation()
    );

    // ═══════════════════════════════════════════════════════════
    // PHASE 3: Summary & Archival
    // ═══════════════════════════════════════════════════════════

    workflow.add_step(
        WorkflowStep::new(
            "phase_3_summary",
            "Phase 3: Summary & Archival",
            "Generate final summary report and archive the requirement"
        )
        .allow_tools_disallow_code()
        .with_allowed_tools(&["file_write"])
        .with_prompt("## PHASE 3: Summary & Archival\n\n🎉 Final phase! Generate a comprehensive summary.\n\n**Your Task:**\nCreate `.ox/{requirement_name}/summary.md` with:\n\n```markdown\n# Summary: {Requirement Title}\n\n## Overview\n{Brief description of what was implemented}\n\n## Changes Made\n- Modified: {list of files}\n- Created: {list of files}\n- Deleted: {list of files}\n\n## Test Results\n- Tests Run: {count}\n- Tests Passed: {count}\n- Coverage: {percentage if available}\n\n## Key Decisions\n- {Decision 1}\n- {Decision 2}\n\n## Lessons Learned\n- {Lesson 1}\n- {Lesson 2}\n\n## Next Steps\n- [ ] {Recommendation 1}\n- [ ] {Recommendation 2}\n```\n\n**After creating summary.md:**\nRespond with EXACTLY:\n```\n🎊 Workflow Complete!\n\nAll phases completed successfully.\nDocuments archived in: .ox/{requirement_name}/\n- spec.md\n- task.md\n- summary.md\n\nThank you for using Ox Spec Mode!\n```\n\n**This is the final step. No further user confirmation needed.**")
    );

    workflow
}

/// Create Council mode workflow
pub fn create_council_workflow() -> Workflow {
    let mut workflow = Workflow::new("council_workflow", "Council Mode Workflow");

    // Step 1: Topic Definition - Requires user confirmation
    workflow.add_step(
        WorkflowStep::new(
            "topic_definition",
            "Topic Definition",
            "Define the discussion topic and generate meeting name"
        )
        .require_confirmation()
        .allow_tools_disallow_code()
        .with_prompt("STEP 1: Generate a short, descriptive name for this meeting (e.g., 'architecture-review', 'tech-stack-selection'). Use file_read/file_search to understand context if needed.\n\nAfter generating the meeting name, respond with:\n```\nMeeting Name: {your-generated-name}\nTopic: {brief description}\n\nPlease confirm to start the council debate:\n/Y - Approve and begin debate\n/N - Cancel\n/O - Provide feedback\n```\n\n**CRITICAL:** After outputting this message, DO NOT call any more tools. Wait for user's /Y, /N, or /O command.")
    );

    // Step 2-5: Debate phases - NO confirmation needed, let AI debate freely
    workflow.add_step(
        WorkflowStep::new(
            "proposal_phase",
            "Proposal Phase",
            "Multiple agents submit their proposals"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 2: Agents submit proposals. You can use tools for research but CANNOT modify source code files. Proceed automatically to next phase after completion.")
    );

    workflow.add_step(
        WorkflowStep::new(
            "review_phase",
            "Review Phase",
            "Agents critique and review proposals"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 3: Agents review and critique proposals. You can use tools for research but CANNOT modify source code files. Proceed automatically to next phase after completion.")
    );

    workflow.add_step(
        WorkflowStep::new(
            "rebuttal_phase",
            "Rebuttal Phase",
            "Agents defend their proposals against criticism"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 4: Agents defend their proposals. You can use tools for research but CANNOT modify source code files. Proceed automatically to next phase after completion.")
    );

    workflow.add_step(
        WorkflowStep::new(
            "arbitration",
            "Arbitration",
            "Final decision and synthesis of best ideas"
        )
        .allow_tools_disallow_code()
        .with_prompt("STEP 5: Synthesize best ideas and make final decision. You can use tools for research but CANNOT modify source code files. Proceed automatically to next phase after completion.")
    );

    // Step 6: Conclusion - Save meeting record, requires user confirmation
    workflow.add_step(
        WorkflowStep::new(
            "conclusion",
            "Conclusion",
            "Save meeting record and summarize conclusions"
        )
        .require_confirmation()
        .allow_tools_disallow_code()
        .with_allowed_tools(&["file_write"])
        .with_prompt("## STEP 6: Save Meeting Record\n\n**Your Task:**\nUse the SAME meeting_name from Step 1. Use file_write tool to create `{project_ox_dir}/{meeting_name}/council_record.md`\n\n**MANDATORY council_record.md Format:**\n\n```markdown\n# Council Record: {Meeting Topic}\n\n## Meeting Info\n- **Date**: {YYYY-MM-DD}\n- **Participants**: {List of AI models/agents}\n- **Topic**: {Brief description of discussion topic}\n\n## Proposals Submitted\n\n### Proposal 1: {Title}\n**Agent**: {Agent name/model}\n**Summary**: {Key points}\n**Pros**: {Advantages}\n**Cons**: {Disadvantages}\n\n### Proposal 2: {Title}\n**Agent**: {Agent name/model}\n**Summary**: {Key points}\n**Pros**: {Advantages}\n**Cons**: {Disadvantages}\n\n## Key Arguments\n- {Argument 1: What was debated}\n- {Argument 2: What was debated}\n- {Argument 3: What was debated}\n\n## Final Decision\n{Clear statement of the chosen approach and why}\n\n## Action Items\n- [ ] {Action 1: Who does what}\n- [ ] {Action 2: Who does what}\n- [ ] {Action 3: Who does what}\n\n## Consensus Level\n{High/Medium/Low - How much agreement among participants}\n```\n\n**After creating council_record.md:**\nRespond with EXACTLY:\n```\n✅ Council debate completed!\n\nMeeting record saved to: .ox/{meeting_name}/council_record.md\n\nPlease confirm:\n/Y - Approve and finish\n/N - Discard results\n/O - Request changes\n```\n\n**CRITICAL:** After outputting this message, DO NOT call any more tools. Wait for user's /Y, /N, or /O command.")
    );

    workflow
}

/// Create Free mode workflow (single step, no restrictions)
pub fn create_free_workflow() -> Workflow {
    let mut workflow = Workflow::new("free_workflow", "Free Exploration Workflow");

    workflow.add_step(WorkflowStep::new(
        "free_interaction",
        "Free Interaction",
        "Open-ended conversation and coding assistance",
    ));

    workflow
}
