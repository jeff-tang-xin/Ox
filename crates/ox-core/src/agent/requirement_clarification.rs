//! Clarification gates — Intent requirements & parked-session disambiguation.

use super::engine::WorkflowEngine;

const AWAIT_KEY: &str = "_await_clarification";
const QUESTIONS_KEY: &str = "_clarification_questions";
const PENDING_ADVANCE_KEY: &str = "_clarification_pending_advance";
const KIND_KEY: &str = "_clarification_kind";
const PENDING_INPUT_KEY: &str = "_park_disambiguation_input";
const PARK_STAGE_KEY: &str = "_park_follow_up_stage";
const PARK_DETAIL_KIND_KEY: &str = "_park_detail_kind";

const KIND_INTENT: &str = "intent";
const KIND_PARK: &str = "park";
const STAGE_MENU: &str = "menu";
const STAGE_DETAIL: &str = "detail";
const DETAIL_CONTINUE: &str = "continue";
const DETAIL_FEEDBACK: &str = "feedback";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentParseResult {
    pub routing: super::engine::IntentRouting,
    pub needs_clarification: bool,
    pub clarification_questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkDisambiguationResolution {
    /// Resume and implement / execute fixes from the review.
    ContinuePrevious { follow_up: String },
    /// Clarify or dispute findings — discuss only, no auto-implementation.
    Feedback { text: String },
    /// End session and start fresh Intent with `task`.
    NewTask { task: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkFollowUpOutcome {
    Resolved(ParkDisambiguationResolution),
    /// User picked 继续/意见 without detail — wait for next message.
    NeedDetail { hint: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParkMenuChoice {
    Continue,
    Feedback,
    NewTask,
    Unclear,
}

pub fn extract_clarification(v: &serde_json::Value) -> (bool, Vec<String>) {
    let needs = v
        .get("needs_clarification")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let questions: Vec<String> = v
        .get("clarification_questions")
        .and_then(|q| q.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if needs && questions.is_empty() {
        return (true, vec!["请补充更具体的需求说明（目标、范围、约束）。".to_string()]);
    }
    (needs, questions)
}

pub fn is_awaiting(engine: &WorkflowEngine) -> bool {
    engine.get_variable(AWAIT_KEY).as_deref() == Some("1")
}

pub fn is_park_disambiguation(engine: &WorkflowEngine) -> bool {
    is_awaiting(engine) && engine.get_variable(KIND_KEY).as_deref() == Some(KIND_PARK)
}

pub fn is_intent_clarification(engine: &WorkflowEngine) -> bool {
    is_awaiting(engine) && engine.get_variable(KIND_KEY).as_deref() != Some(KIND_PARK)
}

pub fn pending_advance_step(engine: &WorkflowEngine) -> usize {
    engine
        .get_variable(PENDING_ADVANCE_KEY)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

pub fn park_pending_input(engine: &WorkflowEngine) -> String {
    engine
        .get_variable(PENDING_INPUT_KEY)
        .unwrap_or_default()
}

fn arm_gate_inner(
    engine: &WorkflowEngine,
    kind: &str,
    questions: &[String],
    pending_advance: usize,
    pending_input: Option<&str>,
) {
    engine.set_variable(AWAIT_KEY, "1".to_string());
    engine.set_variable(KIND_KEY, kind.to_string());
    engine.set_variable(PENDING_ADVANCE_KEY, pending_advance.to_string());
    let json = serde_json::to_string(questions).unwrap_or_else(|_| "[]".to_string());
    engine.set_variable(QUESTIONS_KEY, json);
    if let Some(p) = pending_input {
        engine.set_variable(PENDING_INPUT_KEY, p.to_string());
    } else {
        engine.set_variable(PENDING_INPUT_KEY, String::new());
    }
    engine.set_confirmation_flag();
    tracing::info!("[CLARIFICATION] armed kind={kind} questions={}", questions.len());
}

pub fn arm_gate(engine: &WorkflowEngine, questions: &[String], pending_advance: usize) {
    arm_gate_inner(engine, KIND_INTENT, questions, pending_advance, None);
}

pub fn clear_gate(engine: &WorkflowEngine) {
    engine.set_variable(AWAIT_KEY, String::new());
    engine.set_variable(KIND_KEY, String::new());
    engine.set_variable(QUESTIONS_KEY, String::new());
    engine.set_variable(PENDING_ADVANCE_KEY, String::new());
    engine.set_variable(PENDING_INPUT_KEY, String::new());
    engine.set_variable(PARK_STAGE_KEY, String::new());
    engine.set_variable(PARK_DETAIL_KIND_KEY, String::new());
    engine.clear_confirmation_flag();
}

/// Park complete — show explicit menu (继续 / 意见 / 新任务); no intent guessing.
pub fn arm_park_follow_up_menu(engine: &WorkflowEngine) {
    engine.set_variable(PARK_STAGE_KEY, STAGE_MENU.to_string());
    engine.set_variable(PARK_DETAIL_KIND_KEY, String::new());
    let questions = vec![
        "审查已完成。请选择下一步（输入 **1/2/3** 或关键词）：".to_string(),
        "**1 · 继续** — 按审查意见修改/执行（如「修复 1、2」）".to_string(),
        "**2 · 意见** — 澄清或质疑审查结论（**只读讨论，不进入实施**）".to_string(),
        "**3 · 新任务** — 结束当前会话，从 Intent 重新开始".to_string(),
        "也可一次说完：`继续：修复1、2` / `意见：envConfig 有默认值` / `新任务：…`".to_string(),
    ];
    arm_gate_inner(engine, KIND_PARK, &questions, 0, None);
}

/// Legacy name — reactive disambiguation replaced by proactive menu on park.
pub fn arm_park_disambiguation_gate(engine: &WorkflowEngine, _pending_input: &str) {
    arm_park_follow_up_menu(engine);
}

pub fn questions(engine: &WorkflowEngine) -> Vec<String> {
    engine
        .get_variable(QUESTIONS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn format_markdown(engine: &WorkflowEngine) -> String {
    let qs = questions(engine);
    if is_park_disambiguation(engine) {
        return format_park_disambiguation_markdown(&qs);
    }
    format_intent_clarification_markdown(&qs)
}

fn format_intent_clarification_markdown(questions: &[String]) -> String {
    let mut lines = vec![
        "## ❓ 需求澄清".to_string(),
        String::new(),
        "意图分析后仍需确认以下信息。**请直接、具体地回答**（可逐条回答）：".to_string(),
        String::new(),
        "> **不会根据模糊回复猜测你的意思**；若回答过于笼统（如「嗯」「随便」「你看着办」），会继续追问。"
            .to_string(),
        String::new(),
    ];
    for (i, q) in questions.iter().enumerate() {
        lines.push(format!("{}. {q}", i + 1));
    }
    lines.push(String::new());
    lines.push(
        "> 澄清后将进入下一步（规划或执行确认），不会从 Intent 重来。".to_string(),
    );
    lines.join("\n")
}

fn format_park_disambiguation_markdown(_questions: &[String]) -> String {
    [
        "## ⏸️ 审查完成 — 请选择下一步",
        "",
        "> 按 **1** / **2** / **3** 选择（或输入关键词）",
        "",
        "- **1 · 继续** — 按审查意见修改/执行（如「修复 1、2」）",
        "- **2 · 意见** — 澄清或质疑结论（**只读讨论，不实施**）",
        "- **3 · 新任务** — 结束会话，从 Intent 重新开始",
        "",
        "也可一次说完：`继续：…` / `意见：…` / `新任务：…`",
    ]
    .join("\n")
}

/// Merge user answer into the active request (Intent clarification only).
pub fn apply_answer(engine: &WorkflowEngine, answer: &str) {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return;
    }
    let prev = engine
        .get_variable("_current_user_request")
        .unwrap_or_default();
    let merged = if prev.trim().is_empty() {
        trimmed.to_string()
    } else {
        format!("{prev}\n\n【需求澄清 — 用户补充】\n{trimmed}")
    };
    engine.set_variable("_current_user_request", merged);
    engine.append_workflow_guidance(trimmed);
}

pub fn resolve_park_follow_up(
    engine: &WorkflowEngine,
    answer: &str,
) -> Result<ParkFollowUpOutcome, String> {
    if !is_park_disambiguation(engine) {
        return Err("当前不在 park 跟进选择状态。".to_string());
    }
    let stage = engine
        .get_variable(PARK_STAGE_KEY)
        .unwrap_or_else(|| STAGE_MENU.to_string());
    if stage == STAGE_DETAIL {
        return resolve_park_detail(engine, answer);
    }
    resolve_park_menu(engine, answer)
}

/// Alias for callers expecting the old name.
pub fn resolve_park_disambiguation(
    engine: &WorkflowEngine,
    answer: &str,
) -> Result<ParkFollowUpOutcome, String> {
    resolve_park_follow_up(engine, answer)
}

fn resolve_park_menu(
    engine: &WorkflowEngine,
    answer: &str,
) -> Result<ParkFollowUpOutcome, String> {
    let t = answer.trim();
    if t.is_empty() {
        return Err("请先选择：1/继续、2/意见、3/新任务".to_string());
    }
    if let Some(text) = strip_explicit_prefix(t, FEEDBACK_PREFIXES) {
        clear_gate(engine);
        return Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::Feedback {
                text: text.to_string(),
            },
        ));
    }
    if let Some(text) = strip_explicit_prefix(t, CONTINUE_PREFIXES) {
        clear_gate(engine);
        return Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::ContinuePrevious {
                follow_up: text.to_string(),
            },
        ));
    }
    if let Some(text) = strip_explicit_prefix(t, NEW_TASK_PREFIXES) {
        clear_gate(engine);
        let task = if text.is_empty() {
            "新任务".to_string()
        } else {
            text.to_string()
        };
        return Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::NewTask { task },
        ));
    }
    if is_bare_new_task_token(t) {
        clear_gate(engine);
        return Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::NewTask {
                task: "新任务".to_string(),
            },
        ));
    }
    match classify_park_menu_choice(t) {
        ParkMenuChoice::Continue => {
            engine.set_variable(PARK_STAGE_KEY, STAGE_DETAIL.to_string());
            engine.set_variable(PARK_DETAIL_KIND_KEY, DETAIL_CONTINUE.to_string());
            Ok(ParkFollowUpOutcome::NeedDetail {
                hint: "已选「继续」— 请说明要执行/修复的范围（如「修复问题 1、2」）。".to_string(),
            })
        }
        ParkMenuChoice::Feedback => {
            engine.set_variable(PARK_STAGE_KEY, STAGE_DETAIL.to_string());
            engine.set_variable(PARK_DETAIL_KIND_KEY, DETAIL_FEEDBACK.to_string());
            Ok(ParkFollowUpOutcome::NeedDetail {
                hint: "已选「意见」— 请说明你的澄清或质疑（只讨论，不会修改代码）。".to_string(),
            })
        }
        ParkMenuChoice::NewTask => {
            engine.set_variable(PARK_STAGE_KEY, STAGE_DETAIL.to_string());
            engine.set_variable(PARK_DETAIL_KIND_KEY, "newtask".to_string());
            Ok(ParkFollowUpOutcome::NeedDetail {
                hint: "已选「新任务」— 请描述新任务内容。".to_string(),
            })
        }
        ParkMenuChoice::Unclear => Err(
            "请先选择：**1/继续**、**2/意见**、**3/新任务**（或使用「继续：…」「意见：…」「新任务：…」一次说完）。"
                .to_string(),
        ),
    }
}

fn resolve_park_detail(
    engine: &WorkflowEngine,
    answer: &str,
) -> Result<ParkFollowUpOutcome, String> {
    let t = answer.trim();
    if t.is_empty() {
        return Err("请补充具体说明，不能为空。".to_string());
    }
    let kind = engine
        .get_variable(PARK_DETAIL_KIND_KEY)
        .unwrap_or_default();
    clear_gate(engine);
    match kind.as_str() {
        DETAIL_CONTINUE => Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::ContinuePrevious {
                follow_up: t.to_string(),
            },
        )),
        DETAIL_FEEDBACK => Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::Feedback {
                text: t.to_string(),
            },
        )),
        "newtask" => Ok(ParkFollowUpOutcome::Resolved(
            ParkDisambiguationResolution::NewTask {
                task: t.to_string(),
            },
        )),
        _ => Err("内部状态错误，请重新选择继续/意见/新任务。".to_string()),
    }
}

fn classify_park_menu_choice(t: &str) -> ParkMenuChoice {
    let lower = t.trim().to_lowercase();
    if matches!(
        lower.as_str(),
        "1" | "继续" | "continue" | "resume" | "执行" | "修复" | "继续上一任务"
    ) {
        return ParkMenuChoice::Continue;
    }
    if matches!(
        lower.as_str(),
        "2" | "意见" | "反馈" | "澄清" | "说明" | "feedback" | "comment"
    ) {
        return ParkMenuChoice::Feedback;
    }
    if matches!(lower.as_str(), "3" | "新任务" | "new" | "/new" | "new task") {
        return ParkMenuChoice::NewTask;
    }
    ParkMenuChoice::Unclear
}

const CONTINUE_PREFIXES: &[&str] = &[
    "继续：",
    "继续:",
    "继续 ",
    "继续上一任务：",
    "继续上一任务:",
    "继续上一任务 ",
    "continue:",
    "continue ",
];

const FEEDBACK_PREFIXES: &[&str] = &[
    "意见：",
    "意见:",
    "意见 ",
    "反馈：",
    "反馈:",
    "反馈 ",
    "澄清：",
    "澄清:",
    "说明：",
    "说明:",
];

const NEW_TASK_PREFIXES: &[&str] = &[
    "新任务：",
    "新任务:",
    "新任务 ",
    "/new ",
    "/new:",
];

pub fn is_explicit_parked_continue(user_text: &str) -> bool {
    let t = user_text.trim();
    !t.is_empty()
        && (classify_park_menu_choice(t) == ParkMenuChoice::Continue
            || strip_explicit_prefix(t, CONTINUE_PREFIXES).is_some())
}

pub fn is_explicit_parked_new_task(user_text: &str) -> bool {
    let t = user_text.trim();
    !t.is_empty()
        && (is_bare_new_task_token(t)
            || classify_park_menu_choice(t) == ParkMenuChoice::NewTask
            || strip_explicit_prefix(t, NEW_TASK_PREFIXES).is_some())
}

fn is_bare_new_task_token(t: &str) -> bool {
    let lower = t.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "新任务" | "新的" | "新话题" | "new" | "new task" | "/new"
    )
}

fn strip_explicit_prefix<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    let t = text.trim();
    for p in prefixes {
        if let Some(rest) = t.strip_prefix(p) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

/// Reject vague / non-answers during Intent clarification (no guessing).
pub fn validate_intent_clarification_answer(answer: &str) -> Result<(), String> {
    let t = answer.trim();
    if t.is_empty() {
        return Err("请直接回答上方澄清问题，不能为空。".to_string());
    }
    if t.chars().count() < 2 {
        return Err("回答过短，请补充具体信息（对象、范围或约束）。".to_string());
    }
    let lower = t.to_lowercase();
    const VAGUE: &[&str] = &[
        "嗯", "恩", "好", "好的", "ok", "okay", "yes", "y", "行", "可以", "随便", "你看着办",
        "看着办", "不知道", "不确定", "都行", "都可以", "无所谓", "差不多", "大概", "maybe",
        "idk",
    ];
    if VAGUE.contains(&lower.as_str()) {
        return Err(
            "回答过于笼统，无法据此开工。请针对上方问题给出**具体**说明（不要让我猜测）。"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use std::sync::Arc;

    #[test]
    fn extract_requires_questions_when_flagged() {
        let v = serde_json::json!({"needs_clarification": true, "clarification_questions": []});
        let (needs, qs) = extract_clarification(&v);
        assert!(needs);
        assert_eq!(qs.len(), 1);
    }

    #[test]
    fn arm_and_clear_gate() {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        arm_gate(&engine, &["改哪个文件？".into()], 1);
        assert!(is_awaiting(&engine));
        assert!(is_intent_clarification(&engine));
        assert_eq!(pending_advance_step(&engine), 1);
        clear_gate(&engine);
        assert!(!is_awaiting(&engine));
    }

    #[test]
    fn park_menu_feedback_one_shot() {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        arm_park_follow_up_menu(&engine);
        let r = resolve_park_follow_up(
            &engine,
            "意见：Boolean envConfig 有默认值 @Value true",
        )
        .unwrap();
        assert_eq!(
            r,
            ParkFollowUpOutcome::Resolved(ParkDisambiguationResolution::Feedback {
                text: "Boolean envConfig 有默认值 @Value true".into()
            })
        );
    }

    #[test]
    fn park_menu_bare_feedback_needs_detail() {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        arm_park_follow_up_menu(&engine);
        let r = resolve_park_follow_up(&engine, "意见").unwrap();
        assert!(matches!(r, ParkFollowUpOutcome::NeedDetail { .. }));
        let r2 = resolve_park_follow_up(&engine, "envConfig 有默认值").unwrap();
        assert_eq!(
            r2,
            ParkFollowUpOutcome::Resolved(ParkDisambiguationResolution::Feedback {
                text: "envConfig 有默认值".into()
            })
        );
    }

    #[test]
    fn park_menu_free_text_rejected() {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        arm_park_follow_up_menu(&engine);
        assert!(resolve_park_follow_up(&engine, "Boolean envConfig 有默认值").is_err());
        assert!(is_park_disambiguation(&engine));
    }

    #[test]
    fn park_menu_continue_one_shot() {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        arm_park_follow_up_menu(&engine);
        let r = resolve_park_follow_up(&engine, "继续：修复 1、2").unwrap();
        assert_eq!(
            r,
            ParkFollowUpOutcome::Resolved(ParkDisambiguationResolution::ContinuePrevious {
                follow_up: "修复 1、2".into()
            })
        );
    }

    #[test]
    fn park_menu_new_task() {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        arm_park_follow_up_menu(&engine);
        let r = resolve_park_follow_up(&engine, "新任务：写爬虫").unwrap();
        assert_eq!(
            r,
            ParkFollowUpOutcome::Resolved(ParkDisambiguationResolution::NewTask {
                task: "写爬虫".into()
            })
        );
    }

    #[test]
    fn intent_clarification_rejects_vague_answer() {
        assert!(validate_intent_clarification_answer("嗯").is_err());
        assert!(validate_intent_clarification_answer("随便").is_err());
        assert!(validate_intent_clarification_answer("改 user.rs 的登录校验").is_ok());
    }
}
