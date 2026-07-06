//! Repair malformed LLM tool arguments (empty JSON, XML tool_call, param aliases).

use regex::Regex;
use serde_json::{Value, json};

use super::unified_action::{self, UnifiedActionRequest};

/// Repair `complete_and_check` arguments; returns canonical JSON string if possible.
pub fn repair_unified_arguments(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return None;
    }

    if let Ok(req) = try_parse_unified(trimmed) {
        return serde_json::to_string(&req).ok();
    }

    if let Some(xml) = parse_xml_arg_pairs(trimmed)
        && let Ok(req) = build_unified_from_pairs(&xml) {
            return serde_json::to_string(&req).ok();
        }

    if let Some(extracted) = extract_json_object_with_action(trimmed)
        && let Ok(req) = try_parse_unified(&extracted) {
            return serde_json::to_string(&req).ok();
        }

    None
}

/// Normalize `complete_and_check` arguments into canonical JSON, repairing the
/// common malformations: Hermes-style `<tool_call>` XML, empty/`{}` args, and
/// param aliases. We deliberately AUTO-REPAIR XML here: GLM/Qwen-family models
/// structurally emit `<tool_call>` XML even under tool_choice=function, and error
/// feedback does not change their decoder — rejecting it just produced an endless
/// "参数格式错误" loop. Since `build_unified_from_pairs` reliably converts XML pairs
/// to JSON, prefer recovery over teaching-by-error.
pub fn recover_tool_call_arguments(tc_name: &str, arguments: &str, _fallbacks: &[&str]) -> String {
    if tc_name == unified_action::TOOL_NAME {
        if arguments.contains("<tool_call>") || arguments.contains("<arg_key>") {
            if let Some(repaired) = repair_unified_arguments(arguments) {
                tracing::info!("[TOOL_ARGS] Repaired XML <tool_call> args → JSON");
                return repaired;
            }
            // Couldn't repair — fall through to error so the model gets feedback.
            return arguments.to_string();
        }
        // Repair truly empty or JSON-malformed args from the surrounding text.
        if arguments.trim().is_empty() || arguments.trim() == "{}" {
            for fb in _fallbacks {
                if let Some(repaired) = repair_unified_arguments(fb) {
                    tracing::warn!("[TOOL_ARGS] Recovered empty args from text fallback");
                    return repaired;
                }
            }
        }
    }
    arguments.to_string()
}

fn try_parse_unified(raw: &str) -> Result<UnifiedActionRequest, String> {
    let cleaned = strip_code_fences(raw.trim());
    if cleaned.is_empty() {
        return Err("empty".into());
    }
    let mut value: Value =
        serde_json::from_str(&cleaned).map_err(|e| format!("invalid JSON: {e}"))?;
    normalize_unified_value(&mut value)?;
    serde_json::from_value(value).map_err(|e| format!("invalid unified shape: {e}"))
}

pub fn normalize_unified_value(value: &mut Value) -> Result<(), String> {
    let obj = value
        .as_object_mut()
        .ok_or_else(|| "expected JSON object".to_string())?;

    // Flatten {"action":"file_read","path":"x"} → params.path
    if !obj.contains_key("params") {
        let action = obj
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(action) = action {
            let mut params = serde_json::Map::new();
            for (k, v) in obj.clone() {
                if k != "action" {
                    params.insert(k, v);
                }
            }
            obj.clear();
            obj.insert("action".into(), Value::String(action));
            obj.insert("params".into(), Value::Object(params));
        }
    }

    let action = obj
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing field: action".to_string())?
        .to_string();

    if !obj.contains_key("params") {
        obj.insert("params".into(), json!({}));
    }

    if let Some(params) = obj.get_mut("params") {
        normalize_delegate_params(&action, params);
    }

    Ok(())
}

/// Map common LLM param aliases to canonical tool schemas.
pub fn normalize_delegate_params(action: &str, params: &mut Value) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };

    match action {
        "find_symbol"
            if !obj.contains_key("name") => {
                for alias in ["symbol", "query", "pattern", "class", "type"] {
                    if let Some(v) = obj.get(alias).cloned() {
                        obj.insert("name".into(), v);
                        break;
                    }
                }
            }
        "file_read" | "file_write" | "edit_file" | "delete_range"
            if !obj.contains_key("path") => {
                for alias in ["file", "filepath", "file_path", "filename", "key"] {
                    if let Some(v) = obj.get(alias).cloned() {
                        obj.insert("path".into(), v);
                        break;
                    }
                }
            }
        "file_search" => {
            if !obj.contains_key("pattern")
                && let Some(v) = obj.get("query").cloned() {
                    obj.insert("pattern".into(), v);
                }
            if !obj.contains_key("path")
                && let Some(v) = obj.get("dir").or(obj.get("directory")).cloned() {
                    obj.insert("path".into(), v);
                }
        }
        "code_search" => {
            if !obj.contains_key("query")
                && let Some(v) = obj.get("pattern").or(obj.get("q")).cloned() {
                    obj.insert("query".into(), v);
                }
        }
        "shell_exec" => {
            if !obj.contains_key("command")
                && let Some(v) = obj.get("cmd").cloned() {
                    obj.insert("command".into(), v);
                }
        }
        "recall" => {
            if !obj.contains_key("node_id")
                && let Some(v) = obj.get("key").or(obj.get("id")).cloned() {
                    obj.insert("node_id".into(), v);
                }
        }
        "finish" | "deliver" | "report" | "done" | "complete" => {
            // Normalize free-text aliases → content.
            if !obj.contains_key("content")
                && let Some(v) = obj
                    .get("text")
                    .or(obj.get("body"))
                    .or(obj.get("summary"))
                    .or(obj.get("message"))
                    .cloned()
                {
                    obj.insert("content".into(), v);
                }
            // Normalize review-item aliases → finding_json.
            if !obj.contains_key("finding_json")
                && let Some(v) = obj.get("findings").or(obj.get("finding")).cloned() {
                    obj.insert("finding_json".into(), v);
                }
        }
        _ => {}
    }
}

/// If LLM used `recall` with a file path, redirect to `file_read`.
pub fn redirect_recall_file_path(req: &UnifiedActionRequest) -> Option<UnifiedActionRequest> {
    if req.action != "recall" {
        return None;
    }
    let key = req
        .params
        .get("node_id")
        .or(req.params.get("key"))
        .and_then(|v| v.as_str())?;
    let looks_like_path =
        key.contains('/') || key.contains('\\') || key.contains('.') || key.starts_with("src/");
    if looks_like_path {
        Some(UnifiedActionRequest {
            action: "file_read".into(),
            params: json!({ "path": key }),
        })
    } else {
        None
    }
}

fn strip_code_fences(s: &str) -> String {
    let t = s.trim();
    if t.starts_with("```") {
        let inner = t
            .trim_start_matches('`')
            .trim_start_matches("json")
            .trim_start_matches("JSON");
        if let Some(end) = inner.rfind("```") {
            return inner[..end].trim().to_string();
        }
    }
    t.to_string()
}

fn parse_xml_arg_pairs(s: &str) -> Option<Vec<(String, String)>> {
    if !s.contains("<arg_key>") {
        return None;
    }
    static PAIR: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?is)<arg_key>\s*([^<]+?)\s*</arg_key>\s*<arg_value>\s*(.*?)\s*</arg_value>")
            .unwrap()
    });
    let pairs: Vec<_> = PAIR
        .captures_iter(s)
        .map(|c| (c[1].trim().to_string(), c[2].trim().to_string()))
        .collect();
    if pairs.is_empty() { None } else { Some(pairs) }
}

fn build_unified_from_pairs(pairs: &[(String, String)]) -> Result<UnifiedActionRequest, String> {
    let mut map: std::collections::HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();

    let action = map
        .remove("action")
        .ok_or_else(|| "XML tool_call missing action".to_string())?;

    let params = if let Some(params_raw) = map.remove("params") {
        serde_json::from_str(&params_raw).unwrap_or_else(|_| json!({ "path": params_raw }))
    } else {
        let mut obj = serde_json::Map::new();
        for (k, v) in map {
            obj.insert(k, serde_json::from_str(&v).unwrap_or(Value::String(v)));
        }
        Value::Object(obj)
    };

    let mut root = json!({ "action": action, "params": params });
    normalize_unified_value(&mut root)?;
    serde_json::from_value(root).map_err(|e| e.to_string())
}

fn extract_json_object_with_action(s: &str) -> Option<String> {
    let needle = r#""action""#;
    let start = s.find(needle)?;
    let brace_start = s[..start].rfind('{')?;
    let slice = &s[brace_start..];
    let end = matching_brace_end(slice)?;
    Some(slice[..=end].to_string())
}

fn matching_brace_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in s.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract `<tool_call>NAME<arg_key>K</arg_key><arg_value>V</arg_value>...</tool_call>`
/// blocks from GLM models that don't use the OpenAI function-calling protocol.
/// Returns properly formatted ToolCall structs with repaired JSON arguments.
pub fn extract_xml_tool_calls(text: &str) -> Vec<crate::message::ToolCall> {
    if !text.contains("<tool_call>") {
        return Vec::new();
    }
    static XML_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?is)<tool_call>\s*(.*?)\s*</tool_call>").unwrap()
    });
    let mut calls = Vec::new();
    for cap in XML_RE.captures_iter(text) {
        let inner = &cap[1];
        // Extract tool name: text before the first <arg_key>
        let name_end = inner.find("<arg_key>").unwrap_or(inner.len());
        let tool_name = inner[..name_end].trim().to_string();
        if tool_name.is_empty() {
            continue;
        }
        // Parse <arg_key>K</arg_key><arg_value>V</arg_value> pairs
        let pairs = parse_xml_arg_pairs(inner).unwrap_or_default();
        let args = if let Ok(req) = build_unified_from_pairs(&pairs) {
            serde_json::to_string(&req).unwrap_or_default()
        } else {
            // Fallback: build raw JSON from pairs
            let map: serde_json::Map<String, serde_json::Value> = pairs
                .into_iter()
                .map(|(k, v)| {
                    let val = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v));
                    (k, val)
                })
                .collect();
            serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_default()
        };
        let id = format!(
            "xml-tc-{}",
            uuid::Uuid::new_v4()
                .to_string()
                .chars()
                .take(8)
                .collect::<String>()
        );
        calls.push(crate::message::ToolCall {
            id,
            name: tool_name,
            arguments: args,
        });
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_symbol_alias() {
        let raw = r#"{"action":"find_symbol","params":{"symbol":"Foo"}}"#;
        let out = repair_unified_arguments(raw).unwrap();
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.params["name"], "Foo");
    }

    #[test]
    fn repairs_xml_tool_call() {
        let raw = r#"<tool_call>complete_and_check<arg_key>action</arg_key><arg_value>file_read</arg_value><arg_key>params</arg_key><arg_value>{"path":"a.rs"}</arg_value></tool_call>"#;
        let out = repair_unified_arguments(raw).unwrap();
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.action, "file_read");
        assert_eq!(req.params["path"], "a.rs");
    }

    #[test]
    fn recover_repairs_xml_args_to_json() {
        // Path A: a structured tool_call whose `arguments` came through as XML.
        // recover_tool_call_arguments must convert it to JSON, not pass it through.
        let xml = r#"<tool_call>complete_and_check<arg_key>action</arg_key><arg_value>file_read</arg_value><arg_key>params</arg_key><arg_value>{"path":"a.rs"}</arg_value></tool_call>"#;
        let out = recover_tool_call_arguments(unified_action::TOOL_NAME, xml, &[]);
        assert!(!out.contains("<tool_call>"));
        assert!(!out.contains("<arg_key>"));
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.action, "file_read");
        assert_eq!(req.params["path"], "a.rs");
    }

    #[test]
    fn recover_repairs_bare_arg_pairs() {
        let xml = r#"<arg_key>action</arg_key><arg_value>finish</arg_value><arg_key>params</arg_key><arg_value>{"content":"done"}</arg_value>"#;
        let out = recover_tool_call_arguments(unified_action::TOOL_NAME, xml, &[]);
        assert!(!out.contains("<arg_key>"));
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.action, "finish");
        assert_eq!(req.params["content"], "done");
    }

    #[test]
    fn recall_path_redirects() {
        let req = UnifiedActionRequest {
            action: "recall".into(),
            params: json!({ "key": "src/Foo.java" }),
        };
        let redirected = redirect_recall_file_path(&req).unwrap();
        assert_eq!(redirected.action, "file_read");
    }

    #[test]
    fn extracts_json_from_prose() {
        let text = r#"Let me read: {"action":"file_read","params":{"path":"x.rs"}}"#;
        let out = repair_unified_arguments(text).unwrap();
        assert!(out.contains("file_read"));
    }
}
