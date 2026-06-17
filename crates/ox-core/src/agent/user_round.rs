//! User-round memory — segments workflow state by each user message.
//!
//! Each new user input starts a fresh round: archives the previous round's
//! request + outcome, clears ephemeral exploration state, and injects a
//! high-priority anchor so the LLM focuses on the current request only.

use serde::{Deserialize, Serialize};

use crate::agent::engine::WorkflowEngine;

pub const USER_ROUND_TAG: &str = "[USER_ROUND]";
const MAX_HISTORY: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRoundArchive {
    pub user_request: String,
    pub outcome_summary: String,
}

/// Archive previous round and reset workflow for a new user message.
pub fn begin_user_round(engine: &mut WorkflowEngine, user_message: &str) {
    if engine.is_workflow_parked() {
        if WorkflowEngine::looks_like_new_task(user_message)
            || crate::agent::requirement_clarification::is_explicit_parked_new_task(user_message)
        {
            let _ = engine.finish_workflow_session();
            // fall through to full new round below
        } else if engine.is_park_disambiguation_awaiting() {
            return;
        } else {
            crate::agent::requirement_clarification::arm_park_follow_up_menu(engine);
            return;
        }
    }

    if engine.is_workflow_active()
        && !engine.is_workflow_complete()
        && crate::agent::workflow_phases::get_phase(engine)
            == crate::agent::workflow_phases::WorkflowPhase::Act
        && !WorkflowEngine::looks_like_new_task(user_message)
    {
        tracing::info!(
            "[WORKFLOW] Act phase: blocked user round (not park resume /new): {}",
            user_message.chars().take(80).collect::<String>()
        );
        return;
    }

    if engine.workflow_preserves_on_user_input() {
        crate::agent::workflow_guidance::append(engine, user_message);
        return;
    }

    if let Some(prev) = engine.get_variable("_current_user_request") {
        if !prev.trim().is_empty() && prev.trim() != user_message.trim() {
            archive_round(engine, &prev);
        }
    }
    engine.reset_workflow();
    engine.set_variable("_current_user_request", user_message.to_string());
}

fn archive_round(engine: &WorkflowEngine, prev_user: &str) {
    let outcome = build_round_outcome_summary(engine);
    if outcome.is_empty() && !round_had_activity(engine) {
        return;
    }
    let mut history = load_round_history(engine);
    history.push(UserRoundArchive {
        user_request: prev_user.to_string(),
        outcome_summary: if outcome.is_empty() {
            "（未完成或无记录）".to_string()
        } else {
            outcome
        },
    });
    while history.len() > MAX_HISTORY {
        history.remove(0);
    }
    if let Ok(json) = serde_json::to_string(&history) {
        engine.set_variable("_round_history", json);
    }
}

fn round_had_activity(engine: &WorkflowEngine) -> bool {
    engine.get_current_step_index() > 0
        || engine
            .load_turn_memory()
            .map(|tm| !tm.entries.is_empty())
            .unwrap_or(false)
        || engine
            .get_variable("_step1_output")
            .is_some_and(|s| !s.is_empty())
}

pub fn build_round_outcome_summary(engine: &WorkflowEngine) -> String {
    let mut parts = Vec::new();

    if let Some(reply) = engine.get_variable("_chat_reply") {
        if !reply.trim().is_empty() {
            let snippet: String = reply.chars().take(1200).collect();
            parts.push(format!("【回复】\n{snippet}"));
        }
    }

    for (i, label) in [
        ("_step3_output", "执行结果"),
        ("_step2_output", "审阅"),
        ("_step1_output", "计划"),
        ("_step0_output", "意图"),
    ] {
        if let Some(raw) = engine.get_variable(i) {
            if raw.trim().is_empty() {
                continue;
            }
            let snippet: String = raw.chars().take(1200).collect();
            parts.push(format!("【{label}】\n{snippet}"));
        }
    }

    if let Some(tm) = engine.load_turn_memory() {
        if !tm.entries.is_empty() {
            let mut lines = vec!["【工具调用】".to_string()];
            for e in tm.entries.iter().take(20) {
                lines.push(format!(
                    "  - {}({}) → {}",
                    e.tool, e.target, e.outcome
                ));
            }
            parts.push(lines.join("\n"));
        }
    }

    parts.join("\n\n")
}

pub fn load_round_history(engine: &WorkflowEngine) -> Vec<UserRoundArchive> {
    engine
        .get_variable("_round_history")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// High-priority block: current request + last round recap (reference only).
pub fn format_user_round_block(engine: &WorkflowEngine) -> String {
    let current = engine
        .get_variable("_current_user_request")
        .unwrap_or_default();
    if current.trim().is_empty() {
        return String::new();
    }

    let mut parts = vec![
        format!("{USER_ROUND_TAG}"),
        format!(
            "📥 **本轮用户请求**（只处理这一条，勿执行上轮遗留任务）:\n{}",
            current.chars().take(2000).collect::<String>()
        ),
    ];

    let history = load_round_history(engine);
    if let Some(last) = history.last() {
        let user_snip: String = last.user_request.chars().take(500).collect();
        let out_snip: String = last.outcome_summary.chars().take(1500).collect();
        parts.push(format!(
            "📤 **上轮回顾**（已完成，仅供参考 — 勿重复上轮工具调用）:\n\
             用户: {user_snip}\n\
             你做了: {out_snip}"
        ));
    }

    if history.len() > 1 {
        parts.push(format!(
            "（更早 {} 轮记录已省略）",
            history.len() - 1
        ));
    }

    let guidance = crate::agent::workflow_guidance::format_block(engine);
    if engine.is_awaiting_clarification() {
        if engine.is_park_disambiguation_awaiting() {
            parts.push(
                "⏸️ **请选择下一步** — 输入 **1/继续**（执行修复）、**2/意见**（澄清审查结论）、**3/新任务**。"
                    .to_string(),
            );
        } else {
            parts.push(
                "❓ **等待需求澄清** — 请回答 Intent 阶段提出的问题后继续。".to_string(),
            );
        }
    } else if engine.is_workflow_parked() {
        parts.push(
            "⏸️ **任务会话已暂停** — 请先选 1继续 / 2意见 / 3新任务，或用「意见：…」「继续：…」一次说完。"
                .to_string(),
        );
        if !guidance.is_empty() {
            parts.push(guidance);
        }
    } else if !guidance.is_empty() {
        parts.push(guidance);
        parts.push(
            "⚠️ workflow 进行中 — 上方补充说明优先；继续**当前步骤**，勿从 Intent 重来。"
                .to_string(),
        );
    } else {
        parts.push(
            "⚠️ 本轮 workflow 已从 Intent 重新开始；上轮探索/工具记录已清空。"
                .to_string(),
        );
    }

    parts.join("\n\n")
}

pub fn inject_user_round(messages: &mut Vec<crate::message::Message>, block: &str) {
    if block.is_empty() {
        return;
    }
    strip_user_round(messages);
    messages.push(crate::message::Message::system(block));
}

pub fn strip_user_round(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        !matches!(m, crate::message::Message::System { content } if content.starts_with(USER_ROUND_TAG))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn begin_user_round_preserves_mid_workflow() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.set_variable("_current_user_request", "fix bug A".into());
        engine.set_variable("_step0_output", r#"{"intent":"coding"}"#.into());
        engine.set_variable("_step1_output", r#"{"plan":[]}"#.into());
        let _ = engine.advance_to_step(Some(1));

        engine.begin_user_round("add feature B");

        let current = engine.get_variable("_current_user_request").unwrap();
        assert_eq!(current, "fix bug A");
        let history = load_round_history(&engine);
        assert_eq!(history.len(), 0);
        let guidance = crate::agent::workflow_guidance::load(&engine);
        assert_eq!(guidance.len(), 1);
        assert_eq!(guidance[0].text, "add feature B");
        assert!(!engine.get_variable("_step1_output").unwrap().is_empty());
    }

    #[test]
    fn begin_user_round_parked_arms_menu() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        crate::agent::workflow_session::park(&engine);

        engine.begin_user_round("问题3其实不是问题，怎么处理？");

        assert!(engine.is_park_disambiguation_awaiting());
        assert!(crate::agent::workflow_session::is_parked(&engine));
    }

    #[test]
    fn begin_user_round_parked_feedback_via_menu() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        crate::agent::workflow_session::park(&engine);
        crate::agent::requirement_clarification::arm_park_follow_up_menu(&engine);

        let outcome = crate::agent::requirement_clarification::resolve_park_follow_up(
            &engine,
            "意见：envConfig 有默认值",
        )
        .unwrap();
        assert!(matches!(
            outcome,
            crate::agent::requirement_clarification::ParkFollowUpOutcome::Resolved(
                crate::agent::requirement_clarification::ParkDisambiguationResolution::Feedback { .. }
            )
        ));
    }
}