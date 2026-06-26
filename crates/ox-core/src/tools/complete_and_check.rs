use serde_json::Value;

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub const TOOL_NAME: &str = "complete_and_check";

/// Placeholder `Tool` — execution is handled by [`crate::agent::unified_handler`] in `mod.rs`.
pub struct CompleteAndCheckTool;

#[async_trait::async_trait]
impl Tool for CompleteAndCheckTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        "Unified action router (executed by agent runtime, not directly)."
    }

    fn parameters_schema(&self) -> Value {
        crate::agent::unified_action::tool_schema().parameters
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::error("complete_and_check must be routed by agent runtime")
    }
}
