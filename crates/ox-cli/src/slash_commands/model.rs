use crate::slash_commands::{CommandMeta, CommandResult};
/// Model management commands: /model
use crate::terminal::app::App as AppState;
use crate::terminal::output_pane::OutputLine;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const MODEL_COMMAND: CommandMeta = CommandMeta {
    name: "model",
    aliases: &["m"],
    description: "Switch or show current model: /model [model_name]",
    handler: handle_model,
};

pub fn handle_model(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let model_name = args.trim();
    if !model_name.is_empty() {
        app.pending_model_switch = Some(model_name.to_string());
        app.output
            .push_line(OutputLine::System(format!("Switching to: {}", model_name)));
    } else {
        app.output.push_line(OutputLine::System(format!(
            "Current model: {}",
            app.model_name
        )));
    }
    CommandResult::Success
}
