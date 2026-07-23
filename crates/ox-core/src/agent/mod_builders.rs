//! Shared context builder functions for agent module.
//!
//! These functions build individual sections of the LLM's context window.
//! They are called by `ContextAssembler` to produce the final context block.

use std::sync::Arc;

use crate::agent::engine::WorkflowEngine;
use crate::agent::gate::explore_reflect::ConvergeMode;
use crate::memory::turn_memory::TurnMemory;

/// Section: task anchor + blackboard + phase/progress + budget gauge + plan recap.
pub fn build_task_anchor_block(
    user_task: &str,
    iteration: u32,
    turn_memory: &TurnMemory,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<WorkflowEngine>>>,
    explore_streak: u32,
    total_explore: u32,
    impl_streak: u32,
    in_impl_phase: bool,
) -> String {
    let mut b = String::new();
    let task: String = user_task.chars().take(300).collect();
    let ellipsis = if task.len() < user_task.len() {
        "…"
    } else {
        ""
    };
    b.push_str(&format!("[TURN_CONTEXT]\n🎯 任务: {task}{ellipsis}\n"));

    if let Some(wf) = workflow_engine
        && let Ok(engine) = wf.try_lock()
    {
        let bb = crate::memory::blackboard::block(&engine);
        if !bb.is_empty() {
            b.push_str(&bb);
            b.push('\n');
        }
    }

    let tool_count = turn_memory.entries.len();
    let mut plan_recap = String::new();
    let mut converge = ConvergeMode::SubmitPlan;
    let phase_line = if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            plan_recap = engine.plan_progress_summary();
            converge = ConvergeMode::from_intent(engine.get_task_intent());
            format_phase_ripple(&crate::agent::phase::get(&engine), &engine)
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    if phase_line.is_empty() {
        b.push_str(&format!(
            "📍 iteration {} · 工具 {} 次\n",
            iteration + 1,
            tool_count
        ));
    } else {
        b.push_str(&format!(
            "📍 iteration {} · 工具 {} 次 · {phase_line}\n",
            iteration + 1,
            tool_count
        ));
    }

    let intent_reason = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .and_then(|e| e.get_task_intent_reason());
    b.push_str(&crate::agent::gate::explore_reflect::budget_gauge(
        explore_streak,
        total_explore,
        impl_streak,
        in_impl_phase,
        converge,
        intent_reason.as_deref(),
    ));

    if !plan_recap.is_empty() {
        b.push('\n');
        b.push_str(&plan_recap);
        b.push('\n');
    }
    b
}

/// Section: edit file deduplication — shows which files were edited this turn
/// to prevent the LLM from repeatedly editing the same files.
/// This is UNIQUE to TurnMemory — not available from react_log.
pub fn build_edit_dedup_block(turn_memory: &TurnMemory) -> String {
    let mut b = String::new();
    const EDIT_TOOLS: [&str; 3] = ["file_write", "edit_file", "delete_range"];
    let is_edit = |tool: &str| EDIT_TOOLS.contains(&tool);

    if turn_memory.entries.iter().any(|e| is_edit(&e.tool)) {
        let mut order: Vec<String> = Vec::new();
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for e in turn_memory.entries.iter().filter(|e| is_edit(&e.tool)) {
            let target: String = e.target.chars().take(120).collect();
            if counts
                .insert(
                    target.clone(),
                    counts.get(&target).copied().unwrap_or(0) + 1,
                )
                .is_none()
            {
                order.push(target);
            }
        }
        b.push_str("\n✏️ 你本轮已修改的文件 (勿重复编辑):\n");
        for target in &order {
            let n = counts.get(target).copied().unwrap_or(1);
            if n > 1 {
                b.push_str(&format!("  · {target} (已编辑 {n} 次)\n"));
            } else {
                b.push_str(&format!("  · {target}\n"));
            }
        }
    }

    if !turn_memory.decisions.is_empty() {
        let window = 4;
        b.push_str("\n你刚才形成的判断:\n");
        for d in turn_memory.decisions.iter().rev().take(window).rev() {
            b.push_str(&format!(
                "  - {}\n",
                d.chars().take(220).collect::<String>()
            ));
        }
    }
    b
}

/// Section: workspace-derived guidance — required_action, scope gate, review handoff, durable memory.
pub fn build_workspace_block(
    engine: &WorkflowEngine,
    unified_tool_mode: bool,
) -> String {
    let mut b = String::new();

    if crate::agent::phase::should_inject_workspace(engine)
        && let Some(ws) = crate::agent::workspace::WorkflowWorkspace::build(engine)
    {
        let action_text = if unified_tool_mode {
            format_required_action_one_liner_unified(&ws.required_action)
        } else {
            format_required_action_one_liner(&ws.required_action)
        };
        b.push_str(&format!("\n下一步: {action_text}\n"));
    }

    if crate::agent::gate::business_gate::is_pending_scope(engine) {
        b.push_str("\n⏸️ 门禁: 等待用户 c /confirm 确认范围\n");
        if let Some(store) = crate::agent::findings::load_or_migrate(engine)
            && !store.findings.is_empty()
        {
            b.push_str("\n📋 当前 findings (用户按编号讨论):\n");
            for f in &store.findings {
                let icon =
                    if store.active_indices.is_empty() || store.active_indices.contains(&f.index) {
                        "☐"
                    } else {
                        "⊘"
                    };
                b.push_str(&format!(
                    "  {icon} #{} [{}] {} — {}\n",
                    f.index,
                    f.severity.label(),
                    f.file.rsplit('/').next().unwrap_or(&f.file),
                    f.issue.chars().take(80).collect::<String>()
                ));
            }
        }
    }

    if crate::agent::phase::get(engine) == crate::agent::phase::SingleFlowPhase::Implement {
        let mut files: Vec<String> = engine.review_handoff_files();
        if files.is_empty()
            && let Some(store) = crate::agent::findings::load_or_migrate(engine)
        {
            files = store
                .findings
                .iter()
                .filter(|f| !f.file.is_empty())
                .map(|f| f.file.clone())
                .collect();
        }
        if !files.is_empty() {
            b.push_str("\n📂 审查阶段已读文件 (内容在上文，直接编辑，勿重新探索):\n");
            let mut seen = std::collections::HashSet::new();
            for f in &files {
                if seen.insert(f.clone()) {
                    b.push_str(&format!("  · {f}\n"));
                }
            }
        }
    }

    if !crate::agent::phase::should_inject_workspace(engine) {
        let dm = engine.durable_memory_block();
        if !dm.is_empty() {
            b.push_str(&format!(
                "\n记忆: {}\n",
                dm.chars().take(400).collect::<String>()
            ));
        }
        let ur = engine.user_round_memory_block();
        if !ur.is_empty() {
            b.push_str(&format!(
                "\n上下文: {}\n",
                ur.chars().take(200).collect::<String>()
            ));
        }
    }

    b
}

fn format_phase_ripple(
    phase: &crate::agent::phase::SingleFlowPhase,
    engine: &WorkflowEngine,
) -> String {
    match phase {
        crate::agent::phase::SingleFlowPhase::Receive
        | crate::agent::phase::SingleFlowPhase::Review => {
            let has_findings = crate::agent::findings::load_or_migrate(engine)
                .is_some_and(|s| !s.findings.is_empty());
            if has_findings {
                "🔍 已探索 → finish(finding_json) 确认".to_string()
            } else {
                "🔍 探索代码".to_string()
            }
        }
        crate::agent::phase::SingleFlowPhase::AwaitUser => {
            if crate::agent::gate::business_gate::scope_implementation_unlocked(engine) {
                "✏️ 已确认 → 开始实施".to_string()
            } else {
                "⏸️ 等待确认".to_string()
            }
        }
        crate::agent::phase::SingleFlowPhase::Implement => {
            if let Some(store) = crate::agent::findings::load_or_migrate(engine) {
                let done = store
                    .findings
                    .iter()
                    .filter(|f| f.status == crate::agent::findings::FindingStatus::Done)
                    .count();
                let total = store.findings.len();
                if total > 0 {
                    format!("✏️ 实施中 ({done}/{total})")
                } else {
                    "✏️ 实施中".to_string()
                }
            } else {
                "✏️ 实施中".to_string()
            }
        }
        crate::agent::phase::SingleFlowPhase::Complete => "✅ 完成".to_string(),
    }
}

fn format_required_action_one_liner(action: &crate::agent::workspace::RequiredAction) -> String {
    match action {
        crate::agent::workspace::RequiredAction::Explore { hint } => {
            format!("探索 — {hint}")
        }
        crate::agent::workspace::RequiredAction::ReadFile {
            path,
            finding_index,
            ..
        } => {
            format!("file_read finding #{finding_index}: `{path}`")
        }
        crate::agent::workspace::RequiredAction::EditFile {
            path,
            finding_index,
            ..
        } => {
            format!("edit_file finding #{finding_index}: `{path}`")
        }
        crate::agent::workspace::RequiredAction::Verify {
            command,
            finding_index,
        } => {
            let cmd: String = command.chars().take(80).collect();
            format!("验证 finding #{finding_index}: `{cmd}`")
        }
        crate::agent::workspace::RequiredAction::EmitFindingsAndDone => {
            "finish(finding_json) 提交计划".into()
        }
        crate::agent::workspace::RequiredAction::EmitCompletionReceipt => {
            "finish(content) 收尾结束".into()
        }
        crate::agent::workspace::RequiredAction::AwaitUser => "等待用户确认范围".into(),
        crate::agent::workspace::RequiredAction::DiscussOnly => "讨论模式 — finish(content)".into(),
    }
}

fn format_required_action_one_liner_unified(
    action: &crate::agent::workspace::RequiredAction,
) -> String {
    match action {
        crate::agent::workspace::RequiredAction::Explore { hint } => {
            format!("find_symbol(name=目标符号) → file_read(path, offset) — {hint}")
        }
        crate::agent::workspace::RequiredAction::ReadFile {
            path,
            finding_index,
            ..
        } => {
            format!(
                "先 find_symbol 定位 #{finding_index} 对应方法 → 再 file_read(path={path}, offset=行号)"
            )
        }
        crate::agent::workspace::RequiredAction::EditFile {
            path,
            finding_index,
            ..
        } => {
            format!(
                "edit_file / patch_file finding #{finding_index}: `{path}`"
            )
        }
        crate::agent::workspace::RequiredAction::Verify {
            command,
            finding_index,
        } => {
            let cmd: String = command.chars().take(80).collect();
            format!("验证 finding #{finding_index}: `{cmd}`")
        }
        crate::agent::workspace::RequiredAction::EmitFindingsAndDone => {
            "finish(finding_json) 提交计划".into()
        }
        crate::agent::workspace::RequiredAction::EmitCompletionReceipt => {
            "finish(content) 收尾结束".into()
        }
        crate::agent::workspace::RequiredAction::AwaitUser => "等待用户确认范围".into(),
        crate::agent::workspace::RequiredAction::DiscussOnly => "讨论模式 — finish(content)".into(),
    }
}
