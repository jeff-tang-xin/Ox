use std::sync::Arc;

use crate::agent::engine::WorkflowEngine;
use crate::message::Message;
use crate::memory::store::MemoryStore;
use crate::memory::turn_memory::TurnMemory;

/// Unified context assembler — the **single entry point** for building
/// the LLM's context window each turn.
///
/// # Assembly order (each section stripped + rebuilt fresh)
///
/// ```text
/// [USER_ROUND]          ← Current user task (top, always visible)
/// [TURN_CONTEXT]        ← Iteration, phase, budget gauge, plan recap
/// ── Edit Dedup ──      ← Files edited this turn (prevents duplicate edits)
/// 🔄 ReAct Mainline     ← FULL cross-turn action history (LLM's backbone memory)
///                         includes: task, decision, reasoning, assistant text, tool result
/// ── Workspace State ── ← Intent, findings, implementation progress
/// ```
///
/// # Design
///
/// - **ReAct backbone**: `react_log` is the primary memory. Every tool call
///   is recorded with full context (reasoning, decision, result).
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
            && let Some(graph) = engine.get_variable(crate::memory::memory_offload::MEMORY_GRAPH_VAR)
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
        block.push_str(&crate::agent::mod_builders::build_edit_dedup_block(turn_memory));

        // 2d. 🔄 ReAct Mainline — the LLM's backbone memory (replaces old Timeline)
        // This is the key upgrade: full ReAct history with reasoning + results.
        if let Some(ms) = memory_store {
            let mainline_limit = if in_impl_phase { 50 } else { 30 };
            if let Ok(mainline) = ms.get_react_mainline(session_id, mainline_limit) {
                if !mainline.trim().is_empty() {
                    block.push_str("🔄 ReAct Mainline (Full Action History):\n");
                    block.push_str(&mainline);
                    block.push('\n');
                }
            }
        }

        // 2e. Workspace state
        if let Some(wf) = workflow_engine
            && let Ok(engine) = wf.try_lock()
        {
            block.push_str(&crate::agent::mod_builders::build_workspace_block(&engine, unified_tool_mode));
        }

        messages.push(Message::system(&block));
    }
}
