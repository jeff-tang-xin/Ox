use super::WorkflowEngine;

pub(crate) const IMPL_READ_KEY: &str = "_impl_files_read";
pub(crate) const IMPL_EDITED_KEY: &str = "_impl_files_edited";
pub(crate) const IMPL_IMPACT_KEY: &str = "_impl_impact_done";
pub(crate) const CODE_GRAPH_QUERIED_KEY: &str = "_code_graph_queried";
pub(crate) const REVIEW_HANDOFF_KEY: &str = "_review_handoff_files";

pub(crate) fn bootstrap_implementation_plan(engine: &WorkflowEngine) {
    if let Some(findings) = crate::agent::perception::load(engine) {
        let tracker = crate::agent::perception::to_plan_tracker(&findings);
        tracing::info!(
            "[IMPL] Loaded {} steps from frozen findings",
            tracker.steps.len()
        );
        engine.set_variable(
            "_plan_tracker",
            crate::agent::plan_tracker::tracker_to_json(&tracker),
        );
        clear_impl_files_read(engine);
        return;
    }

    let report = get_execute_review_report(engine)
        .or_else(|| engine.get_variable("_step3_output"));
    let Some(report) = report.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    if let Some(tracker) = crate::agent::plan_tracker::load_from_review_report(&report) {
        tracing::info!(
            "[IMPL] Loaded {} implementation steps from review report",
            tracker.steps.len()
        );
        engine.set_variable(
            "_plan_tracker",
            crate::agent::plan_tracker::tracker_to_json(&tracker),
        );
        clear_impl_files_read(engine);
    }
}

pub(crate) fn bootstrap_implementation_plan_from_findings(engine: &WorkflowEngine) {
    if let Some(store) = crate::agent::findings::load_or_migrate(engine) {
        let only_scoped = !store.active_indices.is_empty();
        let tracker = store.to_plan_tracker(only_scoped);
        engine.set_variable(
            "_plan_tracker",
            crate::agent::plan_tracker::tracker_to_json(&tracker),
        );
        clear_impl_files_read(engine);
        return;
    }
    bootstrap_implementation_plan(engine);
}

pub(crate) fn sync_plan_from_findings(engine: &WorkflowEngine) {
    bootstrap_implementation_plan_from_findings(engine);
}

pub(crate) fn record_impl_file_read(engine: &WorkflowEngine, path: &str, _arguments: &str) {
    let norm = crate::agent::plan_tracker::normalize_path(path);
    let key = format!("{}:{}", IMPL_READ_KEY, norm);
    let count = engine
        .get_variable(&key)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    engine.set_variable(&key, (count + 1).to_string());
}

pub(crate) fn clear_impl_files_read(engine: &WorkflowEngine) {
    engine.set_variable(IMPL_READ_KEY, "[]".to_string());
}

pub(crate) fn impl_file_already_read(engine: &WorkflowEngine, path: &str) -> bool {
    let norm = crate::agent::plan_tracker::normalize_path(path);
    super::validation::impl_file_read_count(engine, &norm) > 0
}

pub(crate) fn record_impl_file_edited(engine: &WorkflowEngine, path: &str) {
    let norm = crate::agent::plan_tracker::normalize_path(path);
    let mut list: Vec<String> = engine
        .get_variable(IMPL_EDITED_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if list
        .iter()
        .any(|p| crate::agent::plan_tracker::normalize_path(p) == norm)
    {
        return;
    }
    list.push(path.to_string());
    if let Ok(json) = serde_json::to_string(&list) {
        engine.set_variable(IMPL_EDITED_KEY, json);
    }
}

pub(crate) fn impl_files_read_set(
    engine: &WorkflowEngine,
) -> std::collections::HashSet<String> {
    engine
        .get_variable(IMPL_READ_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn impl_impact_done(engine: &WorkflowEngine, finding_index: u32) -> bool {
    let list: Vec<u32> = engine
        .get_variable(IMPL_IMPACT_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    list.contains(&finding_index)
}

pub(crate) fn record_impl_impact(engine: &WorkflowEngine, finding_index: u32) {
    let mut list: Vec<u32> = engine
        .get_variable(IMPL_IMPACT_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !list.contains(&finding_index) {
        list.push(finding_index);
        if let Ok(json) = serde_json::to_string(&list) {
            engine.set_variable(IMPL_IMPACT_KEY, json);
        }
    }
}

pub(crate) fn clear_impl_impact(engine: &WorkflowEngine) {
    engine.set_variable(IMPL_IMPACT_KEY, "[]".to_string());
}

pub(crate) fn impl_code_graph_queried(engine: &WorkflowEngine) -> bool {
    engine.get_variable(CODE_GRAPH_QUERIED_KEY).as_deref() == Some("1")
}

pub(crate) fn record_code_graph_queried(engine: &WorkflowEngine) {
    engine.set_variable(CODE_GRAPH_QUERIED_KEY, "1".to_string());
}

pub(crate) fn clear_code_graph_queried(engine: &WorkflowEngine) {
    engine.set_variable(CODE_GRAPH_QUERIED_KEY, String::new());
}

pub(crate) fn snapshot_review_handoff(engine: &WorkflowEngine) {
    let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for p in super::exploration::get_explored_path_set(engine) {
        if !p.trim().is_empty() {
            files.insert(p);
        }
    }
    // Findings files are the highest-signal set — always carry them over.
    if let Some(store) = crate::agent::findings::load_or_migrate(engine) {
        for f in &store.findings {
            if !f.file.trim().is_empty() {
                files.insert(f.file.clone());
            }
        }
    }
    let files: Vec<String> = files.into_iter().collect();
    if files.is_empty() {
        return;
    }
    if let Ok(json) = serde_json::to_string(&files) {
        engine.set_variable(REVIEW_HANDOFF_KEY, json);
    }
}

pub(crate) fn review_handoff_files(engine: &WorkflowEngine) -> Vec<String> {
    engine
        .get_variable(REVIEW_HANDOFF_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn clear_review_handoff(engine: &WorkflowEngine) {
    engine.set_variable(REVIEW_HANDOFF_KEY, String::new());
}

pub(crate) fn mark_execute_report_delivered(engine: &WorkflowEngine) {
    engine.set_variable("_execute_report_delivered", "1".to_string());
}

pub(crate) fn execute_report_already_delivered(engine: &WorkflowEngine) -> bool {
    if crate::agent::phase::is_implementation_phase(engine) {
        return false;
    }
    if engine.get_variable("_execute_report_delivered").as_deref() == Some("1") {
        return true;
    }
    // Prior review in _step3_output — only block re-explore while still in read-only phase.
    engine
        .get_variable("_step3_output")
        .is_some_and(|s| WorkflowEngine::looks_like_review_report(&s))
}

pub(crate) fn clear_execute_report_delivered(engine: &WorkflowEngine) {
    engine.set_variable("_execute_report_delivered", String::new());
}

pub(crate) fn should_block_execute_reexplore(
    engine: &WorkflowEngine,
    tool_calls: &[crate::message::ToolCall],
    assistant_text: &str,
) -> bool {
    if !tool_calls.is_empty() && WorkflowEngine::looks_like_review_report(assistant_text) {
        mark_execute_report_delivered(engine);
    }
    if crate::agent::phase::is_implementation_phase(engine) {
        return false;
    }
    (execute_report_already_delivered(engine) || should_park_execute_output(engine, assistant_text))
        && super::exploration::tool_calls_are_reexplore_only(tool_calls)
}

pub(crate) fn is_perceive_execute(_engine: &WorkflowEngine) -> bool {
    false
}

pub(crate) fn should_park_execute_output(_engine: &WorkflowEngine, _text: &str) -> bool {
    false
}

pub(crate) fn get_execute_review_report(engine: &WorkflowEngine) -> Option<String> {
    engine
        .get_variable("_step3_output")
        .filter(|s| !s.trim().is_empty())
        .filter(|s| WorkflowEngine::looks_like_review_report(s))
}

pub(crate) fn execute_review_report_block(
    engine: &WorkflowEngine,
    max_chars: usize,
) -> Option<String> {
    get_execute_review_report(engine).map(|report| {
        let snippet: String = report.chars().take(max_chars).collect();
        format!("【审查报告 — park 前输出，用户在此基础上跟进】\n{snippet}")
    })
}