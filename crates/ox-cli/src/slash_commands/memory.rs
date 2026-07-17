use crate::slash_commands::{CommandMeta, CommandResult};
/// Memory management commands: /remember, /forget, /memory
///
/// KnowledgeEngine has been removed; these commands now display an
/// informational message so users aren't confused by disappearing entries.
use crate::terminal::app::App as AppState;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const REMEMBER_COMMAND: CommandMeta = CommandMeta {
    name: "remember",
    aliases: &["mem"],
    description: "(deprecated) Memory engine removed — use /skill or a project skill file",
    handler: handle_remember,
};

pub const FORGET_COMMAND: CommandMeta = CommandMeta {
    name: "forget",
    aliases: &[],
    description: "(deprecated) Memory engine removed",
    handler: handle_forget,
};

pub const MEMORY_COMMAND: CommandMeta = CommandMeta {
    name: "memory",
    aliases: &["memories"],
    description: "(deprecated) Memory engine removed — use /skill list",
    handler: handle_memory,
};

fn deprecated_notice(app: &mut AppState) {
    app.output.push_system(
        "ℹ️ Memory engine (KnowledgeEngine) has been removed. \
         Persistent knowledge now lives in project skills — use `/skill` commands \
         (e.g. /skill new, /skill list) or write directly under .ox/skills/.",
    );
}

pub fn handle_remember(
    app: &mut AppState,
    _args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    deprecated_notice(app);
    CommandResult::Success
}

pub fn handle_forget(
    app: &mut AppState,
    _args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    deprecated_notice(app);
    CommandResult::Success
}

pub fn handle_memory(
    app: &mut AppState,
    _args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    deprecated_notice(app);
    CommandResult::Success
}
