//! Feedback middleware for implicit and explicit feedback detection.

use crate::terminal::app::App;

/// Process implicit feedback signals from file override detection.
pub fn process_implicit_feedback(
    app: &mut App,
    override_signals: &[ox_core::feedback::OverrideSignal],
) {
    use ox_core::feedback::map_override_to_feedback;

    for signal in override_signals {
        if let Some(feedback) = map_override_to_feedback(signal.change_ratio) {
            match feedback {
                ox_core::feedback::ImplicitFeedback::WeakNegative => {
                    tracing::debug!(
                        "[IMPLICIT FEEDBACK] Minor change: {:?} ({:.1}%)",
                        signal.path,
                        signal.change_ratio * 100.0
                    );
                }
                ox_core::feedback::ImplicitFeedback::StrongNegative => {
                    tracing::info!(
                        "[IMPLICIT FEEDBACK] Major rewrite: {:?} ({:.1}%)",
                        signal.path,
                        signal.change_ratio * 100.0
                    );
                }
                ox_core::feedback::ImplicitFeedback::VeryStrongNegative => {
                    tracing::warn!("[IMPLICIT FEEDBACK] File deleted: {:?}", signal.path);
                }
            }
        } else {
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
pub fn update_feedback_metrics(app: &mut App, metrics_path: &std::path::Path) {
    if app.total_file_writes == 0 {
        return;
    }

    let accept_rate = app.ema_manager.calculate_accept_rate(
        app.total_file_writes,
        app.accepted_file_writes,
    );

    if app.total_file_writes % 10 == 0 {
        let metric_name = "code_accept_rate".to_string();
        let ema_clone = app.ema_manager.clone();
        let path = metrics_path.to_path_buf();

        tokio::spawn(async move {
            if let Err(e) = ema_clone.persist_to_file(&metric_name, &path) {
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
