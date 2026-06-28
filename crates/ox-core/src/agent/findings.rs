//! Canonical findings store — single source of truth for review → park → implement.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;
use super::perception::{self, FindingItem, PerceptionFindings};
use super::plan_tracker::{PlanStep, PlanTracker, StepStatus};

const STORE_KEY: &str = "_findings_store";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Medium,
    High,
    Low,
}

impl Severity {
    pub fn from_label(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "high" | "高" => Self::High,
            "low" | "低" => Self::Low,
            _ => Self::Medium,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::High => "高",
            Self::Low => "低",
            Self::Medium => "中",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    #[default]
    Open,
    Disputed,
    Scoped,
    InProgress,
    AwaitingVerify,
    Done,
    WontFix,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DisputeKind {
    FalsePositive,
    WontFix,
    NeedsClarification,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dispute {
    pub kind: DisputeKind,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ImplAction {
    pub tool: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub index: u32,
    pub severity: Severity,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub symbol: String,
    pub issue: String,
    #[serde(default)]
    pub recommendation: String,
    /// Concrete fix: which lines, how to change them, code sketch. Captured during
    /// review so the Implement phase inherits the plan instead of re-analyzing.
    #[serde(default)]
    pub fix_plan: String,
    #[serde(default)]
    pub status: FindingStatus,
    #[serde(default)]
    pub user_notes: Vec<String>,
    #[serde(default)]
    pub dispute: Option<Dispute>,
    #[serde(default)]
    pub impl_log: Vec<ImplAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FindingsStore {
    #[serde(default)]
    pub summary: String,
    pub findings: Vec<Finding>,
    /// User-confirmed implementation scope (1-based indices).
    #[serde(default)]
    pub active_indices: Vec<u32>,
}

impl FindingsStore {
    /// Check if there are any pending (not Done/Skipped/WontFix) findings
    pub fn has_pending_findings(&self) -> bool {
        self.findings.iter().any(|f| {
            !matches!(
                f.status,
                FindingStatus::Done | FindingStatus::Skipped | FindingStatus::WontFix
            )
        })
    }

    /// Get pending findings count
    pub fn pending_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| {
                !matches!(
                    f.status,
                    FindingStatus::Done | FindingStatus::Skipped | FindingStatus::WontFix
                )
            })
            .count()
    }

    pub fn from_perception(p: &PerceptionFindings) -> Self {
        let findings = p.findings.iter().map(finding_from_item).collect();
        Self {
            summary: p.findings_summary.clone(),
            findings,
            active_indices: Vec::new(),
        }
    }

    pub fn get(&self, index: u32) -> Option<&Finding> {
        self.findings.iter().find(|f| f.index == index)
    }

    pub fn get_mut(&mut self, index: u32) -> Option<&mut Finding> {
        self.findings.iter_mut().find(|f| f.index == index)
    }

    pub fn open_findings(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| matches!(f.status, FindingStatus::Open))
            .collect()
    }

    pub fn scoped_findings(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| {
                matches!(
                    f.status,
                    FindingStatus::Scoped
                        | FindingStatus::InProgress
                        | FindingStatus::AwaitingVerify
                )
            })
            .collect()
    }

    pub fn set_scope(&mut self, indices: &[u32]) {
        self.active_indices = indices.to_vec();
        for f in &mut self.findings {
            if indices.contains(&f.index) {
                if matches!(
                    f.status,
                    FindingStatus::Open | FindingStatus::Scoped | FindingStatus::InProgress
                ) {
                    f.status = FindingStatus::Scoped;
                }
            } else if f.status == FindingStatus::Scoped {
                f.status = FindingStatus::Open;
            }
        }
    }

    pub fn add_scope(&mut self, indices: &[u32]) {
        let mut merged: Vec<u32> = self.active_indices.clone();
        for i in indices {
            if !merged.contains(i) {
                merged.push(*i);
            }
        }
        merged.sort_unstable();
        self.set_scope(&merged);
    }

    pub fn remove_scope(&mut self, indices: &[u32]) {
        let merged: Vec<u32> = self
            .active_indices
            .iter()
            .filter(|i| !indices.contains(i))
            .copied()
            .collect();
        self.set_scope(&merged);
    }

    pub fn mark_dispute(&mut self, index: u32, dispute: Dispute) {
        if let Some(f) = self.get_mut(index) {
            f.dispute = Some(dispute);
            f.status = FindingStatus::Disputed;
            self.active_indices.retain(|i| *i != index);
        }
    }

    pub fn skip(&mut self, index: u32) {
        if let Some(f) = self.get_mut(index) {
            f.status = FindingStatus::Skipped;
            self.active_indices.retain(|i| *i != index);
        }
    }

    pub fn to_plan_tracker(&self, only_scoped: bool) -> PlanTracker {
        let steps: Vec<PlanStep> = self
            .findings
            .iter()
            .filter(|f| {
                if only_scoped {
                    self.active_indices.contains(&f.index)
                        && !matches!(
                            f.status,
                            FindingStatus::Disputed
                                | FindingStatus::Skipped
                                | FindingStatus::WontFix
                                | FindingStatus::Done
                        )
                } else {
                    !matches!(
                        f.status,
                        FindingStatus::Disputed | FindingStatus::Skipped | FindingStatus::WontFix
                    )
                }
            })
            .map(|f| PlanStep {
                index: f.index,
                file: f.file.clone(),
                action: "edit".to_string(),
                target: f.symbol.clone(),
                desc: {
                    let base = if f.recommendation.is_empty() {
                        f.issue.clone()
                    } else {
                        format!("{} → {}", f.issue, f.recommendation)
                    };
                    // Carry the concrete fix plan into the step so the Implement
                    // phase can edit directly instead of re-analyzing the code.
                    if f.fix_plan.trim().is_empty() {
                        base
                    } else {
                        format!("{base}\n方案: {}", f.fix_plan)
                    }
                },
                verify: String::new(),
                status: match f.status {
                    FindingStatus::Done => StepStatus::Done,
                    FindingStatus::Skipped => StepStatus::Skipped,
                    FindingStatus::InProgress | FindingStatus::AwaitingVerify => {
                        StepStatus::InProgress
                    }
                    _ => StepStatus::Pending,
                },
                awaiting_verify: f.status == FindingStatus::AwaitingVerify,
            })
            .collect();
        let mut tracker = PlanTracker {
            current_index: steps.first().map(|s| s.index).unwrap_or(1),
            steps,
        };
        if let Some(first) = tracker
            .steps
            .iter_mut()
            .find(|s| s.status == StepStatus::Pending)
        {
            first.status = StepStatus::InProgress;
            tracker.current_index = first.index;
        }
        tracker
    }

    pub fn progress_rows(&self) -> Vec<FindingProgressRow> {
        self.findings
            .iter()
            .map(|f| FindingProgressRow {
                index: f.index,
                severity: f.severity.label().to_string(),
                file: f.file.clone(),
                symbol: f.symbol.clone(),
                issue: f.issue.clone(),
                status: f.status,
                in_scope: self.active_indices.contains(&f.index),
            })
            .collect()
    }

    pub fn scope_confirm_summary(&self) -> String {
        if self.active_indices.is_empty() {
            return "（未选择任何 finding）".to_string();
        }
        let mut lines = vec![format!(
            "将修复 {} 项：{}",
            self.active_indices.len(),
            self.active_indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )];
        for idx in &self.active_indices {
            if let Some(f) = self.get(*idx) {
                let loc = if f.file.is_empty() {
                    f.symbol.clone()
                } else {
                    format!("`{}`", f.file)
                };
                lines.push(format!("  • #{} {} — {}", idx, loc, f.issue));
            }
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingProgressRow {
    pub index: u32,
    pub severity: String,
    pub file: String,
    pub symbol: String,
    pub issue: String,
    pub status: FindingStatus,
    pub in_scope: bool,
}

pub fn save(engine: &WorkflowEngine, store: &FindingsStore) {
    if let Ok(json) = serde_json::to_string(store) {
        engine.set_variable(STORE_KEY, json);
    }
}

pub fn load(engine: &WorkflowEngine) -> Option<FindingsStore> {
    engine
        .get_variable(STORE_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn clear(engine: &WorkflowEngine) {
    engine.set_variable(STORE_KEY, String::new());
}

/// Sync perception freeze → canonical store (Phase A dual-write).
pub fn sync_from_perception(engine: &WorkflowEngine, p: &PerceptionFindings) {
    let store = FindingsStore::from_perception(p);
    save(engine, &store);
    tracing::info!(
        "[FINDINGS] synced {} finding(s) to canonical store",
        store.findings.len()
    );
}

/// Load store or build from legacy perception key.
pub fn load_or_migrate(engine: &WorkflowEngine) -> Option<FindingsStore> {
    if let Some(store) = load(engine) {
        if !store.findings.is_empty() {
            return Some(store);
        }
    }
    if let Some(p) = perception::load(engine) {
        let store = FindingsStore::from_perception(&p);
        save(engine, &store);
        return Some(store);
    }
    engine
        .get_execute_review_report()
        .and_then(|r| synthesize_from_review_prose(&r))
        .map(|store| {
            save(engine, &store);
            store
        })
}

/// Build canonical findings from a prose review report (BUG-N / table / numbered).
pub fn synthesize_from_review_prose(report: &str) -> Option<FindingsStore> {
    let tracker = crate::agent::plan_tracker::load_from_review_report(report)?;
    if tracker.steps.is_empty() {
        return None;
    }
    let findings: Vec<Finding> = tracker
        .steps
        .iter()
        .map(|s| finding_from_plan_step(s))
        .collect();
    let summary = perception::extract_from_text(report)
        .map(|p| p.findings_summary)
        .filter(|s| s.chars().count() >= 8)
        .unwrap_or_else(|| {
            let n = findings.len();
            format!("审查发现 {n} 项问题（由报告正文解析）")
        });
    Some(FindingsStore {
        summary,
        findings,
        active_indices: Vec::new(),
    })
}

/// Ensure findings exist after review output — JSON preferred, prose fallback.
pub fn ensure_from_review_output(engine: &WorkflowEngine, output: &str) {
    // Try structured JSON first — always apply if found (even if findings already exist).
    if let Some(p) = perception::extract_from_text(output) {
        perception::save(engine, &p);
        sync_from_perception(engine, &p);
        return;
    }
    // Try prose synthesis — only if no findings exist yet.
    // (We already tried JSON above, so this is a last-resort fallback.)
    if load(engine).is_some_and(|s| !s.findings.is_empty()) {
        return; // Keep existing findings if LLM didn't provide structured JSON
    }
    if let Some(store) = synthesize_from_review_prose(output) {
        save(engine, &store);
        tracing::info!(
            "[FINDINGS] synthesized {} finding(s) from review prose",
            store.findings.len()
        );
    }
}

fn finding_from_plan_step(s: &PlanStep) -> Finding {
    let severity = if s.desc.contains("严重") || s.desc.to_lowercase().contains("high") {
        Severity::High
    } else if s.desc.contains("低") || s.desc.to_lowercase().contains("low") {
        Severity::Low
    } else {
        Severity::Medium
    };
    Finding {
        index: s.index,
        severity,
        file: s.file.clone(),
        symbol: s.target.clone(),
        issue: s.desc.clone(),
        recommendation: String::new(),
        fix_plan: String::new(),
        status: FindingStatus::Open,
        user_notes: Vec::new(),
        dispute: None,
        impl_log: Vec::new(),
    }
}

pub fn parse_scope_indices(text: &str) -> Vec<u32> {
    let mut indices = Vec::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = text[start..i].parse::<u32>() {
                if n > 0 && !indices.contains(&n) {
                    indices.push(n);
                }
            }
            continue;
        }
        i += 1;
    }
    indices.sort_unstable();
    indices
}

fn finding_from_item(item: &FindingItem) -> Finding {
    Finding {
        index: item.index,
        severity: Severity::from_label(&item.severity),
        file: item.file.clone(),
        symbol: item.target.clone(),
        issue: item.issue.clone(),
        recommendation: item.recommendation.clone(),
        fix_plan: item.fix_plan.clone(),
        status: FindingStatus::Open,
        user_notes: Vec::new(),
        dispute: None,
        impl_log: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_scope_marks_findings() {
        let mut store = FindingsStore {
            summary: String::new(),
            findings: vec![
                Finding {
                    index: 1,
                    severity: Severity::High,
                    file: "a.rs".into(),
                    symbol: String::new(),
                    issue: "i1".into(),
                    recommendation: String::new(),
                    fix_plan: String::new(),
                    status: FindingStatus::Open,
                    user_notes: vec![],
                    dispute: None,
                    impl_log: vec![],
                },
                Finding {
                    index: 2,
                    severity: Severity::Medium,
                    file: "b.rs".into(),
                    symbol: String::new(),
                    issue: "i2".into(),
                    recommendation: String::new(),
                    fix_plan: String::new(),
                    status: FindingStatus::Open,
                    user_notes: vec![],
                    dispute: None,
                    impl_log: vec![],
                },
            ],
            active_indices: vec![],
        };
        store.set_scope(&[1, 2]);
        assert_eq!(store.findings[0].status, FindingStatus::Scoped);
        assert_eq!(store.active_indices, vec![1, 2]);
        let tracker = store.to_plan_tracker(true);
        assert_eq!(tracker.steps.len(), 2);
    }

    #[test]
    fn parse_scope_indices_from_text() {
        assert_eq!(parse_scope_indices("修复 1、2和5"), vec![1, 2, 5]);
    }

    #[test]
    fn skip_removes_from_active() {
        let mut store = FindingsStore {
            summary: String::new(),
            findings: vec![Finding {
                index: 3,
                severity: Severity::Low,
                file: String::new(),
                symbol: String::new(),
                issue: "x".into(),
                recommendation: String::new(),
                fix_plan: String::new(),
                status: FindingStatus::Scoped,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![3],
        };
        store.skip(3);
        assert_eq!(store.findings[0].status, FindingStatus::Skipped);
        assert!(store.active_indices.is_empty());
    }
}
