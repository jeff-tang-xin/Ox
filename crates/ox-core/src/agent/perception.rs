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
    /// Concrete fix plan (lines + how + code sketch), carried into implementation.
    #[serde(default)]
    pub fix_plan: String,
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
        tracing::info!("[PERCEPTION] frozen {} finding(s)", findings.findings.len());
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
            fix_plan: obj
                .get("fix_plan")
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

fn matching_close_brace(s: &str) -> Option<usize> {
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

/// Locate a findings JSON block (```json fence or bare object anchored on markers).
fn find_findings_json_range(text: &str) -> Option<(usize, usize)> {
    let mut search_from = 0;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find("```json") else {
            break;
        };
        let fence_start = search_from + rel;
        let after = fence_start + 7;
        let Some(end_off) = text[after..].find("```") else {
            break;
        };
        let fence_end = after + end_off + 3;
        let inner = text[after..after + end_off].trim();
        if inner.contains("\"findings\"") {
            return Some((fence_start, fence_end));
        }
        search_from = fence_end;
    }

    let mut best: Option<(usize, usize)> = None;
    for marker in ["\"findings_summary\"", "\"findings\""] {
        let mut from = 0;
        while let Some(pos) = text[from..].find(marker) {
            let abs = from + pos;
            if let Some(open) = text[..abs].rfind('{')
                && let Some(close_rel) = matching_close_brace(&text[open..])
            {
                let end = open + close_rel + 1;
                let slice = &text[open..end];
                if slice.contains("\"findings\"")
                    && serde_json::from_str::<serde_json::Value>(slice).is_ok()
                {
                    best = Some((open, end));
                }
            }
            from = abs + marker.len();
        }
    }
    best
}

fn find_incomplete_findings_suffix_start(text: &str) -> Option<usize> {
    for marker in ["\"findings_summary\"", "\"findings\""] {
        let Some(pos) = text.rfind(marker) else {
            continue;
        };
        let Some(open) = text[..pos].rfind('{') else {
            continue;
        };
        let tail = &text[open..];
        if matching_close_brace(tail).is_none() {
            return Some(open);
        }
    }
    None
}

/// Find the first valid JSON block containing `"findings"` field in text.
pub fn extract_json_block(text: &str) -> Option<String> {
    let (start, end) = find_findings_json_range(text)?;
    let slice = &text[start..end];
    if slice.starts_with("```json") {
        let after = start + 7;
        let end_off = text[after..].find("```")?;
        Some(text[after..after + end_off].trim().to_string())
    } else {
        Some(slice.to_string())
    }
}

/// Remove findings ```json``` / bare JSON from user-visible text (machine still gets full output).
pub fn strip_findings_json_blocks(text: &str) -> String {
    let mut out = text.to_string();
    while let Some((start, end)) = find_findings_json_range(&out) {
        out.replace_range(start..end, "\n");
    }
    out = hide_incomplete_findings_suffix(&out);
    collapse_blank_lines(out.trim())
}

/// While streaming, hide an unfinished ```json … findings block at the end.
fn hide_incomplete_findings_suffix(text: &str) -> String {
    if let Some(start) = text.find("```json") {
        let rest = &text[start..];
        if rest[7..].contains("```") {
            return hide_incomplete_bare_findings_suffix(text);
        }
        if rest.contains("\"findings\"") || rest.contains("\"issue\"") {
            return text[..start].trim_end().to_string();
        }
    }
    hide_incomplete_bare_findings_suffix(text)
}

fn hide_incomplete_bare_findings_suffix(text: &str) -> String {
    if let Some(start) = find_incomplete_findings_suffix_start(text) {
        return text[..start].trim_end().to_string();
    }
    text.to_string()
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
            if delta.is_empty() { None } else { Some(delta) }
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

/// User-facing review output: strip machine JSON (findings list → TUI panel).
pub fn format_for_user_display(text: &str) -> String {
    strip_findings_json_blocks(text)
}

/// Whether prose already describes findings (skip duplicate appendix).
#[allow(dead_code)]
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
    f.findings
        .iter()
        .all(|item| !item.issue.is_empty() && prose.contains(item.issue.as_str()))
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
                awaiting_verify: false,
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
    crate::agent::findings::ensure_from_review_output(engine, output);
    if let Some(tracker) = plan_tracker::load_from_review_report(output)
        && let Ok(json) = serde_json::to_string(&tracker)
    {
        engine.set_variable("_plan_tracker", json);
        tracing::info!(
            "[PERCEPTION] derived plan tracker ({} steps) from review prose",
            tracker.steps.len()
        );
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
        assert!(!shown.contains("\"findings\""));
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
    fn json_only_output_strips_machine_json() {
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
        assert!(shown.contains("## Done"));
    }

    #[test]
    fn stream_filter_flush_no_duplicate_appendix() {
        let mut f = FindingsStreamFilter::new();
        assert!(f.push("## 完成\n").unwrap().contains("完成"));
        assert!(
            f.push(
                r#"```json
{"findings_summary":"s","findings":[{"index":1,"issue":"i","recommendation":"r"}]}
```"#
            )
            .is_none()
        );
        assert!(f.flush_tail().is_none());
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
    fn strip_bare_findings_json_after_prose() {
        let text = r#"## 审查报告
**发现 6 (LOW)**: expectedDeliveryDate 已废弃
**需要商量的关键决策点**:
1. 同一 SKU 多交期是否改为 Map<String, List>

{
  "findings_summary": "MaintainDeliveryStrategy 未实现拆单",
  "findings": [
    {"index":1,"severity":"high","file":"Foo.java","target":"doHandle","issue":"未实现拆单","recommendation":"补全逻辑"}
  ]
}"#;
        let shown = format_for_user_display(text);
        assert!(!shown.contains("\"findings_summary\""));
        assert!(!shown.contains("\"findings\""));
        assert!(shown.contains("审查报告"));
        assert!(shown.contains("关键决策点"));
    }

    #[test]
    fn strip_preserves_unrelated_braces_in_prose() {
        let text = r#"use foo::{bar, baz};

{
  "findings_summary": "x",
  "findings": [{"index":1,"issue":"i","recommendation":"r"}]
}"#;
        let shown = strip_findings_json_blocks(text);
        assert!(shown.contains("use foo::{bar, baz}"));
        assert!(!shown.contains("findings_summary"));
    }

    #[test]
    fn stream_filter_suppresses_bare_findings_json() {
        let mut f = FindingsStreamFilter::new();
        assert!(f.push("## 审查\n").unwrap().contains("审查"));
        assert!(
            f.push("\n{\n  \"findings_summary\": \"s\",\n  \"findings\": [\n")
                .is_none()
        );
        assert!(
            f.push(
                r#"{"index":1,"issue":"i","recommendation":"r"}]}
"#
            )
            .is_none()
        );
        let tail = f.flush_tail();
        assert!(
            tail.is_none() || !tail.unwrap().contains("\"findings\""),
            "completed bare JSON must not reach UI"
        );
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
                fix_plan: String::new(),
            }],
        };
        let t = to_plan_tracker(&f);
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].action, "edit");
    }
}
