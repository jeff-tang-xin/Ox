//! Detect LLM idle narration (acknowledgment / intent without tools or deliverable).

use crate::agent::engine::WorkflowEngine;
use crate::message::Message;

pub const RESPONSE_DISCIPLINE: &str = "【输出纪律】每轮二选一：① 调工具 ② 交本步产物（JSON/报告/## Done/直接答）。\
禁止空转：不说「好的/明白/让我先/被摘要/需重读」而不立刻行动。详见 ox-output-discipline skill。";

const OUTPUT_DISCIPLINE_SKILL: &str = include_str!("../skill/builtin/ox-output-discipline.md");

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
    match ctx.step_idx {
        0 => {
            if let Some(engine) = ctx.engine {
                if engine.is_single_step() {
                    return WorkflowEngine::text_signals_done(t)
                        || WorkflowEngine::looks_like_review_report(t)
                        || crate::agent::perception::extract_from_text(t).is_some()
                        || (!is_idle_narrative(t) && t.chars().count() >= 80);
                }
            }
            crate::agent::engine::extract_json_block(t)
                .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                .map(|v| v.get("routing").is_some() || v.get("intent").is_some())
                .unwrap_or(false)
        }
        1 => {
            if crate::agent::engine::extract_json_block(t)
                .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                .map(|v| v.get("plan").is_some())
                .unwrap_or(false)
            {
                return true;
            }
            if let Some(engine) = ctx.engine {
                if engine.plan_exploration_satisfied() && t.len() > 400 {
                    return true;
                }
            }
            false
        }
        2 => crate::agent::engine::extract_json_block(t)
            .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
            .map(|v| v.get("safe").is_some() && v.get("complete").is_some())
            .unwrap_or(false),
        3 => {
            if WorkflowEngine::looks_like_review_report(t) || WorkflowEngine::text_signals_done(t) {
                return true;
            }
            if ctx
                .engine
                .is_some_and(|e| crate::agent::workflow_session::is_feedback_discuss(e))
            {
                return !is_idle_narrative(t) && t.len() >= 40;
            }
            false
        }
        _ => !is_idle_narrative(t),
    }
}

pub fn idle_streak_limit(ctx: &IdleContext<'_>) -> u32 {
    if ctx.step_idx == 3 {
        if ctx
            .engine
            .is_some_and(|e| crate::agent::workflow_session::is_feedback_discuss(e))
        {
            2
        } else {
            3
        }
    } else {
        2
    }
}

pub fn directive_for(ctx: &IdleContext<'_>) -> String {
    if ctx.engine.is_some_and(|e| e.is_single_step()) {
        return "审查/问答：输出报告 + findings JSON + ## Done 即结束；禁止重复 file_read/shell 读同一文件。"
            .into();
    }
    match ctx.step_idx {
        0 => "立即输出 intent JSON（含 routing），不要解释。".into(),
        1 => {
            if ctx
                .engine
                .is_some_and(|e| e.plan_exploration_satisfied())
            {
                "探索已完成：立即输出 plan JSON，禁止 prose，禁止再调探索工具。".into()
            } else {
                "立即调用 file_read / code_search / find_symbol，或输出 plan JSON；禁止只说「要先探索」。".into()
            }
        }
        2 => "立即输出审阅 JSON（safe、complete、issues），禁止 Markdown 摘要。".into(),
        3 => {
            if ctx
                .engine
                .is_some_and(|e| crate::agent::workflow_session::is_feedback_discuss(e))
            {
                "直接回答用户问题（引用审查报告）；若需核对代码立即 file_read，禁止重出报告。".into()
            } else if ctx.engine.is_some_and(|e| e.is_perceive_execute()) {
                "立即写审查报告 + findings JSON + ## Done，或调用 file_read；禁止空转重读叙述。".into()
            } else {
                "立即 edit_file / shell_exec 推进计划，或输出 ## Done + completion_receipt。".into()
            }
        }
        _ => "调用工具或输出最终结果，禁止空转。".into(),
    }
}

/// Handle empty tool-call responses; returns whether to end the turn.
pub fn handle_empty_response(
    ctx: &IdleContext<'_>,
    text: &str,
    idle_streak: &mut u32,
    had_validation_error: bool,
) -> IdleAction {
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
                .is_some_and(|e| crate::agent::workflow_session::is_feedback_discuss(e)) =>
            {
                "⚠️ 讨论空转 — 已暂停，请重新说明你的意见".into()
            }
            3 => "⚠️ 执行空转 — 已暂停，请重试或缩小任务范围".into(),
            _ => "⚠️ 空转过多 — 已暂停".into(),
        };
        return IdleAction::EndTurn { user_status: status };
    }
    IdleAction::Continue {
        directive: Some(format!(
            "{IDLE_HINT_TAG} 已连续 {}/{} 轮空转。{}",
            *idle_streak,
            limit,
            directive_for(ctx)
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
    }) = messages.iter().rev().find(|m| matches!(m, Message::Assistant { .. }))
    {
        if prev_tc.is_empty() && is_idle_narrative(prev) {
            if let Some(idx) = messages.iter().rposition(|m| matches!(m, Message::Assistant { .. })) {
                messages[idx] = new_msg.clone();
                return;
            }
        }
    }
    messages.push(new_msg.clone());
}

pub fn upsert_idle_hint(messages: &mut Vec<Message>, hint: &str) {
    if let Some(Message::System { content }) = messages.iter_mut().rev().find(|m| {
        matches!(m, Message::System { content } if is_idle_system_hint(content))
    }) {
        *content = hint.to_string();
    } else {
        messages.push(Message::system(hint));
    }
}

pub fn strip_idle_hints(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !matches!(m, Message::System { content } if is_idle_system_hint(content))
    });
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
    fn ends_after_streak() {
        let ctx = IdleContext {
            step_idx: 1,
            engine: None,
        };
        let mut streak = 0;
        let t = "好的，让我先看看项目结构";
        assert!(matches!(
            handle_empty_response(&ctx, t, &mut streak, false),
            IdleAction::Continue { .. }
        ));
        assert!(matches!(
            handle_empty_response(&ctx, t, &mut streak, false),
            IdleAction::EndTurn { .. }
        ));
    }
}
