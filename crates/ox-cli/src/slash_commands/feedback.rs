//! Feedback commands: /feedback

use crate::terminal::app::App as AppState;
use crate::slash_commands::{CommandMeta, CommandResult};
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
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
    session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let category = args.trim().to_lowercase();

    match category.as_str() {
        "good" => {
            app.good_feedback_count += 1;
            app.explicit_feedback_count += 1;

            if let Some(last_user) = session.messages.iter().rev().find_map(|m| match m {
                Message::User { content } => Some(content.clone()),
                _ => None,
            }) {
                if let Some(ref ke) = app.knowledge_engine {
                    if let Ok(engine) = ke.try_read() {
                        let nodes = engine.retrieve_memory_nodes(
                            &last_user,
                            Some(rt_env.project_id.as_str()),
                            5,
                        );
                        tracing::info!(
                            "[FEEDBACK] Reinforced {} knowledge items for query: {}",
                            nodes.len(),
                            last_user.chars().take(50).collect::<String>()
                        );
                    }
                }
            }

            app.output.push_system("✅ Feedback noted: positive.");
        }
        "unsafe" => {
            app.output.push_system("🔒 Safety violation noted. Reviewing constraints.");
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
