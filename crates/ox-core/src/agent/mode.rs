/// Agent mode definition - each mode has its own workflow and behavior rules
#[derive(Debug, Clone)]
pub struct AgentMode {
    /// Unique mode identifier
    pub id: String,
    /// Display name
    pub name: String,
    /// Description
    pub description: String,
    /// Associated workflow ID
    pub workflow_id: String,
    /// Whether this mode allows tool execution
    pub allow_tools: bool,
    /// System prompt template for this mode
    pub system_prompt_template: String,
}

impl AgentMode {
    pub fn new(id: &str, name: &str, description: &str, workflow_id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            workflow_id: workflow_id.to_string(),
            allow_tools: true,
            system_prompt_template: String::new(),
        }
    }
    
    pub fn free_mode() -> Self {
        let mut mode = Self::new(
            "free",
            "Free Exploration",
            "Default mode for open-ended conversation and coding",
            "free_workflow"
        );
        mode.allow_tools = true;
        mode
    }
    
    pub fn spec_mode() -> Self {
        let mut mode = Self::new(
            "spec",
            "Spec Mode",
            "Structured task planning with mandatory specification workflow",
            "spec_workflow"
        );
        mode.system_prompt_template = include_str!("../../context/spec.rs").to_string();
        mode
    }
    
    pub fn council_mode() -> Self {
        let mut mode = Self::new(
            "council",
            "Council Mode",
            "Multi-agent debate and discussion for complex decisions",
            "council_workflow"
        );
        mode.allow_tools = false; // Council mode uses internal dialogue
        mode
    }
}
