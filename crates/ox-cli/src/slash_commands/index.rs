/// Index command - manage code symbol indexing
///
/// Usage:
/// /index          - Show index status and statistics
/// /index build    - Trigger full project re-indexing
/// /index clear    - Clear the symbol database

use crate::slash_commands::{CommandResult, CommandMeta};
use crate::terminal::app::App as AppState;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::config::OxConfig;
use ox_core::memory::MemoryManager;
use ox_core::cost::CostTracker;
use ox_core::safety::TrustManager;
use std::sync::Arc;

pub const INDEX_COMMAND: CommandMeta = CommandMeta {
    name: "index",
    aliases: &["idx"],
    description: "Manage code symbol indexing (AST + vector embeddings)",
    handler: handle_index_command,
};

fn handle_index_command(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    _rt_env: &mut RuntimeEnvironment,
    _config: &OxConfig,
    _memory: &Arc<MemoryManager>,
    _cost_tracker: &mut CostTracker,
    _trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
) -> CommandResult {
    let args_trimmed = args.trim();
    
    match args_trimmed {
        "build" | "rebuild" => {
            // Trigger background re-indexing
            app.output.push_system("🔨 Starting full project re-indexing...");
            app.output.push_system("   This will parse all source files and build semantic embeddings.");
            app.output.push_system("   The process runs in the background. You can continue working.");
            
            if let Some(ref indexer) = app.code_indexer {
                let indexer_clone = Arc::clone(indexer);
                tokio::spawn(async move {
                    let mut idx = indexer_clone.lock().await;
                    match idx.index_project(None).await {
                        Ok(count) => {
                            tracing::info!("[INDEX] ✅ Re-indexed {} symbols", count);
                        }
                        Err(e) => {
                            tracing::error!("[INDEX] ❌ Re-indexing failed: {}", e);
                        }
                    }
                });
            } else {
                app.output.push_system("⚠️ Code indexer not initialized. Restart Ox to enable indexing.");
            }
            
            CommandResult::Success
        }
        
        "clear" | "reset" => {
            // Clear the database file
            let db_path = std::env::var("OX_HOME")
                .ok()
                .map(|home| std::path::PathBuf::from(home).join("db").join("symbols.tdb"))
                .or_else(|| {
                    dirs::home_dir().map(|h| h.join(".ox").join("db").join("symbols.tdb"))
                });
            
            if let Some(path) = db_path {
                if path.exists() {
                    match std::fs::remove_file(&path) {
                        Ok(_) => {
                            app.output.push_system(&format!("✅ Cleared symbol database: {:?}", path));
                            app.output.push_system("   Restart Ox or run /index build to rebuild.");
                        }
                        Err(e) => {
                            app.output.push_system(&format!("❌ Failed to clear database: {}", e));
                        }
                    }
                } else {
                    app.output.push_system("ℹ️ Symbol database does not exist.");
                }
            } else {
                app.output.push_system("⚠️ Could not determine database path.");
            }
            
            CommandResult::Success
        }
        
        "" | "status" | "stat" => {
            // Show index status
            if let Some(ref indexer) = app.code_indexer {
                let indexer_clone = Arc::clone(indexer);
                tokio::spawn(async move {
                    let idx = indexer_clone.lock().await;
                    let count = idx.symbol_count().await;
                    
                    // Check if database file exists
                    let db_path = std::env::var("OX_HOME")
                        .ok()
                        .map(|home| std::path::PathBuf::from(home).join("db").join("symbols.tdb"))
                        .or_else(|| {
                            dirs::home_dir().map(|h| h.join(".ox").join("db").join("symbols.tdb"))
                        });
                    
                    let db_status = if let Some(path) = db_path {
                        if path.exists() {
                            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            format!("✅ Database exists ({:.2} MB)", size as f64 / 1_048_576.0)
                        } else {
                            "❌ Database not found".to_string()
                        }
                    } else {
                        "⚠️ Unknown".to_string()
                    };
                    
                    // We can't directly push to app.output from async context
                    // So we log and the user can check logs
                    tracing::info!(
                        "[INDEX STATUS] Symbols: {}, {}",
                        count,
                        db_status
                    );
                });
                
                app.output.push_system("📊 Checking index status... (see logs for details)");
                app.output.push_system("   Tip: Use /index build to trigger full indexing");
            } else {
                app.output.push_system("⚠️ Code indexer not initialized.");
            }
            
            CommandResult::Success
        }
        
        "help" | "--help" | "-h" => {
            app.output.push_system("📖 Index Command Help");
            app.output.push_system("");
            app.output.push_system("Usage: /index [action]");
            app.output.push_system("");
            app.output.push_system("Actions:");
            app.output.push_system("  (no args)  - Show index status and statistics");
            app.output.push_system("  build      - Trigger full project re-indexing");
            app.output.push_system("  clear      - Clear the symbol database");
            app.output.push_system("  help       - Show this help message");
            app.output.push_system("");
            app.output.push_system("Examples:");
            app.output.push_system("  /index          # Check status");
            app.output.push_system("  /index build    # Rebuild index");
            app.output.push_system("  /index clear    # Reset database");
            app.output.push_system("");
            app.output.push_system("Notes:");
            app.output.push_system("  • Indexing happens automatically when you read files");
            app.output.push_system("  • Full indexing runs in background on startup");
            app.output.push_system("  • Use find_symbol tool to search indexed symbols");
            
            CommandResult::Success
        }
        
        _ => {
            CommandResult::Error(format!("Unknown index action: '{}'. Use /index help for usage.", args_trimmed))
        }
    }
}
