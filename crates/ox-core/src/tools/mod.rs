pub mod code_graph;
pub mod code_search;
pub mod complete_and_check;
pub mod content_validation;
pub mod delete_range;
pub mod edit_file;
pub mod file_list;
pub mod file_read;
pub mod file_search;
pub mod file_write;
pub mod find_symbol;
pub mod git;
pub mod intent_classifier;
pub mod load_skill;
pub mod project_detect;
pub mod shell_exec;
pub mod web_fetch;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::OxConfig;
use crate::runtime::RuntimeEnvironment;

/// Safety level of a tool operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyLevel {
    /// Always safe — no side effects (e.g. file_read, file_list).
    Safe,
    /// Modifies files — requires confirmation unless trusted.
    RequiresConfirmation,
    /// Dangerous — always requires confirmation (e.g. shell_exec, git_commit).
    Dangerous,
}

/// Output of a tool execution.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// If the tool changed the working directory (e.g. shell cd), carry the new path.
    pub new_working_dir: Option<std::path::PathBuf>,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            new_working_dir: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            new_working_dir: None,
        }
    }
}

/// Context passed to tools during execution.
/// Owns its data so it can be shared via `Arc` across async tasks.
#[derive(Clone)]
pub struct ToolContext {
    pub runtime: RuntimeEnvironment,
    pub working_dir: std::path::PathBuf,
    pub config: Arc<OxConfig>,
    /// Unified knowledge engine (optional; disabled when embedding is removed)
    pub knowledge: Option<Arc<tokio::sync::RwLock<crate::knowledge::KnowledgeEngine>>>,
    /// GitNexus code-graph service (optional; None when unavailable/disabled).
    pub gitnexus: Option<Arc<crate::mcp::GitNexusService>>,
    /// Cross-session memory store (SQLite-backed).
    pub memory_store: Option<Arc<crate::memory::store::MemoryStore>>,
    /// Optional summarizer LLM for memory-graph offload (None = use main provider).
    pub summarizer: Option<Arc<dyn crate::llm::LlmProvider>>,
    /// Current tool call ID (for progress reporting)
    pub tool_call_id: String,
    /// Optional progress callback for real-time updates
    pub progress_callback: Option<Arc<dyn Fn(ToolProgress) + Send + Sync>>,
}

/// Progress update from a tool execution
#[derive(Debug, Clone)]
pub struct ToolProgress {
    pub tool_call_id: String,
    pub tool_name: String,
    pub message: String,
    pub progress_percent: Option<u8>, // 0-100
}

impl ToolContext {
    /// Create a new ToolContext with the given runtime and working directory.
    pub fn new(
        runtime: RuntimeEnvironment,
        working_dir: std::path::PathBuf,
        config: Arc<OxConfig>,
        knowledge: Option<Arc<tokio::sync::RwLock<crate::knowledge::KnowledgeEngine>>>,
    ) -> Self {
        Self {
            runtime,
            working_dir,
            config,
            knowledge,
            gitnexus: None,
            memory_store: None,
            summarizer: None,
            tool_call_id: String::new(),
            progress_callback: None,
        }
    }

    /// Attach the GitNexus code-graph service (builder style).
    pub fn with_gitnexus(mut self, gitnexus: Option<Arc<crate::mcp::GitNexusService>>) -> Self {
        self.gitnexus = gitnexus;
        self
    }

    /// Attach the cross-session memory store (builder style).
    pub fn with_memory_store(mut self, store: Option<Arc<crate::memory::store::MemoryStore>>) -> Self {
        self.memory_store = store;
        self
    }

    /// Attach the memory-graph offload summarizer (builder style).
    pub fn with_summarizer(mut self, summarizer: Option<Arc<dyn crate::llm::LlmProvider>>) -> Self {
        self.summarizer = summarizer;
        self
    }

    /// Create a new ToolContext with progress callback support
    pub fn with_progress_callback(
        runtime: RuntimeEnvironment,
        working_dir: std::path::PathBuf,
        config: Arc<OxConfig>,
        knowledge: Option<Arc<tokio::sync::RwLock<crate::knowledge::KnowledgeEngine>>>,
        tool_call_id: String,
        progress_callback: impl Fn(ToolProgress) + Send + Sync + 'static,
    ) -> Self {
        Self {
            runtime,
            working_dir,
            config,
            knowledge,
            gitnexus: None,
            memory_store: None,
            summarizer: None,
            tool_call_id,
            progress_callback: Some(Arc::new(progress_callback)),
        }
    }

    /// Report progress if callback is available
    pub fn report_progress(&self, message: String, progress_percent: Option<u8>) {
        if let Some(callback) = &self.progress_callback {
            callback(ToolProgress {
                tool_call_id: self.tool_call_id.clone(),
                tool_name: "".to_string(), // Will be set by caller
                message,
                progress_percent,
            });
        }
    }
}

/// Trait for all tools that the agent can invoke.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique name (matches LLM function call name).
    fn name(&self) -> &str;

    /// Description for LLM tool schema.
    fn description(&self) -> &str;

    /// JSON Schema for parameters.
    fn parameters_schema(&self) -> Value;

    /// Safety level of this tool.
    fn safety_level(&self) -> SafetyLevel;

    /// Execute the tool with given arguments.
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput;
}

/// Registry of all available tools.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    /// Skills loaded from files (treated as special composite tools)
    skills: std::sync::Mutex<Vec<crate::skill::Skill>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Create a new registry with all built-in tools.
    pub fn new() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            skills: std::sync::Mutex::new(Vec::new()),
        };

        registry.register(Box::new(file_read::FileReadTool));
        registry.register(Box::new(file_write::FileWriteTool));
        registry.register(Box::new(edit_file::EditFileTool));
        registry.register(Box::new(file_list::FileListTool));
        registry.register(Box::new(file_search::FileSearchTool));
        registry.register(Box::new(code_search::CodeSearchTool));
        registry.register(Box::new(delete_range::DeleteRangeTool));
        registry.register(Box::new(find_symbol::FindSymbolTool));
        registry.register(Box::new(load_skill::LoadSkillTool));
        registry.register(Box::new(shell_exec::ShellExecTool));
        registry.register(Box::new(project_detect::ProjectDetectTool));
        registry.register(Box::new(web_fetch::WebFetchTool));
        registry.register(Box::new(git::GitStatusTool));
        registry.register(Box::new(git::GitDiffTool));
        registry.register(Box::new(code_graph::CodeGraphTool));
        registry.register(Box::new(complete_and_check::CompleteAndCheckTool));

        registry
    }

    /// Load Skills from filesystem and register them
    pub fn load_skills(&self, rt_env: &crate::runtime::RuntimeEnvironment) -> anyhow::Result<()> {
        use crate::skill::SkillLoader;

        let loader = SkillLoader::new(
            rt_env.ox_home_dir.join("skills"),
            rt_env.working_dir.join(".ox").join("skills"),
        );

        // ⚠️ Cap at 10 skills to prevent context bloat
        // Keep the most recently modified skills (created_at = file mtime / frontmatter)
        let mut skills = loader.load_enabled_skills()?;
        const MAX_SKILLS: usize = 10;
        if skills.len() > MAX_SKILLS {
            skills.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            skills.truncate(MAX_SKILLS);
            tracing::info!("Capped skills at {} (oldest by mtime trimmed)", MAX_SKILLS);
        }
        *self.skills.lock().unwrap() = skills;

        tracing::info!("Loaded {} skills", self.skills.lock().unwrap().len());

        Ok(())
    }

    /// Get a snapshot of all loaded skills.
    pub fn get_skills_list(&self) -> Vec<crate::skill::Skill> {
        self.skills.lock().unwrap().clone()
    }

    /// Return true if any skills are loaded.
    pub fn has_skills(&self) -> bool {
        !self.skills.lock().unwrap().is_empty()
    }

    /// Reload skills from disk. Call after files created/modified in .ox/skills/.
    pub fn reload_skills(&self, rt_env: &RuntimeEnvironment) -> anyhow::Result<()> {
        self.load_skills(rt_env)
    }

    fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Get all tool schemas for LLM API calls.
    /// Skills are listed in the system prompt, not as tool schemas.
    pub fn schemas(&self) -> Vec<crate::llm::ToolSchema> {
        self.tools
            .values()
            .map(|t| crate::llm::ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect()
    }

    /// Agent tool list: unified single schema or full registry.
    pub fn schemas_for_agent(&self, unified_tool_mode: bool) -> Vec<crate::llm::ToolSchema> {
        if unified_tool_mode {
            crate::agent::unified_action::unified_tool_schemas()
        } else {
            self.schemas()
        }
    }

    /// List all tool names.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}
