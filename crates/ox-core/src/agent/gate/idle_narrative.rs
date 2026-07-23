//! Detect LLM idle narration (acknowledgment / intent without tools or deliverable).

use crate::agent::engine::WorkflowEngine;
use crate::message::Message;

pub const RESPONSE_DISCIPLINE: &str = "【输出纪律】每轮二选一：① 调工具 ② 交本步产物（JSON/报告/## Done/直接答）。\
禁止空转：不说「好的/明白/让我先/被摘要/需重读」而不立刻行动。详见 ox-output-discipline skill。";

pub const UNIFIED_RESPONSE_DISCIPLINE: &str = "【输出纪律】每轮必须调用 `complete_and_check`（禁止纯文本）。\
二选一：① action=read/write/探索 ② action=finish（有 finding_json→确认 / 无→结束）。禁止空转寒暄。";

const OUTPUT_DISCIPLINE_SKILL: &str = include_str!("../../skill/builtin/ox-output-discipline.md");

/// Per-iteration discipline — full skill body on first iteration, one-liner after.
pub fn discipline_for_iteration(iteration: u32) -> String {
    if iteration == 0 {
        let body = output_discipline_skill_body();
        let excerpt: String = body.chars().take(2200).collect();
        format!("{RESPONSE_DISCIPLINE}\n\n{excerpt}")
    } else {
        RESPONSE_DISCIPLINE.to_string()
    }
}

/// Unified mode discipline — references ox-unified-tooling skill.
pub fn discipline_for_iteration_unified(iteration: u32) -> String {
    if iteration > 0 {
        return String::new();
    }
    format!(
        "{UNIFIED_RESPONSE_DISCIPLINE}\n\
         主流程见 [WORKSPACE]：探索 → finish(finding_json) 确认一次 → 实施 → finish 结束。"
    )
}

fn output_discipline_skill_body() -> &'static str {
    static BODY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BODY.get_or_init(|| {
        let parts: Vec<&str> = OUTPUT_DISCIPLINE_SKILL.splitn(3, "---").collect();
        parts
            .get(2)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| OUTPUT_DISCIPLINE_SKILL.trim().to_string())
    })
}

const IDLE_HINT_TAG: &str = "❌ [IDLE]";

/// Context for deliverable checks.
pub struct IdleContext<'a> {
    pub step_idx: usize,
    pub engine: Option<&'a WorkflowEngine>,
}

pub enum IdleAction {
    /// Keep looping within the turn.
    Continue { directive: Option<String> },
    /// End turn — stop prose↔gate loop.
    EndTurn { user_status: String },
}

/// True when assistant text is acknowledgment / planning prose without substance.
pub fn is_idle_narrative(content: &str) -> bool {
    let t = content.trim();
    if t.is_empty() {
        return true;
    }
    if WorkflowEngine::looks_like_review_report(t) {
        return false;
    }
    if WorkflowEngine::text_signals_done(t) {
        return false;
    }
    if crate::agent::engine::extract_json_block(t).is_some() {
        return false;
    }
    // Long prose is substantive reasoning — not "空转" just because it says "让我先读".
    const SUBSTANTIVE_CHARS: usize = 280;
    if t.chars().count() >= SUBSTANTIVE_CHARS {
        return false;
    }
    let lower = t.to_lowercase();
    const MARKERS: &[&str] = &[
        "好的",
        "明白",
        "收到",
        "了解",
        "知道了",
        "让我",
        "我需要",
        "我先",
        "我来",
        "接下来",
        "逐条",
        "逐注释",
        "对照",
        "重新读",
        "重新读取",
        "被摘要",
        "完整注释",
        "需要先读",
        "需要先读取",
        "先读取",
        "先读",
        "仔细读",
        "重新仔细",
        "让我重新",
        "before modifying",
        "before i can",
        "need to read",
        "let me read",
        "let me re",
        "i need to re",
        "i will read",
        "re-read",
        "was summarized",
        "read the full",
        "read the remaining",
        "剩余部分",
        "完整枚举",
    ];
    if MARKERS.iter().any(|m| t.contains(m) || lower.contains(m)) {
        return true;
    }
    // Short acknowledgment-only lines.
    if t.len() < 80
        && (t.starts_with("好的")
            || t.starts_with("明白")
            || t.starts_with("Ok")
            || t.starts_with("OK")
            || lower.starts_with("understood"))
    {
        return true;
    }
    false
}

/// Whether text satisfies the current workflow step output requirement.
pub fn is_step_deliverable(ctx: &IdleContext<'_>, content: &str) -> bool {
    let t = content.trim();
    if t.is_empty() {
        return false;
    }
    if ctx.engine.is_some_and(|e| e.is_single_step()) {
        return WorkflowEngine::text_signals_done(t)
            || WorkflowEngine::looks_like_review_report(t)
            || crate::agent::perception::extract_from_text(t).is_some()
            || (!is_idle_narrative(t) && t.chars().count() >= 80);
    }
    !is_idle_narrative(t)
}

pub fn idle_streak_limit(_: &IdleContext<'_>) -> u32 {
    2
}

pub fn directive_for(ctx: &IdleContext<'_>) -> String {
    if ctx.engine.is_some_and(|e| e.is_single_step()) {
        return "调 complete_and_check（探索或 finish）；禁止空转重读。".into();
    }
    "调用 complete_and_check，禁止空转。".into()
}

pub fn directive_for_legacy(ctx: &IdleContext<'_>) -> String {
    if ctx.engine.is_some_and(|e| e.is_single_step()) {
        return "调工具或交产物（报告 + findings JSON + ## Done）；禁止空转重读。".into();
    }
    "调用工具或输出 ## Done，禁止空转。".into()
}

/// Handle empty tool-call responses; returns whether to end the turn.
pub fn handle_empty_response(
    ctx: &IdleContext<'_>,
    text: &str,
    idle_streak: &mut u32,
    had_validation_error: bool,
    completion_tokens: Option<u32>,
    unified_tool_mode: bool,
) -> IdleAction {
    // Hitting max_tokens often yields long prose with no tools — not intentional idle.
    if completion_tokens.is_some_and(|ct| ct >= 7500) {
        let ct = completion_tokens.unwrap_or(0);
        let hint = if unified_tool_mode {
            "请直接 complete_and_check（read 或 finish），勿长篇 prose"
        } else {
            "请直接调工具或交产物（报告/findings/## Done），勿长篇空转"
        };
        return IdleAction::Continue {
            directive: Some(format!(
                "{IDLE_HINT_TAG} 输出约 {ct} tokens，可能已达 max_tokens 上限被截断。{hint}。"
            )),
        };
    }
    if is_step_deliverable(ctx, text) {
        return IdleAction::Continue { directive: None };
    }
    if !is_idle_narrative(text) && !had_validation_error {
        return IdleAction::Continue { directive: None };
    }
    *idle_streak += 1;
    let limit = idle_streak_limit(ctx);
    if *idle_streak >= limit {
        let status = match ctx.step_idx {
            0 => "⚠️ 意图识别空转 — 已暂停，请重试或补充说明".into(),
            1 => "⚠️ 规划空转 — 已暂停，请检查模型 max_tokens 或重试".into(),
            2 => "⚠️ 审阅空转 — 已暂停，请重试".into(),
            3 if ctx
                .engine
                .is_some_and(crate::agent::workflow_session::is_feedback_discuss) =>
            {
                "⚠️ 讨论空转 — 已暂停，请重新说明你的意见".into()
            }
            3 => "⚠️ 执行空转 — 已暂停，请重试或缩小任务范围".into(),
            _ => "⚠️ 空转过多 — 已暂停".into(),
        };
        return IdleAction::EndTurn {
            user_status: status,
        };
    }
    IdleAction::Continue {
        directive: Some(format!(
            "{IDLE_HINT_TAG} 已连续 {}/{} 轮空转。{}",
            *idle_streak,
            limit,
            if unified_tool_mode {
                directive_for(ctx)
            } else {
                directive_for_legacy(ctx)
            }
        )),
    }
}

pub fn is_idle_system_hint(content: &str) -> bool {
    content.starts_with(IDLE_HINT_TAG)
        || content.starts_with("❌ 审查报告已输出")
        || content.starts_with("❌ Infinite Loop")
}

/// Replace stacked idle assistant lines (within a turn).
pub fn upsert_idle_assistant(messages: &mut Vec<Message>, new_msg: &Message) {
    let Message::Assistant {
        content: new_content,
        tool_calls: new_tc,
        ..
    } = new_msg
    else {
        messages.push(new_msg.clone());
        return;
    };
    if !new_tc.is_empty() || !is_idle_narrative(new_content) {
        messages.push(new_msg.clone());
        return;
    }
    strip_idle_hints(messages);
    if let Some(Message::Assistant {
        content: prev,
        tool_calls: prev_tc,
        ..
    }) = messages
        .iter()
        .rev()
        .find(|m| matches!(m, Message::Assistant { .. }))
        && prev_tc.is_empty()
        && is_idle_narrative(prev)
        && let Some(idx) = messages
            .iter()
            .rposition(|m| matches!(m, Message::Assistant { .. }))
    {
        messages[idx] = new_msg.clone();
        return;
    }
    messages.push(new_msg.clone());
}

pub fn upsert_idle_hint(messages: &mut Vec<Message>, hint: &str) {
    if let Some(Message::System { content }) = messages
        .iter_mut()
        .rev()
        .find(|m| matches!(m, Message::System { content } if is_idle_system_hint(content)))
    {
        *content = hint.to_string();
    } else {
        messages.push(Message::system(hint));
    }
}

pub fn strip_idle_hints(messages: &mut Vec<Message>) {
    messages.retain(|m| !matches!(m, Message::System { content } if is_idle_system_hint(content)));
}

/// Collapse consecutive idle assistant runs to the latest only.
pub fn collapse_redundant_idle(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i < messages.len() {
        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = &messages[i]
        else {
            i += 1;
            continue;
        };
        if !tool_calls.is_empty() || !is_idle_narrative(content) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + 1;
        while end < messages.len() {
            match &messages[end] {
                Message::System { content: s } if is_idle_system_hint(s) => end += 1,
                Message::Assistant {
                    content: c,
                    tool_calls: tc,
                    ..
                } if tc.is_empty() && is_idle_narrative(c) => end += 1,
                _ => break,
            }
        }
        if end > start + 1 {
            let mut keep_assistant = None;
            let mut keep_system = None;
            for m in messages[start..end].iter().rev() {
                match m {
                    Message::Assistant { .. } if keep_assistant.is_none() => {
                        keep_assistant = Some(m.clone());
                    }
                    Message::System { content: s }
                        if keep_system.is_none() && is_idle_system_hint(s) =>
                    {
                        keep_system = Some(m.clone());
                    }
                    _ => {}
                }
            }
            messages.drain(start..end);
            let mut insert_at = start;
            if let Some(a) = keep_assistant {
                messages.insert(insert_at, a);
                insert_at += 1;
            }
            if let Some(s) = keep_system {
                messages.insert(insert_at, s);
            }
            i = start + 1;
        } else {
            i += 1;
        }
    }
}

/// Strip idle assistants from persisted session (e.g. discuss rounds).
pub fn strip_idle_assistants(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            Message::Assistant {
                content,
                tool_calls,
                ..
            } if tool_calls.is_empty() && is_idle_narrative(content)
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chinese_ack_spin() {
        assert!(is_idle_narrative(
            "好的，我需要逐条注释对照代码实现。之前读取的内容被摘要了。"
        ));
    }

    #[test]
    fn json_is_not_idle() {
        assert!(!is_idle_narrative(r#"{"plan":[]}"#));
    }

    #[test]
    fn long_prose_with_planning_marker_not_idle() {
        let body = "让我先逐条对照代码。".repeat(30);
        assert!(!is_idle_narrative(&body));
    }

    #[test]
    fn ends_after_streak() {
        let ctx = IdleContext {
            step_idx: 1,
            engine: None,
        };
        let mut streak = 0;
        let t = "好的，让我先看看项目结构";
        assert!(matches!(
            handle_empty_response(&ctx, t, &mut streak, false, None, false),
            IdleAction::Continue { .. }
        ));
        assert!(matches!(
            handle_empty_response(&ctx, t, &mut streak, false, None, false),
            IdleAction::EndTurn { .. }
        ));
    }

    #[test]
    fn truncation_does_not_end_turn() {
        let ctx = IdleContext {
            step_idx: 0,
            engine: None,
        };
        let mut streak = 1;
        let t = "好的，让我先看看";
        assert!(matches!(
            handle_empty_response(&ctx, t, &mut streak, false, Some(8192), false),
            IdleAction::Continue { .. }
        ));
        assert_eq!(streak, 1);
    }
}