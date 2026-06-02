//! Recall tool — retrieve offloaded tool results and memory nodes.
//!
//! Supports two modes:
//! 1. `node_id` — retrieve full content from .ox/refs/{node_id}.md
//! 2. `query` — search memory nodes via MemoryManager

use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct RecallTool;

#[async_trait::async_trait]
impl Tool for RecallTool {
    fn name(&self) -> &str {
        "recall"
    }

    fn description(&self) -> &str {
        "Retrieve full content of offloaded tool results or search memory. \
         Use this when a previous tool output was summarized with a node_id, \
         or when you need to recall past knowledge. \
         Provide either `node_id` (to fetch a specific offloaded result) or \
         `query` (to search memory nodes by keyword). \
         Example: {\"node_id\": \"session_1_step3_20250601_120000\"} \
         Example: {\"query\": \"auth module architecture\"}"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The node_id from a previous 'Result saved to .ox/refs/{node_id}.md' message"
                },
                "query": {
                    "type": "string",
                    "description": "Search query for memory nodes (project knowledge, past decisions, code patterns)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max memory search results. Default 5.",
                    "minimum": 1,
                    "maximum": 20
                }
            }
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // Mode 1: retrieve by node_id
        if let Some(node_id) = args.get("node_id").and_then(|v| v.as_str()) {
            return retrieve_by_node_id(node_id, ctx);
        }

        // Mode 2: search memory
        if let Some(query) = args.get("query").and_then(|v| v.as_str()) {
            let max_results = args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            return search_memory(query, max_results, ctx);
        }

        ToolOutput::error(
            "recall requires either `node_id` or `query` parameter. \
             Use `node_id` to fetch an offloaded tool result, \
             or `query` to search project memory."
        )
    }
}

fn retrieve_by_node_id(node_id: &str, ctx: &ToolContext) -> ToolOutput {
    use crate::agent::context_offloader::ContextOffloader;

    let offloader = ContextOffloader::new(&ctx.working_dir, "recall");

    match offloader.retrieve_full_content(node_id) {
        Some(content) => {
            tracing::info!("[RECALL] Retrieved offloaded content for node_id: {}", node_id);
            ToolOutput::success(content)
        }
        None => {
            // Try fallback: direct file_read in case node_id has a different format
            let ref_path = ctx.working_dir.join(".ox").join("refs").join(format!("{}.md", node_id));
            if ref_path.exists() {
                match std::fs::read_to_string(&ref_path) {
                    Ok(content) => ToolOutput::success(content),
                    Err(e) => ToolOutput::error(format!(
                        "Found file at {} but failed to read: {}",
                        ref_path.display(),
                        e
                    )),
                }
            } else {
                ToolOutput::error(format!(
                    "No offloaded result found for node_id '{}'. \
                     The result may have been cleaned up or the node_id may be incorrect. \
                     Try using `recall` with a `query` to search memory instead.",
                    node_id
                ))
            }
        }
    }
}

fn search_memory(query: &str, max_results: usize, ctx: &ToolContext) -> ToolOutput {
    let project_id: Option<&str> = if ctx.runtime.project_id.is_empty() {
        None
    } else {
        Some(&ctx.runtime.project_id)
    };

    let nodes = ctx.memory.retrieve(query, &project_id, max_results);

    if nodes.is_empty() {
        return ToolOutput::success("No matching memories found. This topic may not have been discussed yet.");
    }

    let mut output = String::new();
    output.push_str(&format!(
        "📚 Found {} relevant memories:\n\n", nodes.len()
    ));

    for (i, node) in nodes.iter().enumerate() {
        let type_label = match node.node_type {
            crate::memory::MemoryNodeType::Architectural => "🏗️ Architecture",
            crate::memory::MemoryNodeType::BestPractice => "✅ Best Practice",
            crate::memory::MemoryNodeType::Style => "🎨 Style",
            crate::memory::MemoryNodeType::Pattern => "📐 Pattern",
            crate::memory::MemoryNodeType::Fact => "📋 Fact",
            crate::memory::MemoryNodeType::Business => "💼 Business",
            crate::memory::MemoryNodeType::AntiPattern => "⚠️ Anti-Pattern",
            crate::memory::MemoryNodeType::MetaSkill => "🛠️ Skill",
        };

        let score = if node.avg_llm_score > 0.0 {
            format!(" (score: {:.1}/10)", node.avg_llm_score)
        } else {
            String::new()
        };

        output.push_str(&format!(
            "**{}. {}**{} | {}\n{}\n",
            i + 1,
            type_label,
            score,
            if node.language.is_empty() { "multi" } else { &node.language },
            node.content
        ));

        if !node.related_files.is_empty() {
            output.push_str(&format!(
                "   📁 Related files: {}\n",
                node.related_files.join(", ")
            ));
        }

        if i < nodes.len() - 1 {
            output.push('\n');
        }
    }

    ToolOutput::success(output)
}
