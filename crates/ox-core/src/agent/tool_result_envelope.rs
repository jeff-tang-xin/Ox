//! Unified JSON envelope for `complete_and_check` tool results (Observation).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeStatus {
    #[default]
    Ok,
    Denied,
    Confirmed,
    Discuss,
    Rejected,
    UserFinished,
    UserContinue,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolResultEnvelope {
    pub status: EnvelopeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(default)]
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolResultEnvelope {
    pub fn ok(data: Value) -> Self {
        Self {
            status: EnvelopeStatus::Ok,
            gate: None,
            data,
            user: None,
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            status: EnvelopeStatus::Error,
            gate: None,
            data: Value::Null,
            user: None,
            error: Some(msg.into()),
        }
    }

    pub fn gate_status(status: EnvelopeStatus, gate: &str, data: Value) -> Self {
        Self {
            status,
            gate: Some(gate.to_string()),
            data,
            user: None,
            error: None,
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| json!({"status":"error","error":"serialize failed"}).to_string())
    }

    /// Compact LLM-facing format — saves ~150 tokens per tool result vs full JSON.
    pub fn to_compact(&self) -> String {
        match self.status {
            EnvelopeStatus::Ok => {
                let action = self
                    .data
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = self
                    .data
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if action.is_empty() {
                    output.to_string()
                } else {
                    format!("✓ {action}\n{output}")
                }
            }
            EnvelopeStatus::Error => {
                let err = self.error.as_deref().unwrap_or("unknown error");
                format!("✗ {err}")
            }
            EnvelopeStatus::Confirmed => {
                let gate = self.gate.as_deref().unwrap_or("");
                let kind = self.data.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if kind.is_empty() {
                    format!("✓ confirmed ({gate})")
                } else {
                    format!("✓ confirmed ({gate}:{kind})")
                }
            }
            EnvelopeStatus::Discuss => {
                let gate = self.gate.as_deref().unwrap_or("");
                format!("💬 discuss ({gate}) — 见系统指令")
            }
            EnvelopeStatus::Denied | EnvelopeStatus::Rejected => {
                let gate = self.gate.as_deref().unwrap_or("safety");
                format!("✗ denied ({gate})")
            }
            EnvelopeStatus::UserFinished => "✓ finished — 用户确认结束".to_string(),
            EnvelopeStatus::UserContinue => "→ continue — 用户要求继续".to_string(),
        }
    }
}
