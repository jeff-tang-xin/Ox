/// System commands: /exit, /cd, /init, /debug, /cost, /plan, /reload, /cancel, /clear

use crate::terminal::app::App as AppState;
use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::output_pane::OutputLine;
use ox_core::message::Session;
use ox_core::runtime::{self, DirectoryChangeResult};
use ox_core::config::OxConfig;
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
pub const CANCEL_COMMAND: CommandMeta = CommandMeta {
    name: "cancel", aliases: &[], description: "Cancel current operation", handler: handle_cancel,
};
pub const CLEAR_COMMAND: CommandMeta = CommandMeta {
    name: "clear", aliases: &["cls"], description: "Clear the terminal screen", handler: handle_clear,
};

pub fn handle_exit(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    app.output.push_system("Goodbye.");
    app.should_quit = true;
    CommandResult::Success
}

pub fn handle_cd(app: &mut AppState, args: &str, session: &mut Session, rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
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
                    
                    // 🧠 Project onboarding: generate project Skills if missing
                    let proj_root = rt_env.effective_project_root();
                    if ox_core::agent::onboarding::needs_project_onboarding(&proj_root) {
                        let _ =
                            ox_core::agent::onboarding::prepare_project_for_onboarding(&proj_root);
                        let prompt =
                            ox_core::agent::onboarding::build_onboarding_user_prompt(&proj_root);
                        app.output.push_system(
                            "🔍 首次进入本项目 — 将生成项目规范与业务指导 Skill…",
                        );
                        app.output.push_system(
                            "   → project-conventions.md + project-business-guide.md",
                        );
                        if ox_core::agent::onboarding::is_greenfield_project(&proj_root) {
                            app.output.push_system(
                                "   ℹ️ 未检测到工程标记（空目录或未初始化）— 将创建初始 Skill 模板",
                            );
                        }
                        return CommandResult::LlmRequest {
                            prompt,
                            description: "生成项目规范与业务指导 Skill".to_string(),
                            skip_workflow: true,
                        };
                    } else {
                        app.output.push_system("📋 项目 Skill 已存在 — 已从缓存加载。");
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
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    match OxConfig::init_default_config() {
        Ok(path) => app.output.push_system(&format!("Config created at {}. Edit it to add API keys.", path.display())),
        Err(e) => app.output.push_system(&format!("Init failed: {}", e)),
    }
    CommandResult::Success
}

pub fn handle_debug(app: &mut AppState, _args: &str, session: &mut Session, rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    
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
    _config: &OxConfig, cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    for line in cost_tracker.summary().lines() {
        app.output.push_line(OutputLine::System(line.to_string()));
    }
    CommandResult::Success
}

pub fn handle_plan(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    app.output.push_system("Task plan: (not yet active)");
    CommandResult::Success
}

pub fn handle_reload(app: &mut AppState, _args: &str, session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
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

pub fn handle_cancel(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>) -> CommandResult {
    app.output.push_system("Nothing to cancel.");
    CommandResult::Success
}

pub fn handle_clear(app: &mut AppState, _args: &str, _session: &mut Session, _rt_env: &mut runtime::RuntimeEnvironment,
    _config: &OxConfig, _cost_tracker: &mut CostTracker,
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
