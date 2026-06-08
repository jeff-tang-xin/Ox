pub mod code_search;
pub mod content_validation;
pub mod delete_range;
pub mod edit_file;
pub mod file_list;
pub mod file_read;
pub mod file_search;
pub mod file_write;
pub mod find_symbol;
pub mod git;
pub mod intent_classifier;  // 新增：意图分类器
pub mod memory_search;
pub mod project_detect;
pub mod recall;
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
    /// Reference to the memory manager for knowledge retrieval
    pub memory: Arc<crate::memory::MemoryManager>,
    /// Reference to the code indexer for AST-aware symbol queries
    pub code_indexer: Arc<tokio::sync::Mutex<crate::symbol::CodeIndexer>>,
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
        memory: Arc<crate::memory::MemoryManager>,
        code_indexer: Arc<tokio::sync::Mutex<crate::symbol::CodeIndexer>>,
    ) -> Self {
        Self {
            runtime,
            working_dir,
            config,
            memory,
            code_indexer,
            tool_call_id: String::new(),
            progress_callback: None,
        }
    }

    /// Create a new ToolContext with progress callback support
    pub fn with_progress_callback(
        runtime: RuntimeEnvironment,
        working_dir: std::path::PathBuf,
        config: Arc<OxConfig>,
        memory: Arc<crate::memory::MemoryManager>,
        code_indexer: Arc<tokio::sync::Mutex<crate::symbol::CodeIndexer>>,
        tool_call_id: String,
        progress_callback: impl Fn(ToolProgress) + Send + Sync + 'static,
    ) -> Self {
        Self {
            runtime,
            working_dir,
            config,
            memory,
            code_indexer,
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
    pub skills: Vec<crate::skill::Skill>,
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
            skills: Vec::new(),
        };

        registry.register(Box::new(file_read::FileReadTool));
        registry.register(Box::new(file_write::FileWriteTool));
        registry.register(Box::new(edit_file::EditFileTool));
        registry.register(Box::new(file_list::FileListTool));
        registry.register(Box::new(file_search::FileSearchTool));
        registry.register(Box::new(code_search::CodeSearchTool));
        registry.register(Box::new(delete_range::DeleteRangeTool));
        registry.register(Box::new(find_symbol::FindSymbolTool));
        registry.register(Box::new(shell_exec::ShellExecTool));
        registry.register(Box::new(project_detect::ProjectDetectTool));
        registry.register(Box::new(web_fetch::WebFetchTool));
        registry.register(Box::new(memory_search::MemorySearchTool));
        registry.register(Box::new(recall::RecallTool));
        registry.register(Box::new(git::GitStatusTool));
        registry.register(Box::new(git::GitDiffTool));

        registry
    }
    
    /// Load Skills from filesystem and register them
    pub fn load_skills(&mut self, rt_env: &crate::runtime::RuntimeEnvironment) -> anyhow::Result<()> {
        use crate::skill::SkillLoader;
        
        let loader = SkillLoader::new(
            rt_env.ox_home_dir.join("skills"),
            rt_env.working_dir.join(".ox").join("skills")
        );
        
        // ⚠️ Cap at 10 skills to prevent context bloat
        // Keep the most recent skills (sorted by modification time)
        let mut skills = loader.load_enabled_skills()?;
        const MAX_SKILLS: usize = 10;
        if skills.len() > MAX_SKILLS {
            // Sort by creation time (newest first) and keep top N
            skills.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            skills.truncate(MAX_SKILLS);
            tracing::info!("Capped skills at {} (oldest trimmed)", MAX_SKILLS);
        }
        self.skills = skills;
        
        tracing::info!("Loaded {} skills", self.skills.len());
        
        Ok(())
    }

    fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Get all tool schemas for LLM API calls (includes Skills as special tools).
    pub fn schemas(&self) -> Vec<crate::llm::ToolSchema> {
        let mut schemas: Vec<crate::llm::ToolSchema> = self.tools
            .values()
            .map(|t| crate::llm::ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();
        
        // Add Skills as special composite tools
        for skill in &self.skills {
            schemas.push(crate::llm::ToolSchema {
                name: format!("skill_{}", skill.id),
                description: format!(
                    "[SKILL] {} - {}\n\n{}",
                    skill.name,
                    skill.description,
                    skill.content
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "description": "This skill provides guidance. No parameters needed."
                }),
            });
        }
        
        schemas
    }

    /// List all tool names.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}
