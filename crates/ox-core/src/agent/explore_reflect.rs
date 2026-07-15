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
///
/// Deliberately tight: the goal is "don't over-explore, converge fast". Combined
/// with the per-turn [`budget_gauge`] hint (which renders from the very first
/// exploration turn), this keeps the model from circling — while `made_discovery`
/// still lets legitimate deep reading of *new* files run uncapped up to the
/// cumulative [`TOTAL_EXPLORE_CEILING`].
pub const REFLECT_AT: u32 = 4;

/// Further read-only turns after reflection before handing back to the user.
pub const STOP_AFTER_REFLECT: u32 = 4;

/// Absolute ceiling on **total** exploration turns in a single pre-implementation
/// stretch, regardless of information gain. The low-gain streak ([`REFLECT_AT`])
/// catches *circling* — repeated reads of the same thing — but a model that reads
/// a fresh file every turn keeps resetting that streak and could wander the whole
/// repo unbounded. This ceiling is the backstop: only real progress (an edit or
/// `finish`) clears it; discovering new files does NOT. Set well above the
/// low-gain threshold so legitimate deep exploration is never clipped early.
pub const TOTAL_EXPLORE_CEILING: u32 = 20;

/// Consecutive no-edit turns **during the implementation phase** before an
/// implementation-reflection prompt is injected. Far tighter than exploration:
/// once the plan is confirmed, drifting into read-after-read instead of editing
/// is the failure we want to catch quickly.
pub const IMPL_REFLECT_AT: u32 = 3;

/// How the model should *converge* when it stops exploring — the crux of "reflect
/// by phase, not blindly".
///
/// The old reflection was phase-blind: it always told the model the write lock was
/// on and the only convergence action was submitting a `finding_json` plan. That is
/// correct for a **review** (writes locked until the user picks scope), but wrong
/// for a **fix / greenfield / general** task, where writes are already unlocked and
/// the right move is to just `edit_file`. Telling those tasks to "submit a plan and
/// wait for confirmation" stalls work that should proceed directly.
///
/// The caller derives this from [`crate::agent::task_intent::TaskIntent`] and passes
/// it in, so the reflection prompt and the per-turn gauge speak the correct
/// convergence action for the task at hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvergeMode {
    /// Review / audit: writes are locked; converge by submitting a plan for the
    /// user to confirm — `finish(finding_json=[…])`.
    SubmitPlan,
    /// Fix / greenfield / general coding: writes are unlocked; converge by acting
    /// directly — `file_read` the target then `edit_file`.
    DirectEdit,
    /// Q&A / explanation: minimal exploration; converge by answering —
    /// `finish(content=…)`.
    Answer,
}

impl ConvergeMode {
    /// Map task intent → convergence action. Fix and General share DirectEdit
    /// (both have write access and should implement, not submit a plan). Review
    /// submits a plan; Qa answers.
    pub fn from_intent(intent: crate::agent::task_intent::TaskIntent) -> Self {
        use crate::agent::task_intent::TaskIntent;
        match intent {
            TaskIntent::Review => Self::SubmitPlan,
            TaskIntent::Qa => Self::Answer,
            TaskIntent::Fix | TaskIntent::General => Self::DirectEdit,
        }
    }
}

/// Render the exploration/implementation budget gauge for the turn-context block.
///
/// This makes the *cost* of continued exploration visible to the model every
/// turn — not just at the moment a reflection fires. Returns an empty string
/// when both counters are zero (normal cadence; no need to nag).
///
/// Two exploration signals are surfaced: the low-gain circling streak
/// ([`REFLECT_AT`]) and the cumulative hard ceiling ([`TOTAL_EXPLORE_CEILING`],
/// which discovery does NOT reset). Thresholds are the same constants the brakes
/// use, so the gauge can never drift from when the guard actually trips.
pub fn budget_gauge(
    explore_streak: u32,
    total_explore: u32,
    impl_streak: u32,
    in_impl_phase: bool,
    converge: ConvergeMode,
) -> String {
    if in_impl_phase {
        if impl_streak == 0 {
            return String::new();
        }
        return format!(
            "🛠️ 实施预算: 已连续 {impl_streak}/{IMPL_REFLECT_AT} 轮未改代码 · 下轮请 edit_file/file_write 或 finish\n"
        );
    }
    if explore_streak == 0 && total_explore == 0 {
        return String::new();
    }

    // Convergence action differs by task: a review submits a plan; a fix/general
    // task edits directly; a Q&A answers. Keep the gauge honest to the task so it
    // never nags a fix task to "submit a plan and wait".
    let converge_short = gauge_converge_hint(converge);

    let mut out = String::new();
    // Low-gain circling line (only when a streak is building).
    if explore_streak > 0 {
        let stop_at = REFLECT_AT + STOP_AFTER_REFLECT;
        if explore_streak >= REFLECT_AT {
            let left = stop_at.saturating_sub(explore_streak);
            out.push_str(&format!(
                "🔍 探索预算: {explore_streak}/{REFLECT_AT}（连续无新发现，已超阈值）· ⚠️ 再 {left} 轮仍无进展将交还用户 — {converge_short}\n"
            ));
        } else {
            let left = REFLECT_AT.saturating_sub(explore_streak);
            out.push_str(&format!(
                "🔍 探索预算: 连续 {explore_streak}/{REFLECT_AT} 轮无新发现（重复读/重复搜）· 再 {left} 轮空转将强制收敛（信息够了就{converge_short}；读新文件不计入）\n"
            ));
        }
    }
    // Cumulative ceiling line — always shown once exploration has begun; nudges
    // toward focus even while reading new files. Highlight when close.
    if total_explore > 0 {
        let left = TOTAL_EXPLORE_CEILING.saturating_sub(total_explore);
        let warn = if total_explore * 4 >= TOTAL_EXPLORE_CEILING * 3 {
            "⚠️ "
        } else {
            ""
        };
        out.push_str(&format!(
            "🧭 {warn}累计探索: {total_explore}/{TOTAL_EXPLORE_CEILING} 轮 · 再 {left} 轮（含读新文件）仍不收敛（{converge_short}）将强制收敛\n"
        ));
    }
    out
}

/// Short convergence-action phrase for the per-turn gauge, by task mode.
fn gauge_converge_hint(converge: ConvergeMode) -> &'static str {
    match converge {
        ConvergeMode::SubmitPlan => {
            "立即 finish(finding_json=[…]) 提交计划 / finish(content=问题) 问用户 / 说明唯一缺口"
        }
        ConvergeMode::DirectEdit => {
            "立即 file_read 目标文件后 edit_file 直接实施 / finish(content=问题) 问用户 / 说明唯一缺口"
        }
        ConvergeMode::Answer => {
            "立即 finish(content=答案) 回答 / finish(content=问题) 问用户 / 说明唯一缺口"
        }
    }
}

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
/// `streak` is the caller-owned counter (consecutive *low-gain* exploration
/// turns). `reflected` tracks whether the reflection prompt has already been
/// injected this streak, so it fires once rather than every turn past the
/// threshold.
///
/// `made_discovery` is the information-gain signal: true when this turn's
/// read-only calls surfaced something new (an unread file, a further slice, a
/// fresh symbol query, a structural listing). A discovering turn is genuine
/// progress for the *low-gain* streak — that streak resets, exactly like an
/// edit. This is what keeps the budget from punishing legitimate deep
/// exploration in large projects: reading 30 *different* files never trips the
/// low-gain guard; only reading the *same* things over and over does.
///
/// `total_explore` is the caller-owned **cumulative** exploration counter for
/// this pre-implementation stretch. It increments on every pure-exploration turn
/// — discovery included — and is only cleared by real progress (edit / finish).
/// When it reaches [`TOTAL_EXPLORE_CEILING`] the guard stops regardless of gain:
/// the backstop against unbounded breadth-first wandering that the low-gain
/// streak alone cannot catch.
pub fn evaluate(
    streak: &mut u32,
    reflected: &mut bool,
    total_explore: &mut u32,
    tool_names: &[String],
    had_finish: bool,
    made_discovery: bool,
    user_task: &str,
    converge: ConvergeMode,
) -> ReflectAction {
    // Real progress (edit / finish / non-exploration tool) clears BOTH counters.
    if !is_pure_exploration(tool_names, had_finish) {
        *streak = 0;
        *reflected = false;
        *total_explore = 0;
        return ReflectAction::Continue;
    }

    // This turn is pure exploration → it always counts toward the hard ceiling,
    // whether or not it discovered anything new.
    *total_explore += 1;
    if *total_explore >= TOTAL_EXPLORE_CEILING {
        // Reset so a user "continue" starts a fresh ceiling window.
        *total_explore = 0;
        *streak = 0;
        *reflected = false;
        return ReflectAction::Stop(ceiling_message(TOTAL_EXPLORE_CEILING));
    }

    // Discovery resets only the low-gain (circling) streak, not the ceiling.
    if made_discovery {
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
        return ReflectAction::Reflect(reflect_message(*streak, user_task, converge));
    }

    ReflectAction::Continue
}

/// Implementation-phase reflection: catch the "confirmed the plan, then drifted
/// back into read-after-read instead of editing" loop.
///
/// Unlike [`evaluate`], this is scoped to the implementation phase and fires on
/// consecutive NON-EDIT turns — any turn without a progress tool
/// (`file_write` / `edit_file` / `delete_range`) increments the streak; any edit
/// resets it. `finish` also resets (the model chose to end, not drift).
///
/// Returns [`ReflectAction::Reflect`] exactly once per streak at
/// [`IMPL_REFLECT_AT`]; never escalates to `Stop` (implementation should push
/// toward acting, not hand back to the user).
pub fn evaluate_impl(
    streak: &mut u32,
    reflected: &mut bool,
    tool_names: &[String],
    had_finish: bool,
    user_task: &str,
) -> ReflectAction {
    let made_progress = had_finish
        || tool_names
            .iter()
            .any(|t| PROGRESS_TOOLS.contains(&t.as_str()));

    if made_progress {
        *streak = 0;
        *reflected = false;
        return ReflectAction::Continue;
    }

    *streak += 1;

    if !*reflected && *streak >= IMPL_REFLECT_AT {
        *reflected = true;
        return ReflectAction::Reflect(impl_reflect_message(*streak, user_task));
    }

    ReflectAction::Continue
}

fn impl_reflect_message(streak: u32, user_task: &str) -> String {
    let task: String = user_task.chars().take(300).collect();
    format!(
        "🛠️ **实施反思检查点**：已进入实施阶段，但你连续 {streak} 轮没有动手改代码（无 edit_file / file_write / delete_range）。\n\
         \n\
         计划已确认，现在的目标是**改代码**，不是重新分析。请**立即**做以下之一：\n\
         \n\
         1. **直接改** — 对计划内的文件 `file_read`（每文件仅一次）后立刻 `edit_file`。\n\
         \n\
         2. **改完了 → 收尾** — `finish(content=...)` 说明改动与验证结果。\n\
         \n\
         3. **遇到计划外阻碍 → 问用户** — `finish(content=你的问题)`，不要自己扩大范围或反复泛读。\n\
         \n\
         原始任务：{task}"
    )
}

fn reflect_message(streak: u32, user_task: &str, converge: ConvergeMode) -> String {
    let task: String = user_task.chars().take(300).collect();
    let header = format!(
        "🪞 **反思检查点**：你已连续 {streak} 轮探索却没有新发现（在重复读/重复搜同样的内容），也没有收尾。\n"
    );
    let body = match converge {
        ConvergeMode::SubmitPlan => {
            "\n\
             注意：审查/评审阶段写权限是锁的，收敛动作是**提交计划**而非直接改代码。请**立即**做以下之一：\n\
             \n\
             1. **信息够了 → 提交计划** — `finish(finding_json=[{index,severity,file,issue,recommendation,fix_plan}])`，等用户 `c` 确认后再实施。\n\
             \n\
             2. **不确定 → 问用户** — 业务逻辑、命名意图、方案选择不明确时，直接 `finish(content=你的问题)` 问用户。不要自己猜。\n\
             \n\
             3. **真就差一个具体信息 → 只补那一个文件** — 说出缺什么，读完立即回头提交计划。禁止再泛读。\n"
        }
        ConvergeMode::DirectEdit => {
            "\n\
             注意：本任务写权限已解锁，收敛动作是**直接动手改代码**，不是再提交计划或反复泛读。请**立即**做以下之一：\n\
             \n\
             1. **信息够了 → 直接改** — 对目标文件 `file_read`（每文件仅一次）后立刻 `edit_file` / `file_write` 实施。\n\
             \n\
             2. **不确定 → 问用户** — 业务逻辑、命名意图、方案选择不明确时，直接 `finish(content=你的问题)` 问用户。不要自己猜。\n\
             \n\
             3. **真就差一个具体信息 → 只补那一个文件** — 说出缺什么，读完立即动手。禁止再泛读。\n"
        }
        ConvergeMode::Answer => {
            "\n\
             注意：这是问答/解释任务，收敛动作是**直接回答**，不需要全面探索。请**立即**做以下之一：\n\
             \n\
             1. **信息够了 → 回答** — `finish(content=你的答案)`，基于已读到的内容作答。\n\
             \n\
             2. **不确定 → 问用户** — 需求不明确时，直接 `finish(content=你的澄清问题)`。\n\
             \n\
             3. **真就差一个具体信息 → 只核对那一处** — 说出缺什么，单次读取后立即回答。禁止再泛读。\n"
        }
    };
    format!("{header}{body}\n原始任务：{task}")
}

fn stop_message(streak: u32) -> String {
    format!(
        "## Failed\n已连续 {streak} 次只探索不动手，反思后仍未收敛 — 停止本轮，交给你判断。\n\
         可能是任务范围不清、缺少关键信息，或方向需要你确认。请补充指示或缩小范围。"
    )
}

fn ceiling_message(ceiling: u32) -> String {
    format!(
        "## Failed\n累计已探索 {ceiling} 轮（即便一直在读新文件）仍未开始动手或收尾 — 停止本轮。\n\
         这通常意味着在做广度漫游而非聚焦目标。请缩小范围、明确要改什么，或直接给出下一步指示。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn gauge_hidden_at_zero_streak() {
        assert_eq!(budget_gauge(0, 0, 0, false, ConvergeMode::SubmitPlan), "");
        assert_eq!(budget_gauge(0, 0, 0, true, ConvergeMode::SubmitPlan), "");
    }

    #[test]
    fn gauge_shows_remaining_budget_before_threshold() {
        let g = budget_gauge(2, 2, 0, false, ConvergeMode::SubmitPlan);
        assert!(g.contains("2/"));
        assert!(g.contains("无新发现"));
        assert!(g.contains(&format!("再 {} 轮空转", REFLECT_AT - 2)));
        // Review-mode convergence action is submitting a plan.
        assert!(g.contains("finding_json"));
    }

    #[test]
    fn gauge_direct_edit_mode_says_edit_not_plan() {
        // A fix/general task must be told to edit directly, never to submit a plan.
        let g = budget_gauge(2, 2, 0, false, ConvergeMode::DirectEdit);
        assert!(g.contains("edit_file"));
        assert!(!g.contains("finding_json"));
    }

    #[test]
    fn gauge_warns_past_threshold() {
        let g = budget_gauge(REFLECT_AT, REFLECT_AT, 0, false, ConvergeMode::SubmitPlan);
        assert!(g.contains("超阈值"));
        assert!(g.contains("交还用户"));
    }

    #[test]
    fn gauge_shows_cumulative_ceiling_even_without_low_gain_streak() {
        // Discovering every turn keeps explore_streak at 0, but the cumulative
        // line must still show so breadth-first wandering stays visible.
        let g = budget_gauge(0, 8, 0, false, ConvergeMode::SubmitPlan);
        assert!(g.contains("累计探索"));
        assert!(g.contains(&format!("8/{TOTAL_EXPLORE_CEILING}")));
        assert!(!g.contains("无新发现")); // no low-gain line
    }

    #[test]
    fn gauge_impl_phase_uses_impl_streak() {
        let g = budget_gauge(0, 0, 2, true, ConvergeMode::DirectEdit);
        assert!(g.contains(&format!("2/{IMPL_REFLECT_AT}")));
        assert!(g.contains("未改代码"));
    }

    #[test]
    fn edit_breaks_streak() {
        assert!(!is_pure_exploration(&names(&["edit_file"]), false));
        assert!(!is_pure_exploration(
            &names(&["file_read", "edit_file"]),
            false
        ));
    }

    #[test]
    fn finish_is_not_exploration() {
        assert!(!is_pure_exploration(&names(&["file_read"]), true));
    }

    #[test]
    fn pure_reads_are_exploration() {
        assert!(is_pure_exploration(
            &names(&["file_read", "find_symbol"]),
            false
        ));
    }

    #[test]
    fn reflects_at_threshold_then_stops() {
        let mut streak = 0;
        let mut reflected = false;
        let mut total = 0;
        let reads = names(&["file_read"]);
        // Turns 1..REFLECT_AT-1: just continue. made_discovery=false → low-gain.
        for _ in 0..(REFLECT_AT - 1) {
            assert_eq!(
                evaluate(
                    &mut streak,
                    &mut reflected,
                    &mut total,
                    &reads,
                    false,
                    false,
                    "task",
                    ConvergeMode::SubmitPlan
                ),
                ReflectAction::Continue
            );
        }
        // Turn REFLECT_AT: reflect.
        match evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &reads,
            false,
            false,
            "task",
            ConvergeMode::SubmitPlan,
        ) {
            ReflectAction::Reflect(_) => {}
            other => panic!("expected Reflect, got {other:?}"),
        }
        // Continues until the stop threshold.
        for _ in 0..(STOP_AFTER_REFLECT - 1) {
            assert_eq!(
                evaluate(
                    &mut streak,
                    &mut reflected,
                    &mut total,
                    &reads,
                    false,
                    false,
                    "task",
                    ConvergeMode::SubmitPlan
                ),
                ReflectAction::Continue
            );
        }
        match evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &reads,
            false,
            false,
            "task",
            ConvergeMode::SubmitPlan,
        ) {
            ReflectAction::Stop(_) => {}
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn reflect_message_direct_edit_pushes_edit_not_plan() {
        // Fix/general convergence must push editing, never plan submission.
        let mut streak = 0;
        let mut reflected = false;
        let mut total = 0;
        let reads = names(&["file_read"]);
        for _ in 0..(REFLECT_AT - 1) {
            evaluate(
                &mut streak,
                &mut reflected,
                &mut total,
                &reads,
                false,
                false,
                "task",
                ConvergeMode::DirectEdit,
            );
        }
        match evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &reads,
            false,
            false,
            "task",
            ConvergeMode::DirectEdit,
        ) {
            ReflectAction::Reflect(msg) => {
                assert!(msg.contains("edit_file"));
                assert!(!msg.contains("finding_json"));
            }
            other => panic!("expected Reflect, got {other:?}"),
        }
    }

    #[test]
    fn discovery_resets_low_gain_but_ceiling_still_trips() {
        // Reading NEW files every turn keeps the low-gain streak at 0, but the
        // cumulative ceiling must eventually stop the breadth-first wander.
        let mut streak = 0;
        let mut reflected = false;
        let mut total = 0;
        let reads = names(&["file_read"]);
        // Up to the ceiling minus one: always Continue, low-gain streak stays 0.
        for _ in 0..(TOTAL_EXPLORE_CEILING - 1) {
            assert_eq!(
                evaluate(
                    &mut streak,
                    &mut reflected,
                    &mut total,
                    &reads,
                    false,
                    true,
                    "task",
                    ConvergeMode::SubmitPlan
                ),
                ReflectAction::Continue
            );
            assert_eq!(streak, 0);
        }
        // The ceiling turn stops regardless of discovery.
        match evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &reads,
            false,
            true,
            "task",
            ConvergeMode::SubmitPlan,
        ) {
            ReflectAction::Stop(_) => {}
            other => panic!("expected Stop at ceiling, got {other:?}"),
        }
        // Counters reset so a user "continue" starts a fresh window.
        assert_eq!(total, 0);
    }

    #[test]
    fn discovery_midway_resets_low_gain_streak() {
        let mut streak = 0;
        let mut reflected = false;
        let mut total = 0;
        let reads = names(&["file_read"]);
        // Two low-gain turns build the streak.
        evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &reads,
            false,
            false,
            "task",
            ConvergeMode::SubmitPlan,
        );
        evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &reads,
            false,
            false,
            "task",
            ConvergeMode::SubmitPlan,
        );
        assert_eq!(streak, 2);
        // A discovering turn wipes the low-gain streak but not the total.
        assert_eq!(
            evaluate(
                &mut streak,
                &mut reflected,
                &mut total,
                &reads,
                false,
                true,
                "task",
                ConvergeMode::SubmitPlan
            ),
            ReflectAction::Continue
        );
        assert_eq!(streak, 0);
        assert_eq!(total, 3);
    }

    #[test]
    fn edit_clears_cumulative_ceiling() {
        let mut streak = 0;
        let mut reflected = false;
        let mut total = 0;
        let reads = names(&["file_read"]);
        for _ in 0..5 {
            evaluate(
                &mut streak,
                &mut reflected,
                &mut total,
                &reads,
                false,
                true,
                "task",
                ConvergeMode::SubmitPlan,
            );
        }
        assert_eq!(total, 5);
        // An edit is real progress → clears the cumulative counter too.
        evaluate(
            &mut streak,
            &mut reflected,
            &mut total,
            &names(&["edit_file"]),
            false,
            false,
            "task",
            ConvergeMode::SubmitPlan,
        );
        assert_eq!(total, 0);
    }

    #[test]
    fn progress_resets_after_reflect() {
        let mut streak = 0;
        let mut reflected = false;
        let mut total = 0;
        let reads = names(&["file_read"]);
        for _ in 0..REFLECT_AT {
            evaluate(
                &mut streak,
                &mut reflected,
                &mut total,
                &reads,
                false,
                false,
                "task",
                ConvergeMode::SubmitPlan,
            );
        }
        assert!(reflected);
        // An edit resets everything.
        assert_eq!(
            evaluate(
                &mut streak,
                &mut reflected,
                &mut total,
                &names(&["edit_file"]),
                false,
                false,
                "task",
                ConvergeMode::SubmitPlan
            ),
            ReflectAction::Continue
        );
        assert_eq!(streak, 0);
        assert!(!reflected);
    }

    #[test]
    fn impl_reflects_after_three_non_edit_turns() {
        let mut streak = 0;
        let mut reflected = false;
        let reads = names(&["file_read"]);
        // Turns 1..IMPL_REFLECT_AT-1: continue.
        for _ in 0..(IMPL_REFLECT_AT - 1) {
            assert_eq!(
                evaluate_impl(&mut streak, &mut reflected, &reads, false, "task"),
                ReflectAction::Continue
            );
        }
        // Turn IMPL_REFLECT_AT: reflect once.
        match evaluate_impl(&mut streak, &mut reflected, &reads, false, "task") {
            ReflectAction::Reflect(_) => {}
            other => panic!("expected Reflect, got {other:?}"),
        }
        // Does not fire again on the next non-edit turn.
        assert_eq!(
            evaluate_impl(&mut streak, &mut reflected, &reads, false, "task"),
            ReflectAction::Continue
        );
    }

    #[test]
    fn impl_edit_resets_streak() {
        let mut streak = 0;
        let mut reflected = false;
        let reads = names(&["file_read"]);
        for _ in 0..(IMPL_REFLECT_AT - 1) {
            evaluate_impl(&mut streak, &mut reflected, &reads, false, "task");
        }
        // An edit before the threshold resets the streak — no reflection.
        assert_eq!(
            evaluate_impl(
                &mut streak,
                &mut reflected,
                &names(&["edit_file"]),
                false,
                "task"
            ),
            ReflectAction::Continue
        );
        assert_eq!(streak, 0);
        assert!(!reflected);
    }

    #[test]
    fn impl_finish_resets_streak() {
        let mut streak = 0;
        let mut reflected = false;
        let reads = names(&["file_read"]);
        for _ in 0..(IMPL_REFLECT_AT - 1) {
            evaluate_impl(&mut streak, &mut reflected, &reads, false, "task");
        }
        // A finishing turn counts as progress and resets.
        assert_eq!(
            evaluate_impl(&mut streak, &mut reflected, &reads, true, "task"),
            ReflectAction::Continue
        );
        assert_eq!(streak, 0);
    }
}
