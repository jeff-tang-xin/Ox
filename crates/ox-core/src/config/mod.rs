use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub mod rules;
use serde::{Deserialize, Serialize};

// ──────────────────────────── Top-level ────────────────────────────

/// Complete Ox configuration, loaded from `~/.ox/config.toml`.
/// Every field has `#[serde(default)]` so a missing/empty file yields valid defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct OxConfig {
    pub general: GeneralConfig,
    pub repl: ReplConfig,
    pub terminal: TerminalConfig,
    pub session: SessionConfig,
    pub context: ContextConfig,
    pub tools: ToolsConfig,
    pub models: ModelsConfig,
    pub agent: AgentConfig,
    pub memory: MemoryConfig,
    pub behavior_rules: BehaviorRulesConfig,
    pub enforcement_rules: rules::EnforcementRules,
    pub safety: SafetyConfig,
    pub cost: CostConfig,
    pub spec: SpecConfig,
}

impl OxConfig {
    /// Load configuration from file, falling back to defaults for any missing field.
    /// If the file doesn't exist, returns full defaults.
    /// Supports legacy migration of flat API key fields.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path(),
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)?;
        let raw: toml::Value = toml::from_str(&content)?;
        let mut config: OxConfig = toml::from_str(&content)?;
        // Migrate legacy flat API key fields to providers.
        config.models.migrate_legacy(&raw);
        Ok(config)
    }

    /// Default config path: `~/.ox/config.toml`.
    /// Falls back to legacy `~/.config/ox/config.toml` if the new path doesn't exist.
    pub fn default_config_path() -> PathBuf {
        let primary = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ox")
            .join("config.toml");

        if primary.exists() {
            return primary;
        }

        // Legacy path fallback.
        let legacy = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ox")
            .join("config.toml");

        if legacy.exists() {
            tracing::warn!(
                "Config found at old path {}. Please move to {}",
                legacy.display(),
                primary.display()
            );
            return legacy;
        }

        primary
    }

    /// Check whether the default config file exists.
    pub fn config_exists() -> bool {
        let path = Self::default_config_path();
        path.exists()
    }

    /// Create a default config file at `~/.ox/config.toml` with commented examples.
    /// Returns the path written to, or an error if it already exists or IO fails.
    pub fn init_default_config() -> anyhow::Result<PathBuf> {
        let path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ox")
            .join("config.toml");

        if path.exists() {
            anyhow::bail!("Config already exists at {}", path.display());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&path, DEFAULT_CONFIG_TEMPLATE)?;
        Ok(path)
    }
}

/// Default config.toml template with commented examples.
const DEFAULT_CONFIG_TEMPLATE: &str = r##"# ════════════════════════════════════════════════════════════
# Ox CLI Configuration
# Location: ~/.ox/config.toml
# ════════════════════════════════════════════════════════════

# ── General Settings ──────────────────────────────────────
[general]
# version = "2.1"              # Config version (for future compatibility)
# debug_mode = false           # Enable debug logging
# verbose = false              # Show detailed output
# lang = "en"                  # Language: "en", "zh", etc.

# ── LLM Models ───────────────────────────────────────────
[models]
default = "gpt-4o"             # Default model to use
# backup = ["claude-sonnet-4", "gpt-4-turbo"]  # Fallback models
# adaptive_thinking = true     # Enable adaptive reasoning depth
# effort_level = "high"        # Reasoning effort: "low", "medium", "high"

# ── Provider Configuration ───────────────────────────────
# Each provider has its own section under [models.providers.<name>].
# API keys can also be set via environment variables:
#   OX_OPENAI_API_KEY, OX_ANTHROPIC_API_KEY, OX_DEEPSEEK_API_KEY
# Environment variables take priority over config file values.

[models.providers.openai]
api_key = ""                   # Your OpenAI API key (sk-...)
# base_url = "https://api.openai.com/v1"       # Custom API endpoint
# max_tokens = 4096            # Maximum response tokens

[models.providers.anthropic]
api_key = ""                   # Your Anthropic API key (sk-ant-...)
# base_url = "https://api.anthropic.com/v1"    # Custom API endpoint
# max_tokens = 8192            # Maximum response tokens

[models.providers.deepseek]
api_key = ""                   # Your DeepSeek API key
# base_url = "https://api.deepseek.com/v1"     # Custom API endpoint
# max_tokens = 4096            # Maximum response tokens

# ── Advanced Model Configuration ─────────────────────────
# Default provider — overrides automatic model-name prefix detection.
# Example: default_provider = "openai"  (all models route through openai provider)
# default_provider = "openai"

# Explicit model→provider mapping (overrides automatic detection).
# Use this for custom model names or non-standard providers.
# [models.model_providers]
# "deepseek-v4-pro" = "openai"
# "custom-model" = "anthropic"

# ── REPL Settings ────────────────────────────────────────
[repl]
# history_file = "~/.ox/history"         # Command history file
# max_history_entries = 10000            # Maximum history entries
# multiline_enabled = true               # Enable multi-line input
# stream_output = true                   # Stream responses in real-time
# syntax_highlight = true                # Enable syntax highlighting

# ── Terminal UI Settings ─────────────────────────────────
[terminal]
# split_view = true            # Split view mode (input/output panes)
# output_ratio = 85            # Output pane height percentage (0-100)
# urgent_prefix = "!"          # Prefix for urgent messages
# input_during_agent = true    # Allow typing while agent is working

# ── Session Management ───────────────────────────────────
[session]
# auto_restore = true          # Auto-restore last session on startup
# max_archived_sessions = 50   # Maximum archived sessions to keep

# ── Context Window Management ────────────────────────────
[context]
# max_history_turns = 20       # Maximum conversation turns to keep
# memory_budget_tokens = 2000  # Token budget for memory context
# history_budget_tokens = 50000  # Token budget for conversation history
# reply_reserve_tokens = 73000  # Reserve tokens for LLM response
# history_ratio = 0.10         # History budget ratio (10% of context window)
# memory_ratio = 0.02          # Memory budget ratio (2% of context window)
# system_prompt_ratio = 0.02   # System prompt budget ratio (2%)

# Refined context format: "User: ... Assistant: ... [tools]"
# Reduces context length by removing <think> tags and intermediate reasoning
# Recommended for reducing hallucinations and improving instruction following
use_refined_context = true  # Enable refined context format (default: true)

# ── Tool Execution Settings ──────────────────────────────
[tools]
# auto_confirm_safe = true     # Auto-confirm safe tool executions
# confirm_writes = true        # Require confirmation for file writes
# confirm_shell = true         # Require confirmation for shell commands
# shell_timeout_ms = 30000     # Shell command timeout (milliseconds)
# max_output_chars = 10000     # Max shell output characters (≈200-300 lines)
                                # Recommended: 5000(fast) ~ 20000(detailed) ~ 50000(max)

# ── Agent Loop Settings ──────────────────────────────────
[agent]
# max_iterations = 25          # Maximum agent loop iterations per turn
# max_per_turn_tokens = 500000  # Max tokens per turn before user confirmation

# ── Memory System ────────────────────────────────────────
[memory]
# max_nodes = 1000             # Maximum memory nodes to store
# alpha = 0.8                  # Importance weight for new memories
# time_decay = 0.01            # Time-based decay rate
# isolation_application = true # Isolate memories by application
# share_session_group = true   # Share memories within session group
# share_request = true         # Share memories across requests
# export_format = "json"       # Export format: "json" or "csv"
# janitor_run_on_startup_prob = 0.2  # Probability of cleanup on startup (0.0-1.0)

# Refined memory storage: Store condensed summaries instead of raw conversations
# Automatically extracts key insights, tools used, and code changes from each turn
# Significantly reduces memory bloat and improves retrieval quality
store_refined_memories = true  # Enable refined memory storage (default: true)

# Project-specific memory decay
[memory.project_decay]
# base_half_life = 30          # Days until memory importance halves
# critical_threshold = 0.3     # Below this, memory is considered low priority

# Global memory decay
[memory.overall_decay]
# beta = 0.015                 # Global decay rate

# Language-specific decay configurations (optional)
# Customize memory retention for different programming languages.
# [memory.language_config.rust]
# lambda = 0.02                # Decay rate for Rust memories
# max_retention_days = 30      # Maximum days to retain
# traces = [0.1, 0.2, 0.3, 0.4, 0.5]  # Depth-based retention weights

# [memory.language_config.python]
# lambda = 0.01
# max_retention_days = 90
# traces = [0.05, 0.15, 0.25, 0.35, 0.5]

# Memory transformation (consolidate similar memories)
[memory.transform]
# interval_days = 7            # Run transformation every N days
# batch_size = 20              # Process N memories per batch
# daily_token_cap = 10000      # Maximum tokens per day for transformation
# trigger = "manual"           # "manual" (use /memory transform) | "auto" (automatic)

# ── Behavior Rules (Coding Standards) ────────────────────
# Define custom coding rules that override built-in behavior.
# These rules are injected into the system prompt and MUST be followed.
#
# Option 1: Use built-in rules (set enforce_all = true)
# Option 2: Define custom rules (fill custom_rules array)
#
# Custom rules take HIGHEST PRIORITY and override built-in rules.
# Examples:
#   - "Always use Result<T, E> for error handling instead of unwrap()"
#   - "Prefer async/await over blocking operations for I/O"
#   - "Add doc comments to all public functions"
#   - "Follow Rust naming conventions (snake_case, PascalCase)"

[behavior_rules]
# Built-in rule toggles (only used when custom_rules is empty)
# enforce_safe_code = true     # Never bypass safety checks
# enforce_lint = true          # Run lint before declaring complete
# enforce_format = true        # Format code before writing files
# enforce_tests = true         # Write tests for new functions
# enforce_all = true           # Enable all built-in rules

# Custom coding rules (overrides built-in rules when not empty)
# custom_rules = [
#     "Use Result<T, anyhow::Error> for all fallible operations",
#     "Prefer async/await for I/O operations",
#     "Add #[derive(Debug)] to all custom types",
#     "Write integration tests for public APIs",
#     "Log errors with context (request_id, user_id)",
#     "Validate all user input before processing",
#     "Document public APIs with /// doc comments",
#     "Run cargo fmt and cargo clippy before committing"
# ]

# ── Enforcement Rules (Hard Constraints) ─────────────────
# These rules are enforced by code. If violated, tool calls are blocked immediately.
# Extracted from system-level Skills (coding-principles, engineering-practices).

[enforcement_rules]
# enabled = true               # Enable global enforcement
# plan_before_edit = true      # Require LLM to propose a plan before file_write/file_patch
# steps_before_shell = true    # Require LLM to list steps before shell_exec

# ── Safety Settings ──────────────────────────────────────
[safety]
# enable_sandbox = false       # Enable sandboxed execution
# confirm_dangerous_ops = true # Require confirmation for dangerous operations
# high_risk_apis = [           # APIs that require explicit confirmation
#     "Command::new",
#     "remove_dir_all",
#     "fs::remove_dir_all",
#     "os.remove",
#     "os.rmdir"
# ]
# custom_rules = []            # Additional safety rules

# ── Cost Management ──────────────────────────────────────
[cost]
# max_monthly_cost = 5.0       # Maximum monthly spending (USD)
# max_daily_cost = 2.0         # Maximum daily spending (USD)
# budget_alert_threshold = 0.8 # Alert when reaching 80% of budget
# cost_transparency = true     # Show cost breakdown after each response
"##;

// ──────────────────────────── Sections ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub version: String,
    pub debug_mode: bool,
    pub verbose: bool,
    pub lang: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            version: "2.1".into(),
            debug_mode: false,
            verbose: false,
            lang: "en".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplConfig {
    pub history_file: String,
    pub max_history_entries: usize,
    pub multiline_enabled: bool,
    pub stream_output: bool,
    pub syntax_highlight: bool,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            history_file: "~/.ox/history".into(),
            max_history_entries: 10000,
            multiline_enabled: true,
            stream_output: true,
            syntax_highlight: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub split_view: bool,
    pub output_ratio: u16,
    pub urgent_prefix: String,
    pub input_during_agent: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            split_view: true,
            output_ratio: 85,
            urgent_prefix: "!".into(),
            input_during_agent: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    pub auto_restore: bool,
    pub max_archived_sessions: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            auto_restore: true,
            max_archived_sessions: 50,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    pub max_history_turns: usize,
    pub memory_budget_tokens: usize,
    pub history_budget_tokens: usize,
    pub reply_reserve_tokens: usize,
    /// Token budget ratio for history (triggers compression when exceeded).
    /// Default: 0.10 (10% of context window).
    pub history_ratio: f32,
    /// Token budget ratio for memory context.
    /// Default: 0.02 (2% of context window).
    pub memory_ratio: f32,
    /// Token budget ratio for system prompt.
    /// Default: 0.02 (2% of context window).
    pub system_prompt_ratio: f32,
    /// Use refined context format (User: ... Assistant: ... [tools])
    /// Removes <think> tags and intermediate steps
    pub use_refined_context: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_history_turns: 20,
            memory_budget_tokens: 2000,
            history_budget_tokens: 50000,
            reply_reserve_tokens: 73000,
            history_ratio: 0.10,
            memory_ratio: 0.02,
            system_prompt_ratio: 0.02,
            use_refined_context: true, // 🆕 Default: ENABLED to reduce hallucinations and improve instruction following
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub auto_confirm_safe: bool,
    pub confirm_writes: bool,
    pub confirm_shell: bool,
    pub shell_timeout_ms: u64,
    pub max_output_chars: usize,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            auto_confirm_safe: true,
            confirm_writes: true,
            confirm_shell: true,
            shell_timeout_ms: 30000,
            max_output_chars: 10000,
        }
    }
}

/// Per-provider LLM configuration (api_key, base_url, max_tokens).
/// All fields are optional — empty/None means use provider defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// API key for this provider. Empty = not set (use env var).
    pub api_key: String,
    /// Base URL override. Empty = use provider's built-in default.
    pub base_url: String,
    /// Max tokens for response. None = use provider's built-in default.
    pub max_tokens: Option<u32>,
    /// Disable tools/function calling for this provider.
    /// Set true for providers like MiniMax that don't support tools.
    pub disable_tools: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    pub default: String,
    pub backup: Vec<String>,
    pub adaptive_thinking: bool,
    pub effort_level: String,
    /// Per-provider configuration, keyed by provider name ("openai", "anthropic", "deepseek").
    pub providers: HashMap<String, ProviderConfig>,
    /// Default provider that overrides prefix inference. Set via `[models] default_provider = "openai"`.
    /// When set, all models route to this provider unless overridden by `model_providers`.
    #[serde(default)]
    pub default_provider: String,
    /// Explicit model→provider mapping. Key = model name, Value = provider name.
    /// Takes priority over `default_provider` and prefix inference.
    #[serde(default)]
    pub model_providers: HashMap<String, String>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            default: "gpt-4o".into(),
            backup: vec!["claude-sonnet-4".into(), "gpt-4-turbo".into()],
            adaptive_thinking: true,
            effort_level: "high".into(),
            providers: HashMap::new(),
            default_provider: String::new(),
            model_providers: HashMap::new(),
        }
    }
}

impl ModelsConfig {
    /// Migrate legacy flat API key fields to the new providers structure.
    pub fn migrate_legacy(&mut self, raw: &toml::Value) {
        let mappings = [
            ("openai_api_key", "openai"),
            ("anthropic_api_key", "anthropic"),
            ("deepseek_api_key", "deepseek"),
        ];
        for (old_key, provider_name) in mappings {
            if let Some(key) = raw
                .get("models")
                .and_then(|m| m.get(old_key))
                .and_then(|v| v.as_str())
                && !key.is_empty()
            {
                let entry = self.providers.entry(provider_name.to_string()).or_default();
                if entry.api_key.is_empty() {
                    entry.api_key = key.to_string();
                    tracing::warn!(
                        "Legacy config: models.{old_key} found. \
                         Please move to [models.providers.{provider_name}] api_key"
                    );
                }
            }
        }
    }
}

// ──────────────────── Memory ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub max_nodes: usize,
    pub alpha: f64,
    pub time_decay: f64,
    pub isolation_application: bool,
    pub share_session_group: bool,
    pub share_request: bool,
    pub export_format: String,
    pub janitor_run_on_startup_prob: f64,
    pub project_decay: ProjectDecayConfig,
    pub overall_decay: OverallDecayConfig,
    pub language_config: HashMap<String, LanguageDecayConfig>,
    pub transform: MemoryTransformConfig,
    // 🆕 LLM Judge re-ranking configuration
    pub enable_llm_judge: bool,
    pub llm_judge_threshold: u8,
    /// Store refined summaries instead of raw conversations
    pub store_refined_memories: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        let mut lang_config = HashMap::new();
        lang_config.insert(
            "rust".into(),
            LanguageDecayConfig {
                lambda: 0.02,
                max_retention_days: 30,
                traces: vec![0.1, 0.2, 0.3, 0.4, 0.5],
            },
        );
        lang_config.insert(
            "python".into(),
            LanguageDecayConfig {
                lambda: 0.01,
                max_retention_days: 90,
                traces: vec![0.05, 0.15, 0.25, 0.35, 0.5],
            },
        );

        Self {
            max_nodes: 1000,
            alpha: 0.8,
            time_decay: 0.01,
            isolation_application: true,
            share_session_group: true,
            share_request: true,
            export_format: "json".into(),
            janitor_run_on_startup_prob: 0.2,
            project_decay: ProjectDecayConfig::default(),
            overall_decay: OverallDecayConfig::default(),
            language_config: lang_config,
            transform: MemoryTransformConfig::default(),
            // 🆕 LLM Judge defaults (enabled by default)
            enable_llm_judge: true,  // Default: ENABLED
            llm_judge_threshold: 7,   // Only keep memories with score >= 7
            store_refined_memories: true, // Default: ENABLED for better memory quality
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectDecayConfig {
    pub base_half_life: u32,
    pub critical_threshold: f64,
}

impl Default for ProjectDecayConfig {
    fn default() -> Self {
        Self {
            base_half_life: 30,
            critical_threshold: 0.3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverallDecayConfig {
    pub beta: f64,
}

impl Default for OverallDecayConfig {
    fn default() -> Self {
        Self { beta: 0.015 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageDecayConfig {
    pub lambda: f64,
    pub max_retention_days: u32,
    pub traces: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryTransformConfig {
    pub interval_days: u32,
    pub batch_size: u32,
    pub daily_token_cap: u32,
    pub trigger: String,
}

impl Default for MemoryTransformConfig {
    fn default() -> Self {
        Self {
            interval_days: 7,
            batch_size: 20,
            daily_token_cap: 10000,
            trigger: "manual".into(),
        }
    }
}

// ──────────────────── Behavior / Safety / Cost ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorRulesConfig {
    pub enforce_safe_code: bool,
    pub enforce_lint: bool,
    pub enforce_format: bool,
    pub enforce_tests: bool,
    pub enforce_all: bool,

    /// User-defined mandatory coding rules (replaces language-specific rules).
    /// These rules are injected into system prompt and MUST be followed.
    /// Custom rules override built-in behavior rules but NOT basic safety rules.
    pub custom_rules: Vec<String>,
}

impl Default for BehaviorRulesConfig {
    fn default() -> Self {
        Self {
            enforce_safe_code: true,
            enforce_lint: true,
            enforce_format: true,
            enforce_tests: true,
            enforce_all: true,
            custom_rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    pub enable_sandbox: bool,
    pub confirm_dangerous_ops: bool,
    pub high_risk_apis: Vec<String>,
    pub custom_rules: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            enable_sandbox: false,
            confirm_dangerous_ops: true,
            high_risk_apis: vec![
                "Command::new".into(),
                "remove_dir_all".into(),
                "fs::remove_dir_all".into(),
                "os.remove".into(),
                "os.rmdir".into(),
            ],
            custom_rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CostConfig {
    pub max_monthly_cost: f64,
    pub max_daily_cost: f64,
    pub budget_alert_threshold: f64,
    pub cost_transparency: bool,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            max_monthly_cost: 5.0,
            max_daily_cost: 2.0,
            budget_alert_threshold: 0.8,
            cost_transparency: true,
        }
    }
}

// ──────────────────── Agent ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Maximum agent loop iterations per turn (safety limit).
    pub max_iterations: u32,
    /// Maximum total tokens per turn before requesting user confirmation.
    pub max_per_turn_tokens: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 25,
            max_per_turn_tokens: 500_000,
        }
    }
}

// ──────────────────── Spec ────────────────────

/// Specification mode configuration for structured task workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SpecConfig {
    /// Whether to auto-load spec.md on startup.
    pub auto_load: bool,
    /// Path to the spec file (relative to project root).
    pub file_path: String,
    /// Whether spec mode is currently active.
    pub active: bool,
    /// Content of the loaded spec (in-memory cache).
    pub content: String,
}

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            auto_load: false,
            file_path: ".ox/spec.md".to_string(),
            active: false,
            content: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_path_suffix() {
        let path = OxConfig::default_config_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.ends_with(".ox/config.toml") || path_str.ends_with(".ox\\config.toml"),
            "Expected path ending with .ox/config.toml, got: {path_str}"
        );
    }

    #[test]
    fn test_provider_config_defaults() {
        let cfg = ProviderConfig::default();
        assert!(cfg.api_key.is_empty());
        assert!(cfg.base_url.is_empty());
        assert!(cfg.max_tokens.is_none());
    }

    #[test]
    fn test_toml_with_providers() {
        let toml_str = r#"
[models]
default = "gpt-4o"

[models.providers.openai]
api_key = "sk-test-123"
base_url = "https://custom.openai.com/v1"
max_tokens = 4096

[models.providers.anthropic]
api_key = "sk-ant-test"
"#;
        let config: OxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.models.default, "gpt-4o");

        let openai = config.models.providers.get("openai").unwrap();
        assert_eq!(openai.api_key, "sk-test-123");
        assert_eq!(openai.base_url, "https://custom.openai.com/v1");
        assert_eq!(openai.max_tokens, Some(4096));

        let anthropic = config.models.providers.get("anthropic").unwrap();
        assert_eq!(anthropic.api_key, "sk-ant-test");
        assert!(anthropic.base_url.is_empty());
        assert!(anthropic.max_tokens.is_none());
    }

    #[test]
    fn test_toml_minimal_parse() {
        let toml_str = r#"
[models]
default = "claude-sonnet-4"
"#;
        let config: OxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.models.default, "claude-sonnet-4");
        assert!(config.models.providers.is_empty());
        assert!(config.models.adaptive_thinking);
    }

    #[test]
    fn test_legacy_migration() {
        let toml_str = r#"
[models]
default = "gpt-4o"
openai_api_key = "sk-old-key"
anthropic_api_key = "sk-ant-old"
"#;
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let mut config: OxConfig = toml::from_str(toml_str).unwrap();
        config.models.migrate_legacy(&raw);

        let openai = config.models.providers.get("openai").unwrap();
        assert_eq!(openai.api_key, "sk-old-key");

        let anthropic = config.models.providers.get("anthropic").unwrap();
        assert_eq!(anthropic.api_key, "sk-ant-old");
    }

    #[test]
    fn test_legacy_does_not_overwrite_new() {
        let toml_str = r#"
[models]
default = "gpt-4o"
openai_api_key = "sk-old"

[models.providers.openai]
api_key = "sk-new"
"#;
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let mut config: OxConfig = toml::from_str(toml_str).unwrap();
        config.models.migrate_legacy(&raw);

        let openai = config.models.providers.get("openai").unwrap();
        assert_eq!(
            openai.api_key, "sk-new",
            "New config should not be overwritten by legacy"
        );
    }

    #[test]
    fn test_init_default_config() {
        let dir = std::env::temp_dir().join("ox_test_init");
        let path = dir.join("config.toml");

        // Clean up from any prior run.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);

        // Manually test the template is valid TOML that parses into OxConfig.
        let config: OxConfig = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        assert_eq!(config.models.default, "gpt-4o");
        assert!(config.models.providers.contains_key("openai"));
        assert!(config.models.providers.contains_key("anthropic"));
        assert!(config.models.providers.contains_key("deepseek"));
    }

    #[test]
    fn test_config_template_round_trip() {
        // Ensure the template can be serialized back without losing structure.
        let config: OxConfig = toml::from_str(DEFAULT_CONFIG_TEMPLATE).unwrap();
        let re_serialized = toml::to_string(&config).unwrap();
        assert!(re_serialized.contains("gpt-4o"));
    }
}
