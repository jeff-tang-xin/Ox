//! `read_symbol` — combined find_symbol + file_read in one round-trip.
//!
//! Locates a symbol via the shared tree-sitter search and returns its full
//! source range (start..end from AST), plus optional context lines. Preserves
//! the existing `find_symbol` and `file_read` tools untouched.

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct ReadSymbolTool;

#[async_trait::async_trait]
impl Tool for ReadSymbolTool {
    fn name(&self) -> &str {
        "read_symbol"
    }

    fn description(&self) -> &str {
        "Read symbol full source code by name (AST-extracted body + context lines). Use 'kind' to disambiguate same-name symbols. context_lines default 5, max 50."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Symbol name — exact match preferred; substring fallback supported"
                },
                "kind": {
                    "type": "string",
                    "description": "Optional symbol_type filter: function/struct/enum/impl/trait/const/static/mod/macro/…"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Extra lines before/after the symbol body (default 5, max 50)",
                    "minimum": 0,
                    "maximum": 50
                }
            },
            "required": ["name"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::error("❌ Missing required parameter: 'name'."),
        };
        let kind_filter = args
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());
        let ctx_lines = args
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(50) as usize;

        // Reuse find_symbol's tree-sitter search.
        let mut extractor = crate::tools::ast_extractor::AstExtractor::new();
        let hits = crate::tools::find_symbol::search_symbols_public(
            &mut extractor,
            &ctx.working_dir,
            &name,
        );

        // Filter by kind (if given); rank exact-name > prefix > other.
        let name_lower = name.to_lowercase();
        let mut filtered: Vec<_> = hits
            .into_iter()
            .filter(|h| {
                kind_filter
                    .as_deref()
                    .map(|k| h.symbol_type.to_lowercase() == k)
                    .unwrap_or(true)
            })
            .collect();
        filtered.sort_by_key(|h| {
            let short = h.name.rsplit("::").next().unwrap_or(&h.name).to_lowercase();
            if short == name_lower {
                0
            } else if short.starts_with(&name_lower) {
                1
            } else {
                2
            }
        });

        let Some(hit) = filtered.first() else {
            return ToolOutput::error(format!(
                "🔍 read_symbol: '{name}' not found (kind filter = {:?}).\n\
                 → 试用 find_symbol 宽松匹配，或 code_search 直搜文本。",
                kind_filter
            ));
        };

        // Re-parse the hit's file to recover the full end_line of this entity.
        let path = PathBuf::from(&hit.file_path);
        let code = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::error(format!("❌ Cannot read {}: {e}", hit.file_path));
            }
        };
        let symbols = extractor.extract_symbols(&path, &code).unwrap_or_default();
        let short_name = hit.name.rsplit("::").next().unwrap_or(&hit.name);
        let mut end_line = hit.line;
        for s in &symbols {
            if s.start_line == hit.line && s.fq_name.ends_with(short_name) {
                end_line = s.end_line;
                break;
            }
        }

        let lines: Vec<&str> = code.lines().collect();
        let start = hit.line.saturating_sub(ctx_lines).max(1);
        let stop = (end_line + ctx_lines).min(lines.len());

        let mut out = format!(
            "📖 read_symbol: [{}] `{}` @ {}:{}-{}\n",
            hit.symbol_type, hit.name, hit.file_path, hit.line, end_line
        );
        if filtered.len() > 1 {
            out.push_str(&format!(
                "⚠️ {} candidates matched; showing top-1. 传 kind=... 收窄或用 find_symbol 看全部。\n",
                filtered.len()
            ));
        }
        out.push('\n');
        for i in start..=stop {
            out.push_str(&format!("{:>5} | {}\n", i, lines.get(i - 1).unwrap_or(&"")));
        }
        ToolOutput::success(out)
    }
}
