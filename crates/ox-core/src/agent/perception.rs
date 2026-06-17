//! Structured perception output — findings JSON frozen at park, consumed in Act phase.

use serde::{Deserialize, Serialize};

use super::plan_tracker::{self, PlanStep, PlanTracker, StepStatus};

const FINDINGS_KEY: &str = "_perception_findings";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingItem {
    pub index: u32,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub target: String,
    pub issue: String,
    #[serde(default)]
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PerceptionFindings {
    #[serde(default)]
    pub findings_summary: String,
    pub findings: Vec<FindingItem>,
}

pub fn save(engine: &super::engine::WorkflowEngine, findings: &PerceptionFindings) {
    if let Ok(json) = serde_json::to_string(findings) {
        engine.set_variable(FINDINGS_KEY, json);
        tracing::info!(
            "[PERCEPTION] frozen {} finding(s)",
            findings.findings.len()
        );
    }
}

pub fn load(engine: &super::engine::WorkflowEngine) -> Option<PerceptionFindings> {
    engine
        .get_variable(FINDINGS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn clear(engine: &super::engine::WorkflowEngine) {
    engine.set_variable(FINDINGS_KEY, String::new());
}

/// Extract structured findings from LLM output (```json block with "findings" array).
pub fn extract_from_text(text: &str) -> Option<PerceptionFindings> {
    let json_str = extract_json_block(text)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let findings_arr = v.get("findings")?.as_array()?;
    if findings_arr.is_empty() {
        return None;
    }
    let mut findings = Vec::new();
    for (i, item) in findings_arr.iter().enumerate() {
        let obj = item.as_object()?;
        let index = obj
            .get("index")
            .and_then(|n| n.as_u64())
            .unwrap_or((i + 1) as u64) as u32;
        findings.push(FindingItem {
            index,
            severity: obj
                .get("severity")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            file: obj
                .get("file")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            target: obj
                .get("target")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            issue: obj
                .get("issue")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            recommendation: obj
                .get("recommendation")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    let findings_summary = v
        .get("findings_summary")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    Some(PerceptionFindings {
        findings_summary,
        findings,
    })
}

fn extract_json_block(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = start + 7;
        if let Some(end_off) = text[after..].find("```") {
            let inner = text[after..after + end_off].trim();
            if inner.contains("\"findings\"") {
                return Some(inner.to_string());
            }
        }
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end >= start {
            let slice = &text[start..=end];
            if slice.contains("\"findings\"") {
                return Some(slice.to_string());
            }
        }
    }
    None
}

/// Remove findings ```json``` / bare JSON from user-visible text (machine still gets full output).
pub fn strip_findings_json_blocks(text: &str) -> String {
    let mut out = text.to_string();
    loop {
        let Some(start) = out.find("```json") else {
            break;
        };
        let after = start + 7;
        let Some(end_off) = out[after..].find("```") else {
            break;
        };
        let end = after + end_off + 3;
        let block = &out[start..end];
        if block.contains("\"findings\"") {
            out.replace_range(start..end, "\n");
        } else {
            break;
        }
    }
    if let Some(json_str) = extract_json_block(text) {
        if out.contains(&json_str) {
            out = out.replace(&json_str, "");
        }
        let fenced = format!("```json\n{json_str}\n```");
        out = out.replace(&fenced, "");
    }
    out = hide_incomplete_findings_suffix(&out);
    collapse_blank_lines(out.trim())
}

/// While streaming, hide an unfinished ```json … findings block at the end.
fn hide_incomplete_findings_suffix(text: &str) -> String {
    let Some(start) = text.find("```json") else {
        return hide_incomplete_bare_findings_suffix(text);
    };
    let rest = &text[start..];
    if rest[7..].contains("```") {
        return hide_incomplete_bare_findings_suffix(text);
    }
    if rest.contains("\"findings\"") || rest.contains("\"issue\"") {
        return text[..start].trim_end().to_string();
    }
    hide_incomplete_bare_findings_suffix(text)
}

fn hide_incomplete_bare_findings_suffix(text: &str) -> String {
    let Some(start) = text.rfind('{') else {
        return text.to_string();
    };
    let tail = &text[start..];
    if !(tail.contains("\"findings\"") || tail.contains("\"issue\"")) {
        return text.to_string();
    }
    let opens = tail.matches('{').count();
    let closes = tail.matches('}').count();
    if opens > closes {
        text[..start].trim_end().to_string()
    } else {
        text.to_string()
    }
}

/// Incremental filter for Execute perceive streaming — findings JSON never reaches UI.
#[derive(Debug, Default)]
pub struct FindingsStreamFilter {
    buffer: String,
    visible_len: usize,
}

impl FindingsStreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a stream chunk; returns newly visible user text (if any).
    pub fn push(&mut self, chunk: &str) -> Option<String> {
        self.buffer.push_str(chunk);
        let visible = strip_findings_json_blocks(&self.buffer);
        if visible.len() > self.visible_len {
            let delta = visible[self.visible_len..].to_string();
            self.visible_len = visible.len();
            if delta.is_empty() {
                None
            } else {
                Some(delta)
            }
        } else if visible.len() < self.visible_len {
            self.visible_len = visible.len();
            None
        } else {
            None
        }
    }

    /// After stream ends, emit findings markdown appendix (parsed from stripped JSON).
    pub fn flush_tail(&mut self) -> Option<String> {
        let full_visible = format_for_user_display(&self.buffer);
        if full_visible.len() <= self.visible_len {
            return None;
        }
        let delta = full_visible[self.visible_len..].to_string();
        self.visible_len = full_visible.len();
        if delta.trim().is_empty() {
            None
        } else {
            Some(delta)
        }
    }
}

fn collapse_blank_lines(s: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                lines.push(line);
            }
        } else {
            blank_run = 0;
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_string()
}

/// User-facing Execute perceive output: strip machine JSON, append parsed findings as markdown.
pub fn format_for_user_display(text: &str) -> String {
    let findings = extract_from_text(text);
    if !text.contains("\"findings\"") && findings.is_none() {
        return text.to_string();
    }
    let stripped = strip_findings_json_blocks(text);
    let Some(f) = findings else {
        return stripped;
    };
    if f.findings.is_empty() {
        return stripped;
    }
    let summary_md = format_findings_markdown(&f);
    if stripped.trim().is_empty() {
        return summary_md;
    }
    if prose_covers_findings(&stripped, &f) {
        return stripped;
    }
    format!("{}\n\n{}", stripped.trim_end(), summary_md)
}

/// Whether prose already describes findings (skip duplicate appendix).
fn prose_covers_findings(prose: &str, f: &PerceptionFindings) -> bool {
    if prose.contains("## 问题汇总") {
        return true;
    }
    let n = f.findings.len();
    if n == 0 {
        return true;
    }
    let structured = (1..=n as u32)
        .filter(|i| {
            prose.contains(&format!("**{}.", i))
                || prose.contains(&format!("### {}", i))
                || prose.contains(&format!("| {} |", i))
        })
        .count();
    if structured >= n {
        return true;
    }
    f.findings.iter().all(|item| {
        !item.issue.is_empty() && prose.contains(item.issue.as_str())
    })
}

fn format_severity(sev: &str) -> String {
    match sev.trim().to_lowercase().as_str() {
        "" => "—".to_string(),
        "high" | "高" => "高".to_string(),
        "medium" | "中" => "中".to_string(),
        "low" | "低" => "低".to_string(),
        other => other.to_string(),
    }
}

fn format_location(item: &FindingItem) -> String {
    if item.file.is_empty() {
        item.target.clone()
    } else if item.target.is_empty() {
        format!("`{}`", item.file)
    } else {
        format!("`{}` · {}", item.file, item.target)
    }
}

/// Render frozen / extracted findings as a user-readable problem list.
pub fn format_findings_markdown(f: &PerceptionFindings) -> String {
    let mut lines = vec!["## 问题汇总".to_string()];
    if !f.findings_summary.is_empty() {
        lines.push(format!("\n> {}", f.findings_summary));
    }
    lines.push(String::new());
    for item in &f.findings {
        let sev = format_severity(&item.severity);
        let loc = format_location(item);
        lines.push(format!("**{}. [{}] {}**", item.index, sev, loc));
        lines.push(format!("- **问题：** {}", item.issue));
        if !item.recommendation.is_empty() {
            lines.push(format!("- **建议：** {}", item.recommendation));
        }
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

/// Convert frozen findings → executable plan tracker (Think → Act handoff).
pub fn to_plan_tracker(findings: &PerceptionFindings) -> PlanTracker {
    let steps: Vec<PlanStep> = findings
        .findings
        .iter()
        .map(|f| {
            let desc = if f.recommendation.is_empty() {
                f.issue.clone()
            } else {
                format!("{} → {}", f.issue, f.recommendation)
            };
            PlanStep {
                index: f.index,
                file: f.file.clone(),
                action: "edit".to_string(),
                target: f.target.clone(),
                desc,
                verify: String::new(),
                status: StepStatus::Pending,
            }
        })
        .collect();
    let mut tracker = PlanTracker {
        current_index: 1,
        steps,
    };
    if let Some(first) = tracker.steps.first_mut() {
        first.status = StepStatus::InProgress;
    }
    tracker
}

/// Freeze perception from execute output: prefer findings JSON, fallback review parse.
pub fn freeze_from_output(engine: &super::engine::WorkflowEngine, output: &str) {
    if let Some(findings) = extract_from_text(output) {
        save(engine, &findings);
        return;
    }
    if let Some(tracker) = plan_tracker::load_from_review_report(output) {
        if let Ok(json) = serde_json::to_string(&tracker) {
            engine.set_variable("_plan_tracker", json);
            tracing::info!(
                "[PERCEPTION] derived plan tracker ({} steps) from review prose",
                tracker.steps.len()
            );
        }
    }
}

pub fn findings_summary_block(engine: &super::engine::WorkflowEngine) -> String {
    load(engine)
        .map(|f| {
            let mut lines = vec![format!(
                "【感知结论 — findings】\n{}",
                if f.findings_summary.is_empty() {
                    "（见各项）".to_string()
                } else {
                    f.findings_summary.clone()
                }
            )];
            for item in &f.findings {
                lines.push(format!(
                    "  {}. [{}] {} — {} | 建议: {}",
                    item.index,
                    item.severity,
                    if item.file.is_empty() {
                        item.target.clone()
                    } else {
                        format!("`{}` {}", item.file, item.target)
                    },
                    item.issue,
                    item.recommendation
                ));
            }
            lines.join("\n")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_findings_json_keeps_prose() {
        let text = r#"## 审查报告
F1 - 问题A
```json
{"findings_summary":"摘要","findings":[{"index":1,"issue":"x","recommendation":"y"}]}
```
## Done"#;
        let shown = format_for_user_display(text);
        assert!(shown.contains("审查报告"));
        assert!(shown.contains("F1"));
        assert!(!shown.contains("```json"));
        // issue "x" not in prose — appendix from JSON
        assert!(shown.contains("问题汇总"));
        assert!(shown.contains("**问题：** x"));
    }

    #[test]
    fn prose_only_with_f1_skips_duplicate_summary() {
        let text = r#"## 审查报告
### F1 — Foo (high) (`a.rs`)
缺 waitTime
**建议:** 加 leaseTime
```json
{"findings_summary":"摘要","findings":[{"index":1,"severity":"high","file":"a.rs","target":"Foo","issue":"缺 waitTime","recommendation":"加 leaseTime"}]}
```
## Done"#;
        let shown = format_for_user_display(text);
        assert!(shown.contains("审查报告"));
        assert!(!shown.contains("## 问题汇总"));
    }

    #[test]
    fn json_only_output_becomes_markdown_summary() {
        let text = r#"## Done
```json
{
  "findings_summary": "两处配置问题",
  "findings": [
    {"index":1,"severity":"high","file":"a.rs","target":"foo","issue":"缺校验","recommendation":"加校验"},
    {"index":2,"severity":"medium","file":"b.rs","target":"bar","issue":"硬编码","recommendation":"抽配置"}
  ]
}
```"#;
        let shown = format_for_user_display(text);
        assert!(!shown.contains("\"findings\""));
        assert!(shown.contains("## 问题汇总"));
        assert!(shown.contains("两处配置问题"));
        assert!(shown.contains("**1. [高]"));
        assert!(shown.contains("缺校验"));
        assert!(shown.contains("**2. [中]"));
        assert!(!shown.contains("| # |"));
    }

    #[test]
    fn stream_filter_appends_summary_on_flush() {
        let mut f = FindingsStreamFilter::new();
        assert!(f.push("## 完成\n").unwrap().contains("完成"));
        assert!(f
            .push(r#"```json
{"findings_summary":"s","findings":[{"index":1,"issue":"i","recommendation":"r"}]}
```"#)
            .is_none());
        let tail = f.flush_tail().unwrap();
        assert!(tail.contains("问题汇总"));
        assert!(tail.contains("**问题：** i"));
    }

    #[test]
    fn strip_hides_incomplete_fenced_findings() {
        let partial = "## 审查报告\n行1\n```json\n{\"findings\":[\n";
        let shown = strip_findings_json_blocks(partial);
        assert!(shown.contains("审查报告"));
        assert!(!shown.contains("\"findings\""));
    }

    #[test]
    fn stream_filter_suppresses_findings_json() {
        let mut f = FindingsStreamFilter::new();
        assert!(f.push("## 报告\n").unwrap().contains("## 报告"));
        assert!(f.push("```json\n{\"findings\":[").is_none());
        let tail = f.push(r#"{"index":2}]}\n```"#);
        assert!(tail.is_none() || !tail.unwrap().contains("\"index\""));
    }

    #[test]
    fn extract_findings_json() {
        let text = r#"
## 审查报告
...
```json
{
  "findings_summary": "Controller 与 DTO 各有一处问题",
  "findings": [
    {"index":1,"severity":"high","file":"Foo.java","target":"Foo","issue":"缺 waitTime","recommendation":"加 leaseTime"}
  ]
}
```
## 完成
"#;
        let f = extract_from_text(text).unwrap();
        assert_eq!(f.findings.len(), 1);
        assert_eq!(f.findings[0].file, "Foo.java");
    }

    #[test]
    fn findings_to_tracker() {
        let f = PerceptionFindings {
            findings_summary: "x".into(),
            findings: vec![FindingItem {
                index: 1,
                severity: "high".into(),
                file: "a.java".into(),
                target: "A".into(),
                issue: "bug".into(),
                recommendation: "fix".into(),
            }],
        };
        let t = to_plan_tracker(&f);
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].action, "edit");
    }
}
