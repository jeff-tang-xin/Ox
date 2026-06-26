//! Machine-verifiable workflow completion receipt.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;
use super::findings::{self, FindingStatus, FindingsStore};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionReceipt {
    pub complete: bool,
    pub resolved_indices: Vec<u32>,
    #[serde(default)]
    pub verify_results: Vec<VerifyResult>,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyResult {
    pub finding_index: u32,
    pub command: String,
    pub exit_code: i32,
    pub passed: bool,
}

pub const COMPLETION_RECEIPT_SCHEMA: &str = r#"```json
{
  "completion_receipt": {
    "complete": true,
    "resolved_indices": [1, 2],
    "verify_results": [
      {"finding_index": 1, "command": "cargo test -p foo", "exit_code": 0, "passed": true}
    ],
    "summary": "已修复 #1 #2 并通过验证"
  }
}
```"#;

/// Extract completion receipt from assistant output (```json block).
pub fn extract_from_text(text: &str) -> Option<CompletionReceipt> {
    let json_str = extract_json_block(text)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    if v.get("completion_receipt").is_some() {
        serde_json::from_value(v.get("completion_receipt")?.clone()).ok()
    } else if v.get("complete").is_some() {
        serde_json::from_value(v).ok()
    } else {
        None
    }
}

fn extract_json_block(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = start + 7;
        if let Some(end_off) = text[after..].find("```") {
            let inner = text[after..after + end_off].trim();
            if inner.contains("completion_receipt") || inner.contains("\"complete\"") {
                return Some(inner.to_string());
            }
        }
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end >= start {
            let slice = &text[start..=end];
            if slice.contains("completion_receipt") || slice.contains("\"complete\"") {
                return Some(slice.to_string());
            }
        }
    }
    None
}

/// Validate receipt against active scope and finding statuses.
pub fn validate(engine: &WorkflowEngine, receipt: &CompletionReceipt) -> Result<(), String> {
    if !receipt.complete {
        return Err("completion_receipt.complete 必须为 true".to_string());
    }
    // Hard gate first: a non-zero exit code is always invalid, regardless of
    // scope or findings — the model cannot mark a failed build as passed.
    for v in &receipt.verify_results {
        if v.exit_code != 0 {
            return Err(format!(
                "验证命令 `{}` exit_code={} — 构建/测试未通过，禁止 ## Done（勿将 passed 标为 true）",
                v.command, v.exit_code
            ));
        }
        if !v.passed {
            return Err(format!(
                "finding #{} 验证未通过: {} (exit {})",
                v.finding_index, v.command, v.exit_code
            ));
        }
    }
    if let Some(status) =
        engine.get_variable(crate::agent::post_edit_verification::VERIFY_STATUS_KEY)
    {
        if status == "failed" {
            return Err(
                "最近一次 shell 验证失败 — 须修复错误并重新运行验证后再 ## Done".to_string(),
            );
        }
    }
    if crate::agent::post_edit_verification::verify_status_blocks_done(engine) {
        return Err(
            "项目验证尚未通过（verify 状态未为 passed）— 须 shell_exec 验证成功后再 ## Done"
                .to_string(),
        );
    }
    let store = findings::load_or_migrate(engine)
        .ok_or_else(|| "无 findings store，无法校验完成".to_string())?;
    let expected = if store.active_indices.is_empty() {
        store
            .findings
            .iter()
            .filter(|f| {
                !matches!(
                    f.status,
                    FindingStatus::Disputed | FindingStatus::Skipped | FindingStatus::WontFix
                )
            })
            .map(|f| f.index)
            .collect::<Vec<_>>()
    } else {
        store.active_indices.clone()
    };
    let mut resolved = receipt.resolved_indices.clone();
    resolved.sort_unstable();
    let mut exp = expected.clone();
    exp.sort_unstable();
    if resolved != exp {
        return Err(format!(
            "resolved_indices {:?} 与实施范围 {:?} 不一致",
            resolved, exp
        ));
    }
    Ok(())
}

pub fn apply_receipt(store: &mut FindingsStore, receipt: &CompletionReceipt) {
    for idx in &receipt.resolved_indices {
        if let Some(f) = store.get_mut(*idx) {
            f.status = FindingStatus::Done;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_receipt_json() {
        let text = r#"## Done
```json
{"completion_receipt":{"complete":true,"resolved_indices":[1,2],"verify_results":[],"summary":"ok"}}
```"#;
        let r = extract_from_text(text).unwrap();
        assert!(r.complete);
        assert_eq!(r.resolved_indices, vec![1, 2]);
    }

    #[test]
    fn reject_receipt_with_nonzero_exit_code() {
        let session = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::agent::session::SessionState::new("test"),
        ));
        let engine = crate::agent::engine::WorkflowEngine::new(std::sync::Arc::clone(&session));
        let receipt = CompletionReceipt {
            complete: true,
            resolved_indices: vec![1],
            verify_results: vec![VerifyResult {
                finding_index: 1,
                command: "mvn compile".into(),
                exit_code: 1,
                passed: true,
            }],
            summary: "ok".into(),
        };
        let err = validate(&engine, &receipt).unwrap_err();
        assert!(err.contains("exit_code=1"));
    }
}
