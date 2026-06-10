/// find_symbol — AST-aware symbol search with semantic vector search.
///
/// Uses tree-sitter for accurate symbol extraction and triviumdb
/// for semantic search. Falls back to keyword match when needed.
use serde_json::{Value, json};
use std::sync::Arc;
use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FindSymbolTool;

#[async_trait::async_trait]
impl Tool for FindSymbolTool {
    fn name(&self) -> &str {
        "find_symbol"
    }

    fn description(&self) -> &str {
        "Search for symbols (functions, classes, structs, traits, etc.) by name or semantics. \
         Uses AST parsing + vector embeddings for accuracy. \
         Use to find definitions, understand code structure, or explore APIs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Symbol name or description to search for. \
                                   Exact name match first, then semantic search."
                },
                "top_k": {
                    "type": "integer",
                    "description": "Max results (default 10)."
                }
            },
            "required": ["name"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let name = match args.get("name").and_then(|n| n.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => return ToolOutput::error("❌ Missing required parameter: 'name'."),
        };
        let top_k = args.get("top_k")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);

        let knowledge = Arc::clone(&ctx.knowledge);
        let name_owned = name.to_string();

        let result = tokio::task::spawn(async move {
            let engine = knowledge.lock().await;
            engine.retrieve_code(&name_owned, top_k)
                .map_err(|e| e.to_string())
        }).await;

        match result {
            Ok(Ok(hits)) => {
                if hits.is_empty() {
                    ToolOutput::success(format!(
                        "🔍 No symbols found for '{}'.\n\
                         💡 The project index may not be built yet. \
                         Use file_read on key files to auto-index, \
                         or trigger a full index.",
                        name
                    ))
                } else {
                    let mut output = format!(
                        "🔍 Found {} symbol(s) for '{}':\n\n",
                        hits.len(), name
                    );
                    for hit in &hits {
                        let entity = &hit.entity;
                        if let crate::knowledge::entity::EntityMetadata::CodeSymbol {
                            ref symbol_type, ref fq_name, ref file_path,
                            start_line, end_line, ref signature, ref parent, ..
                        } = entity.metadata {
                            output.push_str(&format!(
                                "  [{}] `{}` @ {}:{}-{}\n",
                                symbol_type, fq_name,
                                file_path, start_line, end_line
                            ));
                            if let Some(p) = parent {
                                output.push_str(&format!("       └ in {}\n", p));
                            }
                            if !signature.is_empty() {
                                let sig: String = signature
                                    .chars()
                                    .take(100)
                                    .collect();
                                output.push_str(&format!("       └ {}\n", sig));
                            }
                        }
                    }
                    output.push_str("\n💡 Use file_read to view full source. Use edit_file to modify.");
                    ToolOutput::success(output)
                }
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Symbol search panicked: {e}")),
        }
    }
}
