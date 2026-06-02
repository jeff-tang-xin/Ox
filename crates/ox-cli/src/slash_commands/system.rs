/// System commands: /exit, /cd, /init, /debug, /cost, /plan, /reload, /free, /cancel, /clear

use crate::terminal::app::App as AppState;
use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::app::WorkflowState;
use crate::terminal::output_pane::OutputLine;
use ox_core::message::Session;
use ox_core::runtime::{self, DirectoryChangeResult};
use ox_core::config::OxConfig;
use ox_core::memory::MemoryManager;
use ox_core::cost::CostTracker;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const EXIT_COMMAND: CommandMeta = CommandMeta {
    name: "exit", aliases: &[], description: "Exit the application", handler: handle_exit,
};
pub const CD_COMMAND: CommandMeta = CommandMeta {
    name: "cd", aliases: &[], description: "Change directory: /cd [path]", handler: handle_cd,
};
pub const INIT_COMMAND: CommandMeta = CommandMeta {
    name: "init", aliases: &[], description: "Initialize default config file", handler: handle_init,
};
pub const DEBUG_COMMAND: CommandMeta = CommandMeta {
    name: "debug", aliases: &["dbg"], description: "Show debug information", handler: handle_debug,
};
pub const COST_COMMAND: CommandMeta = CommandMeta {
    name: "cost", aliases: &[], description: "Show cost statistics", handler: handle_cost,
};
pub const PLAN_COMMAND: CommandMeta = CommandMeta {
    name: "plan", aliases: &[], description: "Show task plan status", handler: handle_plan,
};
pub const RELOAD_COMMAND: CommandMeta = CommandMeta {
    name: "reload", aliases: &[], description: "Reload session from disk", handler: handle_reload,
};
pub const FREE_COMMAND: CommandMeta = CommandMeta {
    name: "free", aliases: &[], description: "Switch to free mode", handler: handle_free,
};
pub const CANCEL_COMMAND: CommandMeta = CommandMeta {
    name: "cancel", aliases: &[], description: "Cancel current operation", handler: handle_cancel,
};
pub const CLEAR_COMMAND: CommandMeta = CommandMeta {
    name: "clear", aliases: &["cls"], description: "Clear the terminal screen", handler: handle_clear,
};

pub fn handle_exit(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    app.output.push_system("Goodbye.");
    app.should_quit = true;
    CommandResult::Success
}

pub fn handle_cd(app: &mut AppState, args: &str, session: &mut Session, rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    let path = args.trim();
    if path.is_empty() {
        app.output.push_line(OutputLine::System(format!("Working directory: {}", rt_env.working_dir.display())));
    } else {
        match runtime::change_directory(rt_env, &path) {
            DirectoryChangeResult::Success { new_dir, project_changed } => {
                app.output.push_line(OutputLine::System(format!("Changed to: {}", new_dir.display())));
                
                // ✅ Update session working directory and persist
                let working_dir_str = new_dir.to_string_lossy().to_string();
                if let Err(e) = session.update_working_dir(&working_dir_str) {
                    tracing::warn!("Failed to update session working dir: {}", e);
                }
                
                refresh_header_info(app, rt_env);
                if project_changed {
                    let name = rt_env.project_root.as_ref().and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "(none)".into());
                    app.output.push_system(&format!("Project boundary changed -- {}", name));
                    
                    // 🧠 Project onboarding: generate conventions skill if missing
                    if let Some(ref proj_root) = rt_env.project_root {
                        let skill_path = proj_root.join(".ox").join("skills").join("project-conventions.md");
                        if !skill_path.exists() {
                            let prompt = format!(
                                "You just entered a new project at `{}`. Generate a project conventions skill.\n\n\
                                 ## Step 1: Detect\n\
                                 Run `project_detect`. Note the language, framework, and build system.\n\n\
                                 ## Step 2: Scan\n\
                                 Based on detected language, scan these:\n\n\
                                 | Language | Config files | Code patterns to search |\n\
                                 |----------|-------------|------------------------|\n\
                                 | Rust | Cargo.toml | `fn `, `pub fn`, `impl`, `use `, `mod `, `#!` |\n\
                                 | Python | pyproject.toml, setup.cfg | `def `, `class `, `import `, `from `, `@` |\n\
                                 | TS/JS | package.json, tsconfig.json, .eslintrc | `function`, `const`, `export`, `interface` |\n\
                                 | Go | go.mod | `func `, `type `, `package `, `err ` |\n\
                                 | Java | build.gradle, pom.xml | `public class`, `private`, `@Override` |\n\
                                 | C/C++ | CMakeLists.txt, Makefile | `void `, `int `, `#include`, `class ` |\n\n\
                                 ## Step 3: Generate .ox/skills/project-conventions.md\n\
                                 Use `file_write` to create this file with these sections:\n\n\
                                 ### Project Overview\n\
                                 Language, framework, build tool, entry point (1-2 lines).\n\n\
                                 ### Architecture & Design Rules\n\
                                 **MUST** rules — patterns that MUST be followed:\n\
                                 - Module/dependency structure (e.g. \"MUST NOT import from sibling modules directly\")\n\
                                 - Layer boundaries (e.g. \"MUST keep business logic in services/, not in handlers/\")\n\
                                 - Error propagation (e.g. \"MUST use Result<T, E>, never panic!/unwrap()\")\n\
                                 - Testing (e.g. \"MUST write integration tests for public APIs\")\n\n\
                                 **MUST NOT** rules — anti-patterns to avoid:\n\
                                 - Forbidden imports/dependencies\n\
                                 - Forbidden patterns (e.g. \"MUST NOT use global mutable state\")\n\
                                 - Forbidden shortcuts (e.g. \"MUST NOT commit directly to main\")\n\n\
                                 Derive these from actual code patterns, linter configs, and project structure.\n\n\
                                 ### Naming Conventions\n\
                                 Files, modules, functions, variables — with real examples.\n\n\
                                 ### Code Style\n\
                                 Indent, quotes, line length, semicolons — from config/linter files.\n\n\
                                 ### Commands\n\
                                 Build, test, lint, run — from Makefile/package.json scripts/Cargo.toml.\n\n\
                                 Keep it under 400 words. Be specific — cite real files and patterns found.",
                                proj_root.display()
                            );
                            app.output.push_system("🔍 New project detected — generating project conventions...");
                            return CommandResult::LlmRequest {
                                prompt,
                                description: "Generate project conventions".to_string(),
                            };
                        } else {
                            app.output.push_system("📋 Project conventions already exist — loaded from cache.");
                        }
                    }
                }
            }
            DirectoryChangeResult::NotFound(msg) => app.output.push_system(&msg),
            DirectoryChangeResult::Error(msg) => app.output.push_system(&format!("Error: {}", msg)),
        }
    }
    CommandResult::Success
}

pub fn handle_init(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    match OxConfig::init_default_config() {
        Ok(path) => app.output.push_system(&format!("Config created at {}. Edit it to add API keys.", path.display())),
        Err(e) => app.output.push_system(&format!("Init failed: {}", e)),
    }
    CommandResult::Success
}

pub fn handle_debug(app: &mut AppState, _args: &str, session: &mut Session, rt_env: &mut runtime::RuntimeEnvironment,
    config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    use ox_core::llm::{ApiKeySource, BaseUrlSource};
    app.output.push_line(OutputLine::System(format!("Model: {}", app.model_name)));
    if let Some(info) = &app.resolve_info {
        app.output.push_line(OutputLine::System(format!("Provider: {}", info.provider_name)));
    }
    let config_path = OxConfig::default_config_path();
    app.output.push_line(OutputLine::System(format!("Config file: {}", config_path.display())));
    app.output.push_line(OutputLine::System(format!("OS: {} ({})", rt_env.os, rt_env.arch)));
    app.output.push_line(OutputLine::System(format!("Shell: {}", rt_env.shell.name)));
    app.output.push_line(OutputLine::System(format!("Working dir: {}", rt_env.working_dir.display())));
    app.output.push_line(OutputLine::System(format!("History: {} messages", session.messages.len())));
    let trusted = trust_manager.lock().map(|tm| tm.trusted_list()).unwrap_or_default();
    app.output.push_line(OutputLine::System(format!("Trusted tools: {}", if trusted.is_empty() { "(none)".to_string() } else { trusted.join(", ") })));
    CommandResult::Success
}

pub fn handle_cost(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    for line in cost_tracker.summary().lines() {
        app.output.push_line(OutputLine::System(line.to_string()));
    }
    CommandResult::Success
}

pub fn handle_plan(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    app.output.push_system("Task plan: (not yet active)");
    CommandResult::Success
}

pub fn handle_reload(app: &mut AppState, _args: &str, session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    match Session::load(&session.dir().to_path_buf()) {
        Ok(Some(loaded)) => {
            let old_count = session.messages.len();
            session.messages = loaded.messages.clone();
            session.meta = loaded.meta.clone();
            app.output.clear();
            app.message_count = session.messages.len();
            app.output.push_system(&format!("Session reloaded: {} messages (was {})", session.messages.len(), old_count));
        }
        Ok(None) => app.output.push_error("Failed to reload: session file is empty or corrupted"),
        Err(e) => app.output.push_error(&format!("Failed to reload session: {}", e)),
    }
    CommandResult::Success
}

pub fn handle_free(app: &mut AppState, _args: &str, session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    let prev = match app.workflow_state {
        WorkflowState::Spec { .. } => "Spec",
        WorkflowState::Free => { app.output.push_system("Already in Free mode."); return CommandResult::Success; }
    };
    
    // Switch to free workflow
    app.workflow_state = WorkflowState::Free;
    
    // Update session metadata for persistence
    session.meta.workflow_mode = "free".to_string();
    session.meta.workflow_id = String::new();
    session.meta.workflow_step_index = 0;
    session.meta.requirement_name = None;
    
    // Persist workflow state to disk immediately
    if let Err(e) = session.persist_workflow_state("free", "", 0, None) {
        tracing::warn!("Failed to persist workflow state: {}", e);
    }
    
    // Activate free workflow in engine (if engine exists)
    if let Some(ref engine_arc) = app.workflow_engine {
        if let Ok(mut engine) = engine_arc.try_lock() {
            if let Err(e) = engine.activate_workflow("free_workflow") {
                tracing::warn!("Failed to activate free workflow: {}", e);
            } else {
                tracing::info!("Switched to free_workflow");
            }
        }
    }
    
    // Clear spec state
    app.spec_active = false;
    app.spec_content.clear();
    app.spec_edit_mode = false;
    
    // Force UI refresh to update header immediately
    app.dirty = true;
    
    app.output.push_system(&format!("Switched from {} mode to Free mode", prev));
    CommandResult::Success
}

pub fn handle_cancel(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    if app.spec_edit_mode { app.spec_edit_mode = false; app.output.push_system("Spec edit cancelled."); }
    else { app.output.push_system("Nothing to cancel."); }
    CommandResult::Success
}

pub fn handle_clear(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _memory: &Arc<MemoryManager>, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    app.output.clear();
    CommandResult::Success
}

fn refresh_header_info(app: &mut AppState, rt_env: &runtime::RuntimeEnvironment) {
    app.header_info.clear();
    app.header_info.push(rt_env.banner_summary());
    app.header_info.push("Type a message or /help. /exit to quit.".into());
    app.working_dir = rt_env.working_dir.file_name().map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rt_env.working_dir.display().to_string());
}
