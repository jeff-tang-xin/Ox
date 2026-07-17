use super::{SafetyLevel, Tool, ToolContext, ToolOutput};
/// find_symbol — tree-sitter symbol locator (no vector fallback).
///
/// 1. tree-sitter (in-memory, always available) for exact/prefix name match
/// 2. Optional GitNexus code_graph prefix + relationship suffix when the graph
///    server is ready.
use serde_json::{Value, json};
use std::path::Path;

pub struct FindSymbolTool;

#[async_trait::async_trait]
impl Tool for FindSymbolTool {
    fn name(&self) -> &str {
        "find_symbol"
    }

    fn description(&self) -> &str {
        "定位符号位置(functions, classes, structs) by name. \
         Tree-sitter exact/substring match first, then semantic vector fallback (up to ~20 hits). \
         When code graph is ready, results include caller/callee for the top match. \
         \n\
         **用途**: 快速定位单个符号的定义位置。\
         **不适合**: 分析执行流程、调用链、主流程 → 用 code_graph op=query。\
         \n\
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
        let top_k = args
            .get("top_k")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);

        let name_owned = name.to_string();
        let working_dir = ctx.working_dir.clone();
        let _ = top_k;

        // Before tree-sitter search, try code_graph query to get execution flow
        // context. The LLM doesn't need to call code_graph separately —
        // find_symbol folds in both symbol location AND relationship model.
        // Timeout at 2s so it never slows down the symbol search.
        let mut graph_prefix = String::new();
        if let Some(ref svc) = ctx.gitnexus
            && svc.is_ready().await
        {
            let qp = crate::mcp::gitnexus::QueryParams::new(name);
            if let Ok(graph) =
                tokio::time::timeout(std::time::Duration::from_secs(2), svc.query(&qp)).await
                && let Ok(g) = graph
                && !g.is_error
            {
                let t = g.text.trim();
                if !t.is_empty() && t != "(空结果)" {
                    graph_prefix = format!("── code_graph/query ──\n{t}\n\n");
                }
            }
        }

        let result = tokio::task::spawn(async move {
            // ── tree-sitter direct search (always available, no index needed) ──
            let mut extractor = crate::tools::ast_extractor::AstExtractor::new();
            let ts_hits = search_with_treesitter(&mut extractor, &working_dir, &name_owned);
            if !ts_hits.is_empty() {
                let primary_file = ts_hits.first().map(|h| h.file_path.clone());
                return Ok::<SearchOutcome, String>(SearchOutcome {
                    output: format_treesitter_results(&name_owned, &ts_hits),
                    primary_file,
                });
            }
            Ok(SearchOutcome {
                output: format!(
                    "🔍 No symbols found for '{}'.\n\
                     💡 Try a more specific name, or use code_search to find usages.",
                    name_owned
                ),
                primary_file: None,
            })
        })
        .await;

        match result {
            Ok(Ok(outcome)) => {
                let mut output = String::new();
                // Prepend code_graph query result (execution flow context)
                if !graph_prefix.is_empty() {
                    output.push_str(&graph_prefix);
                }
                // Main symbol search result
                output.push_str(&outcome.output);
                // Seamlessly fold in GitNexus relationship context (callers/callees/
                // refs) when the graph is ready — no separate code_graph call needed.
                if let Some(extra) =
                    enrich_with_graph(ctx, name, outcome.primary_file.as_deref()).await
                {
                    output.push_str(&extra);
                }
                ToolOutput::success(output)
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Symbol search panicked: {e}")),
        }
    }
}

/// Result of the symbol search plus the file of the top hit (for graph disambiguation).
struct SearchOutcome {
    output: String,
    primary_file: Option<String>,
}

/// Cap graph context to ~`max_chars`, but only on a **line** boundary so the LLM
/// never sees a half-written relationship entry or a chopped-up JSON/Markdown
/// structure. A clear marker tells it the view is partial and how to get the rest.
fn truncate_on_line_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    // Largest char boundary <= max_chars (safe to slice there).
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let window = &text[..end];
    // Prefer the last complete line within the window; keep whole entries intact.
    let kept = match window.rfind('\n') {
        Some(nl) if nl > 0 => &window[..nl],
        // One oversized line with no break — keep the char-bounded slice as-is.
        _ => window,
    };
    format!(
        "{}\n…(关系信息已截断；用 code_graph op=context 查看完整)",
        kept.trim_end()
    )
}

/// Append GitNexus `context` (360° relationship view) for the searched symbol.
///
/// Strictly latency-safe: returns `None` (no enrichment) unless the server is
/// already running AND the index is clean. It never spawns, restarts, or
/// reindexes — `find_symbol` must stay fast. Bounded by a short timeout so a
/// slow/hung graph can't stall the tool.
async fn enrich_with_graph(
    ctx: &ToolContext,
    name: &str,
    file_path: Option<&str>,
) -> Option<String> {
    if !ctx.config.gitnexus.augment_find_symbol {
        return None;
    }
    let svc = ctx.gitnexus.as_ref()?;
    if !svc.is_ready().await {
        return None; // not ready → no latency, no enrichment
    }
    if svc.is_dirty() {
        // Edits pending reindex — keep find_symbol fast; relationships may be
        // stale, so skip rather than block on a rebuild.
        return Some(
            "\n\n📎 (代码图谱有未索引改动，调用关系暂略；用 code_graph 查询会先刷新)".to_string(),
        );
    }

    let mut params = crate::mcp::gitnexus::ContextParams::by_name(name);
    if let Some(fp) = file_path {
        params.file_path = Some(fp.to_string());
    }

    let res = tokio::time::timeout(std::time::Duration::from_secs(6), svc.context(&params))
        .await
        .ok()?
        .ok()?;
    if res.is_error {
        return None;
    }
    let text = res.text.trim();
    if text.is_empty() {
        return None;
    }
    let body = truncate_on_line_boundary(text, 2000);
    Some(format!("\n\n📎 调用关系/影响面 (GitNexus):\n{body}"))
}

/// Lightweight symbol info from tree-sitter direct search.
pub(crate) struct TsSymbol {
    pub(crate) symbol_type: String,
    pub(crate) name: String,
    pub(crate) file_path: String,
    pub(crate) line: usize,
    pub(crate) signature: String,
    pub(crate) parent: Option<String>,
    pub(crate) calls: Vec<String>,
}

/// Search project source files with tree-sitter for a symbol name.
/// Uses AstExtractor directly — no KnowledgeEngine or embedding needed.
fn search_with_treesitter(
    extractor: &mut crate::tools::ast_extractor::AstExtractor,
    project_dir: &Path,
    name: &str,
) -> Vec<TsSymbol> {
    let mut results = Vec::new();
    let exts = [
        "rs", "py", "js", "ts", "go", "java", "c", "cpp", "h", "hpp", "toml", "json", "md", "html",
        "css", "yaml", "yml",
    ];
    let name_lower = name.to_lowercase();

    for ext in &exts {
        let pattern = format!("**/*.{}", ext);
        let full_pattern = project_dir.join(&pattern);
        let pattern_str = full_pattern.to_string_lossy();

        if let Ok(entries) = glob::glob(&pattern_str) {
            for entry in entries.flatten() {
                if results.len() >= 20 {
                    return results;
                }
                if let Ok(code) = std::fs::read_to_string(&entry) {
                    if !code.contains(name) && !code.to_lowercase().contains(&name_lower) {
                        continue;
                    }
                    if let Ok(symbols) = extract_file_symbols(extractor, &entry) {
                        for s in symbols {
                            let entity_name = s.fq_name.rsplit("::").next().unwrap_or(&s.fq_name);
                            if entity_name == name
                                || entity_name.to_lowercase() == name_lower
                                || s.fq_name.contains(name)
                                || s.fq_name.to_lowercase().contains(&name_lower)
                            {
                                results.push(TsSymbol {
                                    symbol_type: s.symbol_type,
                                    name: s.fq_name,
                                    file_path: s.file_path,
                                    line: s.start_line,
                                    signature: s.signature,
                                    parent: s.parent,
                                    calls: s.calls,
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
    results
}

fn format_treesitter_results(name: &str, hits: &[TsSymbol]) -> String {
    let mut output = format!(
        "🔍 [tree-sitter] Found {} symbol(s) for '{}':\n\n",
        hits.len(),
        name
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
        if !hit.calls.is_empty() {
            let calls: Vec<String> = hit
                .calls
                .iter()
                .take(5)
                .map(|c| {
                    if let Some(short) = c.rsplit("::").next() {
                        short.to_string()
                    } else {
                        c.clone()
                    }
                })
                .collect();
            output.push_str(&format!("       → calls: {}\n", calls.join(", ")));
        }
    }
    output.push_str("\n💡 Use file_read to view full source. Use edit_file to modify.");
    output
}

// Legacy vector/graph formatters removed — tree-sitter is the sole backend.

/// Standalone tree-sitter extraction — uses tools::ast_extractor directly.
fn extract_file_symbols(
    extractor: &mut crate::tools::ast_extractor::AstExtractor,
    file_path: &Path,
) -> anyhow::Result<Vec<crate::tools::ast_extractor::CodeSymbolInfo>> {
    if extractor.detect_language(file_path).is_none() {
        return Ok(Vec::new());
    }
    let code = std::fs::read_to_string(file_path)?;
    extractor.extract_symbols(file_path, &code)
}

/// Public wrapper so sibling tools (e.g. `read_symbol`) can reuse the
/// tree-sitter search without duplicating the file-walk logic.
pub(crate) fn search_symbols_public(
    extractor: &mut crate::tools::ast_extractor::AstExtractor,
    project_dir: &Path,
    name: &str,
) -> Vec<TsSymbol> {
    search_with_treesitter(extractor, project_dir, name)
}

#[cfg(test)]
mod tests {
    use super::truncate_on_line_boundary;

    #[test]
    fn short_text_unchanged() {
        let t = "callers: a\ncallees: b";
        assert_eq!(truncate_on_line_boundary(t, 2000), t);
    }

    #[test]
    fn truncates_on_whole_lines_only() {
        let text = "line one is here\nline two is here\nline three is here\n";
        let out = truncate_on_line_boundary(text, 25);
        // Keeps only complete line(s); never a partial line.
        assert!(out.starts_with("line one is here"));
        assert!(!out.contains("line two"));
        assert!(out.contains("截断"));
        for line in out.lines().filter(|l| !l.contains('…')) {
            assert!(text.contains(line), "leaked partial line: {line:?}");
        }
    }

    #[test]
    fn oversized_single_line_falls_back_to_char_boundary() {
        let text = "x".repeat(100);
        let out = truncate_on_line_boundary(&text, 20);
        assert!(out.contains("截断"));
        assert!(out.len() < text.len() + 60);
    }

    #[test]
    fn never_panics_on_multibyte_boundary() {
        let text = "关系一：调用者很多很多很多\n关系二：被调用者也很多很多\n关系三：引用点\n";
        // Cap lands inside a multibyte char region; must not panic.
        let _ = truncate_on_line_boundary(text, 15);
    }
}
