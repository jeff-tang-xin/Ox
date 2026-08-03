pub mod ast_extractor;
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
pub mod path_guard;
pub mod project_detect;
pub mod read_symbol;
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
    /// PathGuard — 会话级路径纠偏中间件（带 60s TTL 文件索引缓存）。
    /// None 时 dispatch 层会创建临时实例（无缓存复用）。
    pub path_guard: Option<Arc<crate::tools::path_guard::PathGuard>>,
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
    ) -> Self {
        // 自动创建会话级 PathGuard（如果 project_root 存在）
        let path_guard = runtime
            .project_root
            .as_ref()
            .map(|root| Arc::new(crate::tools::path_guard::PathGuard::new(root.clone())));
        Self {
            runtime,
            working_dir,
            config,
            gitnexus: None,
            memory_store: None,
            summarizer: None,
            tool_call_id: String::new(),
            progress_callback: None,
            path_guard,
        }
    }

    /// Attach the GitNexus code-graph service (builder style).
    pub fn with_gitnexus(mut self, gitnexus: Option<Arc<crate::mcp::GitNexusService>>) -> Self {
        self.gitnexus = gitnexus;
        self
    }

    /// Attach the cross-session memory store (builder style).
    pub fn with_memory_store(
        mut self,
        store: Option<Arc<crate::memory::store::MemoryStore>>,
    ) -> Self {
        self.memory_store = store;
        self
    }

    /// Attach the memory-graph offload summarizer (builder style).
    pub fn with_summarizer(mut self, summarizer: Option<Arc<dyn crate::llm::LlmProvider>>) -> Self {
        self.summarizer = summarizer;
        self
    }

    /// Attach a session-level PathGuard (builder style).
    /// When set, the dispatch layer reuses this instance across all tool calls,
    /// so the 60s TTL file index cache is shared and invalidate() actually works.
    pub fn with_path_guard(mut self, guard: Option<Arc<crate::tools::path_guard::PathGuard>>) -> Self {
        self.path_guard = guard;
        self
    }

    /// Create a new ToolContext with progress callback support
    pub fn with_progress_callback(
        runtime: RuntimeEnvironment,
        working_dir: std::path::PathBuf,
        config: Arc<OxConfig>,
        tool_call_id: String,
        progress_callback: impl Fn(ToolProgress) + Send + Sync + 'static,
    ) -> Self {
        let path_guard = runtime
            .project_root
            .as_ref()
            .map(|root| Arc::new(crate::tools::path_guard::PathGuard::new(root.clone())));
        Self {
            runtime,
            working_dir,
            config,
            gitnexus: None,
            memory_store: None,
            summarizer: None,
            tool_call_id,
            progress_callback: Some(Arc::new(progress_callback)),
            path_guard,
        }
    }

    /// Stable base for relative file-path resolution.
    ///
    /// Returns `runtime.project_root` when available (the project root is
    /// recomputed by `find_project_root` walking up from the current directory,
    /// so it stays anchored to the project even after `/cd` into a subdirectory).
    /// Falls back to `working_dir` when no project markers are found.
    ///
    /// File tools (`file_read`, `edit_file`, `file_write`, `code_search`, …)
    /// MUST resolve relative paths against this base — NOT against
    /// `working_dir`, which can be mutated by `/cd` and would break the LLM's
    /// "paths are relative to the project root" mental model. Shell/git tools
    /// continue to use `working_dir` for process CWD semantics.
    pub fn path_base(&self) -> std::path::PathBuf {
        self.runtime
            .project_root
            .clone()
            .unwrap_or_else(|| self.working_dir.clone())
    }

    /// 获取 PathGuard 纠偏历史的格式化文本（用于 system prompt 注入）
    /// 超过 2 条纠偏记录就返回文本，用模型自己的错误教模型
    /// 委托给全局静态存储函数
    pub fn path_corrections_for_prompt(&self) -> Option<String> {
        crate::tools::path_guard::get_global_corrections_for_prompt()
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

// ── Path display helpers ────────────────────────────────────────────────────

/// Compute the canonical project-relative path for display to the LLM.
///
/// Returns the path relative to `path_base` with forward slashes (cross-platform).
/// Falls back to the resolved path's string form if it can't be stripped (e.g.
/// absolute path outside the project root).
///
/// This is the **single source of truth** for how paths appear in tool outputs.
/// Every path-based tool (file_read/edit_file/file_write/delete_range) MUST use
/// this to format the path in its success output, so the LLM always sees the
/// canonical project-relative path — calibrating its mental model and preventing
/// the "short path" hallucination loop where it keeps using `openai.rs` instead
/// of `crates/ox-core/src/llm/openai.rs`.
pub fn canonical_rel_path(resolved_path: &std::path::Path, path_base: &std::path::Path) -> String {
    resolved_path
        .strip_prefix(path_base)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            // Outside project root or absolute — normalize separators only.
            resolved_path.to_string_lossy().replace('\\', "/")
        })
}

/// Format a path header line for tool success output.
///
/// Prepend this to file_read/edit_file/file_write/delete_range success messages
/// so the LLM sees the canonical path as the first line of every result.
///
/// Example: `📄 crates/ox-core/src/llm/openai.rs\n`
pub fn format_path_header(resolved_path: &std::path::Path, path_base: &std::path::Path) -> String {
    let rel = canonical_rel_path(resolved_path, path_base);
    format!("📄 {rel}\n")
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
        registry.register(Box::new(read_symbol::ReadSymbolTool));
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
            skills.sort_by_key(|s| std::cmp::Reverse(s.created_at));
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

    /// Agent tool list: unified single schema or full registry + finish.
    pub fn schemas_for_agent(&self, unified_tool_mode: bool) -> Vec<crate::llm::ToolSchema> {
        if unified_tool_mode {
            crate::agent::unified_action::unified_tool_schemas()
        } else {
            let mut schemas = self.schemas();
            // 添加 finish 工具（用于结束本轮）
            schemas.push(crate::agent::unified_action::finish_tool_schema());
            schemas
        }
    }

    /// List all tool names.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}
