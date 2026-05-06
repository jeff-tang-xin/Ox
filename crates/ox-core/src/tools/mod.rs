pub mod code_search;
pub mod content_validation;
pub mod file_list;
pub mod file_patch;
pub mod file_read;
pub mod file_search;
pub mod file_write;
pub mod git_commit;
pub mod git_diff;
pub mod git_status;
pub mod memory_search;
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
    /// Reference to the memory manager for knowledge retrieval
    pub memory: Arc<crate::memory::MemoryManager>,
}

impl ToolContext {
    /// Create a new ToolContext with the given runtime and working directory.
    pub fn new(runtime: RuntimeEnvironment, working_dir: std::path::PathBuf, config: Arc<OxConfig>, memory: Arc<crate::memory::MemoryManager>) -> Self {
        Self { runtime, working_dir, config, memory }
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
        };

        registry.register(Box::new(file_read::FileReadTool));
        registry.register(Box::new(file_write::FileWriteTool));
        registry.register(Box::new(file_patch::FilePatchTool));
        registry.register(Box::new(file_list::FileListTool));
        registry.register(Box::new(file_search::FileSearchTool));
        registry.register(Box::new(code_search::CodeSearchTool));
        registry.register(Box::new(shell_exec::ShellExecTool));
        registry.register(Box::new(project_detect::ProjectDetectTool));
        registry.register(Box::new(git_status::GitStatusTool));
        registry.register(Box::new(git_diff::GitDiffTool));
        registry.register(Box::new(git_commit::GitCommitTool));
        registry.register(Box::new(web_fetch::WebFetchTool));
        registry.register(Box::new(memory_search::MemorySearchTool));

        registry
    }

    fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Get all tool schemas for LLM API calls.
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

    /// List all tool names.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}
