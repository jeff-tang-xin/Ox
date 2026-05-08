//! Feedback middleware for implicit and explicit feedback detection.
//!
//! Handles override detection, EMA tracking, and satisfaction metrics.

use std::sync::Arc;
use ox_core::feedback::{ImplicitFeedback, OverrideSignal};
use crate::terminal::app::App;

/// Process implicit feedback signals from file override detection.
pub fn process_implicit_feedback(
    app: &mut App,
    override_signals: &[OverrideSignal],
) {
    use ox_core::feedback::map_override_to_feedback;

    for signal in override_signals {
        if let Some(feedback) = map_override_to_feedback(signal.change_ratio) {
            match feedback {
                ImplicitFeedback::WeakNegative => {
                    tracing::debug!(
                        "[IMPLICIT FEEDBACK] Minor change: {:?} ({:.1}%)",
                        signal.path,
                        signal.change_ratio * 100.0
                    );
                }
                ImplicitFeedback::StrongNegative => {
                    tracing::info!(
                        "[IMPLICIT FEEDBACK] Major rewrite: {:?} ({:.1}%)",
                        signal.path,
                        signal.change_ratio * 100.0
                    );
                }
                ImplicitFeedback::VeryStrongNegative => {
                    tracing::warn!("[IMPLICIT FEEDBACK] File deleted: {:?}", signal.path);
                }
            }
        } else {
            // No significant change (<5%) - count as acceptance
            app.accepted_file_writes += 1;
            tracing::debug!(
                "[IMPLICIT FEEDBACK] Accepted: {:?} (change: {:.1}%)",
                signal.path,
                signal.change_ratio * 100.0
            );
        }
    }
}

/// Update EMA tracker with current accept rate and persist periodically.
#[allow(dead_code)]
pub fn update_feedback_metrics(app: &mut App, memory_arc: &Arc<ox_core::memory::MemoryManager>) {
    if app.total_file_writes == 0 {
        return;
    }

    let accept_rate = app.ema_manager.calculate_accept_rate(
        app.total_file_writes,
        app.accepted_file_writes,
    );

    // Persist EMA state periodically (every 10 writes)
    if app.total_file_writes % 10 == 0 {
        let store_clone = memory_arc.overall_store().clone();
        let metric_name = "code_accept_rate".to_string();
        let ema_clone = app.ema_manager.clone();

        tokio::spawn(async move {
            if let Err(e) = ema_clone.persist_to_store(&metric_name, &store_clone) {
                tracing::warn!("Failed to persist EMA state: {}", e);
            }
        });
    }

    tracing::debug!(
        "[FEEDBACK METRICS] accept_rate={:.2}, total={}, accepted={}",
        accept_rate,
        app.total_file_writes,
        app.accepted_file_writes
    );
}

/// Detect feedback keywords in user text during confirmation steps.
#[allow(dead_code)]
pub fn detect_feedback_keywords(text: &str) -> bool {
    text.contains("修改")
        || text.contains("改")
        || text.contains("调整")
        || text.contains("优化")
        || text.contains("不对")
        || text.contains("错误")
        || text.to_lowercase().contains("revise")
        || text.to_lowercase().contains("modify")
        || text.to_lowercase().contains("change")
        || text.to_lowercase().contains("update")
}
