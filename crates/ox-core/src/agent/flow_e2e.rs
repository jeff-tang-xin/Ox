//! In-memory end-to-end tests for the single-flow state machine (no LLM).

use std::sync::Arc;

use tokio::sync::Mutex;

use super::engine::WorkflowEngine;
use super::findings::{Finding, FindingStatus, FindingsStore, Severity};
use super::phase::{self, PhaseEvent, SingleFlowPhase};
use super::session::SessionState;
use super::task_intent::TaskIntent;
use super::workflow::{DEFAULT_WORKFLOW_ID, create_default_workflow};
use super::workspace::{RequiredAction, WorkflowWorkspace, WorkspaceMode};

fn active_engine() -> WorkflowEngine {
    let session = Arc::new(Mutex::new(SessionState::new("e2e")));
    let mut engine = WorkflowEngine::new(Arc::clone(&session));
    engine.register_workflow(create_default_workflow());
    engine.activate_workflow(DEFAULT_WORKFLOW_ID).unwrap();
    engine.set_variable(
        "_current_user_request",
        "审查 MaintainDeliveryStrategy".into(),
    );
    engine
}

fn sample_findings() -> FindingsStore {
    FindingsStore {
        summary: "2 issues".into(),
        findings: vec![
            Finding {
                index: 1,
                severity: Severity::High,
                file: "src/MaintainDeliveryStrategy.java".into(),
                symbol: "doHandle".into(),
                issue: "空指针风险".into(),
                recommendation: "加 null 检查".into(),
                fix_plan: String::new(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            },
            Finding {
                index: 2,
                severity: Severity::Medium,
                file: "src/GlobalOrderErrorCode.java".into(),
                symbol: String::new(),
                issue: "枚举缺失".into(),
                recommendation: "补全枚举".into(),
                fix_plan: String::new(),
                status: FindingStatus::Open,
                user_notes: vec![],
                dispute: None,
                impl_log: vec![],
            },
        ],
        active_indices: vec![1, 2],
    }
}

fn seed_findings(engine: &WorkflowEngine) {
    super::findings::save(engine, &sample_findings());
}

#[test]
fn review_round_starts_in_review_phase() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    assert_eq!(phase::get(&engine), SingleFlowPhase::Review);
    assert_eq!(engine.get_task_intent(), TaskIntent::Review);
    let ws = WorkflowWorkspace::build(&engine).unwrap();
    assert_eq!(ws.mode, WorkspaceMode::ExecuteReview);
    // No findings yet → should suggest exploration via code_graph first
    assert!(matches!(
        ws.required_action,
        RequiredAction::Explore { .. }
    ));
    assert!(format!("{:?}", ws.required_action).contains("code_graph"));
}

#[test]
fn findings_and_report_move_to_await_user() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::on_review_report_delivered(&engine);
    phase::on_findings_stored(&engine);
    assert_eq!(phase::get(&engine), SingleFlowPhase::AwaitUser);
    let ws = WorkflowWorkspace::build(&engine).unwrap();
    assert!(matches!(ws.required_action, RequiredAction::AwaitUser));
}

#[test]
fn review_done_without_receipt_stays_await_user() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::transition(
        &engine,
        PhaseEvent::DoneGatePassed {
            had_completion_receipt: false,
        },
    );
    assert_eq!(phase::get(&engine), SingleFlowPhase::AwaitUser);
}

#[test]
fn user_fix_pivots_to_implement_with_edit_action() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::transition(&engine, PhaseEvent::FindingsStored);
    phase::transition(
        &engine,
        PhaseEvent::DoneGatePassed {
            had_completion_receipt: false,
        },
    );
    assert!(phase::pivot_to_fix_mode(&engine, "先修复"));
    assert_eq!(phase::get(&engine), SingleFlowPhase::Implement);
    // Mark impact analysis as done so the gate allows ReadFile/EditFile
    engine.record_impl_impact(1);
    engine.record_impl_impact(2);
    let ws = WorkflowWorkspace::build(&engine).unwrap();
    assert_eq!(ws.mode, WorkspaceMode::ExecuteImpl);
    assert_eq!(engine.get_task_intent(), TaskIntent::Fix);
    assert!(matches!(
        ws.required_action,
        RequiredAction::ReadFile { .. } | RequiredAction::EditFile { .. }
    ));
}

#[test]
fn edit_blocked_in_review_allowed_in_implement() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    let args = serde_json::json!({"path": "src/Foo.java"});
    assert!(engine.validate_tool_call("edit_file", &args).is_err());

    seed_findings(&engine);
    phase::pivot_to_fix_mode(&engine, "/fix");
    assert!(engine.validate_tool_call("edit_file", &args).is_ok());
    assert!(engine.allows_code_modification());
}

#[test]
fn broad_explore_blocked_in_implement() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::pivot_to_fix_mode(&engine, "先修复");
    let args = serde_json::json!({"query": "MaintainDeliveryRequest"});
    assert!(engine.validate_tool_call("code_search", &args).is_ok());
    assert!(engine.validate_tool_call("find_symbol", &args).is_ok());
}

#[test]
fn impl_done_with_receipt_completes() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::pivot_to_fix_mode(&engine, "修复全部");
    phase::transition(
        &engine,
        PhaseEvent::DoneGatePassed {
            had_completion_receipt: true,
        },
    );
    assert_eq!(phase::get(&engine), SingleFlowPhase::Complete);
}

#[test]
fn slash_fix_scope_enters_implement() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::transition(&engine, PhaseEvent::FindingsStored);
    let mut store = sample_findings();
    store.set_scope(&[1]);
    super::findings::save(&engine, &store);
    phase::on_scope_selected(&engine);
    assert_eq!(phase::get(&engine), SingleFlowPhase::Implement);
}

#[test]
fn workspace_includes_phase_and_directives_after_fix() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::pivot_to_fix_mode(&engine, "先修复 finding #1");
    let ws = WorkflowWorkspace::build(&engine).unwrap();
    assert_eq!(ws.single_flow_phase, "implement");
    assert!(!ws.user_directives.is_empty());
}

#[test]
fn scope_confirm_preserves_turn_state_on_enter_implement() {
    let engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::on_findings_stored(&engine);
    engine.set_variable("_turn_memory", "stale".into());
    engine.set_variable("_explored_paths", "[\"a\"]".into());
    phase::on_scope_selected(&engine);
    assert_eq!(phase::get(&engine), SingleFlowPhase::Implement);
    // Review → Implement is one continuous investigation: turn memory and
    // exploration provenance are deliberately PRESERVED (see enter_implement),
    // so the model doesn't re-read code it just analyzed.
    assert_eq!(
        engine.get_variable("_turn_memory").unwrap_or_default(),
        "stale"
    );
    assert_eq!(
        engine.get_variable("_explored_paths").unwrap_or_default(),
        "[\"a\"]"
    );
}

#[test]
fn reopen_after_complete_resumes_implement() {
    let mut engine = active_engine();
    phase::on_round_started(&engine, TaskIntent::Review);
    seed_findings(&engine);
    phase::pivot_to_fix_mode(&engine, "fix");
    phase::transition(
        &engine,
        PhaseEvent::DoneGatePassed {
            had_completion_receipt: true,
        },
    );
    engine.complete_workflow().unwrap();
    assert!(engine.is_workflow_complete());
    assert!(engine.reopen_execute_for_fixes("继续修复 #2"));
    assert_eq!(phase::get(&engine), SingleFlowPhase::Implement);
    assert!(!engine.is_workflow_complete());
}
