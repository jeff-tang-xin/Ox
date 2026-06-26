//! User-facing review presentation — executive summary vs full detail.

use super::findings::{self, FindingsStore};
use super::perception::{self, PerceptionFindings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPresentation {
    pub executive_summary: String,
    pub findings_table: String,
    pub detail_available: bool,
}

pub fn from_store(store: &FindingsStore) -> ReviewPresentation {
    let p = PerceptionFindings {
        findings_summary: store.summary.clone(),
        findings: store
            .findings
            .iter()
            .map(|f| perception::FindingItem {
                index: f.index,
                severity: f.severity.label().to_string(),
                file: f.file.clone(),
                target: f.symbol.clone(),
                issue: f.issue.clone(),
                recommendation: f.recommendation.clone(),
                fix_plan: f.fix_plan.clone(),
            })
            .collect(),
    };
    from_perception(&p)
}

pub fn from_perception(p: &PerceptionFindings) -> ReviewPresentation {
    let high = p
        .findings
        .iter()
        .filter(|f| f.severity.to_lowercase().contains("high"))
        .count();
    let med = p
        .findings
        .iter()
        .filter(|f| {
            let s = f.severity.to_lowercase();
            s.contains("medium") || s.contains("中")
        })
        .count();
    let low = p.findings.len().saturating_sub(high + med);
    let executive_summary = if p.findings_summary.is_empty() {
        format!(
            "共 {} 项发现（高 {} / 中 {} / 低 {}）",
            p.findings.len(),
            high,
            med,
            low
        )
    } else {
        format!(
            "{}\n\n共 {} 项（高 {} / 中 {} / 低 {}）",
            p.findings_summary,
            p.findings.len(),
            high,
            med,
            low
        )
    };
    let mut table = String::from("| # | 严重度 | 位置 | 问题 |\n|---|--------|------|------|\n");
    for f in &p.findings {
        let loc = if f.file.is_empty() {
            f.target.clone()
        } else {
            format!("`{}`", f.file)
        };
        let issue: String = f.issue.chars().take(60).collect();
        table.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            f.index, f.severity, loc, issue
        ));
    }
    ReviewPresentation {
        executive_summary,
        findings_table: table,
        detail_available: !p.findings.is_empty(),
    }
}

pub fn format_executive(store: &FindingsStore) -> String {
    let p = from_store(store);
    format!(
        "## 审查摘要\n\n{}\n\n{}\n\n> 详情: /findings · 实施: /fix 1,2 → /confirm",
        p.executive_summary, p.findings_table
    )
}

/// Short summary for TUI findings panel (rows rendered separately — no markdown table).
pub fn panel_summary(store: &FindingsStore) -> String {
    let s = from_store(store).executive_summary;
    let compact: String = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    if compact.chars().count() > 220 {
        format!("{}…", compact.chars().take(219).collect::<String>())
    } else {
        compact
    }
}

/// Compact findings card for chat / TurnDone (no JSON, no markdown table).
pub fn format_findings_card(store: &FindingsStore) -> String {
    let mut lines = vec!["**审查发现**".to_string()];
    if !store.summary.is_empty() {
        lines.push(store.summary.clone());
    }
    lines.push(String::new());
    for f in &store.findings {
        let loc = if f.file.is_empty() {
            f.symbol.clone()
        } else if f.symbol.is_empty() {
            f.file.clone()
        } else {
            format!("{} · {}", f.file, f.symbol)
        };
        lines.push(format!("**#{}** [{}] {}", f.index, f.severity.label(), loc));
        let issue_short: String = f.issue.chars().take(150).collect();
        lines.push(format!("  {}", issue_short));
        if !f.recommendation.is_empty() {
            let rec_short: String = f.recommendation.chars().take(100).collect();
            lines.push(format!("  → {}", rec_short));
        }
        lines.push(String::new());
    }
    lines.push("面板 `1-9` 选范围 · `c` 或 /confirm 确认实施 · /discuss 讨论".to_string());
    lines.join("\n").trim_end().to_string()
}

pub fn load_executive(engine: &super::engine::WorkflowEngine) -> Option<String> {
    findings::load_or_migrate(engine).map(|s| format_executive(&s))
}
