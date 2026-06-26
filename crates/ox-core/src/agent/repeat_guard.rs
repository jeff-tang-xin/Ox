//! Repeat-output guard — catches the degenerate loop where the model emits the
//! same reasoning/visible text turn after turn without making progress.
//!
//! Distinct from the other brakes:
//! - `explore_reflect` looks at TOOL TYPES (read vs edit), not text.
//! - `read_guard` blocks duplicate file reads, not repeated prose.
//! - `should_stop_on_repeated_failure` counts verify failures, not sameness.
//!
//! This one compares the CONTENT of consecutive turns. No embeddings (Ox has
//! none) — normalized string equality plus a cheap word-overlap (Jaccard) ratio.

/// Consecutive near-identical turns before we intervene with a break-the-loop nudge.
pub const NUDGE_AT: u32 = 2;
/// Consecutive near-identical turns before we stop and hand back to the user.
pub const STOP_AT: u32 = 4;
/// Jaccard word-overlap ratio above which two outputs are "the same".
const SIMILAR_RATIO: f32 = 0.85;

#[derive(Debug, Default)]
pub struct RepeatGuard {
    last: String,
    streak: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepeatAction {
    /// Different enough — carry on.
    Continue,
    /// Repeated; inject this nudge to break the pattern.
    Nudge(String),
    /// Repeated past tolerance — stop and ask the user.
    Stop(String),
}

impl RepeatGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed this turn's visible output; decide what to do.
    /// Empty/whitespace output is ignored (does not affect the streak).
    pub fn observe(&mut self, visible: &str) -> RepeatAction {
        let norm = normalize(visible);
        if norm.is_empty() {
            return RepeatAction::Continue;
        }
        let similar = !self.last.is_empty() && is_similar(&self.last, &norm);
        self.last = norm;
        if !similar {
            // This output starts a fresh run — count it as occurrence #1.
            self.streak = 1;
            return RepeatAction::Continue;
        }
        self.streak += 1;
        if self.streak >= STOP_AT {
            RepeatAction::Stop(stop_message(self.streak))
        } else if self.streak >= NUDGE_AT {
            RepeatAction::Nudge(nudge_message(self.streak))
        } else {
            RepeatAction::Continue
        }
    }
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cheap similarity: exact match after normalization, or high word-set overlap.
fn is_similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let wa: std::collections::HashSet<&str> = a.split(' ').collect();
    let wb: std::collections::HashSet<&str> = b.split(' ').collect();
    if wa.is_empty() || wb.is_empty() {
        return false;
    }
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count() as f32;
    (inter / union) >= SIMILAR_RATIO
}

fn nudge_message(streak: u32) -> String {
    format!(
        "🔁 你已连续 {streak} 次输出几乎相同的思考，却没有推进。\n\
         停止重复，立即二选一：\n\
         • 直接发出一个具体动作（file_read 一个**还没读过**的文件 / edit_file / finish）；\n\
         • 如果卡住了，明确说出卡在哪、需要什么，不要再复述同一句话。"
    )
}

fn stop_message(streak: u32) -> String {
    format!(
        "## Failed\n连续 {streak} 次输出重复思考、无法推进 — 停止本轮，交给你判断。\n\
         可能是缺少关键信息或陷入了死循环。请补充指示或换个方向。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_outputs_continue() {
        let mut g = RepeatGuard::new();
        assert_eq!(g.observe("read file A"), RepeatAction::Continue);
        assert_eq!(g.observe("now edit file B with the fix"), RepeatAction::Continue);
    }

    #[test]
    fn identical_repeats_nudge_then_stop() {
        let mut g = RepeatGuard::new();
        let line = "Let me read the implementation method and the VO to understand the logic";
        assert_eq!(g.observe(line), RepeatAction::Continue); // 1st: baseline
        match g.observe(line) {
            RepeatAction::Nudge(_) => {} // 2nd: streak hits NUDGE_AT
            other => panic!("expected Nudge, got {other:?}"),
        }
        g.observe(line); // 3rd
        match g.observe(line) {
            RepeatAction::Stop(_) => {} // 4th: streak hits STOP_AT
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn near_identical_counts_as_repeat() {
        let mut g = RepeatGuard::new();
        g.observe("Let me read the implementation method and the VO to understand");
        // One word different — still > 0.85 overlap.
        match g.observe("Let me read the implementation method and the VO to understand it") {
            RepeatAction::Nudge(_) => {}
            other => panic!("expected Nudge, got {other:?}"),
        }
    }

    #[test]
    fn progress_resets_streak() {
        let mut g = RepeatGuard::new();
        let line = "same thinking over and over again here";
        g.observe(line);
        g.observe(line); // nudge
        assert_eq!(
            g.observe("completely different action: editing the mapper now"),
            RepeatAction::Continue
        );
    }

    #[test]
    fn empty_output_ignored() {
        let mut g = RepeatGuard::new();
        let line = "repeated line of thinking";
        g.observe(line);
        assert_eq!(g.observe("   "), RepeatAction::Continue);
        // Streak preserved across the empty turn.
        match g.observe(line) {
            RepeatAction::Nudge(_) => {}
            other => panic!("expected Nudge, got {other:?}"),
        }
    }
}
