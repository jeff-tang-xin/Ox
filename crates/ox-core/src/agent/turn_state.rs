//! Typed per-turn state -- the P1 scaffold of the incremental rewrite.
//!
//! Historically the agent loop tracked its budget through a dozen loose locals
//! in `run_agent_turn` plus a string-typed `_total_explore` engine variable
//! (`parse::<u32>().unwrap_or(0)` on read, `to_string()` on write). That
//! scattered the turn's control state across the 3800-line function and made
//! the exhaustion rule impossible to unit-test in isolation.
//!
//! [`TurnBudget`] consolidates those counters into a typed value. It follows
//! two rules established during the rewrite:
//!
//! 1. It holds only mutable counts, never thresholds. The exploration ceiling
//!    is dynamic: [`ConvergeMode::ceiling`] returns a different value per task
//!    mode (Answer=6, DirectEdit=10, SubmitPlan=12). Duplicating those numbers
//!    here would create a second source of truth, so exhaustion checks delegate
//!    to [`ConvergeMode`] instead.
//! 2. Its transitions are pure functions -- `on_explore` and `on_edit_or_finish`
//!    mutate counters with no I/O, so they are trivially testable.
//!
//! Wired into `run_agent_turn` as the `budget` local (P5.1), replacing 6
//! previously loose locals (`explore_streak`, `explore_reflected`,
//! `total_explore`, `impl_streak`, `impl_reflected`, and `content_only_streak`).
//! `iteration` remains a separate local because it is too pervasive to rename.
//! `RepeatGuard` and `tools_used_this_turn` stay separate because they are not
//! plain counters.

use crate::agent::gate::explore_reflect::ConvergeMode;

/// Which half of the turn lifecycle the agent is in.
///
/// Exploration budgets and implementation budgets have different cadences: the
/// exploration ceiling is generous (breadth-first investigation is expected),
/// while the implementation phase is tight (once the plan is confirmed, reading
/// instead of editing is the failure mode we want to catch quickly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    /// Reading / searching / analysing before a confirmed plan or edit.
    Explore,
    /// Executing a confirmed plan -- edits expected, drifting back into
    /// read-after-read is what triggers implementation reflection.
    Implement,
}

/// Mutable per-turn counters, consolidated from the loose locals in
/// `run_agent_turn`. Thresholds live elsewhere (see module docs); this struct
/// never stores a ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnBudget {
    /// Total tool-call iterations this turn.
    pub iteration: u32,
    /// Consecutive read-only exploration tool calls (resets on edit/finish).
    pub explore_streak: u32,
    /// Cumulative exploration count -- the hard-ceiling counter. Persisted
    /// across turns; only a real edit/finish resets it. Compared against
    /// [`ConvergeMode::ceiling`], never a local constant.
    pub total_explore: u32,
    /// Consecutive no-edit turns during the implementation phase.
    pub impl_streak: u32,
    /// Consecutive `unified_action` parse failures.
    pub unified_parse_error_streak: u32,
    /// Consecutive findings-delivery failures.
    pub findings_deliver_error_streak: u32,
    /// Consecutive bounded API-error recoveries (e.g. ARK 400 body trim+retry).
    pub api_error_recovery_streak: u32,
    /// Whether an exploration-reflection prompt already fired this turn.
    pub explore_reflected: bool,
    /// Whether an implementation-reflection prompt already fired this turn.
    pub impl_reflected: bool,
}

impl TurnBudget {
    /// Fresh budget with every counter at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restore only the cumulative, cross-turn counter (`total_explore`) from a
    /// previously-persisted value, leaving per-turn counters at zero. Used when
    /// loading state at the start of a turn that continues an existing task.
    pub fn with_total_explore(total_explore: u32) -> Self {
        Self {
            total_explore,
            ..Self::default()
        }
    }

    /// Record a read-only exploration tool call: bump both the per-turn streak
    /// and the cumulative ceiling counter.
    pub fn on_explore(&mut self) {
        self.explore_streak = self.explore_streak.saturating_add(1);
        self.total_explore = self.total_explore.saturating_add(1);
    }

    /// Record real progress (an edit or `finish`): clear the exploration streaks
    /// and the cumulative ceiling counter. This is the only thing that resets
    /// `total_explore`, matching the existing `evaluate()` contract.
    pub fn on_edit_or_finish(&mut self) {
        self.explore_streak = 0;
        self.total_explore = 0;
        self.explore_reflected = false;
    }

    /// Whether cumulative exploration has hit the ceiling for `mode`. Delegates
    /// to [`ConvergeMode::ceiling`] so the threshold has a single source of
    /// truth in `explore_reflect`.
    pub fn explore_exhausted(&self, mode: ConvergeMode) -> bool {
        self.total_explore >= mode.ceiling()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_task_starts_at_zero() {
        let b = TurnBudget::new();
        assert_eq!(b.total_explore, 0);
        assert_eq!(b.explore_streak, 0);
        assert!(!b.explore_reflected);
    }

    #[test]
    fn with_total_explore_restores_only_cumulative() {
        let b = TurnBudget::with_total_explore(5);
        assert_eq!(b.total_explore, 5);
        assert_eq!(b.explore_streak, 0);
        assert_eq!(b.iteration, 0);
    }

    #[test]
    fn on_explore_accumulates_both_counters() {
        let mut b = TurnBudget::new();
        b.on_explore();
        b.on_explore();
        assert_eq!(b.explore_streak, 2);
        assert_eq!(b.total_explore, 2);
    }

    #[test]
    fn on_edit_or_finish_resets_exploration() {
        let mut b = TurnBudget::with_total_explore(9);
        b.explore_streak = 4;
        b.explore_reflected = true;
        b.on_edit_or_finish();
        assert_eq!(b.total_explore, 0);
        assert_eq!(b.explore_streak, 0);
        assert!(!b.explore_reflected);
    }

    #[test]
    fn exhaustion_never_triggers() {
        // 不再有硬限制，持续探索也不会耗尽
        let mut b = TurnBudget::new();
        for _ in 0..100 {
            b.on_explore();
        }
        assert!(!b.explore_exhausted(ConvergeMode::Answer));
        assert!(!b.explore_exhausted(ConvergeMode::DirectEdit));
        assert!(!b.explore_exhausted(ConvergeMode::SubmitPlan));
    }

    #[test]
    fn exhaustion_never_triggers_even_after_many() {
        let mut b = TurnBudget::new();
        for _ in 0..1000 {
            b.on_explore();
        }
        // 即使 1000 轮也不会触发
        assert!(!b.explore_exhausted(ConvergeMode::SubmitPlan));
    }
}
