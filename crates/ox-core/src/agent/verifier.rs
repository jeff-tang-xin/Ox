//! Post-edit verifier — read-only review pass after each implementation edit.

use super::engine::WorkflowEngine;
use super::findings::{self, FindingStatus};

/// Inject after a successful edit in implementation phase.
pub fn after_edit_note(
    engine: &WorkflowEngine,
    finding_index: u32,
    file_path: &str,
    tool_output: &str,
) -> Option<String> {
    if !crate::agent::workflow_session::is_implementation_phase(engine) {
        return None;
    }
    let diff_snippet = extract_diff_snippet(tool_output);
    if let Some(mut store) = findings::load_or_migrate(engine) {
        if let Some(f) = store.get_mut(finding_index) {
            f.status = FindingStatus::AwaitingVerify;
            f.impl_log.push(findings::ImplAction {
                tool: "edit_file".into(),
                detail: format!("{file_path}: {}", diff_snippet.chars().take(200).collect::<String>()),
            });
        }
        findings::save(engine, &store);
    }
    let issue = engine
        .get_plan_tracker()
        .and_then(|t| {
            t.steps
                .iter()
                .find(|s| s.index == finding_index)
                .map(|s| s.desc.clone())
        })
        .unwrap_or_default();
    Some(format!(
        "【Verifier — finding #{finding_index}】\n\
         文件: `{file_path}`\n\
         目标: {issue}\n\
         改动摘要:\n{diff_snippet}\n\n\
         **Verifier 规则（只读复核）**:\n\
         1. 改动是否只解决 finding #{finding_index}，无无关修改\n\
         2. 若有问题 → 继续 edit_file 修正\n\
         3. 若 OK → shell_exec 运行 verify（见 plan 或 cargo test / 项目惯例）\n\
         4. verify 通过后进入下一 finding；全部完成后 ## Done + completion_receipt"
    ))
}

fn extract_diff_snippet(tool_output: &str) -> String {
    if tool_output.contains("@@") {
        let lines: Vec<&str> = tool_output.lines().take(24).collect();
        return lines.join("\n");
    }
    tool_output.chars().take(800).collect()
}

pub fn after_verify_pass(engine: &WorkflowEngine, finding_index: u32) {
    if let Some(mut store) = findings::load_or_migrate(engine) {
        if let Some(f) = store.get_mut(finding_index) {
            f.status = FindingStatus::Done;
        }
        findings::save(engine, &store);
    }
}
