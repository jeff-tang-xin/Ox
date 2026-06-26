//! Structured plan progress tracking for Execute step.

use serde::{Deserialize, Serialize};

use crate::agent::exploration_snapshot::ExplorationEntry;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Skipped,
}

impl Default for StepStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    pub index: u32,
    pub file: String,
    pub action: String,
    pub target: String,
    pub desc: String,
    #[serde(default)]
    pub verify: String,
    #[serde(default)]
    pub status: StepStatus,
    /// File edited but step.verify not yet passed (enforce-quality mode).
    #[serde(default)]
    pub awaiting_verify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PlanTracker {
    pub steps: Vec<PlanStep>,
    /// 1-based index of the active step.
    #[serde(default = "default_current")]
    pub current_index: u32,
}

/// Outcome when a write tool succeeds on a plan file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteCompletionOutcome {
    MarkedDone,
    AstIncomplete,
    AwaitingVerify(String),
    NoMatchingStep,
}

fn verify_command_matches(expected: &str, actual: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let e = norm(expected);
    let a = norm(actual);
    if e.is_empty() {
        return false;
    }
    a.contains(&e) || e.contains(&a)
}

fn default_current() -> u32 {
    1
}

pub fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_matches(|c| c == '/' || c == '\\')
        .replace('\\', "/")
        .to_lowercase()
}

/// Parse plan JSON (full LLM output or extracted block) into a tracker.
pub fn load_from_output(text: &str) -> Option<PlanTracker> {
    let json_str = extract_json_block(text)?;
    load_from_json(&json_str)
}

fn extract_json_block(text: &str) -> Option<String> {
    if let (Some(start), Some(end)) = (text.find("```json"), text.rfind("```")) {
        let inner = &text[start + 7..end].trim();
        if inner.starts_with('{') {
            return Some(inner.to_string());
        }
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            return Some(text[start..=end].to_string());
        }
    }
    None
}

pub fn load_from_json(json_str: &str) -> Option<PlanTracker> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let plan = v.get("plan")?.as_array()?;
    if plan.is_empty() {
        return None;
    }

    let mut steps = Vec::new();
    for (i, step) in plan.iter().enumerate() {
        let obj = step.as_object()?;
        let index = obj
            .get("step")
            .and_then(|s| s.as_u64())
            .unwrap_or((i + 1) as u64) as u32;
        let file = obj
            .get("file")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let action = obj
            .get("action")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let target = obj
            .get("target")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let desc = obj
            .get("desc")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let verify = obj
            .get("verify")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        steps.push(PlanStep {
            index,
            file,
            action,
            target,
            desc,
            verify,
            status: StepStatus::Pending,
            awaiting_verify: false,
        });
    }

    if let Some(first) = steps.first_mut() {
        first.status = StepStatus::InProgress;
    }

    Some(PlanTracker {
        steps,
        current_index: 1,
    })
}

impl PlanTracker {
    pub fn progress_summary(&self) -> String {
        if self.steps.is_empty() {
            return String::new();
        }
        let done = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count();
        let total = self.steps.len();
        let mut lines = vec![format!("【计划进度】{done}/{total} 完成")];

        for step in &self.steps {
            let icon = if step.awaiting_verify {
                "🔄"
            } else {
                match step.status {
                    StepStatus::Done => "✅",
                    StepStatus::InProgress => "▶",
                    StepStatus::Skipped => "⏭",
                    StepStatus::Pending => "⏳",
                }
            };
            let file_part = if step.file.is_empty() {
                String::new()
            } else {
                format!(" `{}`", step.file)
            };
            let target_part = if step.target.is_empty() {
                String::new()
            } else {
                format!(" `{}`", step.target)
            };
            let action = if step.action.is_empty() {
                "step".to_string()
            } else {
                step.action.clone()
            };
            lines.push(format!(
                "  {icon} {}.{action}{target_part}{file_part} — {}{}",
                step.index,
                if step.desc.is_empty() {
                    "(无描述)".to_string()
                } else {
                    step.desc.clone()
                },
                if step.awaiting_verify {
                    " (已改文件，待验证)"
                } else {
                    ""
                }
            ));
        }

        lines.push("完成当前步骤后再进入下一项；已完成项勿重复。".to_string());
        lines.join("\n")
    }

    /// Enforce-quality completion: AST clean; optional step.verify via shell before Done.
    pub fn try_complete_after_write(
        &mut self,
        path: &str,
        ast_clean: bool,
        enforce_quality: bool,
        implicit_verify: Option<&str>,
    ) -> WriteCompletionOutcome {
        let norm = normalize_path(path);
        let pos = self.steps.iter().position(|s| {
            !s.file.is_empty() && normalize_path(&s.file) == norm && s.status != StepStatus::Done
        });
        let Some(pos) = pos else {
            return WriteCompletionOutcome::NoMatchingStep;
        };

        if enforce_quality && !ast_clean {
            if self.steps[pos].status == StepStatus::Pending {
                self.steps[pos].status = StepStatus::InProgress;
            }
            self.steps[pos].awaiting_verify = false;
            return WriteCompletionOutcome::AstIncomplete;
        }

        let verify = self.steps[pos].verify.trim().to_string();
        let needs_verify = if !verify.is_empty() {
            Some(verify)
        } else if enforce_quality {
            implicit_verify
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        } else {
            None
        };

        if enforce_quality && needs_verify.is_some() {
            self.steps[pos].status = StepStatus::InProgress;
            self.steps[pos].awaiting_verify = true;
            if self.steps[pos].verify.is_empty() {
                if let Some(ref cmd) = needs_verify {
                    self.steps[pos].verify = cmd.clone();
                }
            }
            return WriteCompletionOutcome::AwaitingVerify(needs_verify.unwrap_or_default());
        }

        self.steps[pos].status = StepStatus::Done;
        self.steps[pos].awaiting_verify = false;
        self.advance_current();
        WriteCompletionOutcome::MarkedDone
    }

    /// Mark awaiting-verify step done when shell command matches step.verify and succeeded.
    pub fn try_confirm_verify(&mut self, command: &str) -> bool {
        let pos = self.steps.iter().position(|s| {
            s.awaiting_verify
                && !s.verify.trim().is_empty()
                && verify_command_matches(&s.verify, command)
        });
        let Some(pos) = pos else {
            return false;
        };
        self.steps[pos].status = StepStatus::Done;
        self.steps[pos].awaiting_verify = false;
        self.advance_current();
        true
    }

    pub fn current_step(&self) -> Option<&PlanStep> {
        self.steps
            .iter()
            .find(|s| s.index == self.current_index)
            .or_else(|| {
                self.steps
                    .iter()
                    .find(|s| s.status == StepStatus::InProgress)
            })
            .or_else(|| self.steps.iter().find(|s| s.status == StepStatus::Pending))
    }

    pub fn verify_hint_for_path(&self, path: &str) -> Option<String> {
        let norm = normalize_path(path);
        let step = self
            .steps
            .iter()
            .find(|s| !s.file.is_empty() && normalize_path(&s.file) == norm)?;
        if step.verify.trim().is_empty() {
            None
        } else {
            Some(step.verify.clone())
        }
    }

    /// Mark a step done when a write tool succeeds on its file path.
    pub fn try_mark_done_for_path(&mut self, path: &str) -> bool {
        let norm = normalize_path(path);
        let idx = self.steps.iter().position(|s| {
            !s.file.is_empty() && normalize_path(&s.file) == norm && s.status != StepStatus::Done
        });
        let Some(pos) = idx else {
            return false;
        };
        self.steps[pos].status = StepStatus::Done;
        self.advance_current();
        true
    }

    /// Mark the current in-progress or first pending step done (shell/git tasks).
    pub fn mark_current_step_done(&mut self) -> bool {
        let pos = self
            .steps
            .iter()
            .position(|s| matches!(s.status, StepStatus::InProgress | StepStatus::Pending));
        let Some(pos) = pos else {
            return false;
        };
        self.steps[pos].status = StepStatus::Done;
        self.advance_current();
        true
    }

    fn advance_current(&mut self) {
        let next_index = self
            .steps
            .iter()
            .find(|s| s.status == StepStatus::Pending)
            .map(|s| s.index);
        if let Some(idx) = next_index {
            self.current_index = idx;
            if let Some(s) = self.steps.iter_mut().find(|s| s.index == idx) {
                s.status = StepStatus::InProgress;
            }
        }
    }

    /// Match a file path to a plan step (exact, basename, or target keyword).
    pub fn step_for_path(&self, path: &str) -> Option<&PlanStep> {
        let norm = normalize_path(path);
        let basename = norm.rsplit('/').next().unwrap_or(&norm);
        self.steps
            .iter()
            .find(|s| !s.file.is_empty() && normalize_path(&s.file) == norm)
            .or_else(|| {
                self.steps.iter().find(|s| {
                    !s.file.is_empty()
                        && normalize_path(&s.file)
                            .rsplit('/')
                            .next()
                            .is_some_and(|b| b == basename)
                })
            })
            .or_else(|| {
                self.steps.iter().find(|s| {
                    !s.target.is_empty()
                        && (norm.contains(&normalize_path(&s.target))
                            || basename.contains(&normalize_path(&s.target)))
                })
            })
            .or_else(|| self.current_step())
    }

    pub fn all_done(&self) -> bool {
        self.steps
            .iter()
            .all(|s| matches!(s.status, StepStatus::Done | StepStatus::Skipped))
    }

    pub fn pending_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Pending | StepStatus::InProgress))
            .count()
    }

    pub fn check_done_gate(&self) -> Option<String> {
        if self.steps.is_empty() || self.all_done() {
            return None;
        }
        let pending: Vec<String> = self
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Pending | StepStatus::InProgress))
            .map(|s| format!("  - 步骤 {}: {} ({})", s.index, s.desc, s.file))
            .collect();
        Some(format!(
            "❌ 计划尚未全部完成，不能输出 ## Done。未完成:\n{}\n请继续执行或说明跳过原因。",
            pending.join("\n")
        ))
    }
}

pub fn tracker_from_json(s: &str) -> Option<PlanTracker> {
    serde_json::from_str(s).ok()
}

pub fn tracker_to_json(tracker: &PlanTracker) -> String {
    serde_json::to_string(tracker).unwrap_or_else(|_| "{}".to_string())
}

/// Parse a parked code-review report into an implementation checklist.
pub fn load_from_review_report(report: &str) -> Option<PlanTracker> {
    let mut steps = extract_review_items(report);
    if steps.is_empty() {
        return None;
    }
    steps.sort_by_key(|s| s.index);
    steps.dedup_by_key(|s| s.index);
    if let Some(first) = steps.iter_mut().find(|s| s.status == StepStatus::Pending) {
        first.status = StepStatus::InProgress;
    }
    let current_index = steps
        .iter()
        .find(|s| s.status == StepStatus::InProgress)
        .map(|s| s.index)
        .unwrap_or(1);
    Some(PlanTracker {
        steps,
        current_index,
    })
}

fn extract_review_items(report: &str) -> Vec<PlanStep> {
    let mut steps = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in report.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('|') {
            let cols: Vec<&str> = line
                .split('|')
                .map(str::trim)
                .filter(|c| !c.is_empty() && *c != "---")
                .collect();
            if cols.len() >= 3 {
                if let Ok(n) = cols[0].parse::<u32>() {
                    if seen.insert(n) {
                        let file = cols
                            .iter()
                            .find_map(|c| {
                                let p = extract_path_from_text(c);
                                if p.is_empty() { None } else { Some(p) }
                            })
                            .unwrap_or_else(|| extract_path_from_text(line));
                        let target = cols
                            .iter()
                            .find_map(|c| {
                                let t = extract_symbol_target(c);
                                if t.is_empty() { None } else { Some(t) }
                            })
                            .unwrap_or_default();
                        let desc = cols.last().copied().unwrap_or("").to_string();
                        if desc.chars().count() >= 8 || !file.is_empty() {
                            steps.push(make_impl_step(n, file, target, desc));
                        }
                    }
                }
            }
            continue;
        }

        if let Some((n, rest)) = parse_numbered_review_line(line) {
            if seen.insert(n) {
                let file = extract_path_from_text(rest);
                let target = extract_symbol_target(rest);
                steps.push(make_impl_step(n, file, target, rest.to_string()));
            }
            continue;
        }

        if let Some((n, rest)) = parse_bug_review_line(line) {
            if seen.insert(n) {
                let file = extract_path_from_text(rest);
                let target = extract_symbol_target(rest);
                steps.push(make_impl_step(n, file, target, rest.to_string()));
            }
        }
    }

    steps
}

/// `BUG-1（严重）`, `F1 —`, `**BUG-2**` style headings.
fn parse_bug_review_line(line: &str) -> Option<(u32, &str)> {
    let mut t = line.trim();
    while t.starts_with(|c: char| c == '-' || c == '*' || c == ' ') {
        t = t.trim_start_matches(|c: char| c == '-' || c == '*' || c == ' ');
    }
    let upper = t.to_ascii_uppercase();
    let rest = if upper.strip_prefix("BUG-").is_some() {
        t.get(4..)?
    } else if upper.starts_with('F') && t.len() > 1 {
        let digit_end = t[1..].chars().take_while(|c| c.is_ascii_digit()).count();
        if digit_end == 0 {
            return None;
        }
        t.get(1 + digit_end..)?
    } else {
        return None;
    };
    let num_part: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: u32 = num_part.parse().ok()?;
    let after = rest[num_part.len()..]
        .trim_start_matches(['（', '(', ')', '）', '：', ':', '、', '.', ' ', '-', '—'])
        .trim();
    if after.is_empty() {
        return None;
    }
    Some((n, after))
}

fn make_impl_step(index: u32, file: String, target: String, desc: String) -> PlanStep {
    PlanStep {
        index,
        file,
        action: "edit".to_string(),
        target,
        desc: desc.trim().to_string(),
        verify: String::new(),
        status: StepStatus::Pending,
        awaiting_verify: false,
    }
}

fn parse_numbered_review_line(line: &str) -> Option<(u32, &str)> {
    let trimmed = line.trim_start_matches('#').trim();
    if let Some(rest) = trimmed.strip_prefix("问题") {
        let num_part: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num_part.parse::<u32>() {
            let after = rest[num_part.len()..]
                .trim_start_matches(['：', ':', '、', '.', ' '])
                .trim();
            if !after.is_empty() {
                return Some((n, after));
            }
        }
    }
    let bytes = trimmed.as_bytes();
    if bytes.first()?.is_ascii_digit() {
        let mut end = 0usize;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let n: u32 = trimmed[..end].parse().ok()?;
        let rest = trimmed[end..].trim_start_matches(['.', '、', ')', '）', ':', '：', ' ']);
        if rest.is_empty() {
            return None;
        }
        Some((n, rest))
    } else {
        None
    }
}

fn extract_path_from_text(s: &str) -> String {
    let mut in_tick = false;
    let mut buf = String::new();
    for ch in s.chars() {
        if ch == '`' {
            if in_tick {
                if looks_like_source_path(&buf) {
                    return buf;
                }
                buf.clear();
            }
            in_tick = !in_tick;
        } else if in_tick {
            buf.push(ch);
        }
    }
    for token in s.split(|c: char| c.is_whitespace() || c == '，' || c == ',') {
        let t = token.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '（' | '）' | '[' | ']' | '*' | '：' | ':')
        });
        if looks_like_source_path(t) {
            return t.to_string();
        }
    }
    String::new()
}

fn looks_like_source_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let lower = s.to_lowercase();
    [
        ".java", ".kt", ".kts", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".rs", ".cs", ".vue",
        ".rb", ".php", ".scala", ".xml",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn extract_symbol_target(s: &str) -> String {
    if let Some(start) = s.find("**") {
        let rest = &s[start + 2..];
        if let Some(end) = rest.find("**") {
            return rest[..end].trim().to_string();
        }
    }
    for key in [
        "Controller",
        "ServiceImpl",
        "Service",
        "RequestDto",
        "RequestDTO",
        "Dto",
        "DTO",
        "Dao",
        "Util",
        "Entity",
    ] {
        if let Some(pos) = s.find(key) {
            let start = s[..pos]
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map(|(i, _)| i + 1)
                .unwrap_or(0);
            let end = s[pos..]
                .char_indices()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map(|(i, _)| pos + i)
                .unwrap_or(s.len());
            let word = s[start..end].trim();
            if !word.is_empty() {
                return word.to_string();
            }
        }
    }
    String::new()
}

/// Validate plan steps have minimum required fields and exploration depth.
pub fn validate_plan_steps(json_str: &str) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("❌ plan JSON 解析失败: {e}"))?;

    let summary = v
        .get("structure_summary")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    if summary.chars().count() < 40 {
        return Err(
            "❌ 缺少 structure_summary（≥40 字）：先总结你探索到的项目结构（类型、目录、入口文件），再输出 plan。"
                .to_string(),
        );
    }

    let plan = v
        .get("plan")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "❌ 缺少 plan 数组".to_string())?;
    if plan.is_empty() {
        return Err("❌ `plan` 数组为空".to_string());
    }
    for (i, step) in plan.iter().enumerate() {
        let obj = step
            .as_object()
            .ok_or_else(|| format!("❌ plan[{}] 不是对象", i))?;
        let file = obj.get("file").and_then(|s| s.as_str()).unwrap_or("");
        let desc = obj.get("desc").and_then(|s| s.as_str()).unwrap_or("");
        if file.trim().is_empty() {
            return Err(format!("❌ plan[{}] 缺少 file 字段", i + 1));
        }
        if desc.trim().is_empty() {
            return Err(format!("❌ plan[{}] 缺少 desc 字段", i + 1));
        }
        if desc.chars().count() < 15 {
            return Err(format!(
                "❌ plan[{}] desc 太笼统（至少 15 字）：需写明改哪里、怎么改",
                i + 1
            ));
        }
        let action = obj.get("action").and_then(|s| s.as_str()).unwrap_or("");
        let target = obj.get("target").and_then(|s| s.as_str()).unwrap_or("");
        if matches!(action, "modify" | "delete") && target.trim().is_empty() {
            return Err(format!(
                "❌ plan[{}] action={action} 时必须填写 target",
                i + 1
            ));
        }
        let verify = obj.get("verify").and_then(|s| s.as_str()).unwrap_or("");
        if matches!(action, "modify" | "delete" | "create" | "add") && verify.trim().is_empty() {
            return Err(format!(
                "❌ plan[{}] action={action} 时必须填写 verify（如何验证）",
                i + 1
            ));
        }
    }
    Ok(())
}

fn dir_listed(path: &str, explored: &std::collections::HashSet<String>) -> bool {
    let norm = normalize_path(path);
    let key = if norm.is_empty() || norm == "." {
        "file_list:.".to_string()
    } else {
        format!("file_list:{norm}")
    };
    explored.contains(&key) || explored.contains(&format!("file_list:./{norm}"))
}

fn is_root_list_path(path: &str) -> bool {
    let norm = normalize_path(path);
    norm.is_empty() || norm == "."
}

fn has_root_directory_list(
    entries: &[ExplorationEntry],
    explored: &std::collections::HashSet<String>,
) -> bool {
    dir_listed(".", explored)
        || entries
            .iter()
            .any(|e| e.tool == "file_list" && is_root_list_path(&e.target))
}

fn has_non_root_directory_list(
    entries: &[ExplorationEntry],
    explored: &std::collections::HashSet<String>,
) -> bool {
    entries
        .iter()
        .any(|e| e.tool == "file_list" && !is_root_list_path(&e.target))
        || explored.iter().any(|p| {
            p.starts_with("file_list:")
                && !is_root_list_path(p.strip_prefix("file_list:").unwrap_or(""))
        })
}

fn count_tool(entries: &[ExplorationEntry], tool: &str) -> usize {
    entries.iter().filter(|e| e.tool == tool).count()
}

fn count_code_probes(entries: &[ExplorationEntry]) -> usize {
    entries
        .iter()
        .filter(|e| {
            matches!(
                e.tool.as_str(),
                "find_symbol" | "code_search" | "file_search"
            )
        })
        .count()
}

/// Actionable hint for what to call next during Plan exploration (reduces tool loops).
pub fn exploration_next_action(
    entries: &[ExplorationEntry],
    explored: &std::collections::HashSet<String>,
) -> String {
    if !entries.iter().any(|e| e.tool == "project_detect") {
        return "下一步: 调用 project_detect（仅一次）".to_string();
    }

    let file_lists = count_tool(entries, "file_list");
    let file_reads = count_tool(entries, "file_read");
    let code_probes = count_code_probes(entries);

    if file_lists == 0 {
        return "下一步: file_list(\".\") 查看根目录".to_string();
    }

    if !has_non_root_directory_list(entries, explored) && file_lists < 2 {
        return "下一步: file_list 一个子目录（如源码目录），不要重复已列过的目录".to_string();
    }

    if file_reads < 1 {
        return "下一步: file_read 一个入口/配置文件（不要重复 file_list 已列目录）".to_string();
    }

    if file_reads < 2 && code_probes == 0 {
        return "下一步: 再 file_read 一个相关文件，或 find_symbol/code_search 确认符号"
            .to_string();
    }

    if code_probes == 0 && !(file_lists >= 2 && file_reads >= 1) {
        return "下一步: find_symbol 或 code_search 确认关键符号/模块存在".to_string();
    }

    if validate_plan_exploration(entries, explored).is_ok() {
        "探索已满足 — 输出 plan JSON（含 structure_summary），不要再调工具".to_string()
    } else {
        "下一步: 补全探索（换未读的文件/子目录），或 code_search 确认".to_string()
    }
}

/// Minimum exploration before a plan JSON is accepted (language-agnostic).
pub fn validate_plan_exploration(
    entries: &[ExplorationEntry],
    explored: &std::collections::HashSet<String>,
) -> Result<(), String> {
    if !entries.iter().any(|e| e.tool == "project_detect") {
        return Err(
            "❌ 探索不足：必须先 project_detect 了解项目类型，再探索目录与关键文件。".to_string(),
        );
    }

    let file_lists = count_tool(entries, "file_list");
    let file_reads = count_tool(entries, "file_read");
    let code_probes = count_code_probes(entries);

    if file_reads < 1 {
        return Err(
            "❌ 探索不足：至少 file_read 1 个关键文件（入口、配置或计划将修改的文件）。"
                .to_string(),
        );
    }

    let hierarchical_ok = file_lists >= 2
        && has_root_directory_list(entries, explored)
        && has_non_root_directory_list(entries, explored);
    let flat_ok = file_lists >= 1 && file_reads >= 2;
    let search_ok = file_lists >= 1 && file_reads >= 1 && code_probes >= 1;

    if !(hierarchical_ok || flat_ok || search_ok) {
        return Err(
            "❌ 探索不足：需满足以下任一条件后再输出 plan：\n\
             • 分层项目：file_list 根目录 + file_list 至少一个子目录，且 file_read ≥1\n\
             • 扁平/小项目：file_list ≥1 且 file_read ≥2\n\
             • 搜索确认：file_list ≥1、file_read ≥1，且 find_symbol / code_search / file_search ≥1\n\
             不要假设 src/、crates/ 等路径 — 以 file_list 实际结果为准。"
                .to_string(),
        );
    }

    Ok(())
}

/// Plan file paths must be grounded in exploration (listed parent dir or file_read).
pub fn validate_plan_paths_known(
    json_str: &str,
    entries: &[ExplorationEntry],
    explored: &std::collections::HashSet<String>,
) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("❌ plan JSON 解析失败: {e}"))?;
    let plan = v
        .get("plan")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "❌ 缺少 plan 数组".to_string())?;

    for (i, step) in plan.iter().enumerate() {
        let file = step
            .get("file")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim();
        if file.is_empty() {
            continue;
        }
        if !path_known_in_exploration(file, entries, explored) {
            return Err(format!(
                "❌ plan[{}] 路径 `{file}` 未在探索中确认。请先 file_list 其父目录或 file_read 该文件。",
                i + 1
            ));
        }
    }
    Ok(())
}

fn path_known_in_exploration(
    file: &str,
    entries: &[ExplorationEntry],
    explored: &std::collections::HashSet<String>,
) -> bool {
    let norm = normalize_path(file);
    if entries
        .iter()
        .any(|e| e.tool == "file_read" && normalize_path(&e.target) == norm)
    {
        return true;
    }
    if explored.iter().any(|p| p == &format!("file_read:{norm}")) {
        return true;
    }

    // Nested paths: each ancestor directory must have been listed (root-only is not enough).
    if norm.contains('/') {
        let mut rest = norm.as_str();
        while !rest.is_empty() {
            if let Some((parent, _)) = rest.rsplit_once('/') {
                if parent.is_empty() {
                    break;
                }
                if dir_listed(parent, explored) {
                    return true;
                }
                rest = parent;
            } else {
                break;
            }
        }
        return false;
    }

    // Top-level file: root directory must have been listed.
    dir_listed(".", explored)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"structure_summary":"Rust workspace with crates ox-cli and ox-core; entry at crates/ox-cli/src/main.rs","plan":[{"step":1,"file":"crates/ox-cli/src/a.rs","action":"modify","target":"foo","desc":"change foo signature in handle_key","verify":"cargo check"}]}"#;

    #[test]
    fn load_and_mark_done() {
        let mut t = load_from_json(SAMPLE).unwrap();
        assert_eq!(t.steps.len(), 1);
        assert!(t.try_mark_done_for_path("crates/ox-cli/src/a.rs"));
        assert!(t.all_done());
    }

    #[test]
    fn progress_summary_shows_icons() {
        let t = load_from_json(SAMPLE).unwrap();
        let s = t.progress_summary();
        assert!(s.contains("▶"));
        assert!(s.contains("crates/ox-cli/src/a.rs"));
    }

    #[test]
    fn done_gate_blocks_incomplete() {
        let t = load_from_json(SAMPLE).unwrap();
        assert!(t.check_done_gate().is_some());
    }

    #[test]
    fn validate_requires_file_and_desc() {
        assert!(validate_plan_steps(r#"{"plan":[{"step":1,"file":"","desc":""}]}"#).is_err());
        assert!(validate_plan_steps(SAMPLE).is_ok());
        assert!(validate_plan_steps(
            r#"{"structure_summary":"short","plan":[{"step":1,"file":"a.rs","action":"explain","target":"x","desc":"read file for context"}]}"#
        )
        .is_err());
    }

    #[test]
    fn exploration_gate_requires_detect_and_reads() {
        use crate::agent::exploration_snapshot::ExplorationEntry;
        let empty: Vec<ExplorationEntry> = vec![];
        let explored = std::collections::HashSet::new();
        assert!(validate_plan_exploration(&empty, &explored).is_err());

        let only_detect = vec![ExplorationEntry {
            tool: "project_detect".into(),
            target: ".".into(),
            content: "Language: Rust (build: Cargo)".into(),
            ref_path: None,
            full_chars: 10,
        }];
        assert!(validate_plan_exploration(&only_detect, &explored).is_err());
    }

    #[test]
    fn exploration_gate_hierarchical_layout() {
        use crate::agent::exploration_snapshot::ExplorationEntry;
        let entries = vec![
            ExplorationEntry {
                tool: "project_detect".into(),
                target: ".".into(),
                content: "Language: Java (build: Maven)".into(),
                ref_path: None,
                full_chars: 10,
            },
            ExplorationEntry {
                tool: "file_list".into(),
                target: ".".into(),
                content: "pom.xml src".into(),
                ref_path: None,
                full_chars: 10,
            },
            ExplorationEntry {
                tool: "file_list".into(),
                target: "src/main/java".into(),
                content: "com/example".into(),
                ref_path: None,
                full_chars: 10,
            },
            ExplorationEntry {
                tool: "file_read".into(),
                target: "src/main/java/com/example/App.java".into(),
                content: "class App".into(),
                ref_path: None,
                full_chars: 10,
            },
        ];
        let mut explored = std::collections::HashSet::new();
        explored.insert("file_list:.".into());
        explored.insert("file_list:src/main/java".into());
        assert!(validate_plan_exploration(&entries, &explored).is_ok());
    }

    #[test]
    fn exploration_gate_flat_project_two_reads() {
        use crate::agent::exploration_snapshot::ExplorationEntry;
        let entries = vec![
            ExplorationEntry {
                tool: "project_detect".into(),
                target: ".".into(),
                content: "Language: Python (build: pip)".into(),
                ref_path: None,
                full_chars: 10,
            },
            ExplorationEntry {
                tool: "file_list".into(),
                target: ".".into(),
                content: "app.py requirements.txt".into(),
                ref_path: None,
                full_chars: 10,
            },
            ExplorationEntry {
                tool: "file_read".into(),
                target: "app.py".into(),
                content: "def main".into(),
                ref_path: None,
                full_chars: 10,
            },
            ExplorationEntry {
                tool: "file_read".into(),
                target: "requirements.txt".into(),
                content: "flask".into(),
                ref_path: None,
                full_chars: 10,
            },
        ];
        let mut explored = std::collections::HashSet::new();
        explored.insert("file_list:.".into());
        assert!(validate_plan_exploration(&entries, &explored).is_ok());
    }

    #[test]
    fn ungrounded_nested_path_rejected() {
        use crate::agent::exploration_snapshot::ExplorationEntry;
        let entries = vec![ExplorationEntry {
            tool: "project_detect".into(),
            target: ".".into(),
            content: "Language: TypeScript (build: npm)".into(),
            ref_path: None,
            full_chars: 10,
        }];
        let mut explored = std::collections::HashSet::new();
        explored.insert("file_list:.".into());
        let plan = r#"{"structure_summary":"Node project with source under src/ and entry index.ts at project root layout","plan":[{"step":1,"file":"src/index.ts","action":"explain","target":"main","desc":"read typescript entry module for routing overview","verify":"file_read"}]}"#;
        assert!(validate_plan_paths_known(plan, &entries, &explored).is_err());
    }

    #[test]
    fn load_from_review_report_table() {
        let report = r#"
## 审查结论

| 1 | 高 | `OmsGlobalOrderController.java` | @Idempotent 缺少 waitTime |
| 2 | 中 | RequestDTO | 删除 pi 字段 |
"#;
        let t = load_from_review_report(report).unwrap();
        assert_eq!(t.steps.len(), 2);
        assert_eq!(t.steps[0].action, "edit");
        assert!(t.steps[0].file.contains("OmsGlobalOrderController.java"));
    }

    #[test]
    fn load_from_review_report_numbered() {
        let report =
            "1. **Controller** — @Idempotent 加 waitTime\n2. **Request DTO** — 删除 pi 字段";
        let t = load_from_review_report(report).unwrap();
        assert_eq!(t.steps.len(), 2);
        assert_eq!(t.steps[0].target, "Controller");
    }

    #[test]
    fn load_from_review_report_bug_lines() {
        let report = "\
**BUG-1（严重）**：原订单状态无条件修改
- **位置**：L117
**BUG-2（严重）**：场景3未实现拆分";
        let t = load_from_review_report(report).unwrap();
        assert_eq!(t.steps.len(), 2);
        assert_eq!(t.steps[0].index, 1);
        assert!(t.steps[0].desc.contains("原订单"));
    }
}
