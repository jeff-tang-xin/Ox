use std::sync::Arc;

use crate::agent::engine::WorkflowEngine;
use crate::memory::store::MemoryStore;
use crate::memory::turn_memory::TurnMemory;
use crate::message::Message;

/// Unified context assembler — the **single entry point** for building
/// the LLM's context window each turn.
///
/// # Assembly order (each section stripped + rebuilt fresh)
///
/// ```text
/// [USER_ROUND]          ← Current user task (top, always visible)
/// [TURN_CONTEXT]        ← Iteration, phase, budget gauge, plan recap
/// ── Edit Dedup ──      ← Files edited this turn (prevents duplicate edits)
/// 🔄 ReAct Log         ← FULL cross-turn action history (LLM's backbone memory)
///                         includes: task, decision, reasoning, assistant text, tool result
///                         ordered by time (oldest first → newest last)
/// ── Workspace State ── ← Intent, findings, implementation progress
/// ```
///
/// # Design
///
/// - **ReAct Log = single source of truth**: Every LLM action (thinking,
///   text output, tool call, tool result) is recorded in `react_log`.
///   The LLM reads this log to know what it did last round.
/// - **No stop-signal dependency**: The loop does NOT rely on detecting
///   text markers like `## Done` or `总结` to know when to stop.
///   Instead, LLM naturally stops when it outputs plain text without tool calls.
/// - **TurnMemory = dedup**: Used only for edit tracking + in-turn prevention.
///   NOT an independent memory source.
/// - **Single injection**: One `assemble()` call replaces all scattered
///   `inject_slim_context` blocks.
pub struct ContextAssembler;

impl ContextAssembler {
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        messages: &mut Vec<Message>,
        user_task: &str,
        iteration: u32,
        turn_memory: &TurnMemory,
        workflow_engine: &Option<Arc<tokio::sync::Mutex<WorkflowEngine>>>,
        unified_tool_mode: bool,
        memory_store: &Option<Arc<MemoryStore>>,
        session_id: &str,
        explore_streak: u32,
        total_explore: u32,
        impl_streak: u32,
        in_impl_phase: bool,
    ) {
        // ── 1. Strip all prior injection blocks in ONE pass ──
        crate::agent::strip_all_injection_blocks(messages);

        // ── 1b. Rebuild [USER_ROUND] if stripped ──
        let has_user_round = messages.iter().any(|m| match m {
            Message::System { content } => content.contains("[USER_ROUND]"),
            Message::User { content } => content.contains("[USER_ROUND]"),
            Message::Assistant { content, .. } => content.contains("[USER_ROUND]"),
            Message::ToolResult { content, .. } => content.contains("[USER_ROUND]"),
        });
        if !has_user_round && !user_task.is_empty() {
            let user_round = format!("[USER_ROUND]\n{}\n[/USER_ROUND]", user_task);
            messages.insert(0, Message::system(&user_round));
        }

        // ── 2. Build context block ──
        let mut block = String::with_capacity(3000);

        // 2a. Memory graph (pinned at top, only after offload)
        if let Some(wf) = workflow_engine
            && let Ok(engine) = wf.try_lock()
            && let Some(graph) =
                engine.get_variable(crate::memory::memory_offload::MEMORY_GRAPH_VAR)
            && !graph.trim().is_empty()
        {
            block.push_str("📚 Archived Memory:\n");
            block.push_str(&graph);
            block.push_str("\n\n");
        }

        // 2b. Task anchor + phase/progress (existing logic, kept intact)
        block.push_str(&crate::agent::mod_builders::build_task_anchor_block(
            user_task,
            iteration,
            turn_memory,
            workflow_engine,
            explore_streak,
            total_explore,
            impl_streak,
            in_impl_phase,
        ));

        // 2c. Edit dedup (from TurnMemory — unique info, not in react_log)
        block.push_str(&crate::agent::mod_builders::build_edit_dedup_block(
            turn_memory,
        ));

        // 2d. ReAct Log — Active Memory + Archived Graphs
        // All data stored in Tantivy with ZERO truncation
        if let Some(ms) = memory_store {
            let current_files: Vec<String> = turn_memory
                .entries
                .iter()
                .filter_map(|e| {
                    if crate::memory::store::is_file_path(&e.target) {
                        Some(e.target.clone())
                    } else {
                        None
                    }
                })
                .collect();

            if let Ok(context) = ms.get_context_for_injection(session_id, user_task, &current_files) {
                if !context.trim().is_empty() {
                    block.push_str(&context);
                    block.push_str("\n");
                }
            }
        }

        // 2e. Workspace state
        if let Some(wf) = workflow_engine
            && let Ok(engine) = wf.try_lock()
        {
            block.push_str(&crate::agent::mod_builders::build_workspace_block(
                &engine,
                unified_tool_mode,
            ));
        }

        messages.push(Message::system(&block));
    }
}
