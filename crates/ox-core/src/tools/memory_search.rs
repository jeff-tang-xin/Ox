//! Memory search tool - enables LLM to actively query project and global knowledge.
//!
//! This tool allows the LLM to retrieve relevant context during task analysis,
//! design, and coding phases. The LLM can query for:
//! - Architectural decisions
//! - Code conventions and patterns
//! - User preferences and working style
//! - Historical issues and solutions
//! - Best practices and anti-patterns

use serde_json::Value;
use std::sync::Arc;

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
        "Search project and global knowledge base for relevant context. Use to recall architecture, conventions, user preferences, or past solutions."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "✅ REQUIRED: Natural language query. Describe what knowledge you need."
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "global", "both"],
                    "description": "Search scope. Default: both.",
                    "default": "both"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max results (1-20). Default: 5.",
                    "default": 5,
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": ["query"],
            "examples": [
                {"query": "authentication architecture", "scope": "project"},
                {"query": "error handling conventions", "scope": "project"},
                {"query": "coding style preferences", "scope": "global"}
            ]
        })
    }

    fn safety_level(&self) -> crate::tools::SafetyLevel {
        crate::tools::SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let query = match args.get("query").and_then(|q| q.as_str()) {
            Some(q) if !q.is_empty() => q,
            _ => {
                return ToolOutput::error(
                    "❌ Missing Required Parameter: 'query'\n\n\
                 💡 How to use:\n\
                 • Describe what knowledge you need in natural language\n\
                 • Be specific about the type (architecture, conventions, preferences, etc.)\n\n\
                 📝 Examples:\n\
                 {\"query\": \"authentication architecture\", \"scope\": \"project\"}\n\
                 {\"query\": \"error handling conventions\", \"scope\": \"project\"}\n\
                 {\"query\": \"user coding style preferences\", \"scope\": \"global\"}\n\
                 {\"query\": \"common Rust web API patterns\", \"scope\": \"global\"}",
                );
            }
        };

        let _scope = args.get("scope").and_then(|s| s.as_str()).unwrap_or("both");

        let max_results = args
            .get("max_results")
            .and_then(|m| m.as_u64())
            .unwrap_or(5)
            .min(20) as usize;

        // Access knowledge engine through ToolContext
        let knowledge = Arc::clone(&ctx.knowledge);
        let _project_id = ctx.runtime.project_id.clone();
        let query_owned = query.to_string();

        let nodes = tokio::task::spawn(async move {
            let engine = knowledge.lock().await;
            engine.retrieve_memories(&query_owned, max_results)
                .unwrap_or_default()
                .into_iter()
                .map(|h| h.entity)
                .collect::<Vec<_>>()
        }).await.unwrap_or_default();

        if nodes.is_empty() {
            return ToolOutput::success(format!(
                "🔍 No relevant knowledge found for '{}'.\n\n\
                 💡 Suggestions:\n\
                 • Try rephrasing your query with different keywords\n\
                 • Broaden the scope (e.g., change from 'project' to 'both')\n\
                 • Check if this knowledge needs to be captured first",
                query
            ));
        }

        // Format results with clear structure
        let mut output = format!(
            "🔍 Found {} relevant knowledge items for '{}':\n\n",
            nodes.len(),
            query
        );

        for (i, entity) in nodes.iter().enumerate() {
            let kind_tag = format!("[{}]", entity.kind.as_str());
            let depth = entity.coordinate.depth;

            output.push_str(&format!(
                "{}. {} (Depth: {})\n   {}\n\n",
                i + 1,
                kind_tag,
                depth,
                entity.content
            ));
        }

        // Add usage guidance
        output.push_str(
            "💡 Tip: Use this information to inform your approach. \
                        If you need more details, try a more specific query.\n",
        );

        ToolOutput::success(output)
    }
}
