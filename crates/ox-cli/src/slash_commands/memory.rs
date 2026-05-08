/// Memory management commands: /remember, /forget, /memory

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
    description: "Show memory statistics and recent memories",
    handler: handle_memory,
};

pub fn handle_remember(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let content = args.trim();
    if content.is_empty() {
        app.output.push_system("Usage: /remember <content>  (stores as Style memory)");
    } else {
        memory.store_explicit(&content, &rt_env.project_id, &rt_env.project_language);
        app.output.push_system(&format!("Remembered: {}", content.chars().take(100).collect::<String>()));
    }
    CommandResult::Success
}

pub fn handle_forget(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let keyword = args.trim();
    if keyword.is_empty() {
        app.output.push_system("Usage: /forget <keyword>  (deletes matching memories)");
    } else {
        let deleted = memory.forget(&keyword, &rt_env.project_id);
        app.output.push_system(&format!("Forgot {} memory(ies) matching '{}'", deleted, keyword));
    }
    CommandResult::Success
}

pub fn handle_memory(
    app: &mut AppState,
    _args: &str,
    _session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let (project_count, overall_count) = memory.stats(&rt_env.project_id);
    
    // Get detailed learning statistics
    let stats = memory.get_learning_stats(&rt_env.project_id);
    
    app.output.push_line(OutputLine::System(format!(
        "📚 Learning Statistics:\n\
         Project memories: {} | Global memories: {}\n",
         project_count, overall_count
    )));
    
    // Show breakdown by type
    if !stats.memories_by_type.is_empty() {
        app.output.push_system("Memories by type:");
        let mut sorted_types: Vec<_> = stats.memories_by_type.iter().collect();
        sorted_types.sort_by(|a, b| b.1.cmp(a.1)); // Sort by count descending
        
        for (mem_type, count) in sorted_types {
            let icon = match *mem_type {
                "fact" => "📝",
                "style" => "🎨",
                "architectural" => "🏗️",
                "anti_pattern" => "⚠️",
                "business" => "💼",
                "best_practice" => "✅",
                "pattern" => "🔄",
                "meta_skill" => "🧠",
                _ => "📌",
            };
            app.output.push_line(OutputLine::System(format!(
                "  {} {}: {}", icon, mem_type.replace('_', " "), count
            )));
        }
        app.output.push_line(OutputLine::System(String::new()));
    }

    let nodes = memory.retrieve("", &Some(rt_env.project_id.as_str()), 8);
    if nodes.is_empty() {
        app.output.push_system("No memories found yet. Start interacting to build knowledge!");
    } else {
        app.output.push_system("Recent memories:");
        for node in &nodes {
            let scope = if node.project_id.is_some() { "project" } else { "global" };
            let content = if node.content.len() > 100 {
                format!("{}...", &node.content[..100])
            } else {
                node.content.clone()
            };
            app.output.push_line(OutputLine::System(format!(
                "  [{}] {} (depth: {})", scope, content, node.depth
            )));
        }
    }
    CommandResult::Success
}
