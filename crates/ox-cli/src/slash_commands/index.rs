/// Index command - manage GitNexus code graph indexing.
///
/// Usage:
/// /index          - Show index status
/// /index build    - Trigger full project re-indexing (auto-init + analyze)
/// /index clear    - Clear the .gitnexus directory
use crate::slash_commands::{CommandMeta, CommandResult};
use crate::terminal::app::App as AppState;
use ox_core::config::OxConfig;
use ox_core::cost::CostTracker;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const INDEX_COMMAND: CommandMeta = CommandMeta {
    name: "index",
    aliases: &["idx"],
    description: "Manage GitNexus code graph indexing",
    handler: handle_index_command,
};

fn handle_index_command(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let args_trimmed = args.trim();

    match args_trimmed {
        "build" | "rebuild" => handle_build(app),
        "clear" | "reset" => handle_clear(app),
        "" | "status" | "stat" => handle_status(app),
        "help" | "--help" | "-h" => handle_help(app),
        _ => CommandResult::Error(format!(
            "Unknown index action: '{}'. Use /index help for usage.",
            args_trimmed
        )),
    }
}

/// `/index build` — auto-init (if needed) + analyze via GitNexusService.
///
/// Uses the `GitNexusService` already wired into `App` instead of spawning
/// `gitnexus` directly, so it works cross-platform (Windows included) and
/// reuses the existing MCP connection.
fn handle_build(app: &mut AppState) -> CommandResult {
    let gn = match app.gitnexus.as_ref() {
        Some(svc) => Arc::clone(svc),
        None => {
            app.output.push_system(
                "❌ GitNexus service not available. Check that [gitnexus] enabled = true in config.",
            );
            return CommandResult::Error("GitNexus not configured".into());
        }
    };

    app.output.push_system("🔨 Starting GitNexus code graph indexing…");

    // Clone what we need for the async closure.
    let project_root = gn.project_root().to_path_buf();
    let needs_init = !project_root.join(".gitnexus").join("meta.json").exists();

    if needs_init {
        app.output.push_system("   No .gitnexus index found — running init first…");
    }
    app.output.push_system("   The process runs in the background. You can continue working.");

    // Spawn a background task so the TUI stays responsive.
    let gn_clone = Arc::clone(&gn);
    tokio::spawn(async move {
        // 1. Ensure the MCP server is started.
        if let Err(e) = gn_clone.start().await {
            tracing::error!("[index] GitNexus start failed: {e}");
            return;
        }

        // 2. Auto-init if the project has never been indexed.
        if needs_init {
            tracing::info!("[index] running gitnexus init for {:?}", project_root);
            match gn_clone.call("init", serde_json::json!({})).await {
                Ok(res) => {
                    if res.is_error {
                        tracing::error!("[index] init returned error: {}", res.text);
                    } else {
                        tracing::info!("[index] init succeeded");
                    }
                }
                Err(e) => {
                    tracing::error!("[index] init call failed: {e}");
                }
            }
        }

        // 3. Run analyze to build/refresh the code graph.
        tracing::info!("[index] running gitnexus analyze for {:?}", project_root);
        match gn_clone.call("analyze", serde_json::json!({})).await {
            Ok(res) => {
                if res.is_error {
                    tracing::error!("[index] analyze returned error: {}", res.text);
                } else {
                    tracing::info!("[index] analyze succeeded");
                }
            }
            Err(e) => {
                tracing::error!("[index] analyze call failed: {e}");
            }
        }
    });

    app.output.push_system("✅ GitNexus indexing started in background.");
    CommandResult::Success
}

/// `/index clear` — remove the `.gitnexus` directory.
fn handle_clear(app: &mut AppState) -> CommandResult {
    let project_root = app
        .gitnexus
        .as_ref()
        .map(|gn| gn.project_root().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(&app.working_dir));

    let index_dir = project_root.join(".gitnexus");
    if index_dir.exists() {
        match std::fs::remove_dir_all(&index_dir) {
            Ok(_) => {
                app.output
                    .push_system(&format!("✅ Cleared GitNexus index: {}", index_dir.display()));
                app.output
                    .push_system("   Run /index build to rebuild the code graph.");
            }
            Err(e) => {
                app.output
                    .push_system(&format!("❌ Failed to clear index: {}", e));
            }
        }
    } else {
        app.output
            .push_system("ℹ️ No .gitnexus directory found in project root.");
    }

    CommandResult::Success
}

/// `/index` (no args) or `/index status` — show index status.
fn handle_status(app: &mut AppState) -> CommandResult {
    let project_root = app
        .gitnexus
        .as_ref()
        .map(|gn| gn.project_root().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(&app.working_dir));

    let gn_dir = project_root.join(".gitnexus");
    if gn_dir.join("meta.json").exists() {
        app.output
            .push_system("✅ GitNexus index exists. Use find_symbol / code_graph to query.");
    } else {
        app.output
            .push_system("⚠️ No GitNexus index found — auto-building…");
        return handle_build(app);
    }

    // Also show the GitNexusService status if available.
    if let Some(gn) = app.gitnexus.as_ref() {
        // Show both config state and actual availability
        if gn.is_enabled() {
            app.output.push_system("   GitNexus: configured (run /index check 查看实际状态)");
        } else {
            app.output.push_system("   GitNexus: disabled in config");
        }
    }

    app.output
        .push_system("   Tip: Use /index build to trigger full indexing");

    CommandResult::Success
}

/// `/index help` — show usage.
fn handle_help(app: &mut AppState) -> CommandResult {
    app.output.push_system("📖 Index Command Help");
    app.output.push_system("");
    app.output.push_system("Usage: /index [action]");
    app.output.push_system("");
    app.output.push_system("Actions:");
    app.output
        .push_system("  (no args)  - Show index status");
    app.output
        .push_system("  build      - Auto-init + analyze (build/refresh code graph)");
    app.output
        .push_system("  clear      - Clear the .gitnexus directory");
    app.output
        .push_system("  help       - Show this help message");
    app.output.push_system("");
    app.output.push_system("Examples:");
    app.output.push_system("  /index          # Check status");
    app.output.push_system("  /index build    # Build code graph (auto-inits if needed)");
    app.output.push_system("  /index clear    # Reset index");
    app.output.push_system("");
    app.output.push_system("Notes:");
    app.output
        .push_system("  • Indexing uses GitNexus (code graph + semantic search)");
    app.output
        .push_system("  • /index build auto-initializes .gitnexus if missing");
    app.output
        .push_system("  • Use find_symbol / code_graph tools to query the index");

    CommandResult::Success
}
