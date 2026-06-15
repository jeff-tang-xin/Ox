/// /help command
use crate::terminal::app::App as AppState;
use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::output_pane::OutputLine;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const HELP_COMMAND: CommandMeta = CommandMeta {
    name: "help",
    aliases: &["h"],
    description: "Show help information",
    handler: handle_help,
};

pub fn handle_help(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let topic = if args.is_empty() { None } else { Some(args.to_string()) };
    let text = ox_core::slash::help_text(topic.as_deref());
    for line in text.lines() {
        app.output.push_line(OutputLine::System(line.to_string()));
    }
    CommandResult::Success
}
