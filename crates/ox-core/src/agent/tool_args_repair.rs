//! Repair malformed LLM tool arguments (empty JSON, XML tool_call, param aliases).

use regex::Regex;
use serde_json::{Value, json};

use super::unified_action::{self, UnifiedActionRequest};

// ── Direct-tool-call repair (non-unified mode) ──────────────────────────────

/// Repair arguments for a directly-called tool (file_read, edit_file, etc.)
/// Handles: bare strings, positional arrays, common JSON syntax errors, param aliases.
/// Returns `Some(repaired_json_string)` on success, `None` if no repair needed or unrepairable.
pub fn repair_direct_tool_arguments(tc_name: &str, arguments: &str) -> Option<String> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. If it's already valid JSON object with canonical keys, no repair needed.
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if v.is_object() {
            let mut repaired = v.clone();
            normalize_direct_tool_params(tc_name, &mut repaired);
            if repaired != v {
                return serde_json::to_string(&repaired).ok();
            }
            return None; // Already valid and canonical
        }
        // Valid JSON but not an object (string, array, number) — fall through to repair
    }

    // 2. Try to fix common JSON syntax errors first
    let syntax_fixed = fix_json_syntax(trimmed);
    if let Ok(v) = serde_json::from_str::<Value>(&syntax_fixed) {
        return convert_direct_value(tc_name, v);
    }

    // 3. Bare string (e.g. "src/main.rs" instead of {"path":"src/main.rs"})
    if let Ok(s) = serde_json::from_str::<String>(trimmed) {
        return bare_string_to_params(tc_name, &s).and_then(|v| serde_json::to_string(&v).ok());
    }
    // Also try: bare unquoted string treated as path (e.g. arguments = src/main.rs )
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') && !trimmed.starts_with('"') {
        if let Some(v) = bare_string_to_params(tc_name, trimmed) {
            return serde_json::to_string(&v).ok();
        }
    }

    // 4. Positional JSON array (e.g. ["src/main.rs", 0, 200])
    if let Ok(arr) = serde_json::from_str::<Vec<Value>>(trimmed) {
        return positional_to_named(tc_name, &arr).and_then(|v| serde_json::to_string(&v).ok());
    }
    // Also try array after syntax fix
    if syntax_fixed.trim().starts_with('[') {
        if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&syntax_fixed) {
            return positional_to_named(tc_name, &arr).and_then(|v| serde_json::to_string(&v).ok());
        }
    }

    None
}

/// Normalize param aliases for direct-tool-call mode (reuses the logic from
/// `normalize_delegate_params` but works directly on the params object).
pub fn normalize_direct_tool_params(action: &str, params: &mut Value) {
    // Reuse normalize_delegate_params by mapping action names to their unified equivalents.
    // The unified function expects action names like "file_read", "edit_file", etc.
    // For direct tool calls, the tc_name already matches these names (after aliases resolved).
    let unified_action = match action {
        "file_read" | "file_write" | "edit_file" | "delete_range" |
        "file_list" | "file_search" | "code_search" | "shell_exec" |
        "git" | "symbol" | "code_graph" | "load_skill" | "project_detect" |
        "web_fetch" | "recall" | "finish" => action,
        "read" => "file_read",
        "write" => "file_write",
        "edit" => "edit_file",
        "git_status" | "git_diff" => "git",
        "find_symbol" | "read_symbol" => "symbol",
        _ => action,
    };
    normalize_delegate_params(unified_action, params);
}

/// Convert a bare string argument to the appropriate named params for a tool.
fn bare_string_to_params(tc_name: &str, s: &str) -> Option<Value> {
    match tc_name {
        "file_read" | "file_write" | "edit_file" | "delete_range" | "file_list" | "load_skill" => {
            Some(json!({ "path": s }))
        }
        "shell_exec" => Some(json!({ "command": s })),
        "code_search" | "file_search" => Some(json!({ "pattern": s })),
        "recall" => Some(json!({ "node_id": s })),
        "web_fetch" => Some(json!({ "url": s })),
        "symbol" | "find_symbol" | "read_symbol" => Some(json!({ "name": s })),
        "finish" | "deliver" | "report" | "done" | "complete" => Some(json!({ "content": s })),
        _ => None,
    }
}

/// Convert positional array arguments to named arguments for common tools.
fn positional_to_named(tc_name: &str, arr: &[Value]) -> Option<Value> {
    if arr.is_empty() {
        return None;
    }
    match tc_name {
        "file_read" => {
            // [path] or [path, offset] or [path, offset, limit]
            let path = arr.get(0)?.as_str()?;
            let offset = arr.get(1).and_then(|v| v.as_u64());
            let limit = arr.get(2).and_then(|v| v.as_u64());
            let mut obj = serde_json::Map::new();
            obj.insert("path".into(), Value::String(path.into()));
            if let Some(o) = offset { obj.insert("offset".into(), json!(o)); }
            if let Some(l) = limit { obj.insert("limit".into(), json!(l)); }
            Some(Value::Object(obj))
        }
        "file_write" => {
            // [path, content]
            let path = arr.get(0)?.as_str()?;
            let content = arr.get(1).map(|v| {
                v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
            })?;
            Some(json!({ "path": path, "content": content }))
        }
        "edit_file" => {
            // [path, old_string, new_string]
            let path = arr.get(0)?.as_str()?;
            let old = arr.get(1).map(|v| {
                v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
            })?;
            let new = arr.get(2).map(|v| {
                v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
            })?;
            Some(json!({ "path": path, "old_string": old, "new_string": new }))
        }
        "delete_range" => {
            // [path, start_anchor, end_anchor]
            let path = arr.get(0)?.as_str()?;
            let start = arr.get(1)?.clone();
            let end = arr.get(2)?.clone();
            Some(json!({ "path": path, "start_anchor": start, "end_anchor": end }))
        }
        "code_search" | "file_search" => {
            // [pattern] or [pattern, path]
            let pattern = arr.get(0)?.as_str()?;
            let path = arr.get(1).and_then(|v| v.as_str());
            let mut obj = serde_json::Map::new();
            obj.insert("pattern".into(), Value::String(pattern.into()));
            if let Some(p) = path { obj.insert("path".into(), Value::String(p.into())); }
            Some(Value::Object(obj))
        }
        "shell_exec" => {
            // [command]
            let cmd = arr.get(0)?.as_str()?;
            Some(json!({ "command": cmd }))
        }
        "symbol" | "find_symbol" => {
            // [name] or [name, top_k]
            let name = arr.get(0)?.as_str()?;
            let top_k = arr.get(1).and_then(|v| v.as_u64());
            let mut obj = serde_json::Map::new();
            obj.insert("name".into(), Value::String(name.into()));
            if let Some(k) = top_k { obj.insert("top_k".into(), json!(k)); }
            Some(Value::Object(obj))
        }
        _ => None,
    }
}

/// Fix common JSON syntax errors that LLM frequently makes:
/// - Single quotes instead of double quotes: `{'path': 'x'}`
/// - Unquoted object keys: `{path: "x"}`
/// - Trailing commas: `{"a": 1, "b": 2,}`
/// - Python `None` / `True` / `False`
fn fix_json_syntax(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    // Fast path: if it already parses, skip
    if serde_json::from_str::<Value>(&s).is_ok() {
        return s;
    }

    // Replace Python-style constants first (before quote handling)
    s = s.replace(": None", ": null")
         .replace(":None", ":null")
         .replace(": True", ": true")
         .replace(":True", ":true")
         .replace(": False", ": false")
         .replace(":False", ":false");

    // Heuristic: convert single-quoted JSON to double-quoted.
    // We do this carefully to avoid destroying escaped quotes inside strings.
    // Strategy: if the string contains `'` but no valid JSON `"key":` pattern, try converting.
    let has_single_quotes = s.contains('\'');
    let has_double_key_colon = regex_has_double_key(&s);
    if has_single_quotes && !has_double_key_colon {
        s = single_to_double_quotes(&s);
    } else if has_single_quotes {
        // Mixed: try to still fix single-quoted keys
        s = fix_single_quoted_keys(&s);
    }

    // Fix trailing commas before ] or }
    s = regex_remove_trailing_commas(&s);

    // Fix unquoted object keys: {key: "value"} -> {"key": "value"}
    s = fix_unquoted_keys(&s);

    s
}

fn regex_has_double_key(s: &str) -> bool {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#""[A-Za-z_][A-Za-z0-9_]*"\s*:"#).unwrap()
    });
    RE.is_match(s)
}

fn single_to_double_quotes(s: &str) -> String {
    // Simple conversion: replace all ' with ", then fix escaped quotes.
    // This is heuristic but works for the common malformed patterns LLM emits.
    s.replace('\'', "\"")
}

fn fix_single_quoted_keys(s: &str) -> String {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"'([A-Za-z_][A-Za-z0-9_-]*)'\s*:").unwrap()
    });
    RE.replace_all(s, "\"$1\":").to_string()
}

fn regex_remove_trailing_commas(s: &str) -> String {
    let mut result = s.to_string();
    // Trailing comma before }: {"a":1,}
    static RE1: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r",\s*}").unwrap()
    });
    result = RE1.replace_all(&result, "}").to_string();
    // Trailing comma before ]: [1,2,]
    static RE2: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r",\s*]").unwrap()
    });
    result = RE2.replace_all(&result, "]").to_string();
    result
}

fn fix_unquoted_keys(s: &str) -> String {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        // Match { or , followed by whitespace, then identifier, then :
        Regex::new(r"([{,])\s*([A-Za-z_][A-Za-z0-9_]*)\s*:").unwrap()
    });
    RE.replace_all(s, |caps: &regex::Captures| {
        format!("{}\"{}\":", &caps[1], &caps[2])
    }).to_string()
}

fn convert_direct_value(tc_name: &str, v: Value) -> Option<String> {
    let result = match v {
        Value::String(s) => bare_string_to_params(tc_name, &s)?,
        Value::Array(arr) => positional_to_named(tc_name, &arr)?,
        Value::Object(obj) => {
            let mut val = Value::Object(obj);
            normalize_direct_tool_params(tc_name, &mut val);
            val
        }
        _ => return None,
    };
    serde_json::to_string(&result).ok()
}

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
        && let Ok(req) = build_unified_from_pairs(&xml)
    {
        return serde_json::to_string(&req).ok();
    }

    if let Some(extracted) = extract_json_object_with_action(trimmed)
        && let Ok(req) = try_parse_unified(&extracted)
    {
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
///
/// For direct-tool-call mode (tc_name is file_read/edit_file/etc.) also repairs:
/// bare strings, positional arrays, single-quoted JSON, unquoted keys, param aliases.
pub fn recover_tool_call_arguments(tc_name: &str, arguments: &str, _fallbacks: &[&str]) -> String {
    // ── Unified mode (complete_and_check) ──
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
        return arguments.to_string();
    }

    // ── Direct-tool-call mode ──
    // Apply syntax + shape repair for all real tool names (file_read, edit_file, ...)
    let is_known_direct_tool = matches!(tc_name,
        "file_read" | "file_write" | "edit_file" | "delete_range" |
        "file_list" | "file_search" | "code_search" | "shell_exec" |
        "git" | "symbol" | "code_graph" | "load_skill" | "project_detect" |
        "web_fetch" | "recall" | "finish" |
        "read" | "write" | "edit" |
        "git_status" | "git_diff" | "find_symbol" | "read_symbol" |
        "deliver" | "report" | "done" | "complete"
    );
    if is_known_direct_tool {
        if let Some(repaired) = repair_direct_tool_arguments(tc_name, arguments) {
            tracing::info!(
                "[TOOL_ARGS] Repaired direct-tool args for `{}`: {} → {}",
                tc_name,
                preview(arguments, 120),
                preview(&repaired, 120)
            );
            return repaired;
        }
    }
    arguments.to_string()
}

fn preview(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
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

    // ── If action is missing, infer it from params keys ──
    if !obj.contains_key("action") {
        if let Some(params) = obj.get("params") {
            if let Some(inferred) = infer_action_from_params(params) {
                tracing::info!(
                    "[NORMALIZE] Inferred action=`{}` from params keys",
                    inferred
                );
                obj.insert("action".into(), Value::String(inferred));
            }
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

/// Infer the intended action from params keys when `action` is missing.
///
/// This catches the common failure where the LLM sends correct params but
/// forgets the `action` field entirely.
pub fn infer_action_from_params(params: &Value) -> Option<String> {
    let obj = params.as_object()?;
    let keys: std::collections::HashSet<&str> = obj.keys().map(|k| k.as_str()).collect();

    // Write operations
    if keys.contains("old_string") && keys.contains("new_string") {
        return Some("edit_file".into());
    }
    if keys.contains("content") && keys.contains("path") {
        // Could be file_write or finish; if path looks like a file, prefer file_write
        if let Some(path) = obj.get("path").and_then(|v| v.as_str()) {
            if path.contains('.') || path.contains('/') || path.contains('\\') {
                return Some("file_write".into());
            }
        }
    }
    if keys.contains("start_anchor") && keys.contains("end_anchor") {
        return Some("delete_range".into());
    }
    if keys.contains("command") {
        return Some("shell_exec".into());
    }

    // Read operations
    // file_pattern is unique to code_search - check it first
    if keys.contains("file_pattern") {
        return Some("code_search".into());
    }
    if keys.contains("pattern") {
        // pattern -> file_search (code_search already handled above)
        return Some("file_search".into());
    }
    if keys.contains("offset") || keys.contains("limit") {
        return Some("file_read".into());
    }
    if keys.contains("path") {
        return Some("file_read".into());
    }
    if keys.contains("op") {
        // Could be code_graph or git; code_graph is more common
        return Some("code_graph".into());
    }
    if keys.contains("node_id") {
        return Some("recall".into());
    }
    if keys.contains("url") {
        return Some("web_fetch".into());
    }
    if keys.contains("name") {
        return Some("symbol".into());
    }
    None
}

/// Map common LLM param aliases to canonical tool schemas.
pub fn normalize_delegate_params(action: &str, params: &mut Value) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };

    match action {
        "find_symbol" | "read_symbol" | "symbol" if !obj.contains_key("name") => {
            for alias in ["symbol", "query", "pattern", "class", "type"] {
                if let Some(v) = obj.get(alias).cloned() {
                    obj.insert("name".into(), v);
                    break;
                }
            }
        }
        // Composite: git - infer op from params if missing
        "git" => {
            if !obj.contains_key("op") {
                if obj.contains_key("staged") || obj.contains_key("path") {
                    obj.insert("op".into(), json!("diff"));
                } else {
                    obj.insert("op".into(), json!("status"));
                }
            }
        }
        // Composite: symbol - infer op from params if missing
        "symbol" => {
            if !obj.contains_key("op") {
                if obj.contains_key("pattern") {
                    obj.insert("op".into(), json!("find"));
                } else if obj.contains_key("name") {
                    obj.insert("op".into(), json!("read"));
                } else {
                    obj.insert("op".into(), json!("find"));
                }
            }
        }
        "file_read" | "file_write" | "edit_file" | "delete_range" if !obj.contains_key("path") => {
            for alias in ["file", "filepath", "file_path", "filename", "key"] {
                if let Some(v) = obj.get(alias).cloned() {
                    obj.insert("path".into(), v);
                    break;
                }
            }
        }
        "file_search" => {
            if !obj.contains_key("pattern")
                && let Some(v) = obj.get("query").cloned()
            {
                obj.insert("pattern".into(), v);
            }
            if !obj.contains_key("path")
                && let Some(v) = obj.get("dir").or(obj.get("directory")).cloned()
            {
                obj.insert("path".into(), v);
            }
        }
        "code_search" => {
            if !obj.contains_key("pattern")
                && let Some(v) = obj.get("query").or(obj.get("q")).cloned()
            {
                obj.insert("pattern".into(), v);
            }
        }
        "code_graph" => {
            if !obj.contains_key("op")
                && let Some(v) = obj.get("action").or(obj.get("operation")).cloned()
            {
                obj.insert("op".into(), v);
            }
        }
        "delete_range" => {
            if !obj.contains_key("start_anchor")
                && let Some(v) = obj.get("start_line").or(obj.get("start")).cloned()
            {
                obj.insert("start_anchor".into(), v);
            }
            if !obj.contains_key("end_anchor")
                && let Some(v) = obj.get("end_line").or(obj.get("end")).cloned()
            {
                obj.insert("end_anchor".into(), v);
            }
        }
        "shell_exec" => {
            if !obj.contains_key("command")
                && let Some(v) = obj.get("cmd").cloned()
            {
                obj.insert("command".into(), v);
            }
        }
        "recall" => {
            if !obj.contains_key("node_id")
                && let Some(v) = obj.get("key").or(obj.get("id")).cloned()
            {
                obj.insert("node_id".into(), v);
            }
        }
        "finish" | "deliver" | "report" | "done" | "complete" => {
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
            if !obj.contains_key("finding_json")
                && let Some(v) = obj.get("findings").or(obj.get("finding")).cloned()
            {
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

    #[test]
    fn repairs_read_symbol_name_alias() {
        let raw = r#"{"action":"read_symbol","params":{"symbol":"Foo"}}"#;
        let out = repair_unified_arguments(raw).unwrap();
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.params["name"], "Foo");
    }

    #[test]
    fn repairs_code_graph_op_alias() {
        let raw = r#"{"action":"code_graph","params":{"action":"query","pattern":"main"}}"#;
        let out = repair_unified_arguments(raw).unwrap();
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.params["op"], "query");
    }

    #[test]
    fn repairs_delete_range_anchor_aliases() {
        let raw =
            r#"{"action":"delete_range","params":{"path":"x.rs","start_line":10,"end_line":20}}"#;
        let out = repair_unified_arguments(raw).unwrap();
        let req: UnifiedActionRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(req.params["start_anchor"], 10);
        assert_eq!(req.params["end_anchor"], 20);
    }

    // ── Direct-tool-call repair tests ──────────────────────────────────────────

    #[test]
    fn direct_bare_string_file_read() {
        let out = repair_direct_tool_arguments("file_read", "\"src/main.rs\"").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn direct_unquoted_bare_string() {
        let out = repair_direct_tool_arguments("file_read", "src/main.rs").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn direct_positional_array_file_read() {
        let out = repair_direct_tool_arguments("file_read", r#"["src/a.rs", 0, 200]"#).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "src/a.rs");
        assert_eq!(v["offset"], 0);
        assert_eq!(v["limit"], 200);
    }

    #[test]
    fn direct_positional_array_edit_file() {
        let out = repair_direct_tool_arguments(
            "edit_file",
            r#"["x.rs", "fn foo() {}", "fn bar() {}"]"#,
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "x.rs");
        assert_eq!(v["old_string"], "fn foo() {}");
        assert_eq!(v["new_string"], "fn bar() {}");
    }

    #[test]
    fn direct_single_quoted_json() {
        // {'path': 'a.rs', 'offset': 5}
        let out = repair_direct_tool_arguments(
            "file_read",
            "{'path': 'a.rs', 'offset': 5}",
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
        assert_eq!(v["offset"], 5);
    }

    #[test]
    fn direct_unquoted_keys() {
        let out = repair_direct_tool_arguments(
            "file_read",
            "{path: \"a.rs\", limit: 100}",
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
        assert_eq!(v["limit"], 100);
    }

    #[test]
    fn direct_trailing_comma() {
        let out = repair_direct_tool_arguments(
            "file_write",
            "{\"path\": \"a.rs\", \"content\": \"hi\",}",
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
        assert_eq!(v["content"], "hi");
    }

    #[test]
    fn direct_param_alias_filepath() {
        let out = repair_direct_tool_arguments(
            "file_read",
            r#"{"filepath": "a.rs"}"#,
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn direct_param_alias_shell_cmd() {
        let out = repair_direct_tool_arguments(
            "shell_exec",
            r#"{"cmd": "ls -la"}"#,
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["command"], "ls -la");
    }

    #[test]
    fn direct_bare_string_shell_exec() {
        let out = repair_direct_tool_arguments("shell_exec", "\"echo hello\"").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["command"], "echo hello");
    }

    #[test]
    fn direct_bare_string_search() {
        let out = repair_direct_tool_arguments("code_search", "\"TODO\"").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["pattern"], "TODO");
    }

    #[test]
    fn direct_bare_string_finish() {
        let out = repair_direct_tool_arguments("finish", "\"分析完成\"").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["content"], "分析完成");
    }

    #[test]
    fn direct_bare_string_symbol() {
        let out = repair_direct_tool_arguments("symbol", "\"MyStruct\"").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["name"], "MyStruct");
    }

    #[test]
    fn direct_python_constants() {
        let raw = "{\"path\": \"a.rs\", \"debug\": True, \"extra\": None, \"flag\": False}";
        let out = repair_direct_tool_arguments("file_read", raw).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
        assert_eq!(v["debug"], true);
        assert_eq!(v["extra"], Value::Null);
        assert_eq!(v["flag"], false);
    }

    #[test]
    fn direct_valid_canonical_returns_none() {
        // Already valid and canonical → no repair needed
        let result = repair_direct_tool_arguments(
            "file_read",
            r#"{"path": "a.rs"}"#,
        );
        assert!(result.is_none());
    }

    #[test]
    fn direct_alias_fix_returns_some() {
        // Valid JSON but has alias → must repair (returns Some with normalized form)
        let out = repair_direct_tool_arguments(
            "file_read",
            r#"{"file": "a.rs"}"#,
        ).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn direct_mixed_syntax_errors() {
        // Single-quoted keys + unquoted value string + trailing comma
        let raw = "{'pattern': TODO, 'path': src,}";
        let out = repair_direct_tool_arguments("code_search", raw);
        // This may or may not fully repair depending on regex behavior;
        // if it fails we accept None (error feedback will guide LLM)
        if let Some(s) = out {
            assert!(serde_json::from_str::<Value>(&s).is_ok());
        }
    }

    #[test]
    fn recover_direct_tool_args_through_entry() {
        // recover_tool_call_arguments must route direct tools through repair_direct_tool_arguments
        let args = "{'filepath': 'a.rs'}";
        let out = recover_tool_call_arguments("file_read", args, &[]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn recover_direct_bare_string_through_entry() {
        let out = recover_tool_call_arguments("edit_file", "\"src/a.rs\"", &[]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "src/a.rs");
    }

    #[test]
    fn recover_direct_positional_array_through_entry() {
        let out = recover_tool_call_arguments(
            "file_read",
            r#"["a.rs", 10, 50]"#,
            &[],
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["path"], "a.rs");
        assert_eq!(v["offset"], 10);
        assert_eq!(v["limit"], 50);
    }
}
