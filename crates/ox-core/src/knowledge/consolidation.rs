//! Phase-change and post-tool consolidation triggers for L0→L3 promotion.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::agent::engine::WorkflowEngine;
use crate::agent::phase;

use super::KnowledgeEngine;

const CONSOLIDATED_PHASE_KEY: &str = "_knowledge_consolidated_phase";

/// Run rule-based L0→L3 promotion when workflow phase changes (once per phase).
pub async fn maybe_on_phase_change(
    knowledge: &Arc<RwLock<KnowledgeEngine>>,
    engine: &WorkflowEngine,
) {
    let current = phase::get(engine).as_str().to_string();
    let last = engine
        .get_variable(CONSOLIDATED_PHASE_KEY)
        .unwrap_or_default();
    if last == current {
        return;
    }
    let session_id = engine.session_id();
    if let Ok(mut ke) = knowledge.try_write() {
        match ke.run_consolidation(&session_id, None) {
            Ok(n) if n > 0 => {
                tracing::info!("[CONSOLIDATION] phase={current} promoted {n} entities");
            }
            Ok(_) => tracing::debug!("[CONSOLIDATION] phase={current} — no promotions"),
            Err(e) => tracing::warn!("[CONSOLIDATION] phase change failed: {e}"),
        }
    } else {
        tracing::debug!("[CONSOLIDATION] knowledge busy — skipped on phase={current}");
    }
    engine.set_variable(CONSOLIDATED_PHASE_KEY, current);
}

/// Sync variant for `process_tool_execution` (already holds write lock).
pub fn on_tool_layering_check(engine: &mut KnowledgeEngine, session_id: &str) {
    match engine.run_consolidation(session_id, None) {
        Ok(n) if n > 0 => {
            tracing::info!("[CONSOLIDATION] post-tool promoted {n} entities");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("[CONSOLIDATION] post-tool failed: {e}"),
    }
}
