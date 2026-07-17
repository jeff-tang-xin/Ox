//! `code_graph` — the agent's gateway to the GitNexus code knowledge graph.
//!
//! One tool, many ops. The LLM picks an `op` and supplies that op's native
//! GitNexus arguments; everything except `op` is forwarded verbatim to the
//! GitNexus MCP server, so the full capability surface is available without
//! per-op plumbing. Read-only (safe): even `rename` defaults to a dry-run
//! preview, so the agent can plan against the graph before touching files.

use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

/// Known GitNexus ops (MCP tool names) the agent may call.
const OPS: &[&str] = &[
    // comprehension
    "query",
    "context",
    "cypher",
    "list_repos",
    // pre-change impact
    "impact",
    "detect_changes",
    "api_impact",
    // API surface maps
    "route_map",
    "tool_map",
    "shape_check",
    // refactor (preview by default)
    "rename",
    // multi-repo groups
    "group_list",
    "group_sync",
];

/// Cap forwarded graph output so a huge result can't blow the context window.
const MAX_OUTPUT_CHARS: usize = 20_000;

pub struct CodeGraphTool;

#[async_trait::async_trait]
impl Tool for CodeGraphTool {
    fn name(&self) -> &str {
        "code_graph"
    }

    fn description(&self) -> &str {
        "查询代码知识图谱(GitNexus)。改代码前用它建立关系模型与影响面。\
         先用 list_repos 看有哪些仓库，再选正确仓库查关系。\
         params.op 选择能力，其余字段按该 op 透传：\n\
         • query{query} 概念→执行流(调用链)  • context{name|uid} 单符号360°(谁调谁/读写)\n\
         • impact{target,direction:upstream|downstream} 改动爆炸半径  • detect_changes{} 未提交改动影响\n\
         • api_impact{route|file} 路由改动报告  • route_map/tool_map/shape_check API面貌\n\
         • cypher{query} 原生图查询  • rename{symbol_name,new_name}(默认dry_run预览)\n\
         • list_repos{} / group_list{} / group_sync{name}"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "description": "GitNexus 能力名",
                    "enum": OPS
                }
            },
            "required": ["op"],
            "additionalProperties": true
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        // All ops are read-only (rename defaults to dry-run preview).
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(svc) = ctx.gitnexus.clone() else {
            return ToolOutput::error(
                "代码图谱不可用：GitNexus 未启用或未就绪。可继续用 file_read / find_symbol / code_search 探索。",
            );
        };

        let op = match args.get("op").and_then(Value::as_str) {
            Some(s) if OPS.contains(&s) => s.to_string(),
            Some(s) => {
                return ToolOutput::error(format!("未知 op `{s}`。可用: {}", OPS.join(", ")));
            }
            None => {
                return ToolOutput::error(format!("缺少 params.op。可用: {}", OPS.join(", ")));
            }
        };

        // Light client-side checks for ops with hard-required args — gives a
        // clearer error than a round-trip to the server.
        if let Err(e) = precheck(&op, &args) {
            return ToolOutput::error(e);
        }

        // (E) Reindex is deferred to the pre-turn pipeline so it runs between
        // turns, not during a code_graph call. See gitnexus_service::reindex_if_dirty.

        // Forward everything except `op` as the GitNexus tool arguments.
        let mut forwarded = args.clone();
        if let Some(obj) = forwarded.as_object_mut() {
            obj.remove("op");
            // NOTE: repo is NOT stripped here — the project may have multiple
            // GitNexus repos, and removing `repo` would cause "Multiple
            // repositories indexed" errors. The LLM should include repo when
            // it knows it, and GitNexus uses the default when omitted.
            // GitNexus MCP server's `query` tool expects `pattern` as the
            // search-text parameter key, but the LLM sends `query` (from Ox's
            // precheck error message). Normalize it here.
            if (op == "query" || op == "cypher")
                && let Some(q) = obj.remove("query")
            {
                obj.insert("pattern".to_string(), q);
            }
        }

        match svc.call(&op, forwarded).await {
            Ok(result) => {
                let mut text = result.text;
                if text.trim().is_empty() {
                    text = "(空结果)".to_string();
                }
                // If GitNexus says multiple repos, auto-list them so the LLM
                // knows which repo name to use.
                if text.contains("Multiple repositories indexed")
                    && let Ok(repos) = svc.list_repos().await
                    && !repos.is_error
                {
                    let list = repos.text.trim();
                    text.push_str(&format!(
                        "\n\n📋 可用仓库:\n{list}\n请在下次调用时加上 repo 参数。"
                    ));
                }
                // If GitNexus can't find the target (e.g. file not in git yet,
                // or wrong repo parameter), append a helpful hint.
                if text.contains("Target")
                    && (text.contains("not found") || text.contains("NotFound"))
                {
                    text.push_str(
                        "\n\n⚠️ 目标不在代码图谱中，可能原因：\
                         \n1. 该文件是新增的，尚未 git add → 直接编辑，无需 impact 分析\
                         \n2. GitNexus 索引未覆盖此模块 → 可继续编辑，跳过 impact\
                         \n3. repo 参数不正确 → 用上面的可用仓库列表重试",
                    );
                }
                if text.len() > MAX_OUTPUT_CHARS {
                    let mut cut = MAX_OUTPUT_CHARS;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    text.push_str("\n…(结果已截断；用更具体的参数缩小范围)");
                }
                // Prefix a freshness banner when the index is missing/stale
                // so the LLM can distinguish "truly not found" from
                // "index hasn't caught up". Never triggers a reindex.
                let banner_line = svc
                    .freshness_snapshot()
                    .await
                    .banner()
                    .map(|b| format!("{b}\n"))
                    .unwrap_or_default();
                let header = format!("{banner_line}── code_graph/{op} ──\n");
                if result.is_error {
                    ToolOutput::error(format!("{header}{text}"))
                } else {
                    ToolOutput::success(format!("{header}{text}"))
                }
            }
            Err(e) => ToolOutput::error(format!("code_graph/{op} 失败: {e}")),
        }
    }
}

/// Validate hard-required args for ops the server would otherwise reject.
fn precheck(op: &str, args: &Value) -> Result<(), String> {
    let has = |k: &str| args.get(k).map(|v| !v.is_null()).unwrap_or(false);
    match op {
        "query" | "cypher" if !has("query") => {
            return Err(format!("op={op} 需要 params.query"));
        }
        "context" if !has("name") && !has("uid") => {
            return Err("op=context 需要 params.name 或 params.uid".into());
        }
        "impact" => {
            if !has("target") && !has("target_uid") {
                return Err("op=impact 需要 params.target（或 target_uid）".into());
            }
            if !has("direction") {
                return Err("op=impact 需要 params.direction（upstream 或 downstream）".into());
            }
        }
        "api_impact" if !has("route") && !has("file") => {
            return Err("op=api_impact 需要 params.route 或 params.file".into());
        }
        "rename" => {
            if !has("new_name") {
                return Err("op=rename 需要 params.new_name".into());
            }
            if !has("symbol_name") && !has("symbol_uid") {
                return Err("op=rename 需要 params.symbol_name（或 symbol_uid）".into());
            }
        }
        "group_sync" if !has("name") => {
            return Err("op=group_sync 需要 params.name".into());
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precheck_requires_query() {
        assert!(precheck("query", &json!({})).is_err());
        assert!(precheck("query", &json!({"query": "auth"})).is_ok());
    }

    #[test]
    fn precheck_impact_needs_target_and_direction() {
        assert!(precheck("impact", &json!({"target": "f"})).is_err());
        assert!(precheck("impact", &json!({"target": "f", "direction": "upstream"})).is_ok());
        assert!(
            precheck(
                "impact",
                &json!({"target_uid": "u#1", "direction": "downstream"})
            )
            .is_ok()
        );
    }

    #[test]
    fn precheck_context_accepts_name_or_uid() {
        assert!(precheck("context", &json!({})).is_err());
        assert!(precheck("context", &json!({"name": "AuthService"})).is_ok());
        assert!(precheck("context", &json!({"uid": "x#1"})).is_ok());
    }

    #[test]
    fn precheck_no_arg_ops_ok() {
        assert!(precheck("list_repos", &json!({})).is_ok());
        assert!(precheck("detect_changes", &json!({})).is_ok());
        assert!(precheck("route_map", &json!({})).is_ok());
    }
}
