//! Durable memory block — injected at every turn start and after compaction.
//!
//! Combines TurnMemory, PlanTracker, exploration snapshot, and workflow handoff
//! so the LLM never relies solely on truncated message history.

use crate::agent::engine::WorkflowEngine;
use crate::agent::turn_memory::TURN_MEMORY_TAG;

pub const DURABLE_MEMORY_TAG: &str = "[DURABLE_MEMORY]";

/// Build a single high-priority memory block from all durable workflow state.
pub fn format_durable_memory_block(engine: &WorkflowEngine) -> String {
    let mut parts = vec![
        format!(
            "{DURABLE_MEMORY_TAG}\n\
             📌 持久记忆（跨步骤/跨迭代保留 — 勿重复以下已完成的工具调用）"
        ),
    ];

    if let Some(tm) = engine.load_turn_memory() {
        if !tm.entries.is_empty() {
            let body = tm.format_injection(0);
            // Strip duplicate tag line from nested format
            let body = body
                .strip_prefix(TURN_MEMORY_TAG)
                .unwrap_or(&body)
                .trim_start_matches('\n');
            parts.push(format!("【本轮工具记录】\n{body}"));
        }
    }

    let plan = engine.plan_progress_summary();
    if !plan.is_empty() {
        parts.push(plan);
    }

    let step_idx = engine.get_current_step_index();
    let explored = engine.explored_paths_summary();
    if step_idx == 1 && !explored.is_empty() {
        parts.push(format!("【已探索路径 — 勿重复 file_list/file_read】\n{explored}"));
    }

    if step_idx >= 1 {
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
        if let Some(handoff) = crate::agent::execute_handoff::ExecuteHandoff::load(engine) {
            let block: String = handoff.format_for_execute().chars().take(12_000).collect();
            parts.push(block);
        }
        let findings = crate::agent::perception::findings_summary_block(engine);
        if !findings.is_empty() {
            parts.push(findings);
        } else if let Some(report) = engine.get_execute_review_report() {
            let snippet: String = report.chars().take(6000).collect();
            parts.push(format!("【审查报告 — park 前输出】\n{snippet}"));
        }
    }

    let guidance = engine.workflow_guidance_block();
    if !guidance.is_empty() {
        parts.push(guidance);
    }

    if step_idx >= 1 {
        if let Some(intent) = engine.get_variable("_step0_output") {
            let snippet: String = intent.chars().take(800).collect();
            parts.push(format!("【意图分类】\n{snippet}"));
        }
    }

    if let Some(prev) = engine.get_previous_step_output() {
        let snippet: String = prev.chars().take(2000).collect();
        parts.push(format!("【上一步输出】\n{snippet}"));
    }

    if parts.len() <= 1 {
        return String::new();
    }

    parts.push("基于以上**本轮**记忆继续当前 workflow 步骤，勿重复已列出的工具调用。".to_string());
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
