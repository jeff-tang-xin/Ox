//! Compression middleware for context compression management.
//!
//! Handles the deferred compression trigger logic and async compression spawning.

use std::sync::Arc;
use tokio::sync::mpsc;
use ox_core::agent::{self, AgentToUiEvent};
use ox_core::config::AgentConfig;
use ox_core::context::ContextBuilder;
use ox_core::context::compressed_store::CompressedContextStore;
use ox_core::embedding::CompressionManager;
use ox_core::llm::LlmProvider;
use ox_core::message::{Message, Session};
use ox_core::runtime::RuntimeEnvironment;
use ox_core::safety::TrustManager;
use ox_core::tools::{ToolContext, ToolRegistry};
use crate::terminal::app::App;

/// Handle pending compression request.
/// This function processes the deferred compression and spawns an agent turn.
pub async fn handle_pending_compression(
    app: &mut App,
    session: &mut Session,
    provider: &Option<Arc<dyn LlmProvider>>,
    compression_manager: &Option<CompressionManager>,
    compressed_cache: &Option<(Vec<Message>, usize)>,
    system_prompt: &str,
    context_builder: &ContextBuilder,
    context_window: u32,
    agent_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    tool_registry: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    interrupt_ctrl: &mut ox_core::agent::interrupt::InterruptController,
    trust_manager: &Arc<std::sync::Mutex<TrustManager>>,
    agent_config: &Arc<AgentConfig>,
) {
    let Some(pc) = app.pending_compression.take() else {
        return;
    };

    // Skip if compression is already in progress (prevents re-entrant compression).
    if app.compression_in_progress {
        use crate::terminal::output_pane::OutputLine;
        app.output.push_line(OutputLine::System(
            "Compression in progress, skipping...".to_string(),
        ));
        app.agent_running = false;
        app.dirty = true;
        return;
    }

    app.compression_in_progress = true;
    let source_msg_count = session.messages.len();
    app.last_compression_msg_count = source_msg_count;
    app.agent_running = true;
    app.status = "Compressing...".to_string();
    app.dirty = true;

    let Some(p) = provider else {
        return;
    };

    let cm = compression_manager.clone();
    // Build input: existing compressed context + new messages, or all messages.
    let messages = if let Some((cached, prev_count)) = compressed_cache {
        let pc = *prev_count;
        let new_msgs = &session.messages[pc.min(session.messages.len())..];
        let mut combined = cached.clone();
        combined.extend_from_slice(new_msgs);
        combined
    } else {
        session.messages.clone()
    };

    let sp = system_prompt.to_string();
    let memory_ctx = pc.memory_ctx;
    let query = pc.text;
    let cb = context_builder.clone();
    let cw = context_window;
    let provider = Arc::clone(p);
    let tx = agent_tx.clone();
    let registry = Arc::clone(tool_registry);
    let ctx = Arc::clone(tool_ctx);
    let cancel_token = interrupt_ctrl.token();
    let tm = Arc::clone(trust_manager);
    let ac = Arc::clone(agent_config);
    let (ui_to_agent_tx, ui_to_agent_rx) = mpsc::unbounded_channel::<ox_core::agent::ui_event::UiToAgentEvent>();
    app.ui_to_agent_tx = Some(ui_to_agent_tx);

    // Clone workflow engine for async task
    let workflow_engine_clone = app.workflow_engine.clone();

    tokio::spawn(async move {
        let tx_status = tx.clone();
        let turn_messages = match cm {
            Some(cm) => {
                let q = query;
                let mem_ctx = memory_ctx.clone();
                match tokio::task::spawn_blocking(move || {
                    // Use enhanced compression with memory context
                    let result = if !mem_ctx.is_empty() {
                        cm.compress_with_memory(&messages, &q, Some(&mem_ctx))
                    } else {
                        cm.compress(&messages, &q)
                    };
                    (result, messages, cm)
                })
                .await
                {
                    Ok((Ok(Some(compressed)), original, _cm)) => {
                        let _ = tx_status.send(AgentToUiEvent::Status(format!(
                            "Compressed: {} → {} msgs",
                            original.len(),
                            compressed.len()
                        )));
                        let _ = tx_status.send(AgentToUiEvent::CompressionComplete {
                            compressed_messages: compressed.clone(),
                            source_msg_count,
                        });
                        cb.build(&sp, &memory_ctx, &compressed, cw)
                    }
                    Ok((Ok(None), original, _cm)) => {
                        cb.build(&sp, &memory_ctx, &original, cw)
                    }
                    Ok((Err(e), original, _cm)) => {
                        tracing::error!("Compression failed: {}", e);
                        cb.build(&sp, &memory_ctx, &original, cw)
                    }
                    Err(_) => {
                        tracing::error!("Compression task panicked");
                        return;
                    }
                }
            }
            None => cb.build(&sp, &memory_ctx, &messages, cw),
        };

        agent::run_agent_turn(
            provider,
            turn_messages,
            registry,
            ctx,
            tx,
            ui_to_agent_rx,
            cancel_token,
            tm,
            ac,
            false, // compression path: skip planning
            workflow_engine_clone,
        )
        .await;
    });
}
