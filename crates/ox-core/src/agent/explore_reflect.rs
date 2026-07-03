//! Exploration-reflection guard.
//!
//! Catches the "explore-but-never-act" loop: the agent keeps calling read-only
//! tools (file_read / find_symbol / code_search …) turn after turn without ever
//! editing a file or finishing. The existing brakes miss this — read-only calls
//! are neither "idle" (no tool) nor "failing" (no verify) nor "same-tool repeat".
//!
//! Strategy (reflect-first, stop-as-last-resort):
//! 1. Count CONSECUTIVE read-only-only turns. Any edit / write / delete / finish
//!    resets the streak to zero (real progress was made).
//! 2. At [`REFLECT_AT`] turns, inject a forced self-assessment: restate the goal,
//!    inventory what's known, and decide — act now or name the ONE missing fact.
//! 3. If exploration continues for [`STOP_AFTER_REFLECT`] more turns past the
//!    reflection, hand control back to the user — reflection didn't land.

/// Read-only tools that, on their own, never constitute progress on a task.
const READONLY_TOOLS: &[&str] = &[
    "file_read",
    "find_symbol",
    "code_search",
    "file_search",
    "file_list",
    "code_graph",
    "git_status",
    "git_diff",
    "project_detect",
    "web_fetch",
    "load_skill",
    "recall",
];

/// Tools (or the `finish` action) that count as real progress and reset the streak.
const PROGRESS_TOOLS: &[&str] = &["file_write", "edit_file", "delete_range"];

/// Consecutive read-only turns before a reflection prompt is injected.
pub const REFLECT_AT: u32 = 15;

/// Further read-only turns after reflection before handing back to the user.
pub const STOP_AFTER_REFLECT: u32 = 15;

/// What the loop should do after classifying one turn's tool batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectAction {
    /// Keep going; nothing to inject.
    Continue,
    /// Inject this self-assessment prompt — the model must answer it next turn.
    Reflect(String),
    /// Reflection already fired and exploration continued; stop and ask the user.
    Stop(String),
}

/// Classify a turn's tool batch.
///
/// `tool_names` are the tools called this turn; `had_finish` is true when the
/// batch contained a terminal `finish` action. Returns `true` when the turn was
/// pure exploration (only read-only tools, no progress, no finish).
pub fn is_pure_exploration(tool_names: &[String], had_finish: bool) -> bool {
    if had_finish || tool_names.is_empty() {
        return false;
    }
    if tool_names
        .iter()
        .any(|t| PROGRESS_TOOLS.contains(&t.as_str()))
    {
        return false;
    }
    // Every call must be a known read-only tool. An unknown tool is treated as
    // potential progress (don't penalize), so it also breaks the streak.
    tool_names
        .iter()
        .all(|t| READONLY_TOOLS.contains(&t.as_str()))
}

/// Update the streak for one turn and decide what the loop should do.
///
/// `streak` is the caller-owned counter (consecutive pure-exploration turns).
/// `reflected` tracks whether the reflection prompt has already been injected
/// this streak, so it fires once rather than every turn past the threshold.
pub fn evaluate(
    streak: &mut u32,
    reflected: &mut bool,
    tool_names: &[String],
    had_finish: bool,
    user_task: &str,
) -> ReflectAction {
    if !is_pure_exploration(tool_names, had_finish) {
        *streak = 0;
        *reflected = false;
        return ReflectAction::Continue;
    }

    *streak += 1;

    if *reflected {
        if *streak >= REFLECT_AT + STOP_AFTER_REFLECT {
            return ReflectAction::Stop(stop_message(*streak));
        }
        return ReflectAction::Continue;
    }

    if *streak >= REFLECT_AT {
        *reflected = true;
        return ReflectAction::Reflect(reflect_message(*streak, user_task));
    }

    ReflectAction::Continue
}

fn reflect_message(streak: u32, user_task: &str) -> String {
    let task: String = user_task.chars().take(300).collect();
    format!(
        "🪞 **反思检查点**：你已连续 {streak} 轮只做探索还没有动手或收尾。\n\
         \n\
         请**立即**做以下三件事之一：\n\
         \n\
         1. **信息够了 → 动手** — 直接 `finish(finding_json=[...])` 提交计划，或 `edit_file` 改代码。\n\
         \n\
         2. **不确定 → 问用户** — 业务逻辑、命名意图、方案选择不明确时，直接 `finish(content=你的问题)` 问用户。不要自己猜。\n\
         \n\
         3. **真就差一个具体信息 → 只补那一个文件** — 说出缺什么，读完立即回头动手。禁止再泛读。\n\
         \n\
         原始任务：{task}"
    )
}

fn stop_message(streak: u32) -> String {
    format!(
        "## Failed\n已连续 {streak} 次只探索不动手，反思后仍未收敛 — 停止本轮，交给你判断。\n\
         可能是任务范围不清、缺少关键信息，或方向需要你确认。请补充指示或缩小范围。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn edit_breaks_streak() {
        assert!(!is_pure_exploration(&names(&["edit_file"]), false));
        assert!(!is_pure_exploration(&names(&["file_read", "edit_file"]), false));
    }

    #[test]
    fn finish_is_not_exploration() {
        assert!(!is_pure_exploration(&names(&["file_read"]), true));
    }

    #[test]
    fn pure_reads_are_exploration() {
        assert!(is_pure_exploration(&names(&["file_read", "find_symbol"]), false));
    }

    #[test]
    fn reflects_at_threshold_then_stops() {
        let mut streak = 0;
        let mut reflected = false;
        let reads = names(&["file_read"]);
        // Turns 1..REFLECT_AT-1: just continue.
        for _ in 0..(REFLECT_AT - 1) {
            assert_eq!(
                evaluate(&mut streak, &mut reflected, &reads, false, "task"),
                ReflectAction::Continue
            );
        }
        // Turn REFLECT_AT: reflect.
        match evaluate(&mut streak, &mut reflected, &reads, false, "task") {
            ReflectAction::Reflect(_) => {}
            other => panic!("expected Reflect, got {other:?}"),
        }
        // Continues until the stop threshold.
        for _ in 0..(STOP_AFTER_REFLECT - 1) {
            assert_eq!(
                evaluate(&mut streak, &mut reflected, &reads, false, "task"),
                ReflectAction::Continue
            );
        }
        match evaluate(&mut streak, &mut reflected, &reads, false, "task") {
            ReflectAction::Stop(_) => {}
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn progress_resets_after_reflect() {
        let mut streak = 0;
        let mut reflected = false;
        let reads = names(&["file_read"]);
        for _ in 0..REFLECT_AT {
            evaluate(&mut streak, &mut reflected, &reads, false, "task");
        }
        assert!(reflected);
        // An edit resets everything.
        assert_eq!(
            evaluate(&mut streak, &mut reflected, &names(&["edit_file"]), false, "task"),
            ReflectAction::Continue
        );
        assert_eq!(streak, 0);
        assert!(!reflected);
    }
}
