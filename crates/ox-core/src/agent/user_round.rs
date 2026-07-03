//! User-round memory — segments workflow state by each user message.
//!
//! Each new user input starts a fresh round: archives the previous round's
//! request + outcome, clears ephemeral exploration state, and injects a
//! high-priority anchor so the LLM focuses on the current request only.

use serde::{Deserialize, Serialize};

use crate::agent::engine::WorkflowEngine;

pub const USER_ROUND_TAG: &str = "[USER_ROUND]";
/// Latest user message that triggered the current agent turn (may differ from session task).
pub const TURN_INPUT_TAG: &str = "[TURN_INPUT]";
pub const TURN_INPUT_KEY: &str = "_turn_user_input";
const MAX_HISTORY: usize = 8;
pub const ROUND_FINALIZED_KEY: &str = "_round_finalized";

/// Session-visible marker written when a new user round starts.
pub const ROUND_BOUNDARY_TAG: &str = "[ROUND_BOUNDARY]";
/// Session-visible marker when the user interrupts (Ctrl+C) mid-round.
pub const INTERRUPT_BOUNDARY_TAG: &str = "[INTERRUPT_BOUNDARY]";
/// Session-visible marker written when a round completes successfully (finish).
pub const COMPLETE_BOUNDARY_TAG: &str = "[ROUND_COMPLETE]";
const ROUND_INTERRUPTED_KEY: &str = "_round_interrupted";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRoundArchive {
    pub user_request: String,
    pub outcome_summary: String,
}

/// Archive outcome, clear ephemeral workflow state, mark round closed.
pub fn finalize_completed_round(engine: &mut WorkflowEngine) {
    if engine.get_variable(ROUND_FINALIZED_KEY).as_deref() == Some("1") {
        return;
    }
    if let Some(prev) = engine.get_variable("_current_user_request") {
        if !prev.trim().is_empty() {
            archive_completed_round(engine, &prev);
        }
    }
    engine.clear_ephemeral_workflow_state();
    engine.set_variable(ROUND_FINALIZED_KEY, "1".to_string());
}

/// Mark workflow suspended after Ctrl+C — keep step state for resume, drop stale tool memory.
pub fn suspend_on_interrupt(engine: &mut WorkflowEngine) -> bool {
    if engine.is_workflow_complete()
        || engine.get_variable(ROUND_FINALIZED_KEY).as_deref() == Some("1")
    {
        return false;
    }
    if engine.get_variable(ROUND_INTERRUPTED_KEY).as_deref() == Some("1") {
        return false;
    }
    engine.set_variable("_turn_memory", String::new());
    engine.clear_impl_files_read();
    engine.set_variable("_impl_files_edited", "[]".to_string());
    engine.set_variable(ROUND_INTERRUPTED_KEY, "1".to_string());
    true
}

/// On program exit: archive interrupted round into history for the next session.
pub fn finalize_interrupted_on_exit(engine: &mut WorkflowEngine) {
    if engine.is_workflow_complete()
        || engine.get_variable(ROUND_FINALIZED_KEY).as_deref() == Some("1")
    {
        return;
    }
    let interrupted = engine.get_variable(ROUND_INTERRUPTED_KEY).as_deref() == Some("1");
    if let Some(prev) = engine.get_variable("_current_user_request") {
        if !prev.trim().is_empty() && (interrupted || round_had_activity(engine)) {
            archive_interrupted_round(engine, &prev);
        }
    }
    engine.set_variable(ROUND_INTERRUPTED_KEY, String::new());
}

/// Visible session marker for an interrupted (incomplete) round.
pub fn format_interrupt_boundary_message(task: &str) -> String {
    format!(
        "{INTERRUPT_BOUNDARY_TAG}\n\
         ⏹️ **用户中断（INTERRUPTED — HISTORICAL / 未完成）**\n\
         任务: {}\n\
         ⚠️ 此轮**未正常完成**，不触发 Skill 反思。\n\
         - 继续同一任务：直接说明跟进内容\n\
         - 换新任务：用「新任务」或 /new",
        task.chars().take(1500).collect::<String>()
    )
}

pub fn is_interrupt_boundary(content: &str) -> bool {
    content.starts_with(INTERRUPT_BOUNDARY_TAG)
}

/// Visible session marker for a successfully completed round.
///
/// Symmetric to `format_interrupt_boundary_message`: without this, a completed
/// round left only a trail of tool results (e.g. `file_read` dumps) as the tail,
/// and the LLM could not tell from message history whether that work had finished
/// — so it re-explored or treated stale results as pending. This terminator makes
/// completion explicit and machine-detectable.
pub fn format_complete_boundary_message(task: &str, summary: &str) -> String {
    let task: String = task.trim().chars().take(1500).collect();
    let summary: String = summary.trim().chars().take(800).collect();
    let mut out = format!(
        "{COMPLETE_BOUNDARY_TAG}\n\
         ✅ **上一轮任务已完成并交付（COMPLETED — HISTORICAL）**\n\
         任务: {task}"
    );
    if !summary.is_empty() {
        out.push_str(&format!("\n交付摘要: {summary}"));
    }
    out.push_str(
        "\n此标记**之前**的工具输出与中间步骤均属**已完成**的历史轮次，仅供只读参考；\
         **不要**重复执行、也**不要**当作未完成的待办。新需求以本轮用户输入为准。",
    );
    out
}

pub fn is_complete_boundary(content: &str) -> bool {
    content.starts_with(COMPLETE_BOUNDARY_TAG)
}

pub fn set_turn_user_input(engine: &WorkflowEngine, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    engine.set_variable(TURN_INPUT_KEY, trimmed.to_string());
}

pub fn get_turn_user_input(engine: &WorkflowEngine) -> Option<String> {
    engine
        .get_variable(TURN_INPUT_KEY)
        .filter(|s| !s.trim().is_empty())
}

/// High-priority anchor: what the user just said this turn (overrides historical confusion).
pub fn format_turn_input_block(engine: &WorkflowEngine) -> String {
    let input = get_turn_user_input(engine).unwrap_or_default();
    format_turn_input_text(
        &input,
        engine.get_variable("_current_user_request").as_deref(),
    )
}

pub fn format_turn_input_text(input: &str, session_task: Option<&str>) -> String {
    let input = input.trim();
    if input.is_empty() {
        return String::new();
    }
    let body: String = input.chars().take(2000).collect();
    let mut parts = vec![
        TURN_INPUT_TAG.to_string(),
        format!(
            "## ✉️ 本轮用户输入（**唯一待响应内容** — 覆盖历史误解）\n\
             > {body}"
        ),
        "⚠️ **工具动作：** 以 [WORKSPACE]「本轮唯一动作」为准；本轮输入用于澄清意图、纠正历史误解。"
            .to_string(),
        "若历史 assistant 声称已做某事、或与本轮输入矛盾，**以本轮输入为准**；用户修正立即生效，不得沿用错误结论。"
            .to_string(),
    ];
    if let Some(task) = session_task {
        let task = task.trim();
        if !task.is_empty() && task != input {
            let snip: String = task.chars().take(400).collect();
            parts.push(format!(
                "（会话背景任务：「{snip}」— 仅作参考；**本轮须按上方输入执行**）"
            ));
        }
    }
    parts.join("\n\n")
}

pub fn inject_turn_input(messages: &mut Vec<crate::message::Message>, block: &str) {
    if block.is_empty() {
        return;
    }
    strip_turn_input(messages);
    // Inject as USER message (not System) so it has equal authority to old
    // user messages. A system message saying "ignore old tasks" is easily
    // overridden by visible old user messages like "fix X" or "change Y".
    messages.push(crate::message::Message::user(block));
}

pub fn strip_turn_input(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        let content = match m {
            crate::message::Message::System { content } => content,
            crate::message::Message::User { content } => content,
            _ => return true,
        };
        !content.starts_with(TURN_INPUT_TAG)
    });
}

/// Archive previous round and reset workflow for a new user message.
/// Returns `true` when a fresh round started (workflow reset + new anchor).
pub fn begin_user_round(engine: &mut WorkflowEngine, user_message: &str) -> bool {
    set_turn_user_input(engine, user_message);
    if engine.get_variable(ROUND_INTERRUPTED_KEY).as_deref() == Some("1") {
        let same_or_continue = engine
            .get_variable("_current_user_request")
            .as_ref()
            .is_some_and(|cur| {
                cur.trim() == user_message.trim()
                    || WorkflowEngine::looks_like_workflow_continuation(user_message)
            });
        if same_or_continue && !WorkflowEngine::looks_like_new_task(user_message) {
            engine.set_variable(ROUND_INTERRUPTED_KEY, String::new());
            if crate::agent::phase::on_user_message(engine, user_message).changed {
                return false;
            }
            if engine.workflow_preserves_on_user_input(user_message) {
                return false;
            }
        } else if let Some(prev) = engine.get_variable("_current_user_request") {
            if !prev.trim().is_empty() && prev.trim() != user_message.trim() {
                archive_interrupted_round(engine, &prev);
            }
            engine.set_variable(ROUND_INTERRUPTED_KEY, String::new());
        } else {
            engine.set_variable(ROUND_INTERRUPTED_KEY, String::new());
        }
    }

    if engine.is_workflow_complete()
        || engine.get_variable(ROUND_FINALIZED_KEY).as_deref() == Some("1")
    {
        if engine.reopen_execute_for_fixes(user_message) {
            engine.clear_turn_provenance();
            engine.set_variable("_current_user_request", user_message.to_string());
            return false;
        }
        if let Some(prev) = engine.get_variable("_current_user_request") {
            if !prev.trim().is_empty()
                && prev.trim() != user_message.trim()
                && engine.get_variable(ROUND_FINALIZED_KEY).as_deref() != Some("1")
            {
                archive_round(engine, &prev);
            }
        }
        engine.reset_workflow();
        engine.clear_turn_provenance();
        let intent = crate::agent::task_intent::resolve_for_round(&engine, user_message);
        engine.set_variable("_current_user_request", user_message.to_string());
        crate::agent::phase::on_round_started(&engine, intent);
        return true;
    }

    if engine.is_workflow_active()
        && !engine.is_workflow_complete()
        && crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::Implement
        && !WorkflowEngine::looks_like_new_task(user_message)
    {
        tracing::info!(
            "[WORKFLOW] Implement phase: blocked user round (not /new): {}",
            user_message.chars().take(80).collect::<String>()
        );
        return false;
    }

    if engine.workflow_preserves_on_user_input(user_message) {
        crate::agent::phase::on_user_message(engine, user_message);
        return false;
    }

    if let Some(prev) = engine.get_variable("_current_user_request") {
        if !prev.trim().is_empty() && prev.trim() != user_message.trim() {
            archive_round(engine, &prev);
        }
    }
    engine.reset_workflow();
    engine.clear_turn_provenance();
    let intent = crate::agent::task_intent::resolve_for_round(&engine, user_message);
    engine.set_variable("_current_user_request", user_message.to_string());
    crate::agent::phase::on_round_started(&engine, intent);
    true
}

/// Visible session boundary between historical chat and the current round.
pub fn format_round_boundary_message(current_task: &str) -> String {
    format!(
        "{ROUND_BOUNDARY_TAG}\n\
         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
         🎯 **本轮任务开始**（CURRENT — 唯一执行目标）\n\
         {}\n\
         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
         ⚠️ 此标记**之前**的对话、工具输出、知识库检索与持久记忆均属于**历史轮次（HISTORICAL）**。\n\
         仅供只读参考，**不得**当作本轮待办或继续执行。",
        current_task.chars().take(2000).collect::<String>()
    )
}

pub fn is_round_boundary(content: &str) -> bool {
    content.starts_with(ROUND_BOUNDARY_TAG)
}

fn archive_interrupted_round(engine: &WorkflowEngine, prev_user: &str) {
    let outcome = build_round_outcome_summary(engine);
    if outcome.is_empty() && !round_had_activity(engine) {
        return;
    }
    let body = if outcome.is_empty() {
        "（中断时无步骤记录）".to_string()
    } else {
        outcome
    };
    push_round_history(
        engine,
        prev_user,
        format!("⏹️ **用户中断（未完成）**\n\n{body}"),
    );
}

fn archive_completed_round(engine: &WorkflowEngine, prev_user: &str) {
    let outcome = build_round_outcome_summary(engine);
    if outcome.is_empty() && !round_had_activity(engine) {
        return;
    }
    let body = if outcome.is_empty() {
        "（无步骤记录）".to_string()
    } else {
        outcome
    };
    push_round_history(engine, prev_user, format!("✅ **本轮已完成**\n\n{body}"));
}

fn archive_round(engine: &WorkflowEngine, prev_user: &str) {
    let outcome = build_round_outcome_summary(engine);
    if outcome.is_empty() && !round_had_activity(engine) {
        return;
    }
    push_round_history(
        engine,
        prev_user,
        if outcome.is_empty() {
            "（未完成或无记录）".to_string()
        } else {
            outcome
        },
    );
}

fn push_round_history(engine: &WorkflowEngine, prev_user: &str, outcome_summary: String) {
    let mut history = load_round_history(engine);
    history.push(UserRoundArchive {
        user_request: prev_user.to_string(),
        outcome_summary,
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

    for (i, label, cap) in [
        ("_step3_output", "执行结果", 2200usize),
        ("_step2_output", "审阅", 1200),
        ("_step1_output", "计划", 1200),
        ("_step0_output", "意图", 800),
    ] {
        if let Some(raw) = engine.get_variable(i) {
            if raw.trim().is_empty() {
                continue;
            }
            let snippet: String = raw.chars().take(cap).collect();
            parts.push(format!("【{label}】\n{snippet}"));
        }
    }

    if let Some(tm) = engine.load_turn_memory() {
        if !tm.entries.is_empty() {
            let mut lines = vec!["【工具调用/改动】".to_string()];
            for e in tm.entries.iter().take(30) {
                lines.push(format!("  - {}({}) → {}", e.tool, e.target, e.outcome));
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

/// High-priority block: current request + historical round recap (reference only).
pub fn format_user_round_block(engine: &WorkflowEngine) -> String {
    let current = engine
        .get_variable("_current_user_request")
        .unwrap_or_default();
    if current.trim().is_empty() {
        return String::new();
    }

    let turn_input = get_turn_user_input(engine).unwrap_or_default();
    let mut parts = vec![format!("{USER_ROUND_TAG}")];
    if !turn_input.trim().is_empty() && turn_input.trim() != current.trim() {
        parts.push(format!(
            "## ✉️ 本轮用户输入（CURRENT — 优先于会话任务）\n\
             {}\n\
             ⚠️ 用户刚发送以上内容；若与下方会话任务不一致，**以本轮输入为准**。",
            turn_input.chars().take(2000).collect::<String>()
        ));
        parts.push(format!(
            "## 📋 会话任务（背景）\n\
             {}\n\
             ⚠️ 属较早轮次目标；勿与本轮输入混淆。",
            current.chars().take(1200).collect::<String>()
        ));
    } else {
        parts.push(format!(
            "## 🎯 本轮任务（CURRENT — 唯一执行目标）\n\
             {}\n\
             ⚠️ 只执行以上内容；对话历史与其它记忆中的任务均属 HISTORICAL，勿继续执行。",
            current.chars().take(2000).collect::<String>()
        ));
    }

    let history = load_round_history(engine);
    if !history.is_empty() {
        let mut hist_lines = vec!["## 📚 历史轮次（HISTORICAL — 只读参考，禁止执行）".to_string()];
        for (i, entry) in history.iter().enumerate() {
            let n = i + 1;
            let user_snip: String = entry.user_request.chars().take(500).collect();
            let out_snip: String = entry.outcome_summary.chars().take(2500).collect();
            hist_lines.push(format!(
                "### 历史 #{n}\n\
                 - 用户曾请求: {user_snip}\n\
                 - 当时结果: {out_snip}\n\
                 - 状态: 已结束，非本轮待办"
            ));
        }
        parts.push(hist_lines.join("\n\n"));
    }

    if engine.get_variable(ROUND_INTERRUPTED_KEY).as_deref() == Some("1") {
        parts.push(
            "⏹️ **本轮已中断（INTERRUPTED）** — 未完成；继续请说明跟进，换任务请用「新任务」或 /new。"
                .to_string(),
        );
    }

    let guidance = crate::agent::workflow_guidance::format_block(engine);
    if !guidance.is_empty() {
        parts.push(guidance);
        parts.push(
            "⚠️ workflow 进行中 — 上方补充说明优先；继续当前任务，勿重复已完成的工作。".to_string(),
        );
    } else if !engine.is_workflow_complete() {
        parts.push("⚠️ 本轮 workflow 已重置；上轮探索/工具记录已清空。".to_string());
    }

    parts.join("\n\n")
}

/// Minimal task anchor during Implement — findings live in [WORKSPACE].
pub fn format_impl_anchor(engine: &WorkflowEngine) -> String {
    let current = engine
        .get_variable("_current_user_request")
        .unwrap_or_default();
    if current.trim().is_empty() {
        return String::new();
    }
    let turn_input = get_turn_user_input(engine).unwrap_or_default();
    if !turn_input.trim().is_empty() && turn_input.trim() != current.trim() {
        format!(
            "{USER_ROUND_TAG}\n\
             ✉️ 本轮输入: {}\n\
             📋 会话背景: {}\n\
             ⚠️ 以本轮输入为准；findings / 进度 / 下一步见 [WORKSPACE]。",
            turn_input.chars().take(600).collect::<String>(),
            current.chars().take(300).collect::<String>()
        )
    } else {
        format!(
            "{USER_ROUND_TAG}\n\
             🎯 实施任务: {}\n\
             ⚠️ findings / 进度 / 下一步见 [WORKSPACE]；勿重复审查期探索。",
            current.chars().take(600).collect::<String>()
        )
    }
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
        use crate::agent::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};

        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine.set_variable("_current_user_request", "fix bug A".into());
        engine.set_task_intent(crate::agent::task_intent::TaskIntent::Fix);

        engine.begin_user_round("add feature B");

        let current = engine.get_variable("_current_user_request").unwrap();
        assert_eq!(current, "fix bug A");
        let history = load_round_history(&engine);
        assert_eq!(history.len(), 0);
        let guidance = crate::agent::workflow_guidance::load(&engine);
        assert_eq!(guidance.len(), 1);
        assert_eq!(guidance[0].text, "add feature B");
        assert!(engine.is_workflow_active());
    }

    #[test]
    fn complete_workflow_finalizes_round_without_duplicate_archive() {
        use crate::agent::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};

        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine.set_variable("_current_user_request", "push tag".into());
        engine.set_variable("_step3_output", "## Done\npushed".into());
        session.blocking_lock().current_step_index = 3;

        engine.complete_workflow().unwrap();

        assert_eq!(
            engine.get_variable("_round_finalized").as_deref(),
            Some("1")
        );
        assert!(engine.get_variable("_step3_output").unwrap().is_empty());
        let history = load_round_history(&engine);
        assert_eq!(history.len(), 1);
        assert!(history[0].outcome_summary.contains("本轮已完成"));

        engine.begin_user_round("完善 README");
        assert_eq!(
            engine.get_variable("_current_user_request").unwrap(),
            "完善 README"
        );
        assert_eq!(engine.get_current_step_index(), 0);
        assert_eq!(load_round_history(&engine).len(), 1);
    }

    #[test]
    fn suspend_on_interrupt_marks_round_without_finalizing() {
        use crate::agent::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};

        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine.set_variable("_current_user_request", "push tag".into());
        engine.set_variable("_step3_output", "## Done\npushed".into());
        session.blocking_lock().current_step_index = 0;

        assert!(suspend_on_interrupt(&mut engine));
        assert_eq!(
            engine.get_variable("_round_interrupted").as_deref(),
            Some("1")
        );
        assert!(!engine.get_variable("_step3_output").unwrap().is_empty());
        assert!(!suspend_on_interrupt(&mut engine));
    }

    #[test]
    fn round_boundary_message_tags_current_vs_historical() {
        let msg = format_round_boundary_message("完善 README");
        assert!(msg.contains(ROUND_BOUNDARY_TAG));
        assert!(msg.contains("CURRENT"));
        assert!(msg.contains("HISTORICAL"));
        assert!(msg.contains("完善 README"));
    }

    #[test]
    fn complete_boundary_marks_done_and_is_detectable() {
        let msg = format_complete_boundary_message("审查 Foo.java", "修复了空指针，新增 3 个测试");
        assert!(is_complete_boundary(&msg));
        assert!(msg.contains("COMPLETED"));
        assert!(msg.contains("审查 Foo.java"));
        assert!(msg.contains("修复了空指针"));
        // Must not be mistaken for the other two boundary kinds.
        assert!(!is_round_boundary(&msg));
        assert!(!is_interrupt_boundary(&msg));
    }

    #[test]
    fn complete_boundary_without_summary_still_valid() {
        let msg = format_complete_boundary_message("修复登录 bug", "");
        assert!(is_complete_boundary(&msg));
        assert!(!msg.contains("交付摘要"));
    }

    #[test]
    fn turn_input_overrides_session_task_in_block() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(Arc::clone(&session));
        engine.set_variable("_current_user_request", "审查 Foo.java".into());
        set_turn_user_input(&engine, "先修复");
        let block = format_turn_input_block(&engine);
        assert!(block.contains(TURN_INPUT_TAG));
        assert!(block.contains("先修复"));
        assert!(block.contains("审查 Foo"));
        assert!(block.contains("以本轮输入为准"));
    }

    #[test]
    fn begin_user_round_sets_turn_input() {
        use crate::agent::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};

        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine.set_variable("_current_user_request", "审查".into());
        engine.begin_user_round("先修复");
        assert_eq!(get_turn_user_input(&engine).as_deref(), Some("先修复"));
        assert_eq!(
            engine.get_variable("_current_user_request").as_deref(),
            Some("审查")
        );
    }
}
