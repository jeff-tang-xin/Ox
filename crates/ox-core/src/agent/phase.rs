//! Single-flow phase state machine — Review → AwaitUser → Implement → Complete.
//!
//! All phase changes go through [`transition`]. [WORKSPACE] reads [`get`] only.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;
use super::findings;
use super::task_intent::{self, TaskIntent};
use super::workspace::{RequiredAction, WorkspaceMode, WorkflowWorkspace};

pub const PHASE_STATE_KEY: &str = "_workflow_phase";
/// Legacy flag — set when entering Implement; kept for session helpers.
pub const FIX_PIVOT_KEY: &str = "_fix_pivot";
pub const PHASE_TAG: &str = "[PHASE]";

/// Canonical single-flow phase (persisted in session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SingleFlowPhase {
    #[default]
    Receive,
    Review,
    AwaitUser,
    Implement,
    Complete,
}

impl SingleFlowPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Receive => "receive",
            Self::Review => "review",
            Self::AwaitUser => "await_user",
            Self::Implement => "implement",
            Self::Complete => "complete",
        }
    }

    pub fn from_stored(s: &str) -> Self {
        match s.trim() {
            "review" => Self::Review,
            "await_user" => Self::AwaitUser,
            "implement" => Self::Implement,
            "complete" => Self::Complete,
            _ => Self::Receive,
        }
    }
}

/// Events that may change phase — the only legal entry points.
#[derive(Debug, Clone)]
pub enum PhaseEvent {
    /// New user round after workflow reset.
    RoundStarted { intent: TaskIntent },
    /// Mid-turn or follow-up user text (fix / guidance).
    UserMessage { text: String },
    /// Review report prose delivered (read-only phase may end exploration).
    ReviewReportDelivered,
    /// Findings JSON stored from review output.
    FindingsStored,
    /// ## Done passed all gates.
    DoneGatePassed {
        had_completion_receipt: bool,
    },
    /// /fix 1,2 scope selection.
    ScopeSelected,
    /// Workflow reset (new task).
    WorkflowReset,
    /// Re-open after workflow complete for fix continuation.
    ReopenForFix { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionResult {
    pub changed: bool,
    pub before: SingleFlowPhase,
    pub phase: SingleFlowPhase,
    pub note: Option<String>,
}

pub const PHASE_SWITCH_TAG: &str = "[PHASE_SWITCH]";
/// Injected each LLM iteration while scope-confirm gate is active (AwaitUser + findings).
pub const SCOPE_GATE_TAG: &str = "[SCOPE_GATE]";
const PHASE_TRANSITION_NOTICE_KEY: &str = "_phase_transition_notice";
const PHASE_USER_BANNER_KEY: &str = "_phase_user_banner";

// ── Read ─────────────────────────────────────────────────────────────

pub fn get(engine: &WorkflowEngine) -> SingleFlowPhase {
    engine
        .get_variable(PHASE_STATE_KEY)
        .map(|s| SingleFlowPhase::from_stored(&s))
        .unwrap_or(SingleFlowPhase::Receive)
}

fn persist(engine: &WorkflowEngine, before: SingleFlowPhase, after: SingleFlowPhase) {
    if before != after {
        engine.set_variable("_phase_prev", before.as_str().to_string());
    }
    engine.set_variable(PHASE_STATE_KEY, after.as_str().to_string());
}

/// Map canonical phase → WORKSPACE mode (single source for workspace.rs).
pub fn workspace_mode(engine: &WorkflowEngine) -> WorkspaceMode {
    if crate::agent::workflow_session::is_feedback_discuss(engine) {
        return WorkspaceMode::FeedbackDiscuss;
    }
    match get(engine) {
        SingleFlowPhase::Implement => WorkspaceMode::ExecuteImpl,
        SingleFlowPhase::AwaitUser if has_findings(engine) => WorkspaceMode::ScopeConfirm,
        SingleFlowPhase::Receive | SingleFlowPhase::Review | SingleFlowPhase::AwaitUser => {
            WorkspaceMode::ExecuteReview
        }
        SingleFlowPhase::Complete => {
            if fix_impl_session(engine) {
                WorkspaceMode::ExecuteImpl
            } else {
                WorkspaceMode::ExecuteReview
            }
        }
    }
}

pub fn should_inject_workspace(engine: &WorkflowEngine) -> bool {
    if !engine.is_workflow_active() {
        return false;
    }
    if !engine.is_workflow_complete() {
        return get(engine) != SingleFlowPhase::Complete;
    }
    fix_impl_session(engine)
}

pub fn fix_impl_session(engine: &WorkflowEngine) -> bool {
    matches!(get(engine), SingleFlowPhase::Implement)
}

/// Scope-confirm gate: findings stored, tools blocked, same ReAct session suspended.
pub fn is_scope_gate_active(engine: &WorkflowEngine) -> bool {
    super::business_gate::is_pending_scope(engine)
        && !crate::agent::workflow_session::is_feedback_discuss(engine)
}

/// Per-iteration directive while [`is_scope_gate_active`].
pub fn format_scope_gate_directive(engine: &WorkflowEngine) -> Option<String> {
    if !is_scope_gate_active(engine) {
        return None;
    }
    let scope = findings::load_or_migrate(engine)
        .map(|s| s.scope_confirm_summary())
        .unwrap_or_default();
    let mut body = format!(
        "{SCOPE_GATE_TAG}\n\
         ⏸ **范围确认门禁**（同一会话挂起 — 非新对话）\n\n\
         findings 已入库；runtime 已阻塞工具，等待用户在面板确认。\n\n\
         **此刻只允许：**\n\
         • 用户讨论 → 纯文字回应（引用上方 findings，解答疑问）\n\
         **此刻禁止：**\n\
         • 一切工具调用\n\
         • 重出 findings JSON、审查报告、## Done\n\n\
         用户 c /confirm 后系统注入 [PHASE_SWITCH] 切入实施；\
         上方审查结论与 findings **仍然有效**，实施时勿重出报告。"
    );
    if !scope.is_empty() {
        body.push_str(&format!("\n\n**面板范围：**\n{scope}"));
    }
    Some(body)
}

pub fn strip_scope_gate(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        !matches!(
            m,
            crate::message::Message::System { content }
                if content.starts_with(SCOPE_GATE_TAG)
        )
    });
}

// ── Transition ───────────────────────────────────────────────────────

pub fn transition(engine: &WorkflowEngine, event: PhaseEvent) -> TransitionResult {
    let before = get(engine);
    let after = apply_event(engine, before, &event);
    if after != before {
        persist(engine, before, after);
        tracing::info!(
            "[PHASE] {:?} → {} ({:?})",
            before,
            after.as_str(),
            std::mem::discriminant(&event)
        );
    }
    let note = side_effects(engine, before, after, &event);
    let result = TransitionResult {
        changed: after != before,
        before,
        phase: after,
        note,
    };
    if result.changed {
        arm_transition_notice(engine, &result);
    }
    result
}

fn apply_event(
    engine: &WorkflowEngine,
    current: SingleFlowPhase,
    event: &PhaseEvent,
) -> SingleFlowPhase {
    match event {
        PhaseEvent::WorkflowReset => SingleFlowPhase::Receive,
        PhaseEvent::RoundStarted { intent } => phase_for_round_start(engine, *intent),
        PhaseEvent::ReviewReportDelivered | PhaseEvent::FindingsStored => {
            if has_findings(engine) && matches!(current, SingleFlowPhase::Receive | SingleFlowPhase::Review) {
                SingleFlowPhase::AwaitUser
            } else {
                current
            }
        }
        PhaseEvent::UserMessage { text } | PhaseEvent::ReopenForFix { text } => {
            if can_enter_implement(engine, text) {
                SingleFlowPhase::Implement
            } else {
                current
            }
        }
        PhaseEvent::ScopeSelected => {
            if has_findings(engine) {
                SingleFlowPhase::Implement
            } else {
                current
            }
        }
        PhaseEvent::DoneGatePassed {
            had_completion_receipt,
        } => {
            if *had_completion_receipt {
                SingleFlowPhase::Complete
            } else if current == SingleFlowPhase::Implement {
                // Implement ## Done without completion_receipt must not complete the workflow.
                current
            } else if has_findings(engine) {
                SingleFlowPhase::AwaitUser
            } else {
                SingleFlowPhase::Complete
            }
        }
    }
}

fn phase_for_round_start(engine: &WorkflowEngine, intent: TaskIntent) -> SingleFlowPhase {
    match intent {
        TaskIntent::Fix => {
            if has_findings(engine)
                || task_intent::looks_like_greenfield_impl(&user_request(engine))
            {
                SingleFlowPhase::Implement
            } else {
                SingleFlowPhase::Review
            }
        }
        TaskIntent::Review | TaskIntent::Qa => SingleFlowPhase::Review,
        TaskIntent::General => SingleFlowPhase::Receive,
    }
}

fn side_effects(
    engine: &WorkflowEngine,
    before: SingleFlowPhase,
    after: SingleFlowPhase,
    event: &PhaseEvent,
) -> Option<String> {
    match event {
        PhaseEvent::UserMessage { text } | PhaseEvent::ReopenForFix { text } => {
            if after == SingleFlowPhase::Implement && before != SingleFlowPhase::Implement {
                enter_implement(engine, text);
                Some("进入实施阶段".into())
            } else if after == before {
                crate::agent::workflow_guidance::append(engine, text);
                None
            } else {
                None
            }
        }
        PhaseEvent::RoundStarted { intent } => {
            engine.set_task_intent(*intent);
            if after == SingleFlowPhase::Implement {
                enter_implement(engine, &user_request(engine));
            }
            None
        }
        PhaseEvent::ScopeSelected => {
            if after == SingleFlowPhase::Implement && before != SingleFlowPhase::Implement {
                engine.set_task_intent(TaskIntent::Fix);
                enter_implement(engine, "/fix scope");
                Some("已选范围，进入实施".into())
            } else {
                None
            }
        }
        PhaseEvent::WorkflowReset => {
            engine.set_variable(FIX_PIVOT_KEY, String::new());
            None
        }
        PhaseEvent::ReviewReportDelivered => {
            engine.mark_execute_report_delivered();
            None
        }
        PhaseEvent::DoneGatePassed { .. } => None,
        PhaseEvent::FindingsStored => None,
    }
}

fn enter_implement(engine: &WorkflowEngine, user_text: &str) {
    crate::agent::workflow_session::clear_feedback_discuss(engine);
    engine.clear_turn_memory();
    engine.clear_turn_provenance();
    engine.clear_impl_files_read();
    engine.set_task_intent(TaskIntent::Fix);
    engine.set_variable(FIX_PIVOT_KEY, "1".to_string());
    if let Some(mut store) = findings::load_or_migrate(engine) {
        if store.active_indices.is_empty() {
            let indices: Vec<u32> = store.open_findings().iter().map(|f| f.index).collect();
            if !indices.is_empty() {
                store.set_scope(&indices);
                findings::save(engine, &store);
            }
        }
    }
    if engine.is_workflow_complete() {
        engine.reset_step_for_fix_reopen();
    }
    crate::agent::workflow_session::enter_implementation_phase(engine);
    crate::agent::workflow_phases::set_phase(
        engine,
        crate::agent::workflow_phases::WorkflowPhase::Act,
    );
    engine.sync_plan_from_findings();
    if !user_text.trim().is_empty() {
        crate::agent::workflow_guidance::append(engine, user_text);
    }
}

fn has_findings(engine: &WorkflowEngine) -> bool {
    findings::load_or_migrate(engine)
        .map(|s| !s.findings.is_empty())
        .unwrap_or(false)
}

fn user_request(engine: &WorkflowEngine) -> String {
    engine
        .get_variable("_current_user_request")
        .unwrap_or_default()
}

fn can_enter_implement(engine: &WorkflowEngine, user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    if !task_intent::looks_like_greenfield_impl(t)
        && !crate::agent::workflow_session::looks_like_fix_continuation(t)
        && !t.starts_with("/fix")
    {
        return false;
    }
    let greenfield = task_intent::looks_like_greenfield_impl(t);
    let verify_failed = crate::agent::post_edit_verification::verify_status_failed(engine);
    has_findings(engine) || greenfield || verify_failed
}

// ── Legacy / convenience API ─────────────────────────────────────────

pub fn can_pivot_to_fix(engine: &WorkflowEngine, user_text: &str) -> bool {
    can_enter_implement(engine, user_text)
}

pub fn pivot_to_fix_mode(engine: &WorkflowEngine, user_text: &str) -> bool {
    if !can_enter_implement(engine, user_text) {
        return false;
    }
    if get(engine) == SingleFlowPhase::Implement {
        crate::agent::workflow_guidance::append(engine, user_text);
        return true;
    }
    let event = if engine.is_workflow_complete() {
        PhaseEvent::ReopenForFix {
            text: user_text.to_string(),
        }
    } else {
        PhaseEvent::UserMessage {
            text: user_text.to_string(),
        }
    };
    transition(engine, event).changed
        || get(engine) == SingleFlowPhase::Implement
}

pub fn on_round_started(engine: &WorkflowEngine, intent: TaskIntent) {
    transition(engine, PhaseEvent::RoundStarted { intent });
}

pub fn on_workflow_reset(engine: &WorkflowEngine) {
    transition(engine, PhaseEvent::WorkflowReset);
}

pub fn on_review_report_delivered(engine: &WorkflowEngine) {
    transition(engine, PhaseEvent::ReviewReportDelivered);
}

pub fn on_findings_stored(engine: &WorkflowEngine) {
    transition(engine, PhaseEvent::FindingsStored);
}

pub fn on_done_gate_passed(engine: &WorkflowEngine, had_completion_receipt: bool) {
    transition(
        engine,
        PhaseEvent::DoneGatePassed {
            had_completion_receipt,
        },
    );
}

pub fn on_user_message(engine: &WorkflowEngine, text: &str) -> TransitionResult {
    if can_enter_implement(engine, text) {
        let event = if engine.is_workflow_complete() {
            PhaseEvent::ReopenForFix {
                text: text.to_string(),
            }
        } else {
            PhaseEvent::UserMessage {
                text: text.to_string(),
            }
        };
        transition(engine, event)
    } else {
        crate::agent::workflow_guidance::append(engine, text);
        TransitionResult {
            changed: false,
            before: get(engine),
            phase: get(engine),
            note: None,
        }
    }
}

pub fn on_scope_selected(engine: &WorkflowEngine) {
    transition(engine, PhaseEvent::ScopeSelected);
}

/// User-visible banner for the output pane (on phase change).
pub fn user_transition_banner(
    before: SingleFlowPhase,
    after: SingleFlowPhase,
    engine: &WorkflowEngine,
) -> String {
    if before == after {
        return String::new();
    }
    let action = WorkflowWorkspace::build(engine)
        .map(|ws| format_required_action_short(&ws.required_action))
        .unwrap_or_default();
    let mut lines = vec![
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string(),
        format!(
            "🔄 **阶段切换：** {} → {}",
            phase_display_label(before),
            phase_display_label(after)
        ),
    ];
    lines.push(transition_hint(before, after));
    if !action.is_empty() {
        lines.push(format!("**下一步：** {action}"));
    }
    lines.push("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());
    lines.join("\n")
}

/// LLM directive injected once after a phase change (before [WORKSPACE]).
pub fn llm_transition_directive(before: SingleFlowPhase, after: SingleFlowPhase) -> String {
    if before == after {
        return String::new();
    }
    format!(
        "{PHASE_SWITCH_TAG}\n\
         阶段 **{} → {}**（同一会话继续，非新对话）。上一阶段规则作废。\n\
         {}\n\
         请立即阅读下方 [WORKSPACE]「本轮唯一动作」并严格执行。",
        phase_display_label(before),
        phase_display_label(after),
        transition_hint(before, after),
    )
}

pub fn arm_transition_notice(engine: &WorkflowEngine, result: &TransitionResult) {
    if !result.changed {
        return;
    }
    let llm = llm_transition_directive(result.before, result.phase);
    if !llm.is_empty() {
        engine.set_variable(PHASE_TRANSITION_NOTICE_KEY, llm);
    }
    let banner = user_transition_banner(result.before, result.phase, engine);
    if !banner.is_empty() {
        engine.set_variable(PHASE_USER_BANNER_KEY, banner);
    }
}

pub fn take_pending_user_banner(engine: &WorkflowEngine) -> String {
    let banner = engine
        .get_variable(PHASE_USER_BANNER_KEY)
        .unwrap_or_default();
    if !banner.is_empty() {
        engine.set_variable(PHASE_USER_BANNER_KEY, String::new());
    }
    banner
}

pub fn consume_transition_notice(engine: &WorkflowEngine) -> Option<String> {
    let msg = engine
        .get_variable(PHASE_TRANSITION_NOTICE_KEY)
        .filter(|s| !s.trim().is_empty())?;
    engine.set_variable(PHASE_TRANSITION_NOTICE_KEY, String::new());
    Some(msg)
}

fn transition_hint(before: SingleFlowPhase, after: SingleFlowPhase) -> String {
    match (before, after) {
        (_, SingleFlowPhase::Review) => {
            "• 只读审查：可 file_read / code_search，禁止 edit".to_string()
        }
        (_, SingleFlowPhase::AwaitUser) => {
            "• **门禁暂停**（同一会话）— 禁止一切工具\n\
             • 等待用户在面板选范围并按 c /confirm；或直接文字讨论 findings\n\
             • 确认后 [PHASE_SWITCH] 切入实施，勿重出审查报告".to_string()
        }
        (_, SingleFlowPhase::Implement) => {
            "• **实施阶段**（接续审查 findings）— 按 [WORKSPACE] 逐项 edit_file，禁止 code_search".to_string()
        }
        (_, SingleFlowPhase::Complete) => "• 任务完成 — 可开始新需求".to_string(),
        (SingleFlowPhase::Complete, SingleFlowPhase::Implement) => {
            "• 继续修复 — 按 [WORKSPACE] 执行".to_string()
        }
        _ => format!(
            "• 从 {} 进入 {}",
            phase_display_label(before),
            phase_display_label(after)
        ),
    }
}

fn format_required_action_short(action: &RequiredAction) -> String {
    match action {
        RequiredAction::Explore { hint } => format!("探索 — {hint}"),
        RequiredAction::ReadFile {
            path,
            finding_index,
            ..
        } => format!("file_read finding #{finding_index}: `{path}`"),
        RequiredAction::EditFile {
            path,
            finding_index,
        } => format!("edit_file finding #{finding_index}: `{path}`"),
        RequiredAction::Verify {
            command,
            finding_index,
        } => format!("verify finding #{finding_index}: `{command}`"),
        RequiredAction::EmitFindingsAndDone => "产出审查报告 + findings + ## Done".into(),
        RequiredAction::EmitCompletionReceipt => "completion_receipt + ## Done".into(),
        RequiredAction::AwaitUser => "等待用户（禁止工具）".into(),
        RequiredAction::DiscussOnly => "讨论（禁止工具）".into(),
    }
}

/// One-line phase directive — complements [WORKSPACE].
pub fn format_directive(engine: &WorkflowEngine) -> Option<String> {
    let phase = get(engine);
    let ws = WorkflowWorkspace::build(engine)?;
    let action = match &ws.required_action {
        RequiredAction::Explore { hint } => format!("探索 — {hint}"),
        RequiredAction::ReadFile {
            path,
            finding_index,
            ..
        } => format!("读取 finding #{finding_index}: `{path}`（每文件仅一次）"),
        RequiredAction::EditFile {
            path,
            finding_index,
        } => format!("编辑 finding #{finding_index}: `{path}` — 立即 edit_file"),
        RequiredAction::Verify {
            command,
            finding_index,
        } => format!("验证 finding #{finding_index}: `{command}`"),
        RequiredAction::EmitFindingsAndDone => "产出审查报告 + findings JSON + ## Done".into(),
        RequiredAction::EmitCompletionReceipt => {
            "全部 finding 已处理 — 输出 completion_receipt + ## Done".into()
        }
        RequiredAction::AwaitUser => "门禁暂停 — 禁止工具；等待用户确认范围或讨论".into(),
        RequiredAction::DiscussOnly => "讨论模式 — 直接回应，勿重出报告".into(),
    };

    let mode_label = phase_display_label(phase);

    Some(format!(
        "{PHASE_TAG}\n阶段: {mode_label} | 下一步: {action}"
    ))
}

/// Build UI mode line + consume pending user banner (if any transition since last read).
pub fn workspace_mode_event(engine: &WorkflowEngine) -> (String, String) {
    (
        workspace_status_line(engine),
        take_pending_user_banner(engine),
    )
}

/// Human-readable phase for UI / status line.
pub fn phase_display_label(phase: SingleFlowPhase) -> &'static str {
    match phase {
        SingleFlowPhase::Receive => "接单",
        SingleFlowPhase::Review => "审查",
        SingleFlowPhase::AwaitUser => "待用户",
        SingleFlowPhase::Implement => "实施",
        SingleFlowPhase::Complete => "完成",
    }
}

/// Combined phase + workspace mode for terminal status bar.
pub fn workspace_status_line(engine: &WorkflowEngine) -> String {
    format!(
        "阶段: {} | 模式: {}",
        phase_display_label(get(engine)),
        workspace_mode_label(engine)
    )
}

pub fn workspace_mode_label(engine: &WorkflowEngine) -> String {
    match workspace_mode(engine) {
        WorkspaceMode::ExecuteReview => "execute_review",
        WorkspaceMode::ExecuteImpl => "execute_impl",
        WorkspaceMode::ScopeConfirm => "scope_confirm",
        WorkspaceMode::FeedbackDiscuss => "feedback_discuss",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::findings::{Finding, FindingsStore, FindingStatus};
    use crate::agent::session::SessionState;
    use crate::agent::workflow::{create_default_workflow, DEFAULT_WORKFLOW_ID};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn engine_with_findings() -> WorkflowEngine {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        engine.set_task_intent(TaskIntent::Review);
        persist(
            &engine,
            SingleFlowPhase::Review,
            SingleFlowPhase::AwaitUser,
        );
        engine.mark_execute_report_delivered();
        let store = FindingsStore {
            summary: "2 issues".into(),
            findings: vec![Finding {
                index: 1,
                severity: crate::agent::findings::Severity::High,
                file: "src/Foo.java".into(),
                symbol: "bar".into(),
                issue: "bug".into(),
                recommendation: "fix".into(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![1],
        };
        findings::save(&engine, &store);
        engine
    }

    #[test]
    fn user_fix_transitions_to_implement() {
        let engine = engine_with_findings();
        let r = transition(
            &engine,
            PhaseEvent::UserMessage {
                text: "先修复".into(),
            },
        );
        assert!(r.changed);
        assert_eq!(get(&engine), SingleFlowPhase::Implement);
        let ws = WorkflowWorkspace::build(&engine).unwrap();
        assert_eq!(ws.mode, WorkspaceMode::ExecuteImpl);
    }

    #[test]
    fn review_done_with_findings_awaits_user() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let mut engine = WorkflowEngine::new(Arc::clone(&session));
        engine.register_workflow(create_default_workflow());
        engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        persist(
            &engine,
            SingleFlowPhase::Receive,
            SingleFlowPhase::Review,
        );
        let store = FindingsStore {
            summary: "x".into(),
            findings: vec![Finding {
                index: 1,
                severity: crate::agent::findings::Severity::Medium,
                file: "a.rs".into(),
                symbol: String::new(),
                issue: "i".into(),
                recommendation: String::new(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            }],
            active_indices: vec![],
        };
        findings::save(&engine, &store);
        transition(&engine, PhaseEvent::DoneGatePassed {
            had_completion_receipt: false,
        });
        assert_eq!(get(&engine), SingleFlowPhase::AwaitUser);
    }

    #[test]
    fn cannot_pivot_without_findings() {
        let session = Arc::new(Mutex::new(SessionState::new("t")));
        let engine = WorkflowEngine::new(session);
        assert!(!can_pivot_to_fix(&engine, "先修复"));
    }

    #[test]
    fn round_start_fix_with_findings_is_implement() {
        let engine = engine_with_findings();
        transition(&engine, PhaseEvent::WorkflowReset);
        transition(
            &engine,
            PhaseEvent::RoundStarted {
                intent: TaskIntent::Fix,
            },
        );
        assert_eq!(get(&engine), SingleFlowPhase::Implement);
    }

    #[test]
    fn await_user_with_findings_maps_to_scope_confirm() {
        let engine = engine_with_findings();
        assert_eq!(workspace_mode(&engine), WorkspaceMode::ScopeConfirm);
        let ws = WorkflowWorkspace::build(&engine).unwrap();
        assert_eq!(ws.mode, WorkspaceMode::ScopeConfirm);
    }

    #[test]
    fn scope_gate_directive_active_only_during_await_user() {
        let engine = engine_with_findings();
        assert!(is_scope_gate_active(&engine));
        let gate = format_scope_gate_directive(&engine).unwrap();
        assert!(gate.starts_with(SCOPE_GATE_TAG));
        assert!(gate.contains("禁止"));
        assert!(gate.contains("PHASE_SWITCH"));
        pivot_to_fix_mode(&engine, "先修复 finding #1");
        assert_eq!(get(&engine), SingleFlowPhase::Implement);
        assert!(!is_scope_gate_active(&engine));
        assert!(format_scope_gate_directive(&engine).is_none());
    }

    #[test]
    fn scope_gate_suppressed_in_feedback_discuss() {
        let engine = engine_with_findings();
        crate::agent::workflow_session::enter_feedback_discuss(&engine);
        assert!(!is_scope_gate_active(&engine));
        assert!(format_scope_gate_directive(&engine).is_none());
    }

    #[test]
    fn impl_done_without_receipt_stays_implement() {
        let engine = engine_with_findings();
        pivot_to_fix_mode(&engine, "先修复");
        transition(
            &engine,
            PhaseEvent::DoneGatePassed {
                had_completion_receipt: false,
            },
        );
        assert_eq!(get(&engine), SingleFlowPhase::Implement);
    }

    #[test]
    fn pivot_auto_selects_all_open_findings() {
        let engine = engine_with_findings();
        let store = findings::load_or_migrate(&engine).unwrap();
        assert_eq!(store.active_indices, vec![1]);
        let session = Arc::new(Mutex::new(SessionState::new("t2")));
        let mut engine2 = WorkflowEngine::new(Arc::clone(&session));
        engine2.register_workflow(create_default_workflow());
        engine2.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
        let mut store2 = FindingsStore {
            summary: "s".into(),
            findings: vec![
                Finding {
                    index: 1,
                    severity: crate::agent::findings::Severity::High,
                    file: "a.java".into(),
                    symbol: String::new(),
                    issue: "i1".into(),
                    recommendation: String::new(),
                    status: FindingStatus::Open,
                    user_notes: vec![],
                    dispute: None,
                    impl_log: vec![],
                },
                Finding {
                    index: 2,
                    severity: crate::agent::findings::Severity::Medium,
                    file: "b.java".into(),
                    symbol: String::new(),
                    issue: "i2".into(),
                    recommendation: String::new(),
                    status: FindingStatus::Open,
                    user_notes: vec![],
                    dispute: None,
                    impl_log: vec![],
                },
            ],
            active_indices: vec![],
        };
        findings::save(&engine2, &store2);
        persist(
            &engine2,
            SingleFlowPhase::Review,
            SingleFlowPhase::AwaitUser,
        );
        pivot_to_fix_mode(&engine2, "修复全部");
        let store2 = findings::load_or_migrate(&engine2).unwrap();
        assert_eq!(store2.active_indices, vec![1, 2]);
    }

    #[test]
    fn workspace_status_line_shows_phase_and_mode() {
        let engine = engine_with_findings();
        let line = workspace_status_line(&engine);
        assert!(line.contains("阶段: 待用户"));
        assert!(line.contains("scope_confirm"));
    }

    #[test]
    fn feedback_discuss_overrides_scope_confirm_mode() {
        let engine = engine_with_findings();
        crate::agent::workflow_session::enter_feedback_discuss(&engine);
        assert_eq!(workspace_mode(&engine), WorkspaceMode::FeedbackDiscuss);
        let ws = WorkflowWorkspace::build(&engine).unwrap();
        assert_eq!(ws.mode, WorkspaceMode::FeedbackDiscuss);
        assert!(matches!(
            ws.required_action,
            crate::agent::workspace::RequiredAction::DiscussOnly
        ));
    }

    #[test]
    fn transition_produces_user_and_llm_notices() {
        let engine = engine_with_findings();
        let r = transition(
            &engine,
            PhaseEvent::UserMessage {
                text: "先修复".into(),
            },
        );
        assert!(r.changed);
        assert_eq!(r.phase, SingleFlowPhase::Implement);
        let banner = take_pending_user_banner(&engine);
        assert!(banner.contains("实施"));
        let llm = consume_transition_notice(&engine).unwrap();
        assert!(llm.contains(PHASE_SWITCH_TAG));
    }
}
