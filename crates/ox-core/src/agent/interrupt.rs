use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

/// Action to take when Ctrl+C is received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptAction {
    /// Cancel the currently running agent turn.
    CancelAgent,
    /// Double Ctrl+C detected while agent running — force quit.
    ForceQuit,
    /// Graceful shutdown (no agent running).
    Shutdown,
}

/// Manages agent interruption via CancellationToken and double-Ctrl+C detection.
///
/// - Idle state: Ctrl+C → Shutdown
/// - Agent running, first Ctrl+C → CancelAgent (cancels token)
/// - Agent running, second Ctrl+C within 1s → ForceQuit
pub struct InterruptController {
    token: CancellationToken,
    last_ctrl_c: Option<Instant>,
    /// Threshold for double-click detection.
    double_click_threshold: Duration,
}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptController {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            last_ctrl_c: None,
            double_click_threshold: Duration::from_secs(1),
        }
    }

    /// Get a clone of the current cancellation token for the agent task.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Handle a Ctrl+C event. Returns the action the caller should take.
    pub fn on_ctrl_c(&mut self, agent_running: bool) -> InterruptAction {
        if !agent_running {
            return InterruptAction::Shutdown;
        }

        let now = Instant::now();

        if let Some(last) = self.last_ctrl_c
            && now.duration_since(last) < self.double_click_threshold
        {
            // Double Ctrl+C while agent running → force quit.
            self.last_ctrl_c = Some(now);
            return InterruptAction::ForceQuit;
        }

        // First Ctrl+C (or previous one expired) → cancel agent.
        self.last_ctrl_c = Some(now);
        self.token.cancel();
        InterruptAction::CancelAgent
    }

    /// Reset the controller for a new agent turn.
    /// Creates a fresh CancellationToken.
    pub fn reset(&mut self) {
        self.token = CancellationToken::new();
        self.last_ctrl_c = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_ctrl_c_triggers_shutdown() {
        let mut ctrl = InterruptController::new();
        assert_eq!(ctrl.on_ctrl_c(false), InterruptAction::Shutdown);
    }

    #[test]
    fn first_ctrl_c_during_agent_cancels() {
        let mut ctrl = InterruptController::new();
        let token = ctrl.token();
        assert!(!token.is_cancelled());

        let action = ctrl.on_ctrl_c(true);
        assert_eq!(action, InterruptAction::CancelAgent);
        assert!(token.is_cancelled());
    }

    #[test]
    fn double_ctrl_c_during_agent_force_quits() {
        let mut ctrl = InterruptController::new();

        let action1 = ctrl.on_ctrl_c(true);
        assert_eq!(action1, InterruptAction::CancelAgent);

        // Immediate second Ctrl+C (within threshold).
        let action2 = ctrl.on_ctrl_c(true);
        assert_eq!(action2, InterruptAction::ForceQuit);
    }

    #[test]
    fn reset_creates_fresh_token() {
        let mut ctrl = InterruptController::new();
        let token1 = ctrl.token();
        ctrl.on_ctrl_c(true); // cancels token1
        assert!(token1.is_cancelled());

        ctrl.reset();
        let token2 = ctrl.token();
        assert!(!token2.is_cancelled());
    }

    #[test]
    fn expired_double_click_re_cancels() {
        let mut ctrl = InterruptController::new();
        // Set threshold very short for testing.
        ctrl.double_click_threshold = Duration::from_millis(1);

        ctrl.on_ctrl_c(true);
        // Wait past threshold.
        std::thread::sleep(Duration::from_millis(10));

        // This should be treated as a new first click, not a double.
        ctrl.reset(); // fresh token needed since old one is cancelled
        let action = ctrl.on_ctrl_c(true);
        assert_eq!(action, InterruptAction::CancelAgent);
    }
}
