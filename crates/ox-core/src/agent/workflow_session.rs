//! Persistent task session — park on ## Done, resume on user follow-up (no Intent/Plan restart).

use super::engine::WorkflowEngine;

const PARKED_KEY: &str = "_workflow_parked";
const EXECUTE_APPROVED_KEY: &str = "_execute_user_approved";
const IMPL_PHASE_KEY: &str = "_execute_impl_phase";
const FEEDBACK_DISCUSS_KEY: &str = "_park_feedback_discuss";

pub fn is_parked(engine: &WorkflowEngine) -> bool {
    engine.get_variable(PARKED_KEY).as_deref() == Some("1")
}

pub fn park(engine: &WorkflowEngine) {
    engine.set_variable(PARKED_KEY, "1".to_string());
    tracing::info!("[WORKFLOW_SESSION] parked — awaiting user follow-up");
}

pub fn unpark(engine: &WorkflowEngine) {
    engine.set_variable(PARKED_KEY, String::new());
}

pub fn clear_session_flags(engine: &WorkflowEngine) {
    engine.set_variable(PARKED_KEY, String::new());
    engine.set_variable(EXECUTE_APPROVED_KEY, String::new());
    engine.set_variable(IMPL_PHASE_KEY, String::new());
    engine.set_variable(FEEDBACK_DISCUSS_KEY, String::new());
}

pub fn is_feedback_discuss(engine: &WorkflowEngine) -> bool {
    engine.get_variable(FEEDBACK_DISCUSS_KEY).as_deref() == Some("1")
}

/// Park resume: user chose 意见 — discuss findings only, no implementation.
pub fn enter_feedback_discuss(engine: &WorkflowEngine) {
    engine.set_variable(FEEDBACK_DISCUSS_KEY, "1".to_string());
    engine.set_variable(IMPL_PHASE_KEY, String::new());
    crate::agent::workflow_phases::set_phase(
        engine,
        crate::agent::workflow_phases::WorkflowPhase::Think,
    );
    tracing::info!("[WORKFLOW_SESSION] feedback discuss — read-only, no implementation");
}

pub fn clear_feedback_discuss(engine: &WorkflowEngine) {
    engine.set_variable(FEEDBACK_DISCUSS_KEY, String::new());
}

/// Block writes during feedback discuss mode.
pub fn validate_feedback_discuss_tool(
    engine: &WorkflowEngine,
    tool_name: &str,
) -> Result<(), String> {
    if !is_feedback_discuss(engine) {
        return Ok(());
    }
    match tool_name {
        "file_write" | "edit_file" | "delete_range" | "shell_exec" => Err(format!(
            "❌ 意见模式仅讨论审查结论，禁止 `{tool_name}`。\
             请文字回应；若要修改代码请选「继续」并说明修复范围。"
        )),
        _ => Ok(()),
    }
}

pub fn is_implementation_phase(engine: &WorkflowEngine) -> bool {
    engine.get_variable(IMPL_PHASE_KEY).as_deref() == Some("1")
}

pub fn enter_implementation_phase(engine: &WorkflowEngine) {
    engine.set_variable(IMPL_PHASE_KEY, "1".to_string());
    mark_execute_approved(engine);
    tracing::info!("[WORKFLOW_SESSION] implementation phase — reads/writes allowed");
}

pub fn mark_execute_approved(engine: &WorkflowEngine) {
    engine.set_variable(EXECUTE_APPROVED_KEY, "1".to_string());
}

pub fn is_execute_user_approved(engine: &WorkflowEngine) -> bool {
    engine.get_variable(EXECUTE_APPROVED_KEY).as_deref() == Some("1")
}

/// User is discussing or acting on findings from a parked review report.
pub fn looks_like_review_follow_up(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();

    const DISPUTE: &[&str] = &[
        "不是问题",
        "不算问题",
        "不用改",
        "不用修",
        "无需修改",
        "可以忽略",
        "误报",
        "误判",
        "false positive",
        "其实没问题",
        "不用处理",
    ];
    if DISPUTE.iter().any(|k| t.contains(k) || lower.contains(k)) {
        return true;
    }

    if references_numbered_findings(t) {
        return true;
    }

    // 「帮我修复 1、2」— digits + action verb, without requiring the word「问题」
    if t.chars().any(|c| c.is_ascii_digit())
        && [
            "修复", "改", "处理", "解决", "fix", "只改", "仅改", "只修", "仅修",
        ]
        .iter()
        .any(|k| t.contains(k) || lower.contains(k))
    {
        return true;
    }

    false
}

fn references_numbered_findings(t: &str) -> bool {
    if !t.contains("问题") && !t.contains("finding") && !t.contains("Finding") {
        return false;
    }
    if t.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    ["问题一", "问题二", "问题三", "第一项", "第二项", "第三项"]
        .iter()
        .any(|k| t.contains(k))
}

/// User wants to continue / implement / execute based on prior output.
pub fn looks_like_workflow_continuation(user_text: &str) -> bool {
    if looks_like_review_follow_up(user_text) {
        return true;
    }
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    [
        "执行",
        "去做",
        "开始做",
        "按你说的",
        "按这个",
        "就这个",
        "可以执行",
        "去改",
        "去修改",
        "实现",
        "动手",
        "继续",
        "go ahead",
        "execute",
        "implement",
        "do it",
        "proceed",
        "lgtm",
        "looks good",
        "没问题",
        "好的执行",
        "开始执行",
        "按方案",
        "修复",
        "改掉",
        "改一下",
        "改下",
        "解决问题",
        "处理掉",
        "fix",
        "fixed",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
}

/// User wants to implement fixes after a read-only review (park resume / interjection).
pub fn looks_like_implementation_request(user_text: &str) -> bool {
    looks_like_workflow_continuation(user_text)
        || {
            let t = user_text.trim();
            if t.is_empty() {
                return false;
            }
            let lower = t.to_lowercase();
            [
                "问题1", "问题 1", "问题2", "问题 2", "问题3", "问题 3",
                "1/2/3", "1、2、3", "按审查", "按上面", "按建议",
            ]
            .iter()
            .any(|k| t.contains(k) || lower.contains(k))
                && [
                    "修复", "改", "fix", "处理", "解决", "实施", "动手",
                ]
                .iter()
                .any(|k| t.contains(k) || lower.contains(k))
        }
}

/// Explicit new task — end parked session and restart workflow.
pub fn looks_like_new_task(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    if t.starts_with("/new") || t.starts_with("/reset") || lower.starts_with("new task") {
        return true;
    }
    [
        "新任务",
        "换个话题",
        "另外一件事",
        "重新开始",
        "另一个项目",
        "别管上面",
        "忽略上文",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
}

pub fn implementation_phase_system_note() -> &'static str {
    "【实施阶段】用户已确认按审查意见修改。已生成【计划进度】清单；\
     每文件 file_read 最多 1 次，读后必须 edit_file/file_write；全部完成后 ## Done。"
}

pub fn resume_message(user_text: &str, prior_output: Option<&str>) -> String {
    let impl_note = if looks_like_implementation_request(user_text) {
        format!("\n\n{}", implementation_phase_system_note())
    } else {
        String::new()
    };
    let mut parts = vec![format!(
        "[TASK_SESSION_RESUME — 用户在同一任务会话中跟进；采纳下方指示，**勿**从 Intent/Plan 重来]{impl_note}\n{}",
        user_text.trim()
    )];
    if let Some(out) = prior_output {
        if !out.trim().is_empty() {
            let snippet: String = out.chars().take(8000).collect();
            parts.push(format!(
                "【你上一轮审查报告 — 用户在此基础上跟进（完整版亦在 _step3_output / 对话历史）】\n{snippet}"
            ));
        }
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_follow_up_phrases() {
        assert!(looks_like_review_follow_up("总共问题1、2、3，帮我修复1、2"));
        assert!(looks_like_review_follow_up("问题3其实不是问题，怎么处理？"));
        assert!(!looks_like_review_follow_up("帮我写个爬虫"));
    }

    #[test]
    fn continuation_phrases() {
        assert!(looks_like_workflow_continuation("按这个方案执行"));
        assert!(looks_like_workflow_continuation("go ahead and implement"));
        assert!(looks_like_workflow_continuation("修复问题 1/2/3/4/7"));
        assert!(!looks_like_workflow_continuation("新任务：写个爬虫"));
        assert!(looks_like_implementation_request("修复问题 1/2/3/4/7"));
    }

    #[test]
    fn new_task_phrases() {
        assert!(looks_like_new_task("/new fix login"));
        assert!(looks_like_new_task("新任务 帮我写脚本"));
    }
}
