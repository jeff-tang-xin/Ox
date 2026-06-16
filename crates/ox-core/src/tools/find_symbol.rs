/// find_symbol — tree-sitter exact match + knowledge engine semantic fallback.
///
/// 1. First: tree-sitter (in-memory, always available) for exact/prefix name match
/// 2. If no results: knowledge engine vector search for semantic fallback
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct FindSymbolTool;

#[async_trait::async_trait]
impl Tool for FindSymbolTool {
    fn name(&self) -> &str {
        "find_symbol"
    }

    fn description(&self) -> &str {
        "Search for symbols (functions, classes, structs) by name. \
         Tree-sitter exact/substring match first (up to ~20 hits), then semantic vector fallback. \
         Not a full-text search — use code_search for text in file contents."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Symbol name to search for. Exact/substring tree-sitter match first, then semantic search."
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
        let working_dir = ctx.working_dir.clone();

        let result = tokio::task::spawn(async move {
            let engine = knowledge.read().await;

            // ── Step 1: tree-sitter direct search (always available, no index needed) ──
            let ts_hits = search_with_treesitter(&engine, &working_dir, &name_owned);
            if !ts_hits.is_empty() {
                return Ok(format_treesitter_results(&name_owned, &ts_hits));
            }

            // ── Step 2: knowledge engine semantic fallback ──
            match engine.retrieve_code(&name_owned, top_k) {
                Ok(hits) if !hits.is_empty() => Ok(format_vector_results(&name_owned, &hits)),
                Ok(_) => Ok(format!(
                    "🔍 No symbols found for '{}'.\n\
                     💡 Try a more specific name, or use file_read + code_search.",
                    name_owned
                )),
                Err(e) => Err(e.to_string()),
            }
        }).await;

        match result {
            Ok(Ok(output)) => ToolOutput::success(output),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Symbol search panicked: {e}")),
        }
    }
}

/// Lightweight symbol info from tree-sitter direct search.
struct TsSymbol {
    symbol_type: String,
    name: String,
    file_path: String,
    line: usize,
    signature: String,
    parent: Option<String>,
}

/// Search project source files with tree-sitter for a symbol name.
fn search_with_treesitter(
    engine: &crate::knowledge::KnowledgeEngine,
    project_dir: &Path,
    name: &str,
) -> Vec<TsSymbol> {
    let mut results = Vec::new();

    // Source file extensions to scan
    let exts = ["rs", "py", "js", "ts", "go", "java", "c", "cpp", "h", "hpp",
                "toml", "json", "md", "html", "css", "yaml", "yml"];
    let name_lower = name.to_lowercase();

    for ext in &exts {
        let pattern = format!("**/*.{}", ext);
        // 使用安全的路径连接
        let full_pattern = project_dir.join(&pattern);
        let pattern_str = full_pattern.to_string_lossy();

        if let Ok(entries) = glob::glob(&pattern_str) {
            for entry in entries.flatten() {
                if results.len() >= 20 {
                    return results;
                }

                // Quick pre-filter: check if file might contain the symbol
                if let Ok(code) = std::fs::read_to_string(&entry) {
                    if !code.contains(name) && !code.to_lowercase().contains(&name_lower) {
                        continue;
                    }

                    // Use tree-sitter to extract symbols from this file
                    if let Ok(entities) = engine.extract_file_symbols(&entry) {
                        for entity in entities {
                            if let crate::knowledge::entity::EntityMetadata::CodeSymbol {
                                ref symbol_type, ref fq_name, ref file_path,
                                start_line, ref signature, ref parent, ..
                            } = entity.metadata
                            {
                                // Match: exact name or contains
                                let entity_name = fq_name.rsplit("::").next().unwrap_or(fq_name);
                                if entity_name == name
                                    || entity_name.to_lowercase() == name_lower
                                    || fq_name.contains(name)
                                    || fq_name.to_lowercase().contains(&name_lower)
                                {
                                    results.push(TsSymbol {
                                        symbol_type: symbol_type.to_string(),
                                        name: fq_name.clone(),
                                        file_path: file_path.clone(),
                                        line: start_line as usize,
                                        signature: signature.clone(),
                                        parent: parent.clone(),
                                    });
                                    if results.len() >= 20 {
                                        return results;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    results
}

fn format_treesitter_results(name: &str, hits: &[TsSymbol]) -> String {
    let mut output = format!(
        "🔍 [tree-sitter] Found {} symbol(s) for '{}':\n\n",
        hits.len(), name
    );
    for hit in hits.iter().take(15) {
        output.push_str(&format!(
            "  [{}] `{}` @ {}:{}\n",
            hit.symbol_type, hit.name, hit.file_path, hit.line
        ));
        if let Some(ref p) = hit.parent {
            output.push_str(&format!("       └ in {}\n", p));
        }
        if !hit.signature.is_empty() {
            let sig: String = hit.signature.chars().take(100).collect();
            output.push_str(&format!("       └ {}\n", sig));
        }
    }
    output.push_str("\n💡 Use file_read to view full source. Use edit_file to modify.");
    output
}

fn format_vector_results(name: &str, hits: &[crate::knowledge::vector_store::SearchHit]) -> String {
    let mut output = format!(
        "🔍 [semantic] Found {} symbol(s) for '{}':\n\n",
        hits.len(), name
    );
    for hit in hits.iter().take(15) {
        let entity = &hit.entity;
        if let crate::knowledge::entity::EntityMetadata::CodeSymbol {
            ref symbol_type, ref fq_name, ref file_path,
            start_line, end_line: _, ref signature, ref parent, ..
        } = entity.metadata
        {
            output.push_str(&format!(
                "  [{}] `{}` @ {}:{}\n",
                symbol_type, fq_name, file_path, start_line
            ));
            if let Some(p) = parent {
                output.push_str(&format!("       └ in {}\n", p));
            }
            if !signature.is_empty() {
                let sig: String = signature.chars().take(100).collect();
                output.push_str(&format!("       └ {}\n", sig));
            }
        }
    }
    output.push_str("\n💡 Use file_read to view full source. Use edit_file to modify.");
    output
}
