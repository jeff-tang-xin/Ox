//! Feedback commands: /feedback

use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::app::App as AppState;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const FEEDBACK_COMMAND: CommandMeta = CommandMeta {
    name: "feedback",
    aliases: &["fb"],
    description: "Provide feedback: /feedback <good|unsafe>",
    handler: handle_feedback,
};

pub fn handle_feedback(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let category = args.trim().to_lowercase();

    match category.as_str() {
        "good" => {
            app.good_feedback_count += 1;
            app.explicit_feedback_count += 1;
            tracing::info!("[FEEDBACK] Positive feedback recorded (counter-only, KE removed)");
            app.output.push_system("✅ Feedback noted: positive.");
        }
        "unsafe" => {
            app.output
                .push_system("🔒 Safety violation noted. Reviewing constraints.");
            tracing::warn!("[SAFETY VIOLATION] Reported by user");
        }
        _ => {
            app.output.push_system(
                "Usage: /feedback <good|unsafe>\n\nUse '/feedback good' to reinforce helpful responses.",
            );
        }
    }
    CommandResult::Success
}
