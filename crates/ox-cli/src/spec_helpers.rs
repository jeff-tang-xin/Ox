/// Spec Mode helper functions for handling requirement creation and activation.

use crate::App;
use ox_core::message::Session;
use ox_core::runtime;

/// Pending smart naming request
pub struct PendingSmartNaming {
    pub content: String,
}

/// Result of smart name generation
pub struct SmartNameResult {
    pub requirement_name: String,
    pub content: String,
}

/// Parse /spec command arguments to determine mode and extract parameters
pub enum SpecMode {
    /// Activate existing requirement by name
    Activate(String),
    /// Create new with auto-extracted name
    AutoExtract { content: String },
    /// Create with manually specified name: "/spec <name>: <content>"
    ManualName { name: String, content: String },
    /// Create with LLM-generated smart name: "/spec --smart <content>"
    SmartName { content: String },
}

/// Parse the action string to determine spec mode
/// Note: This function does NOT check if a requirement exists - that should be done separately
pub fn parse_spec_mode(action: &str) -> SpecMode {
    let trimmed = action.trim();
    
    // Check for --smart flag
    if trimmed.starts_with("--smart ") || trimmed.starts_with("--smart\t") {
        let content = trimmed[8..].trim().to_string();
        return SpecMode::SmartName { content };
    }
    
    // Check for manual name pattern: "<name>: <content>"
    if let Some(colon_pos) = trimmed.find(':') {
        let potential_name = trimmed[..colon_pos].trim();
        let content = trimmed[colon_pos + 1..].trim();
        
        // Validate that the name part looks like a valid identifier (no spaces, reasonable length)
        if !potential_name.is_empty() 
            && potential_name.len() <= 50 
            && !potential_name.contains(' ')
            && content.len() > 10 { // Content should be substantial
            return SpecMode::ManualName {
                name: potential_name.to_string(),
                content: content.to_string(),
            };
        }
    }
    
    // Default: treat as content for auto-extraction OR existing requirement name
    // The caller should check if it's an existing requirement first
    SpecMode::AutoExtract {
        content: trimmed.to_string(),
    }
}

/// Activate an existing spec requirement by name
pub fn activate_existing_spec(
    app: &mut App,
    requirement_name: &str,
    project_root: Option<&std::path::Path>,
    session: &mut Session,
) {
    use ox_core::agent::progress::WorkflowProgress;
    
    if let Some(root) = project_root {
        if let Some(progress) = WorkflowProgress::load(root, requirement_name) {
            // Restore workflow state
            app.spec_active = true;
            
            // Activate workflow engine
            if let Some(ref engine_arc) = app.workflow_engine {
                if let Ok(mut engine) = engine_arc.try_lock() {
                    if let Err(e) = engine.activate_workflow("spec_workflow") {
                        tracing::warn!("Failed to activate spec workflow: {}", e);
                    } else {
                        // Advance to saved step
                        for _ in 0..progress.workflow_step_index {
                            let _ = engine.advance_step();
                        }
                    }
                }
            }
            
            // Persist to session
            if let Err(e) = session.persist_workflow_state(
                "spec",
                "spec_workflow",
                progress.workflow_step_index,
                Some(requirement_name),
            ) {
                tracing::error!("Failed to persist workflow state: {}", e);
            }
            
            app.output.push_system(&format!(
                "✅ Resumed requirement: {} (Step {}/{})",
                requirement_name,
                progress.workflow_step_index + 1,
                6
            ));
            
            // Show file status
            let req_dir = progress.get_requirement_dir(root);
            let spec_exists = req_dir.join("spec.md").exists();
            let task_exists = req_dir.join("task.md").exists();
            
            if spec_exists && task_exists {
                app.output.push_system("📄 Both spec.md and task.md exist");
            } else if spec_exists {
                app.output.push_system("📄 spec.md exists (task.md pending)");
            } else {
                app.output.push_system("📝 No files yet (will be created in Phase 1)");
            }
        } else {
            app.output.push_system(&format!("❌ Requirement '{}' not found.", requirement_name));
        }
    }
}

/// Create a new spec requirement from content
pub fn create_new_spec(
    app: &mut App,
    content: &str,
    project_root: Option<&std::path::Path>,
    session: &mut Session,
    _rt_env: &mut runtime::RuntimeEnvironment,
) {
    create_new_spec_with_mode(app, content, project_root, session, NameExtractionMode::Auto)
}

/// Name extraction mode
pub enum NameExtractionMode {
    /// Auto-extract from content (first 2-3 words)
    Auto,
    /// Use manually specified name
    Manual(String),
    /// Smart naming via LLM (placeholder for future implementation)
    Smart,
}

/// Create a new spec with specific name extraction mode
pub fn create_new_spec_with_mode(
    app: &mut App,
    content: &str,
    project_root: Option<&std::path::Path>,
    session: &mut Session,
    mode: NameExtractionMode,
) {
    if content.is_empty() {
        app.output.push_system("Please provide spec content. Usage: /spec <content>");
        return;
    }
    
    // Extract requirement name based on mode
    let requirement_name = match mode {
        NameExtractionMode::Auto => extract_requirement_name(content),
        NameExtractionMode::Manual(name) => sanitize_requirement_name(&name),
        NameExtractionMode::Smart => {
            // 🚨 Use LLM to generate smart name (async, handled in main loop)
            app.output.push_system("🧠 Generating smart name with LLM...");
            // The actual async call happens in the main loop via pending_smart_naming
            extract_requirement_name(content) // Fallback for now
        }
    };
    
    finalize_spec_creation(app, content, &requirement_name, project_root, session)
}

/// Finalize spec creation after name is determined
fn finalize_spec_creation(
    app: &mut App,
    content: &str,
    requirement_name: &str,
    project_root: Option<&std::path::Path>,
    session: &mut Session,
) {
    // 🚨 CRITICAL: Create directory structure FIRST (before LLM call)
    if let Some(root) = project_root {
        let req_dir = root.join(".ox").join("spec").join(requirement_name);
        
        // Create directory if it doesn't exist
        if !req_dir.exists() {
            match std::fs::create_dir_all(&req_dir) {
                Ok(_) => {
                    tracing::info!("Created requirement directory: {}", req_dir.display());
                }
                Err(e) => {
                    tracing::error!("Failed to create directory: {}", e);
                    app.output.push_error(&format!("Failed to create directory: {}", e));
                    return;
                }
            }
        }
        
        // Initialize progress.json
        use ox_core::agent::progress::WorkflowProgress;
        let mut progress = WorkflowProgress::new(
            requirement_name,
            "spec",
            "spec_workflow"
        );
        progress.workflow_step_index = 0; // Start at Step 1 (Phase 1)
        
        if let Err(e) = progress.save(root) {
            tracing::error!("Failed to save progress.json: {}", e);
        }
    }
    
    // Check if files already exist
    let requirement_dir = project_root.map(|root| {
        root.join(".ox").join("spec").join(requirement_name)
    });
    
    let spec_exists = requirement_dir.as_ref()
        .map(|dir| dir.join("spec.md").exists())
        .unwrap_or(false);
    let task_exists = requirement_dir.as_ref()
        .map(|dir| dir.join("task.md").exists())
        .unwrap_or(false);
    
    app.spec_content = content.to_string();
    app.spec_active = true;
    
    // Activate workflow engine
    if let Some(ref engine_arc) = app.workflow_engine {
        if let Ok(mut engine) = engine_arc.try_lock() {
            if let Err(e) = engine.activate_workflow("spec_workflow") {
                tracing::warn!("Failed to activate spec workflow: {}", e);
            } else {
                // 🚨 CRITICAL: Set requirement name in session state
                engine.set_variable("requirement_name", requirement_name.to_string());
                
                // Auto-advance based on existing files
                let mut advanced_steps = 0;
                if spec_exists && task_exists {
                    for _ in 0..3 {
                        let _ = engine.advance_step();
                    }
                    advanced_steps = 3;
                    app.output.push_system(
                        "✅ Detected existing spec.md and task.md. Skipping to execution phase..."
                    );
                } else if spec_exists {
                    for _ in 0..3 {
                        let _ = engine.advance_step();
                    }
                    advanced_steps = 3;
                    app.output.push_system(
                        "✅ Detected existing spec.md. Skipping to task planning phase..."
                    );
                }
                
                // Persist workflow state
                let current_step = if advanced_steps > 0 { advanced_steps } else { 0 };
                if let Err(e) = session.persist_workflow_state(
                    "spec",
                    "spec_workflow",
                    current_step,
                    Some(&requirement_name),
                ) {
                    tracing::error!("Failed to persist workflow state: {}", e);
                }
                
                // Save progress.json
                if let Some(root) = project_root {
                    use ox_core::agent::progress::WorkflowProgress;
                    let mut progress = WorkflowProgress::new(
                        &requirement_name,
                        "spec",
                        "spec_workflow"
                    );
                    progress.workflow_step_index = current_step;
                    if let Err(e) = progress.save(root) {
                        tracing::error!("Failed to save progress.json: {}", e);
                    }
                }
            }
        }
    }
    
    app.output.push_system(&format!(
        "✅ Spec requirement set ({} chars)",
        content.len()
    ));
    app.output.push_system(&format!(
        "📋 Requirement name: {}",
        requirement_name
    ));
    app.dirty = true;
    
    // Trigger auto-planning
    app.pending_spec_planning = Some(content.to_string());
}

/// Extract requirement name from content (first 2-3 words, kebab-case)
pub fn extract_requirement_name(content: &str) -> String {
    // Take first line, split into words, take first 2-3
    let first_line = content.lines().next().unwrap_or("");
    let words: Vec<&str> = first_line
        .split_whitespace()
        .filter(|w| w.len() > 1) // Skip single-char words like "a", "I"
        .take(3)
        .collect();
    
    if words.is_empty() {
        return "untitled-task".to_string();
    }
    
    // Convert to kebab-case: lowercase, replace spaces with hyphens
    let name = words.join("-").to_lowercase();
    
    // Sanitize: only allow alphanumeric and hyphens
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();
    
    // Remove leading/trailing hyphens
    let sanitized = sanitized.trim_matches('-');
    
    if sanitized.is_empty() {
        "untitled-task".to_string()
    } else {
        sanitized.to_string()
    }
}

/// Sanitize a manually specified requirement name
pub fn sanitize_requirement_name(name: &str) -> String {
    // Convert to lowercase and replace spaces/special chars with hyphens
    let lower = name.to_lowercase();
    let sanitized: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    
    // Collapse multiple hyphens into one
    let collapsed = collapse_hyphens(&sanitized);
    
    // Remove leading/trailing hyphens
    let trimmed = collapsed.trim_matches('-');
    
    if trimmed.is_empty() {
        "untitled-task".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Collapse multiple consecutive hyphens into one
fn collapse_hyphens(s: &str) -> String {
    let mut result = String::new();
    let mut last_was_hyphen = false;
    
    for c in s.chars() {
        if c == '-' {
            if !last_was_hyphen {
                result.push(c);
            }
            last_was_hyphen = true;
        } else {
            result.push(c);
            last_was_hyphen = false;
        }
    }
    
    result
}

/// Display incomplete tasks on startup
pub fn display_incomplete_tasks(app: &mut App, project_root: Option<&std::path::Path>) {
    use ox_core::agent::progress::{scan_all_progress, WorkflowProgress};
    
    if let Some(root) = project_root {
        let tasks = scan_all_progress(root);
        
        if tasks.is_empty() {
            return; // No incomplete tasks
        }
        
        app.output.push_system("\n📋 Incomplete Spec Mode Tasks:");
        app.output.push_system("─".repeat(50).as_str());
        
        for task in &tasks {
            let status_icon = match task.workflow_step_index {
                0 => "🔵", // Just started
                1 | 2 | 3 => "🟡", // In progress
                4 | 5 => "🟢", // Almost done
                _ => "⚪",
            };
            
            app.output.push_system(&format!(
                "  {} {} - Step {}/{} ({})",
                status_icon,
                task.requirement_name,
                task.workflow_step_index + 1,
                6,
                format_date(&task.last_updated)
            ));
        }
        
        app.output.push_system("─".repeat(50).as_str());
        app.output.push_system("Use /spec <name> to resume a task\n");
    }
}

/// Format date for display
fn format_date(rfc3339: &str) -> String {
    // Try to parse and format nicely, fallback to raw string
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        rfc3339[..16].to_string() // Just take YYYY-MM-DDTHH:MM
    }
}

/// Generate a smart requirement name using LLM
pub async fn generate_smart_name(
    content: &str,
    provider: &std::sync::Arc<dyn ox_core::llm::LlmProvider>,
) -> anyhow::Result<String> {
    use ox_core::llm::LlmStreamEvent;
    use ox_core::message::Message;
    
    let system_prompt = "You are an expert at extracting core concepts from requirement descriptions. \
                         Your task is to generate a concise, descriptive requirement name in kebab-case format.\n\
                         \n\
                         **RULES:**\n\
                         1. Extract the CORE CONCEPT (nouns/noun phrases), NOT verbs\n\
                         2. Filter out common action words: implement, create, add, fix, build, develop, etc.\n\
                         3. Use 2-4 words maximum\n\
                         4. Format: lowercase with hyphens (kebab-case)\n\
                         5. Focus on WHAT, not HOW\n\
                         \n\
                         **EXAMPLES:**\n\
                         - 'Implement order optimization with batch processing' → 'order-optimization'\n\
                           (NOT 'implement-order-optimization' - remove verb)\n\
                         - 'Add user authentication with OAuth2' → 'user-authentication'\n\
                           (NOT 'add-user-authentication' - remove verb)\n\
                         - 'Create payment integration module' → 'payment-integration'\n\
                           (NOT 'create-payment-module' - focus on core concept)\n\
                         - 'Fix database connection pooling issue' → 'database-connection-pooling'\n\
                           (NOT 'fix-database-issue' - be specific about the concept)\n\
                         \n\
                         **OUTPUT FORMAT:**\n\
                         Return ONLY the kebab-case name, nothing else. No explanation, no quotes.";
    
    let user_message = format!("Generate a requirement name for: {}", content);
    
    let messages = vec![
        Message::system(system_prompt),
        Message::user(&user_message),
    ];
    
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LlmStreamEvent>();
    
    // Spawn the LLM call with timeout
    let provider_clone = std::sync::Arc::clone(provider);
    let msgs = messages.clone();
    let handle = tokio::spawn(async move {
        let _ = provider_clone.stream_chat(&msgs, &[], tx).await;
    });
    
    // Collect the response with timeout (5 seconds)
    let mut name = String::new();
    let timeout_duration = std::time::Duration::from_secs(5);
    
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(LlmStreamEvent::TextDelta(delta)) => {
                        name.push_str(&delta);
                    }
                    Some(LlmStreamEvent::Done { .. }) => break,
                    Some(LlmStreamEvent::Error(err)) => {
                        return Err(anyhow::anyhow!("LLM error: {}", err));
                    }
                    Some(LlmStreamEvent::ToolCallStart { .. }) | 
                    Some(LlmStreamEvent::ToolCallArgumentsDelta { .. }) |
                    Some(LlmStreamEvent::ToolCallEnd { .. }) => {
                        // Ignore tool call events for name generation
                    }
                    None => break, // Channel closed
                }
            }
            _ = tokio::time::sleep(timeout_duration) => {
                tracing::warn!("Smart name generation timed out after {:?}", timeout_duration);
                return Err(anyhow::anyhow!("Timeout: LLM took too long to generate name"));
            }
        }
    }
    
    // Wait for the task to complete
    let _ = handle.await;
    
    if name.is_empty() {
        return Err(anyhow::anyhow!("LLM returned empty response"));
    }
    
    // Clean up the generated name
    let cleaned = name.trim().to_lowercase();
    let sanitized = sanitize_requirement_name(&cleaned);
    
    if sanitized.is_empty() || sanitized == "untitled-task" {
        return Err(anyhow::anyhow!("Generated name is invalid: {}", sanitized));
    }
    
    Ok(sanitized)
}
