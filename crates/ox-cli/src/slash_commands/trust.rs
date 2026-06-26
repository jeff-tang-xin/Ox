use crate::slash_commands::{CommandMeta, CommandResult};
/// Trust management commands: /trust, /untrust
use crate::terminal::app::App as AppState;
use crate::terminal::output_pane::OutputLine;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const TRUST_COMMAND: CommandMeta = CommandMeta {
    name: "trust",
    aliases: &[],
    description: "Trust tools: /trust <tool_name> or /trust --all",
    handler: handle_trust,
};

pub const UNTRUST_COMMAND: CommandMeta = CommandMeta {
    name: "untrust",
    aliases: &[],
    description: "Revoke all tool trust",
    handler: handle_untrust,
};

pub const BLOCK_COMMAND: CommandMeta = CommandMeta {
    name: "block",
    aliases: &[],
    description: "Block command pattern: /block <pattern> (e.g. /block rm -rf)",
    handler: handle_block,
};

pub const UNBLOCK_COMMAND: CommandMeta = CommandMeta {
    name: "unblock",
    aliases: &[],
    description: "Unblock command pattern: /unblock <pattern>",
    handler: handle_unblock,
};

pub fn handle_trust(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let args = args.trim();
    let all = args == "--all" || args == "-a";
    let tools: Vec<&str> = if all {
        vec![]
    } else {
        args.split_whitespace().collect()
    };

    let mut tm = match trust_manager.lock() {
        Ok(guard) => guard,
        Err(e) => {
            app.output.push_line(OutputLine::Error(format!(
                "Failed to lock trust manager: {}",
                e
            )));
            return CommandResult::Error("Lock failed".to_string());
        }
    };

    if all {
        tm.trust_all();
        app.trusted_all = true;
        app.output
            .push_system("Trusted all tools for this session. Use /untrust to revoke.");
    } else if tools.is_empty() {
        let list = tm.trusted_list();
        if list.is_empty() {
            app.output
                .push_system("No tools currently trusted. Use /trust <tool_name> or /trust --all.");
        } else {
            app.output
                .push_system(&format!("Trusted tools: {}", list.join(", ")));
        }
    } else {
        for tool in &tools {
            tm.trust(tool);
        }
        app.output
            .push_system(&format!("Trusted for this session: {}", tools.join(", ")));
    }

    CommandResult::Success
}

pub fn handle_untrust(
    app: &mut AppState,
    _args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    if let Ok(mut tm) = trust_manager.lock() {
        tm.untrust_all();
    }
    app.trusted_all = false;
    app.output
        .push_system("All tool trust revoked. Confirmations restored.");
    CommandResult::Success
}

pub fn handle_block(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let args = args.trim();
    if args.is_empty() {
        // Show current blacklist.
        let tm = match trust_manager.lock() {
            Ok(guard) => guard,
            Err(e) => {
                app.output.push_line(OutputLine::Error(format!(
                    "Failed to lock trust manager: {}",
                    e
                )));
                return CommandResult::Error("Lock failed".to_string());
            }
        };
        let list = tm.blacklist();
        if list.is_empty() {
            app.output
                .push_system("No blocked command patterns. Use /block <pattern> to add one.");
        } else {
            app.output.push_system(&format!(
                "Blocked patterns: {}",
                list.iter()
                    .map(|s| format!("\"{}\"", s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return CommandResult::Success;
    }
    // Add each pattern.
    let mut tm = match trust_manager.lock() {
        Ok(guard) => guard,
        Err(e) => {
            app.output.push_line(OutputLine::Error(format!(
                "Failed to lock trust manager: {}",
                e
            )));
            return CommandResult::Error("Lock failed".to_string());
        }
    };
    for pattern in args.split_whitespace() {
        tm.block_command(pattern);
    }
    app.output
        .push_system(&format!("Blocked command patterns containing: {}", args));
    CommandResult::Success
}

pub fn handle_unblock(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let args = args.trim();
    if args.is_empty() {
        app.output.push_system("Usage: /unblock <pattern>");
        return CommandResult::Success;
    }
    let mut tm = match trust_manager.lock() {
        Ok(guard) => guard,
        Err(e) => {
            app.output.push_line(OutputLine::Error(format!(
                "Failed to lock trust manager: {}",
                e
            )));
            return CommandResult::Error("Lock failed".to_string());
        }
    };
    for pattern in args.split_whitespace() {
        tm.unblock_command(pattern);
    }
    app.output
        .push_system(&format!("Unblocked command patterns: {}", args));
    CommandResult::Success
}
