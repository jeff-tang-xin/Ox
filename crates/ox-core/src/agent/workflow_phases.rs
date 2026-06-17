//! Workflow cognitive phases — 感知 → 思考 → 执行

use super::engine::WorkflowEngine;

const PHASE_KEY: &str = "_workflow_phase";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPhase {
    /// Intent routing
    Route,
    /// 感知 — explore, read, collect evidence (Plan step or exploring Execute)
    Perceive,
    /// 思考 — review, structure, produce executable plan (Review or freeze findings)
    Think,
    /// 执行 — consume plan tracker, edit/write only
    Act,
}

impl WorkflowPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Perceive => "perceive",
            Self::Think => "think",
            Self::Act => "act",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Route => "路由",
            Self::Perceive => "感知",
            Self::Think => "思考",
            Self::Act => "执行",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "route" => Some(Self::Route),
            "perceive" => Some(Self::Perceive),
            "think" => Some(Self::Think),
            "act" => Some(Self::Act),
            _ => None,
        }
    }
}

pub fn set_phase(engine: &WorkflowEngine, phase: WorkflowPhase) {
    let prev = engine.get_variable(PHASE_KEY).unwrap_or_default();
    let next = phase.as_str().to_string();
    if prev != next {
        tracing::info!("[WORKFLOW_PHASE] {} → {}", prev, next);
    }
    engine.set_variable(PHASE_KEY, next);
}

pub fn get_phase(engine: &WorkflowEngine) -> WorkflowPhase {
    engine
        .get_variable(PHASE_KEY)
        .and_then(|s| WorkflowPhase::from_str(&s))
        .unwrap_or_else(|| infer_phase(engine))
}

pub fn clear_phase(engine: &WorkflowEngine) {
    engine.set_variable(PHASE_KEY, String::new());
}

/// Infer phase from workflow step + session flags when not explicitly set.
pub fn infer_phase(engine: &WorkflowEngine) -> WorkflowPhase {
    if crate::agent::workflow_session::is_implementation_phase(engine) {
        return WorkflowPhase::Act;
    }
    if !engine.is_workflow_active() {
        return WorkflowPhase::Route;
    }
    match engine.get_current_step_index() {
        0 => WorkflowPhase::Route,
        1 => WorkflowPhase::Perceive,
        2 => WorkflowPhase::Think,
        3 => {
            if engine.is_perceive_execute() {
                WorkflowPhase::Perceive
            } else {
                WorkflowPhase::Act
            }
        }
        _ => WorkflowPhase::Act,
    }
}

pub fn sync_phase(engine: &WorkflowEngine) {
    set_phase(engine, infer_phase(engine));
}

pub fn phase_prompt_addon(engine: &WorkflowEngine) -> String {
    if crate::agent::workflow_session::is_feedback_discuss(engine) {
        return "【意见模式 — 只读讨论】用户在对审查结论发表意见或澄清。\
             请基于已有审查报告回应，**禁止**修改代码、禁止进入实施、禁止从 Intent/Plan 重来。\
             可读文件核对事实；不得 file_write / edit_file / delete_range / shell_exec。"
            .to_string();
    }
    let phase = get_phase(engine);
    match phase {
        WorkflowPhase::Route => String::new(),
        WorkflowPhase::Perceive => perceive_rules(engine),
        WorkflowPhase::Think => think_rules(),
        WorkflowPhase::Act => act_rules(engine),
    }
}

fn perceive_rules(engine: &WorkflowEngine) -> String {
    let on_execute = engine.get_current_step_index() == 3;
    if on_execute {
        format!(
            "【阶段：感知】只读收集证据（file_read / code_search / find_symbol）。禁止修改文件。\n\
             结束前输出：\n\
             1. **人类可读审查报告**（表格/条目，给用户看）\n\
             2. **findings JSON**（仅机器解析，勿在报告中重复 JSON 字段内容；放在报告末尾独立代码块）\n\
             3. `## Done`\n\
             {schema}",
            schema = FINDINGS_JSON_SCHEMA
        )
    } else {
        "【阶段：感知】探索项目、确认路径与符号，输出含 structure_summary 的 plan JSON。\
         感知未完成前禁止输出最终 plan。".to_string()
    }
}

fn think_rules() -> String {
    "【阶段：思考】基于感知结果审阅计划：安全性、完整性、可行性。\
     只输出审阅 JSON，不调用工具。".to_string()
}

fn act_rules(engine: &WorkflowEngine) -> String {
    let progress = engine.plan_progress_summary();
    let mut parts = vec![
        "【阶段：执行】感知与思考已完成。严格消费【计划进度】清单，禁止退回探索。".to_string(),
        "• 禁止 code_search / find_symbol / file_search / file_list（除非清单明确要求）".to_string(),
        "• 每源文件 file_read 最多 1 次，读后下一 tool 必须是 edit_file / file_write".to_string(),
        "• 做完一项再下一项，全部完成后 ## Done".to_string(),
        "• 执行中不接受开放式中途补充；仅 park 后可跟进，或 /new 重新开始".to_string(),
    ];
    let findings = crate::agent::perception::findings_summary_block(engine);
    if !findings.is_empty() {
        parts.push(findings);
    } else if let Some(report) = engine.get_execute_review_report() {
        let snippet: String = report.chars().take(4000).collect();
        parts.push(format!("【审查报告摘要】\n{snippet}"));
    }
    if !progress.is_empty() {
        parts.push(progress);
    }
    parts.join("\n")
}

/// Act phase: block mid-flight interjection (use park resume or /new instead).
pub fn allows_midflight_interjection(engine: &WorkflowEngine) -> bool {
    if crate::agent::workflow_session::is_parked(engine) {
        return false;
    }
    get_phase(engine) != WorkflowPhase::Act
}

/// Whether a user message may start/resume a round during Act (park resume or /new only).
pub fn accepts_user_round_input(engine: &WorkflowEngine, user_text: &str) -> bool {
    if get_phase(engine) != WorkflowPhase::Act {
        return true;
    }
    if crate::agent::workflow_session::is_parked(engine) {
        return true;
    }
    crate::agent::workflow_session::looks_like_new_task(user_text)
}

pub fn act_interjection_blocked_message() -> &'static str {
    "⏸️ 执行阶段不接受中途开放式补充（避免退回探索/重规划）。\
     请等待当前步骤完成；若任务已暂停(park)，请直接回复「修复/继续」；重新开始请 /new。"
}

/// Act phase: block exploration tools — execution consumes plan, not re-perceive.
pub fn validate_act_tool(engine: &WorkflowEngine, tool_name: &str) -> Result<(), String> {
    if get_phase(engine) != WorkflowPhase::Act {
        return Ok(());
    }
    match tool_name {
        "code_search" | "find_symbol" | "file_search" | "file_list" | "project_detect" => {
            Err(format!(
                "❌ 执行阶段禁止 `{tool_name}`（感知已完成）。\
                 请按【计划进度】对当前项 edit_file / file_write；\
                 需要读文件时用 file_read（每路径仅 1 次）。"
            ))
        }
        _ => Ok(()),
    }
}

pub const FINDINGS_JSON_SCHEMA: &str = r#"```json
{
  "findings_summary": "≥30字审查结论",
  "findings": [
    {
      "index": 1,
      "severity": "high|medium|low",
      "file": "路径或空",
      "target": "类/方法/符号",
      "issue": "问题描述",
      "recommendation": "修改建议（可执行）"
    }
  ]
}
```"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use crate::agent::workflow::{create_default_workflow, DEFAULT_WORKFLOW_ID};
    use std::sync::Arc;

    fn test_engine_at(step: usize) -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("test")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        session.blocking_lock().current_step_index = step;
        engine
    }

    #[test]
    fn infer_perceive_on_plan_step() {
        let engine = test_engine_at(1);
        assert_eq!(infer_phase(&engine), WorkflowPhase::Perceive);
    }

    #[test]
    fn infer_think_on_review_step() {
        let engine = test_engine_at(2);
        assert_eq!(infer_phase(&engine), WorkflowPhase::Think);
    }

    #[test]
    fn act_blocks_midflight_interjection() {
        let engine = test_engine_at(3);
        crate::agent::workflow_session::enter_implementation_phase(&engine);
        sync_phase(&engine);
        assert!(!allows_midflight_interjection(&engine));
        assert!(!accepts_user_round_input(&engine, "改一下 Controller"));
        assert!(accepts_user_round_input(&engine, "/new fix"));
    }

    #[test]
    fn parked_allows_user_round_input() {
        let engine = test_engine_at(3);
        crate::agent::workflow_session::enter_implementation_phase(&engine);
        crate::agent::workflow_session::park(&engine);
        assert!(accepts_user_round_input(&engine, "修复问题 1/2/3"));
    }

    #[test]
    fn act_blocks_file_search() {
        let engine = test_engine_at(3);
        crate::agent::workflow_session::enter_implementation_phase(&engine);
        sync_phase(&engine);
        assert!(validate_act_tool(&engine, "file_search").is_err());
        assert!(validate_act_tool(&engine, "edit_file").is_ok());
    }
}
