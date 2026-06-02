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

        let scope = args.get("scope").and_then(|s| s.as_str()).unwrap_or("both");

        let max_results = args
            .get("max_results")
            .and_then(|m| m.as_u64())
            .unwrap_or(5)
            .min(20) as usize;

        // Access memory system through ToolContext
        let memory = &ctx.memory;
        let project_id = if scope != "global" {
            Some(ctx.runtime.project_id.as_str())
        } else {
            None
        };

        // Perform the search
        let nodes = memory.retrieve(query, &project_id, max_results);

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

        for (i, node) in nodes.iter().enumerate() {
            let scope_tag = if node.project_id.is_some() {
                "[Project]"
            } else {
                "[Global]"
            };

            let type_tag = format!("[{}]", node.node_type.as_str());

            // Calculate confidence score based on depth and recency
            let confidence = calculate_confidence(node);
            let confidence_bar = format_confidence_bar(confidence);

            // Format source information
            let source_info = format_source(&node.source);

            output.push_str(&format!(
                "{}. {} {} (Depth: {} | Confidence: {})\n\
                   Source: {}\n\
                   {}\n\n",
                i + 1,
                scope_tag,
                type_tag,
                node.depth,
                confidence_bar,
                source_info,
                node.content
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

/// Calculate confidence score based on depth and node type
fn calculate_confidence(node: &crate::memory::MemoryNode) -> f32 {
    // Depth is a good indicator of reliability (0-5 scale)
    let depth_score = (node.depth as f32 / 5.0).min(1.0);

    // Node type affects confidence
    let type_weight = match node.node_type {
        crate::memory::MemoryNodeType::Architectural => 0.9,
        crate::memory::MemoryNodeType::BestPractice => 0.85,
        crate::memory::MemoryNodeType::Style => 0.8,
        crate::memory::MemoryNodeType::MetaSkill => 0.85,
        crate::memory::MemoryNodeType::AntiPattern => 0.8,
        crate::memory::MemoryNodeType::Business => 0.75,
        crate::memory::MemoryNodeType::Pattern => 0.75,
        crate::memory::MemoryNodeType::Fact => 0.7,
    };

    (depth_score * 0.6 + type_weight * 0.4).clamp(0.0, 1.0)
}

/// Format confidence as a visual bar
fn format_confidence_bar(confidence: f32) -> String {
    let filled = (confidence * 10.0).round() as usize;
    let empty = 10 - filled;
    format!(
        "{}{:.0}%",
        "█".repeat(filled) + &"░".repeat(empty),
        confidence * 100.0
    )
}

/// Format source information in human-readable form
fn format_source(source: &crate::memory::MemorySource) -> String {
    match source {
        crate::memory::MemorySource::ToolObservation => "🔧 Observed from tool output",
        crate::memory::MemorySource::LlmExtraction => "🤖 Extracted by LLM",
        crate::memory::MemorySource::UserExplicit => "👤 User explicitly stated",
        crate::memory::MemorySource::Feedback => "💬 From user feedback",
        crate::memory::MemorySource::RefinedSummary => "✨ Refined conversation summary",
    }
    .to_string()
}
