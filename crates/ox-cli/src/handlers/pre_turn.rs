//! Unified pre-turn pipeline.
//!
//! Previously, three separate call sites (onboarding, slash commands, normal text)
//! each had their own ~150-line copy of context-building logic. This module provides
//! a single `prepare_turn` function that all three call sites share.

use ox_core::agent::AgentToUiEvent;
use ox_core::context::{self, ContextBuilder, TurnContext, UserIntent};
use ox_core::knowledge::retrieval;
use ox_core::message::Message;
use ox_core::runtime::RuntimeEnvironment;
use ox_core::tools::ToolRegistry;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Max wait for knowledge read lock (background indexer holds write lock while embedding).
const KNOWLEDGE_READ_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
/// Max time for hybrid retrieval (query embed + vector search).
const KNOWLEDGE_RETRIEVAL_TIMEOUT: Duration = Duration::from_secs(5);
/// Max wait for lazy-index write lock before skipping on-demand embed.
const LAZY_INDEX_WRITE_LOCK_TIMEOUT: Duration = Duration::from_secs(3);

/// Specifies which kind of turn is being prepared — affects how user input is handled.
#[derive(Debug, Clone)]
pub enum TurnVariant {
    /// Normal user text input
    Normal,
    /// First-time project onboarding
    Onboarding { prompt_text: String },
    /// Slash command requesting LLM (e.g., /skill create)
    SlashCommand { prompt: String, description: String },
}

/// Result of the pre-turn pipeline — ready for agent::run_agent_turn.
pub struct PreTurnResult {
    pub turn_messages: Vec<Message>,
    pub planning: bool,
}

/// Run the unified pre-turn pipeline.
///
/// Steps:
/// 1. Extract workflow step info (memory layers + step prompt)
/// 2. Knowledge retrieval (step-aware if workflow active)
/// 3. Git + Dir context gathering (via spawn_blocking)
/// 4. System prompt building
/// 5. Context builder assembly (with compressed cache merge)
/// 6. Effort estimation → planning flag
pub async fn prepare_turn(
    config: &ox_core::config::OxConfig,
    rt_env: &RuntimeEnvironment,
    tool_registry: &Arc<ToolRegistry>,
    context_builder: &ContextBuilder,
    context_window: u32,
    knowledge_engine: &Option<Arc<tokio::sync::RwLock<ox_core::knowledge::KnowledgeEngine>>>,
    user_text: &str,
    session_messages: &[Message],
    compressed_cache: &Option<(Vec<Message>, usize)>,
    variant: TurnVariant,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
    session_id: &str,
    status_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
) -> PreTurnResult {
    // The actual user text to use (differs for onboarding/slash commands)
    let effective_text = match &variant {
        TurnVariant::Normal => user_text.to_string(),
        TurnVariant::Onboarding { prompt_text } => prompt_text.clone(),
        TurnVariant::SlashCommand { prompt, .. } => prompt.clone(),
    };

    // 1. Workflow step info
    let (step_memory_layers, step_prompt, step_idx) =
        get_workflow_step_info(workflow_engine);

    // 1b. Lazy index: embed session-relevant paths before retrieval
    if config.embedding.lazy_index {
        if let Some(k_engine) = knowledge_engine {
            let paths = collect_lazy_index_paths(&effective_text, workflow_engine);
            if !paths.is_empty() {
                let max = config.embedding.lazy_index_max_files_per_turn.max(1);
                let _ = status_tx.send(AgentToUiEvent::Status(format!(
                    "📇 Indexing {} path(s)…",
                    paths.len().min(max)
                )));
                match tokio::time::timeout(LAZY_INDEX_WRITE_LOCK_TIMEOUT, k_engine.write()).await
                {
                    Ok(mut engine) => {
                        let path_count = paths.len().min(max);
                        let result = tokio::task::block_in_place(|| {
                            engine.ensure_paths_indexed(&paths, max)
                        });
                        if let Ok(n) = result {
                            if n > 0 {
                                tracing::info!(
                                    "[PRE-TURN] Lazy-indexed {n} symbols from {path_count} paths",
                                );
                            }
                        }
                    }
                    Err(_) => {
                        tracing::warn!(
                            "[PRE-TURN] Lazy index skipped — knowledge engine busy (background indexing)"
                        );
                    }
                }
            }
        }
    }

    // 2. Knowledge retrieval — bounded wait so background embed cannot block the LLM call
    let _ = status_tx.send(AgentToUiEvent::Status(
        "🔍 Retrieving knowledge...".to_string(),
    ));
    let workflow_active = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| e.is_workflow_active())
        .unwrap_or(false);

    let knowledge_context_str = if let Some(k_engine) = knowledge_engine {
        match tokio::time::timeout(KNOWLEDGE_READ_LOCK_TIMEOUT, k_engine.read()).await {
            Ok(engine_guard) => {
                let query = effective_text.clone();
                let sid = session_id.to_string();
                let layers = step_memory_layers.clone();
                let step_layers = !layers.is_empty();
                match tokio::time::timeout(KNOWLEDGE_RETRIEVAL_TIMEOUT, async {
                    tokio::task::block_in_place(|| {
                        if step_layers {
                            retrieval::run_retrieval_for_step(
                                &engine_guard,
                                &query,
                                &sid,
                                3000,
                                &layers,
                            )
                        } else {
                            retrieval::run_retrieval(&engine_guard, &query, &sid, 3000)
                        }
                    })
                })
                .await
                {
                    Ok(Ok(inj)) => retrieval::format_context_for_prompt(&inj),
                    Ok(Err(e)) => {
                        tracing::warn!("[PRE-TURN] Knowledge retrieval failed: {}", e);
                        String::new()
                    }
                    Err(_) => {
                        tracing::warn!(
                            "[PRE-TURN] Knowledge retrieval timed out — proceeding without RAG context"
                        );
                        String::new()
                    }
                }
            }
            Err(_) => {
                tracing::warn!(
                    "[PRE-TURN] Knowledge read lock timeout — skipping retrieval (indexer may be running)"
                );
                String::new()
            }
        }
    } else {
        String::new()
    };

    // 3. Git + Dir context + System prompt + Context builder (spawn_blocking for I/O)
    let _ = status_tx.send(AgentToUiEvent::Status(
        "📊 Gathering context...".to_string(),
    ));
    let tr = Arc::clone(tool_registry);
    let rt_env_clone = rt_env.clone();
    let behavior_rules = config.behavior_rules.clone();
    let compressed_cache_clone = compressed_cache.clone();
    let messages_clone = session_messages.to_vec();
    let context_builder_clone = context_builder.clone();
    let step_prompt_clone = step_prompt.clone();
    let user_text_clone = effective_text.clone();
    let knowledge_ctx = knowledge_context_str;
    let use_refined = config.context.use_refined_context
        && !workflow_active
        && session_messages.len() < 40;
    let system_prompt_variant = match &variant {
        TurnVariant::Onboarding { .. } => UserIntent::Exploration,
        _ => UserIntent::General,
    };
    let onboarding_greenfield = matches!(variant, TurnVariant::Onboarding { .. })
        && ox_core::agent::onboarding::is_greenfield_project(&rt_env.effective_project_root());

    let blocking_result = tokio::task::spawn_blocking(move || {
        let git_log = context::gather_git_context(&rt_env_clone.working_dir);
        let git_diff = context::gather_diff_context(&rt_env_clone.working_dir);
        let dir_tree = context::gather_dir_context(&rt_env_clone.working_dir);

        let turn_ctx = TurnContext {
            git_log: None,
            git_diff_stat: None,
            dir_structure: None,
            recent_summary: None,
            relevant_symbols: None,
        };
        let system_prompt = context::build_system_prompt_with_step(
            &rt_env_clone,
            &tr,
            system_prompt_variant,
            Some(&behavior_rules),
            None,
            &turn_ctx,
            step_prompt_clone.as_deref(),
            step_idx,
        );

        let effective_messages = if let Some((cached, prev_count)) = compressed_cache_clone {
            let start_idx = prev_count.min(messages_clone.len());
            let new_msgs = if start_idx < messages_clone.len() {
                &messages_clone[start_idx..]
            } else {
                &[]
            };
            let mut combined = cached.clone();
            combined.extend_from_slice(new_msgs);
            combined
        } else {
            messages_clone
        };

        let mut turn_messages = crate::helpers::build_context_with_option(
            &context_builder_clone,
            &system_prompt,
            "",
            &effective_messages,
            context_window,
            use_refined,
        );

        // Inject knowledge + background info as one compact system message
        let mut bg_parts = Vec::new();
        if !knowledge_ctx.is_empty() {
            bg_parts.push(knowledge_ctx);
        }
        if let Some(ref log) = git_log {
            bg_parts.push(format!("【参考-Git日志】\n{}", log));
        }
        if let Some(ref diff) = git_diff {
            bg_parts.push(format!("【参考-未提交变更】\n{}", diff));
        }
        if let Some(ref dir) = dir_tree {
            bg_parts.push(format!("【参考-目录结构】\n{}", dir));
        }
        if !bg_parts.is_empty() {
            turn_messages.push(Message::system(&bg_parts.join("\n\n")));
        }

        let effort = ox_core::context::estimate_effort(
            &user_text_clone,
            effective_messages.len(),
        );
        // Workflow steps manage their own tool gating — never use legacy planning mode there
        let planning = !workflow_active && effort == ox_core::context::EffortLevel::High;

        Ok::<_, String>((turn_messages, planning))
    })
    .await;

    match blocking_result {
        Ok(Ok((mut turn_messages, planning))) => {
            if matches!(variant, TurnVariant::Onboarding { .. }) {
                turn_messages.insert(
                    1,
                    Message::system(&ox_core::agent::onboarding::onboarding_system_directive(
                        onboarding_greenfield,
                    )),
                );
            }
            if workflow_active {
                if let Some(wf) = workflow_engine {
                    if let Ok(engine) = wf.try_lock() {
                        let block = engine.durable_memory_block();
                        if !block.is_empty() {
                            turn_messages.push(Message::system(&block));
                        }
                    }
                }
            }
            PreTurnResult {
                turn_messages,
                planning,
            }
        }
        Ok(Err(e)) => {
            tracing::error!("[PRE-TURN] Blocking task failed: {}", e);
            let _ = status_tx.send(AgentToUiEvent::Error(format!(
                "Preparing context failed: {}",
                e
            )));
            PreTurnResult {
                turn_messages: vec![Message::user(&effective_text)],
                planning: false,
            }
        }
        Err(e) => {
            tracing::error!("[PRE-TURN] Blocking task panicked: {}", e);
            let _ = status_tx.send(AgentToUiEvent::Error(format!(
                "Background task crashed: {}",
                e
            )));
            PreTurnResult {
                turn_messages: vec![Message::user(&effective_text)],
                planning: false,
            }
        }
    }
}

/// Extract workflow step information (memory layers, step prompt, step index).
fn get_workflow_step_info(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
) -> (Vec<String>, Option<String>, usize) {
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            let step = engine.current_step();
            let layers = step
                .map(|s| s.memory_layers.clone())
                .unwrap_or_default();
            // Use substituted prompt ({PREVIOUS_OUTPUT} filled in)
            let prompt = engine.get_step_system_prompt();
            let idx = engine.get_current_step_index();
            return (layers, prompt, idx);
        }
    }
    (Vec::new(), None, 0)
}

/// Paths to lazy-embed: explicit paths in query + intent files + explored file_read targets.
fn collect_lazy_index_paths(
    query: &str,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    let mut add = |p: String| {
        let p = p.trim().trim_matches('"').to_string();
        if p.is_empty() || !seen.insert(p.clone()) {
            return;
        }
        paths.push(PathBuf::from(p));
    };

    for p in retrieval::extract_file_paths(query) {
        add(p);
    }

    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            if let Some(intent_raw) = engine.get_variable("_step0_output") {
                if let Some(json) = ox_core::agent::engine::extract_json_block(&intent_raw) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                        if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
                            for f in files {
                                if let Some(s) = f.as_str() {
                                    add(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
            if let Some(explored_json) = engine.get_variable("_explored_paths") {
                if let Ok(set) = serde_json::from_str::<HashSet<String>>(&explored_json) {
                    for key in set {
                        if let Some((_tool, path)) = key.split_once(':') {
                            if path.contains('.') {
                                add(path.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    paths
}
