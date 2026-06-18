//! Durable memory block — injected at every turn start and after compaction.
//!
//! Combines TurnMemory, PlanTracker, exploration snapshot, and workflow handoff
//! so the LLM never relies solely on truncated message history.

use crate::agent::engine::WorkflowEngine;

pub const DURABLE_MEMORY_TAG: &str = "[DURABLE_MEMORY]";

/// Build a single high-priority memory block from all durable workflow state.
pub fn format_durable_memory_block(engine: &WorkflowEngine) -> String {
    if engine.is_workflow_complete() {
        return String::new();
    }
    if crate::agent::workspace::uses_workspace_memory(engine) {
        return crate::agent::workspace::minimal_durable_addon(engine);
    }

    let mut parts = vec![
        format!(
            "{DURABLE_MEMORY_TAG}\n\
             ## 本轮 workflow 记忆（CURRENT ROUND ONLY）\n\
             跨步骤/跨迭代保留的**当前轮次**状态 — 非历史任务；workflow 完成后自动清空。\n\
             ⚠️ 勿将此处记录当作其它轮次或知识库检索结果的待办。"
        ),
    ];

    let plan = engine.plan_progress_summary();
    if !plan.is_empty() {
        parts.push(plan);
    }

    let step_idx = engine.get_current_step_index();
    let is_impl = step_idx == 3 && crate::agent::workflow_session::is_implementation_phase(engine);

    let explored = engine.explored_paths_summary();
    if step_idx == 1 && !explored.is_empty() {
        parts.push(format!("【已探索路径 — 勿重复 file_list/file_read】\n{explored}"));
    }

    if step_idx >= 1 && !is_impl {
        let snap = engine.exploration_snapshot_summary();
        if !snap.is_empty() {
            let excerpt: String = snap.chars().take(10_000).collect();
            let label = if step_idx == 3 {
                "【Preflight / 探索快照 — 勿重复相同命令】"
            } else {
                "【探索快照】"
            };
            parts.push(format!(
                "{label}\n{excerpt}\n\
                 （大文件完整内容在 `.ox/exploration/`）"
            ));
        }
    }

    if step_idx == 3 {
        if !is_impl {
            if let Some(handoff) = crate::agent::execute_handoff::ExecuteHandoff::load(engine) {
                let block: String = handoff.format_for_execute().chars().take(12_000).collect();
                parts.push(block);
            }
        }
        let findings = crate::agent::perception::findings_summary_block(engine);
        if !findings.is_empty() {
            parts.push(findings);
        } else if let Some(report) = engine.get_execute_review_report() {
            let cap = if is_impl { 2500 } else { 6000 };
            let snippet: String = report.chars().take(cap).collect();
            parts.push(format!("【审查报告 — park 前输出】\n{snippet}"));
        }
    }

    let guidance = engine.workflow_guidance_block();
    if !guidance.is_empty() {
        parts.push(guidance);
    }

    if step_idx >= 1 && !is_impl {
        if let Some(intent) = engine.get_variable("_step0_output") {
            let snippet: String = intent.chars().take(800).collect();
            parts.push(format!("【意图分类】\n{snippet}"));
        }
    }

    if !is_impl {
        if let Some(prev) = engine.get_previous_step_output() {
            let snippet: String = prev.chars().take(2000).collect();
            parts.push(format!("【上一步输出】\n{snippet}"));
        }
    }

    if parts.len() <= 1 {
        return String::new();
    }

    parts.push("基于以上**本轮 workflow**记忆继续当前步骤；历史轮次与知识库内容勿重复执行。".to_string());
    parts.join("\n\n")
}

pub fn inject_durable_memory(messages: &mut Vec<crate::message::Message>, block: &str) {
    if block.is_empty() {
        return;
    }
    strip_durable_memory(messages);
    messages.push(crate::message::Message::system(block));
}

pub fn strip_durable_memory(messages: &mut Vec<crate::message::Message>) {
    messages.retain(|m| {
        !matches!(m, crate::message::Message::System { content } if content.starts_with(DURABLE_MEMORY_TAG))
    });
}
