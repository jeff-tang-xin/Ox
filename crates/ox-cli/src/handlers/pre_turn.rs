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
use std::sync::Arc;
use tokio::sync::mpsc;

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

    // 2. Knowledge retrieval
    let _ = status_tx.send(AgentToUiEvent::Status(
        "🔍 Retrieving knowledge...".to_string(),
    ));
    let knowledge_context_str = if let Some(k_engine) = knowledge_engine {
        match k_engine.try_read() {
            Ok(engine) => {
                let result = if step_memory_layers.is_empty() {
                    retrieval::run_retrieval(&engine, &effective_text, session_id, 3000)
                } else {
                    retrieval::run_retrieval_for_step(
                        &engine,
                        &effective_text,
                        session_id,
                        3000,
                        &step_memory_layers,
                    )
                };
                match result {
                    Ok(inj) => retrieval::format_context_for_prompt(&inj),
                    Err(_) => String::new(),
                }
            }
            Err(_) => String::new(),
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
    let use_refined = config.context.use_refined_context;
    let system_prompt_variant = match &variant {
        TurnVariant::Onboarding { .. } => UserIntent::CodeModification,
        _ => UserIntent::General,
    };

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
        let planning = effort == ox_core::context::EffortLevel::High;

        Ok::<_, String>((turn_messages, planning))
    })
    .await;

    match blocking_result {
        Ok(Ok((turn_messages, planning))) => PreTurnResult {
            turn_messages,
            planning,
        },
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
            let prompt = step.and_then(|s| {
                if s.step_prompt.is_empty() {
                    None
                } else {
                    Some(s.step_prompt.clone())
                }
            });
            let idx = engine.get_current_step_index();
            return (layers, prompt, idx);
        }
    }
    (Vec::new(), None, 0)
}
