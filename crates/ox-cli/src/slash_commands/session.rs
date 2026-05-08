/// Session management commands: /new, /resume, /sessions, /clean

use crate::terminal::app::App as AppState;
use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::output_pane::OutputLine;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::config::OxConfig;
use ox_core::memory::MemoryManager;
use ox_core::cost::CostTracker;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const NEW_COMMAND: CommandMeta = CommandMeta {
    name: "new",
    aliases: &["n"],
    description: "Start a new session",
    handler: handle_new,
};

pub const RESUME_COMMAND: CommandMeta = CommandMeta {
    name: "resume",
    aliases: &[],
    description: "Resume an archived session: /resume <filename>",
    handler: handle_resume,
};

pub const SESSIONS_COMMAND: CommandMeta = CommandMeta {
    name: "sessions",
    aliases: &["ls"],
    description: "List archived sessions",
    handler: handle_sessions,
};

pub const CLEAN_COMMAND: CommandMeta = CommandMeta {
    name: "clean",
    aliases: &[],
    description: "Clear all messages from current session",
    handler: handle_clean,
};

pub fn handle_new(
    app: &mut AppState,
    _args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    app.session_action = SessionAction::New;
    CommandResult::Success
}

pub fn handle_resume(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let filename = args.trim();
    if filename.is_empty() {
        app.output.push_system("Usage: /resume <filename>  (use /sessions to list)");
    } else {
        app.session_action = SessionAction::Resume {
            filename: filename.to_string(),
        };
    }
    CommandResult::Success
}

pub fn handle_sessions(
    app: &mut AppState,
    _args: &str,
    session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let session_dir = session.dir().to_path_buf();
    let archived = Session::list_archived(&session_dir);

    if archived.is_empty() {
        app.output.push_system(
            "No archived sessions found. Use /new to start and archive sessions.",
        );
    } else {
        app.output.push_line(OutputLine::System("Archived sessions:".to_string()));
        for (i, (filename, info)) in archived.iter().enumerate() {
            app.output.push_line(OutputLine::System(format!(
                "  {}. {}  ({})",
                i + 1,
                info,
                filename
            )));
        }
        app.output.push_line(OutputLine::System(
            "Use /resume <filename> to restore a session.".to_string(),
        ));
    }
    CommandResult::Success
}

pub fn handle_clean(
    app: &mut AppState,
    _args: &str,
    session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    if let Err(e) = session.clean() {
        app.output.push_error(&format!("Failed to clean session: {}", e));
    } else {
        app.output.clear();
        app.message_count = 0;
        app.cost_summary = String::new();
        app.pending_compressed_cache_clear = true;
        app.output.push_system("Session cleared. All messages removed.");
    }
    CommandResult::Success
}

pub use crate::terminal::app::SessionAction;
