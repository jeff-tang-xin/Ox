//! Memory search tool - queries project and global memories.
//!
//! This tool is a placeholder. Memory context is pre-injected into conversation
//! turns by main.rs via memory.retrieve() and memory.format_memory_context().
//! Full memory search capability will be implemented with proper thread-safe storage.

use serde_json::Value;

use crate::tools::{Tool, ToolContext, ToolOutput};

/// Search memories for relevant context.
pub struct MemorySearchTool;

impl MemorySearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemorySearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search project and global memories for relevant context. Returns architectural \
        decisions, code patterns, previous work, and relevant facts. Use this when you \
        need to recall information from previous sessions or check project conventions."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query describing what context you need"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5, max: 20)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    fn safety_level(&self) -> crate::tools::SafetyLevel {
        crate::tools::SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let query = args["query"].as_str().unwrap_or("");

        if query.is_empty() {
            return ToolOutput::error("query parameter is required");
        }

        // Memory retrieval is handled by main.rs before each turn via memory.retrieve()
        // and pre-injected into the conversation context.
        ToolOutput::success(format!(
            "Memory search for '{}': Memory context is pre-injected into this session. \
             Relevant memories are automatically retrieved and included based on your query. \
             Use /memory command for manual memory management.",
            query
        ))
    }
}
