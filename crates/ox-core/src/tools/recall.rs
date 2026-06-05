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
        "Retrieve the full content of an offloaded tool result by its node_id. \
         Use when a previous large tool output was summarized with a node_id reference. \
         For searching memory/knowledge, use memory_search instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The node_id from a 'Result saved to .ox/refs/{node_id}.md' message."
                }
            },
            "required": ["node_id"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let node_id = match args.get("node_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return ToolOutput::error(
                "recall requires a 'node_id' parameter. \
                 Use it to fetch a previously offloaded tool result. \
                 For searching memory, use memory_search instead."
            ),
        };
        retrieve_by_node_id(node_id, ctx)
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
                     The result may have been cleaned up or the node_id may be incorrect.",
                    node_id
                ))
            }
        }
    }
}
