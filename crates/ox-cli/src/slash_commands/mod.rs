/// Slash Command Registry - Pluggable command system
///
/// Each slash command is implemented as an independent module.

use std::collections::HashMap;
use crate::terminal::app::App as AppState;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::safety::TrustManager;
use std::sync::Arc;

/// Command handler function signature
pub type CommandHandler = fn(
    app: &mut AppState,
    args: &str,
    session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    config: &OxConfig,
    cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult;

/// Command execution result
#[derive(Debug)]
pub enum CommandResult {
    /// Command executed successfully (no further action needed)
    Success,
    /// Command requires async processing (handled in main loop)
    AsyncPending,
    /// Command needs LLM to generate content
    /// Contains: (prompt, callback_description)
    LlmRequest {
        prompt: String,
        description: String,
        /// Skip 4-step workflow (first-time project onboarding).
        skip_workflow: bool,
    },
    /// Command failed with error message
    Error(String),
    /// Unknown command
    Unknown(String),
}

/// Command metadata
pub struct CommandMeta {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub handler: CommandHandler,
}

/// Global command registry
pub struct CommandRegistry {
    commands: HashMap<String, CommandMeta>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            commands: HashMap::new(),
        };
        
        // Register all commands
        register_builtin_commands(&mut registry);
        
        registry
    }
    
    /// Register a command
    pub fn register(&mut self, meta: CommandMeta) {
        let name = meta.name.to_string();
        self.commands.insert(name, meta);
    }
    
    pub fn get_command(&self, cmd: &str) -> Option<&CommandMeta> {
        // Try exact match first
        if let Some(meta) = self.commands.get(cmd) {
            return Some(meta);
        }
        
        // Try aliases
        for meta in self.commands.values() {
            if meta.aliases.contains(&cmd) {
                return Some(meta);
            }
        }
        
        None
    }
    
    pub fn list_commands(&self) -> Vec<&CommandMeta> {
        self.commands.values().collect()
    }
}

/// Register all builtin commands
fn register_builtin_commands(registry: &mut CommandRegistry) {
    // Help commands
    registry.register(help::HELP_COMMAND);
    
    // Session management
    registry.register(session::NEW_COMMAND);
    registry.register(session::RESUME_COMMAND);
    registry.register(session::SESSIONS_COMMAND);
    registry.register(session::CLEAN_COMMAND);
    registry.register(session::CLEAR_CACHE_COMMAND);
    
    // Model management
    registry.register(model::MODEL_COMMAND);
    
    // Trust management
    registry.register(trust::TRUST_COMMAND);
    registry.register(trust::UNTRUST_COMMAND);
    registry.register(trust::BLOCK_COMMAND);
    registry.register(trust::UNBLOCK_COMMAND);
    
    // Memory management
    registry.register(memory::REMEMBER_COMMAND);
    registry.register(memory::FORGET_COMMAND);
    registry.register(memory::MEMORY_COMMAND);
    
    // Feedback
    registry.register(feedback::FEEDBACK_COMMAND);
    
    // System commands
    registry.register(system::EXIT_COMMAND);
    registry.register(system::CD_COMMAND);
    registry.register(system::INIT_COMMAND);
    registry.register(system::DEBUG_COMMAND);
    registry.register(system::COST_COMMAND);
    registry.register(system::PLAN_COMMAND);
    registry.register(system::RELOAD_COMMAND);
    registry.register(system::CANCEL_COMMAND);
    registry.register(system::CLEAR_COMMAND);
    
    // Skill management
    registry.register(skill::SKILL_COMMAND);
    
    // Index management
    registry.register(index::INDEX_COMMAND);
}

// Import command modules (removed spec, council, workflow)
mod help;
mod session;
mod model;
mod trust;
mod memory;
mod feedback;
mod system;
mod skill;
mod index;
