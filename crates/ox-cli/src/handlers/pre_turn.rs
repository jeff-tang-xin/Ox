//! Unified pre-turn pipeline.
//!
//! Previously, three separate call sites (onboarding, slash commands, normal text)
//! each had their own ~150-line copy of context-building logic. This module provides
//! a single `prepare_turn` function that all three call sites share.

use ox_core::agent::AgentToUiEvent;
use ox_core::context::{self, ContextBuilder, TurnContext, UserIntent};
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
    user_text: &str,
    session_messages: &[Message],
    compressed_cache: &Option<(Vec<Message>, usize)>,
    variant: TurnVariant,
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
    _session_id: &str,
    status_tx: &mpsc::UnboundedSender<AgentToUiEvent>,
    gitnexus: Option<Arc<ox_core::mcp::GitNexusService>>,
) -> PreTurnResult {
    // The actual user text to use (differs for onboarding/slash commands)
    let effective_text = match &variant {
        TurnVariant::Normal => user_text.to_string(),
        TurnVariant::Onboarding { prompt_text } => prompt_text.clone(),
        TurnVariant::SlashCommand { prompt, .. } => prompt.clone(),
    };

    // 1. Workflow step info
    let (step_prompt, step_idx) = get_workflow_step_info(workflow_engine);

    let workflow_active = workflow_engine
        .as_ref()
        .and_then(|wf| wf.try_lock().ok())
        .map(|e| e.is_workflow_active())
        .unwrap_or(false);

    // 2. Git + Dir context + System prompt + Context builder (spawn_blocking for I/O)
    let _ = status_tx.send(AgentToUiEvent::Status(
        "📊 Gathering context...".to_string(),
    ));
    let tr = Arc::clone(tool_registry);
    let rt_env_clone = rt_env.clone();
    let behavior_rules = config.behavior_rules.clone();
    let unified_tool_mode = config.agent.unified_tool_mode;
    let compressed_cache_clone = compressed_cache.clone();
    let messages_clone = session_messages.to_vec();
    let context_builder_clone = context_builder.clone();
    let step_prompt_clone = step_prompt.clone();
    let user_text_clone = effective_text.clone();
    let use_refined =
        config.context.use_refined_context && !workflow_active && session_messages.len() < 40;
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
            unified_tool_mode,
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
        // Trim old rounds: keep messages after the LAST [ROUND_BOUNDARY], plus a
        // short read-only "tail bridge" of the immediately-previous round so
        // related follow-ups still have real prior context (not just a recap).
        let (effective_messages, trimmed_to_current_round) =
            truncate_before_last_round_boundary(effective_messages);
        // Once we've scoped to the current round, keep FULL fidelity (real tool
        // outputs) — collapsing the current round into a lossy "refined" text
        // summary is what makes the model forget what it just read.
        let use_refined = use_refined && !trimmed_to_current_round;

        let mut turn_messages = crate::helpers::build_context_with_option(
            &context_builder_clone,
            &system_prompt,
            &effective_messages,
            context_window,
            use_refined,
        );

        // ── One-time project context (new session only) ──
        let is_new_session = effective_messages.len() <= 2;
        if is_new_session {
            let lang = &rt_env_clone.project_language;
            let lang_label = if lang.is_empty() { "未知" } else { lang };
            let proj_root = rt_env_clone
                .project_root
                .as_deref()
                .unwrap_or(&rt_env_clone.working_dir);
            let proj_name = proj_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut proj_ctx = format!(
                "[PROJECT]\n项目: {proj_name} | 语言/框架: {lang_label}\n路径: {}",
                proj_root.display()
            );
            if let Some(ref dir) = dir_tree {
                let dir_slim: String = dir.lines().take(30).collect::<Vec<_>>().join("\n");
                if !dir_slim.is_empty() {
                    proj_ctx.push_str(&format!("\n关键目录:\n{dir_slim}"));
                }
            }
            proj_ctx.push_str("\n\n行动链: find_symbol 定位 → file_read 精准读 → deliver(plan) 确认 → edit/shell 实施 → finish");
            proj_ctx.push_str("\n📚 项目 skill: load_skill(name) 加载项目规范。写代码前先看规范，匹配既有命名/包结构/模式。");
            turn_messages.insert(1, Message::system(&proj_ctx));
        }

        // Inject background info as one compact system message
        let mut bg_parts = Vec::new();
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
            // ── Seamless semantic pre-retrieval (Normal turns only) ──
            // Ground the LLM in the code graph before it starts reasoning, using
            // the user's own words. Latency-safe: only when the graph is already
            // running and clean; bounded by a short timeout.
            if matches!(variant, TurnVariant::Normal) {
                if let Some(hint) = build_codegraph_hint(&gitnexus, &effective_text).await {
                    turn_messages.push(Message::system(&hint));
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

/// Last N messages of the previous round to carry over as read-only context.
const PREV_ROUND_TAIL_BRIDGE: usize = 6;

/// Trim messages before the last [ROUND_BOUNDARY] to prevent old task context
/// from leaking into new tasks, while keeping a short read-only tail of the
/// immediately-previous round so related follow-ups have real prior context.
///
/// Returns `(messages, trimmed)` where `trimmed` is true when we actually
/// scoped down to the current round (so callers can keep full fidelity instead
/// of collapsing into a refined summary).
fn truncate_before_last_round_boundary(messages: Vec<Message>) -> (Vec<Message>, bool) {
    let last_boundary = messages.iter().rposition(
        |m| matches!(m, Message::System { content } if content.starts_with("[ROUND_BOUNDARY]")),
    );
    let Some(pos) = last_boundary else {
        return (messages, false);
    };
    // Boundary at the very front (pos <= 1) means nothing to trim.
    if pos <= 1 {
        return (messages, false);
    }

    let system = messages[0].clone();
    // Bridge: last few messages of the previous round, marked HISTORICAL.
    let tail_start = pos.saturating_sub(PREV_ROUND_TAIL_BRIDGE).max(1);
    let bridge = &messages[tail_start..pos];

    let mut result = Vec::with_capacity(messages.len() - tail_start + 2);
    result.push(system);
    if !bridge.is_empty() {
        result.push(Message::system(
            "[PREV_ROUND_TAIL]\n以下为上一轮末尾片段（HISTORICAL — 只读参考，勿当作本轮待办或重复执行）：",
        ));
        result.extend_from_slice(bridge);
    }
    result.extend_from_slice(&messages[pos..]); // boundary + current round
    (result, true)
}

/// Run a semantic code-graph query on the user's raw input and format it as a
/// compact `[CODE_GRAPH_HINT]` block to pre-ground the LLM.
///
/// Strictly latency-safe, mirroring `find_symbol` enrichment: returns `None`
/// unless the GitNexus server is already running AND the index is clean. It
/// never spawns, restarts, or reindexes, and is bounded by a short timeout so a
/// slow graph can't stall the turn start.
async fn build_codegraph_hint(
    gitnexus: &Option<Arc<ox_core::mcp::GitNexusService>>,
    user_text: &str,
) -> Option<String> {
    let svc = gitnexus.as_ref()?;
    let q = user_text.trim();
    // Skip trivial inputs (confirmations like "ok"/"继续") — no useful semantics.
    if q.chars().count() < 4 {  // 降低到 4，让更多查询触发
        return None;
    }
    if !svc.is_ready().await {
        return None; // not ready → no latency, no hint
    }

    // FIX: dirty 时给降级提示，而非完全跳过
    if svc.is_dirty() {
        return Some(
            "[CODE_GRAPH_HINT]\n\
             🔗 代码图谱可用但有未索引改动（可能不完全准确）。\n\
             💡 手动用 code_graph 查询会自动触发增量更新。".to_string()
        );
    }

    let mut params = ox_core::mcp::gitnexus::QueryParams::new(q);
    params.limit = Some(5);
    // FIX: 增加超时到 10 秒
    let res = tokio::time::timeout(std::time::Duration::from_secs(10), svc.query(&params))
        .await
        .ok()?
        .ok()?;
    if res.is_error {
        return None;
    }
    let text = res.text.trim();
    if text.is_empty() {
        return None;
    }
    let body = truncate_on_line_boundary(text, 1800);
    Some(format!(
        "[CODE_GRAPH_HINT]\n🔗 代码图谱预检索（按你的问题语义检索，仅供定位参考，非完整答案）:\n{body}\n\n（要更深的调用关系/影响面，用 code_graph 继续查；与代码不符以实际 file_read 为准）"
    ))
}

/// Cap text to ~`max_chars` on a **line** boundary so the LLM never sees a
/// half-written entry; appends a clear truncation marker.
fn truncate_on_line_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let window = &text[..end];
    let kept = match window.rfind('\n') {
        Some(nl) if nl > 0 => &window[..nl],
        _ => window,
    };
    format!("{}\n…（已截断；用 code_graph 查看完整）", kept.trim_end())
}

/// Extract workflow step information (step prompt, step index).
fn get_workflow_step_info(
    workflow_engine: &Option<Arc<tokio::sync::Mutex<ox_core::agent::engine::WorkflowEngine>>>,
) -> (Option<String>, usize) {
    if let Some(wf) = workflow_engine {
        if let Ok(engine) = wf.try_lock() {
            let prompt = engine.get_step_system_prompt();
            let idx = engine.get_current_step_index();
            return (prompt, idx);
        }
    }
    (None, 0)
}
