/// Live knowledge graph update hooks — triggered after every tool execution.
///
/// Per design doc §3.1 (提取触发时机):
/// - After tool calls: immediately extract execution result + intent
/// - On key info: when user expresses preference, corrects AI, or provides project background
/// - On session end: global extraction when user exits or idles
///
/// This module provides `on_tool_executed()` which is called by the agent loop
/// after each tool completes, keeping the knowledge graph in sync with reality.

use super::entity::{Entity, EntityKind, EntityMetadata, Relation, RelationType};
use super::graph::EntityGraph;

/// Tool execution context passed to the live update hooks.
pub struct ToolExecutionContext {
    pub session_id: String,
    pub user_message: String,
    pub tool_name: String,
    pub tool_args: String,
    pub tool_result: String,
    pub is_error: bool,
    pub project_root: String,
}

/// Result of a live update: which entities were created/modified and why.
#[derive(Debug, Clone)]
pub struct LiveUpdateResult {
    /// New WorkingMemory entities created
    pub new_working_memories: Vec<Entity>,
    /// Updated/created CodeSymbol entities
    pub updated_symbols: Vec<Entity>,
    /// New AtomicMemory facts extracted from tool results
    pub extracted_facts: Vec<Entity>,
    /// Whether a layering check should be triggered
    pub trigger_layering_check: bool,
}

/// Core hook: called after every tool execution.
///
/// Returns updated entities that should be upserted into the EntityGraph
/// and stored in TriviumDB.
pub fn on_tool_executed(
    ctx: &ToolExecutionContext,
    graph: &EntityGraph,
) -> LiveUpdateResult {
    match ctx.tool_name.as_str() {
        "file_write" | "edit_file" | "delete_range" => {
            on_code_modification(ctx, graph)
        }
        "shell_exec" => {
            on_shell_execution(ctx)
        }
        "file_read" => {
            on_file_read(ctx)
        }
        "memory_search" | "recall" | "find_symbol" | "code_search" | "web_fetch" => {
            on_search_tool(ctx)
        }
        _ => on_generic_tool(ctx),
    }
}

/// Handle code-modifying tools (file_write, edit_file, delete_range).
///
/// Creates:
/// 1. WorkingMemory entity recording the modification
/// 2. Extracts file path for re-indexing
/// 3. Checks for key info extraction (user preferences, architecture patterns)
fn on_code_modification(
    ctx: &ToolExecutionContext,
    graph: &EntityGraph,
) -> LiveUpdateResult {
    let file_path = extract_file_path_from_args(&ctx.tool_args);

    let action = if ctx.is_error {
        format!("FAILED to {}: {}", ctx.tool_name, &ctx.tool_result[..ctx.tool_result.len().min(100)])
    } else {
        format!("{}: {}", ctx.tool_name, file_path.as_deref().unwrap_or("unknown file"))
    };

    let intent = extract_intent_from_message(&ctx.user_message);

    // WorkingMemory entity
    let mut wm = Entity::working_memory(
        &ctx.session_id,
        &action,
        intent.as_deref(),
        Some(&ctx.tool_result[..ctx.tool_result.len().min(200)]),
        vec![ctx.tool_name.clone()],
        !ctx.is_error,
    );

    // Link to modified symbols (if we know which file)
    if let Some(ref fp) = file_path {
        // Find existing symbols in this file
        let symbols_in_file: Vec<&Entity> = graph.find_outgoing(fp, None)
            .into_iter()
            .filter(|e| e.kind == EntityKind::CodeSymbol)
            .collect();

        for sym in &symbols_in_file {
            wm.relations.push(Relation {
                target_id: sym.id.clone(),
                relation_type: RelationType::ModifiesSymbol,
                weight: 0.9,
            });
        }
        wm.metadata = update_modified_entities(&wm.metadata, &symbols_in_file);
    }

    // Extract facts from the tool result
    let facts = extract_facts_from_tool_result(ctx);

    LiveUpdateResult {
        new_working_memories: vec![wm],
        updated_symbols: Vec::new(), // Symbols are re-indexed by the engine separately
        extracted_facts: facts,
        trigger_layering_check: !ctx.is_error, // Only on success
    }
}

/// Handle shell execution — check for build/test results.
fn on_shell_execution(ctx: &ToolExecutionContext) -> LiveUpdateResult {
    let is_build = ctx.tool_args.contains("cargo build")
        || ctx.tool_args.contains("cargo test")
        || ctx.tool_args.contains("go build")
        || ctx.tool_args.contains("go test")
        || ctx.tool_args.contains("npm test")
        || ctx.tool_args.contains("pytest");

    let action = if ctx.is_error {
        format!("shell exec FAILED: {}", &ctx.tool_args[..ctx.tool_args.len().min(80)])
    } else if is_build {
        format!("Build/test PASSED: {}", &ctx.tool_args[..ctx.tool_args.len().min(80)])
    } else {
        format!("shell exec: {}", &ctx.tool_args[..ctx.tool_args.len().min(80)])
    };

    let wm = Entity::working_memory(
        &ctx.session_id,
        &action,
        None,
        Some(&ctx.tool_result[..ctx.tool_result.len().min(200)]),
        vec!["shell_exec".into()],
        !ctx.is_error,
    );

    LiveUpdateResult {
        new_working_memories: vec![wm],
        updated_symbols: Vec::new(),
        extracted_facts: Vec::new(),
        trigger_layering_check: is_build && !ctx.is_error,
    }
}

/// Handle file_read — record what was read.
fn on_file_read(ctx: &ToolExecutionContext) -> LiveUpdateResult {
    let file_path = extract_file_path_from_args(&ctx.tool_args);
    let path_display = file_path.as_deref().unwrap_or("unknown");

    let wm = Entity::working_memory(
        &ctx.session_id,
        &format!("Read file: {}", path_display),
        None,
        None,
        vec!["file_read".into()],
        false,
    );

    LiveUpdateResult {
        new_working_memories: vec![wm],
        updated_symbols: Vec::new(),
        extracted_facts: Vec::new(),
        trigger_layering_check: false,
    }
}

/// Handle search/query tools — record what was searched.
fn on_search_tool(ctx: &ToolExecutionContext) -> LiveUpdateResult {
    let wm = Entity::working_memory(
        &ctx.session_id,
        &format!("Search via {}: {}", ctx.tool_name, &ctx.tool_args[..ctx.tool_args.len().min(80)]),
        None,
        None,
        vec![ctx.tool_name.clone()],
        false,
    );

    LiveUpdateResult {
        new_working_memories: vec![wm],
        updated_symbols: Vec::new(),
        extracted_facts: Vec::new(),
        trigger_layering_check: false,
    }
}

/// Handle generic/unknown tools.
fn on_generic_tool(ctx: &ToolExecutionContext) -> LiveUpdateResult {
    let wm = Entity::working_memory(
        &ctx.session_id,
        &format!("Tool {} executed", ctx.tool_name),
        None,
        None,
        vec![ctx.tool_name.clone()],
        false,
    );

    LiveUpdateResult {
        new_working_memories: vec![wm],
        updated_symbols: Vec::new(),
        extracted_facts: Vec::new(),
        trigger_layering_check: false,
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helpers
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Extract file path from tool arguments JSON.
fn extract_file_path_from_args(args: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
}

/// Extract the user's intent from their message.
fn extract_intent_from_message(message: &str) -> Option<String> {
    if message.is_empty() || message.len() > 500 {
        return None;
    }
    // Simple heuristic: take the first sentence-based chunk
    let first_line = message.lines().next().unwrap_or(message);
    let truncated: String = first_line.chars().take(200).collect();
    if truncated.len() < 5 {
        None
    } else {
        Some(truncated)
    }
}

/// Extract L1 AtomicMemory facts from tool execution results.
///
/// Filters for key info patterns: user preferences, architecture patterns,
/// error details, code changes.
fn extract_facts_from_tool_result(ctx: &ToolExecutionContext) -> Vec<Entity> {
    let mut facts = Vec::new();

    if ctx.is_error {
        // Extract error as AntiPattern
        let error_preview: String = ctx.tool_result.chars().take(300).collect();
        if error_preview.len() > 20 {
            facts.push(Entity::atomic_memory(
                &format!("Error during {}: {}", ctx.tool_name, error_preview),
                "AntiPattern",
                None,
                "",
                "ToolObservation",
            ));
        }
        return facts;
    }

    // Detect user preferences
    if contains_preference(&ctx.user_message) {
        facts.push(Entity::atomic_memory(
            &ctx.user_message,
            "Style",
            None,
            "",
            "UserExplicit",
        ));
    }

    // Detect architecture patterns in tool results
    if contains_arch_keywords(&ctx.tool_result) {
        let preview: String = ctx.tool_result.chars().take(300).collect();
        facts.push(Entity::atomic_memory(
            &format!("Architecture pattern observed: {}", preview),
            "Architectural",
            None,
            "",
            "ToolObservation",
        ));
    }

    // Detect business logic keywords
    if contains_business_keywords(&ctx.tool_result) {
        let preview: String = ctx.tool_result.chars().take(300).collect();
        facts.push(Entity::atomic_memory(
            &format!("Business logic: {}", preview),
            "Business",
            None,
            "",
            "ToolObservation",
        ));
    }

    facts
}

fn contains_preference(text: &str) -> bool {
    let keywords = [
        "prefer", "always use", "never use", "avoid", "should use",
        "习惯", "偏好", "以后都", "不要用", "改成",
    ];
    let lower = text.to_lowercase();
    keywords.iter().any(|k| lower.contains(k))
}

fn contains_arch_keywords(text: &str) -> bool {
    let keywords = [
        "module", "struct ", "trait ", "impl ", "interface", "abstract",
        "architecture", "design pattern", "middleware", "pipeline",
        "handler", "service", "repository", "factory", "builder",
    ];
    let lower = text.to_lowercase();
    keywords.iter().any(|k| lower.contains(k))
}

fn contains_business_keywords(text: &str) -> bool {
    let keywords = [
        "api", "endpoint", "model", "schema", "controller",
        "entity", "dto", "request", "response", "route",
        "auth", "login", "register", "user", "role", "permission",
    ];
    let lower = text.to_lowercase();
    keywords.iter().any(|k| lower.contains(k))
}

/// Update the modified_entities list in a WorkingMemory metadata.
fn update_modified_entities(metadata: &EntityMetadata, symbols: &[&Entity]) -> EntityMetadata {
    if let EntityMetadata::WorkingMemory {
        session_id,
        action,
        intent,
        result,
        tools_used,
        has_code_changes,
        modified_entities,
        self_state,
    } = metadata
    {
        let mut mods = modified_entities.clone();
        for sym in symbols {
            if !mods.contains(&sym.id) {
                mods.push(sym.id.clone());
            }
        }
        EntityMetadata::WorkingMemory {
            session_id: session_id.clone(),
            action: action.clone(),
            intent: intent.clone(),
            result: result.clone(),
            tools_used: tools_used.clone(),
            has_code_changes: *has_code_changes,
            modified_entities: mods,
            self_state: self_state.clone(),
        }
    } else {
        metadata.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_path() {
        let path = extract_file_path_from_args(r#"{"path": "src/auth.rs"}"#);
        assert_eq!(path, Some("src/auth.rs".to_string()));
    }

    #[test]
    fn test_on_code_modification_creates_working_memory() {
        let ctx = ToolExecutionContext {
            session_id: "sess-1".into(),
            user_message: "Fix the token validation bug".into(),
            tool_name: "edit_file".into(),
            tool_args: r#"{"path": "src/auth.rs", "content": "..."}"#.into(),
            tool_result: "Successfully patched src/auth.rs".into(),
            is_error: false,
            project_root: ".".into(),
        };
        let graph = EntityGraph::new();
        let result = on_tool_executed(&ctx, &graph);

        assert_eq!(result.new_working_memories.len(), 1);
        let wm = &result.new_working_memories[0];
        assert_eq!(wm.kind, EntityKind::WorkingMemory);
        assert!(wm.content.contains("edit_file"));
        assert!(wm.content.contains("src/auth.rs"));
        assert!(result.trigger_layering_check);
    }

    #[test]
    fn test_on_shell_execution_build_pass() {
        let ctx = ToolExecutionContext {
            session_id: "sess-1".into(),
            user_message: "Run the build".into(),
            tool_name: "shell_exec".into(),
            tool_args: "cargo build".into(),
            tool_result: "Compiling... Finished".into(),
            is_error: false,
            project_root: ".".into(),
        };
        let graph = EntityGraph::new();
        let result = on_tool_executed(&ctx, &graph);

        assert_eq!(result.new_working_memories.len(), 1);
        assert!(result.trigger_layering_check);
    }

    #[test]
    fn test_on_code_modification_error_extracts_antipattern() {
        let ctx = ToolExecutionContext {
            session_id: "sess-1".into(),
            user_message: "Fix the bug".into(),
            tool_name: "edit_file".into(),
            tool_args: r#"{"path": "src/auth.rs"}"#.into(),
            tool_result: "Error: file not found".into(),
            is_error: true,
            project_root: ".".into(),
        };
        let graph = EntityGraph::new();
        let result = on_tool_executed(&ctx, &graph);

        assert!(!result.extracted_facts.is_empty());
        let anti = &result.extracted_facts[0];
        assert!(anti.content.contains("Error"));
        // Verify it's AntiPattern type
        match &anti.metadata {
            EntityMetadata::AtomicMemory { memory_type, .. } => {
                assert_eq!(memory_type, "AntiPattern");
            }
            _ => panic!("Expected AtomicMemory"),
        }
    }

    #[test]
    fn test_extract_facts_preference_detection() {
        let ctx = ToolExecutionContext {
            session_id: "sess-1".into(),
            user_message: "I prefer tabs over spaces, always use 4-space indentation".into(),
            tool_name: "file_write".into(),
            tool_args: "{}".into(),
            tool_result: "Successfully written".into(),
            is_error: false,
            project_root: ".".into(),
        };
        let graph = EntityGraph::new();
        let result = on_tool_executed(&ctx, &graph);

        let has_style = result.extracted_facts.iter().any(|f| {
            match &f.metadata {
                EntityMetadata::AtomicMemory { memory_type, .. } => memory_type == "Style",
                _ => false,
            }
        });
        assert!(has_style, "Should extract user preference as Style");
    }
}
