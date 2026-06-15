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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PlanTracker {
    pub steps: Vec<PlanStep>,
    /// 1-based index of the active step.
    #[serde(default = "default_current")]
    pub current_index: u32,
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
        let file = obj.get("file").and_then(|s| s.as_str()).unwrap_or("").to_string();
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
        let desc = obj.get("desc").and_then(|s| s.as_str()).unwrap_or("").to_string();
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
            let icon = match step.status {
                StepStatus::Done => "✅",
                StepStatus::InProgress => "▶",
                StepStatus::Skipped => "⏭",
                StepStatus::Pending => "⏳",
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
                "  {icon} {}.{action}{target_part}{file_part} — {}",
                step.index,
                if step.desc.is_empty() {
                    "(无描述)".to_string()
                } else {
                    step.desc.clone()
                }
            ));
        }

        lines.push("完成当前步骤后再进入下一项；已完成项勿重复。".to_string());
        lines.join("\n")
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
            !s.file.is_empty()
                && normalize_path(&s.file) == norm
                && s.status != StepStatus::Done
        });
        let Some(pos) = idx else {
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

    pub fn all_done(&self) -> bool {
        self.steps.iter().all(|s| {
            matches!(s.status, StepStatus::Done | StepStatus::Skipped)
        })
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

/// Validate plan steps have minimum required fields and exploration depth.
pub fn validate_plan_steps(json_str: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| format!("❌ plan JSON 解析失败: {e}"))?;

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
    entries.iter().any(|e| {
        e.tool == "file_list" && !is_root_list_path(&e.target)
    }) || explored.iter().any(|p| {
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
        .filter(|e| matches!(e.tool.as_str(), "find_symbol" | "code_search" | "file_search"))
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
        return "下一步: 再 file_read 一个相关文件，或 find_symbol/code_search 确认符号".to_string();
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
            "❌ 探索不足：必须先 project_detect 了解项目类型，再探索目录与关键文件。"
                .to_string(),
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

    let hierarchical_ok =
        file_lists >= 2 && has_root_directory_list(entries, explored) && has_non_root_directory_list(entries, explored);
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
    let v: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| format!("❌ plan JSON 解析失败: {e}"))?;
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
}
