/// Memory management commands: /remember, /forget, /memory

use crate::terminal::app::App as AppState;
use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::output_pane::OutputLine;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const REMEMBER_COMMAND: CommandMeta = CommandMeta {
    name: "remember",
    aliases: &["mem"],
    description: "Store a memory: /remember <content>",
    handler: handle_remember,
};

pub const FORGET_COMMAND: CommandMeta = CommandMeta {
    name: "forget",
    aliases: &[],
    description: "Delete memories matching keyword: /forget <keyword>",
    handler: handle_forget,
};

pub const MEMORY_COMMAND: CommandMeta = CommandMeta {
    name: "memory",
    aliases: &["memories"],
    description: "Memory management: /memory [stats|promote]",
    handler: handle_memory,
};

pub fn handle_remember(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let content = args.trim();
    if content.is_empty() {
        app.output.push_system("Usage: /remember <content>  (stores as Style memory)");
        return CommandResult::Success;
    }

    if let Some(ref ke) = app.knowledge_engine {
        if let Ok(mut engine) = ke.try_write() {
            let _ = engine.remember_explicit(content, &rt_env.project_id, &rt_env.project_language);
            app.output.push_system(&format!(
                "Remembered: {}",
                content.chars().take(100).collect::<String>()
            ));
        } else {
            app.output.push_system("Knowledge engine busy — try again.");
        }
    } else {
        app.output.push_system("Knowledge engine not available.");
    }
    CommandResult::Success
}

pub fn handle_forget(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let keyword = args.trim();
    if keyword.is_empty() {
        app.output.push_system("Usage: /forget <keyword>  (deletes matching memories)");
        return CommandResult::Success;
    }

    if let Some(ref ke) = app.knowledge_engine {
        if let Ok(mut engine) = ke.try_write() {
            let deleted = engine.forget_matching(keyword);
            app.output.push_system(&format!(
                "Forgot {deleted} memory(ies) matching '{keyword}'"
            ));
        } else {
            app.output.push_system("Knowledge engine busy — try again.");
        }
    } else {
        app.output.push_system("Knowledge engine not available.");
    }
    CommandResult::Success
}

pub fn handle_memory(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let subcmd = args.trim();

    match subcmd {
        "promote" | "p" => return handle_promote(app, rt_env),
        "stats" | "s" | "" => {}
        _ => {
            app.output.push_system("Usage: /memory [stats|promote]");
            app.output.push_system("  stats - Show memory statistics (default)");
            app.output.push_system("  promote - Trigger L0-L3 memory promotion pipeline");
            return CommandResult::Success;
        }
    }

    let Some(ref ke) = app.knowledge_engine else {
        app.output.push_system("Knowledge engine not available.");
        return CommandResult::Success;
    };

    if let Ok(engine) = ke.try_read() {
        let (l0, l1, l2, l3) = engine.memory_layer_counts();
        app.output.push_line(OutputLine::System(format!(
            "📚 Knowledge Engine (unified):\n  L0 working: {l0} | L1 atomic: {l1} | L2 episodic: {l2} | L3 semantic: {l3}"
        )));
        let nodes = engine.retrieve_memory_nodes("", Some(&rt_env.project_id), 8);
        if nodes.is_empty() {
            app.output.push_system("No memories in knowledge engine yet.");
        } else {
            app.output.push_system("Recent knowledge:");
            for node in &nodes {
                let preview: String = node.content.chars().take(100).collect();
                app.output.push_line(OutputLine::System(format!(
                    "  [L{}] {preview}",
                    node.depth
                )));
            }
        }
    } else {
        app.output.push_system("Knowledge engine busy — try again.");
    }

    CommandResult::Success
}

fn handle_promote(app: &mut AppState, rt_env: &mut RuntimeEnvironment) -> CommandResult {
    app.output.push_system("🚀 Starting L0-L3 memory promotion pipeline...");

    let Some(ref ke) = app.knowledge_engine else {
        app.output.push_system("Knowledge engine not available.");
        return CommandResult::Success;
    };

    if let Ok(mut engine) = ke.try_write() {
        match engine.run_consolidation("current", Some(&rt_env.project_id)) {
            Ok(n) => {
                app.output.push_system(&format!(
                    "✅ Knowledge consolidation complete — {n} new entities promoted."
                ));
            }
            Err(e) => {
                app.output.push_system(&format!("⚠️ Knowledge consolidation failed: {e}"));
            }
        }
    } else {
        app.output.push_system("Knowledge engine busy — try again.");
    }

    CommandResult::Success
}
