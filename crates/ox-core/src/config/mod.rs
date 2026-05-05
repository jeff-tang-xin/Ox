use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    pub council: CouncilConfig,
    pub memory: MemoryConfig,
    pub persona: PersonaConfig,
    pub behavior_rules: BehaviorRulesConfig,
    pub safety: SafetyConfig,
    pub cost: CostConfig,
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
const DEFAULT_CONFIG_TEMPLATE: &str = r##"# Ox CLI Configuration
# Location: ~/.ox/config.toml

[general]
# version = "2.1"
# debug_mode = false
# verbose = false
# lang = "en"

[models]
default = "gpt-4o"
# backup = ["claude-sonnet-4", "gpt-4-turbo"]
# adaptive_thinking = true
# effort_level = "high"

# ── Provider configuration ──
# Each provider has its own section under [models.providers.<name>].
# api_key can also be set via environment variable: OX_OPENAI_API_KEY, OX_ANTHROPIC_API_KEY, etc.
# Environment variables take priority over config file values.

[models.providers.openai]
api_key = ""
# base_url = "https://api.openai.com/v1"
# max_tokens = 4096
# stream_usage = true  # Enable for usage tracking (official OpenAI only)

[models.providers.anthropic]
api_key = ""
# base_url = "https://api.anthropic.com/v1"
# max_tokens = 8192

[models.providers.deepseek]
api_key = ""
# base_url = "https://api.deepseek.com/v1"
# max_tokens = 4096

# ── Explicit model→provider mapping ──
# Fallback provider when model name doesn't match any known prefix.
# Example: default_provider = "openai"  (for OpenAI-compatible APIs like DashScope)
# default_provider = "openai"

# Explicit model→provider mapping (overrides prefix inference and default_provider).
# [models.model_providers]
# "deepseek-v4-pro" = "openai"

# ── Embedding Compression (KadaneDial) ──
# Uses BGE embedding model for semantic context compression.
# Triggers automatically when history tokens exceed history budget.
# The history budget is calculated as: context_window * history_ratio (default 10%).
# This ratio is controlled by [context] history_ratio below.
#
# To download a BGE model, use the /download-model command in Ox REPL:
#   /download-model                    # Downloads bge-small-zh-v1.5 (default)
#   /download-model bge-base-zh-v1.5   # Downloads base model
#   /download-model bge-large-zh-v1.5  # Downloads large model
#
# Available models from ModelScope:
#   - bge-small-zh-v1.5  (~130MB, fast, good for most cases)
#   - bge-base-zh-v1.5   (~420MB, balanced performance)
#   - bge-large-zh-v1.5  (~1.2GB, best quality, slower)

[models.embedding]
enabled = false
# model_path = "~/.ox/models/bge-small-zh-v1.5"  # Path to downloaded BGE model
# threshold = 0.0   # Z-score threshold for relevance filtering (higher = stricter)
# stop_threshold = 0.5  # Stop selecting segments when gain drops below this
# max_segments = 5  # Maximum number of conversation segments to keep
# min_segment_len = 2  # Minimum messages per segment
# keep_recent = 4  # Always keep the N most recent messages uncompressed
# chunk_threshold_tokens = 256  # Split messages longer than this into chunks
# max_chunk_tokens = 512  # Maximum tokens per chunk when splitting long messages

[repl]
# history_file = "~/.ox/history"
# max_history_entries = 10000
# multiline_enabled = true
# stream_output = true
# syntax_highlight = true

[terminal]
# split_view = true
# output_ratio = 85
# urgent_prefix = "!"
# input_during_agent = true

[session]
# auto_restore = true
# max_archived_sessions = 50

[context]
# max_history_turns = 20
# memory_budget_tokens = 2000
# history_budget_tokens = 50000
# reply_reserve_tokens = 73000
# history_ratio = 0.10  # 10% of context window for history (triggers compression when exceeded)
# memory_ratio = 0.02    # 2% for memory context
# system_prompt_ratio = 0.02  # 2% for system prompt

[tools]
# auto_confirm_safe = true
# confirm_writes = true
# confirm_shell = true
# shell_timeout_ms = 30000          # Shell命令超时时间（毫秒）
# max_output_chars = 10000         # Shell输出最大字符数（默认10000，约200-300行）
                                    # 推荐范围: 5000(快速) ~ 20000(详细) ~ 50000(最大)

[agent]
# max_iterations = 25
# max_per_turn_tokens = 500000

[council]
# default_rounds = 2
# max_rounds = 3
# max_participants = 4
# participants = ["gpt-4o", "claude-sonnet-4-20250514", "deepseek-coder"]
# arbiter_model = "default"
# early_convergence_threshold = 0.8
# verbose_by_default = false
# budget_warning = true
# council_memory_decay_factor = 0.7

[memory]
# max_nodes = 1000
# alpha = 0.8
# time_decay = 0.01
# isolation_application = true
# share_session_group = true
# share_request = true
# export_format = "json"
# janitor_run_on_startup_prob = 0.2

[memory.project_decay]
# base_half_life = 30
# critical_threshold = 0.3

[memory.overall_decay]
# beta = 0.015

# Language-specific decay configurations
# [memory.language_config.rust]
# lambda = 0.02
# max_retention_days = 30
# traces = [0.1, 0.2, 0.3, 0.4, 0.5]

# [memory.language_config.python]
# lambda = 0.01
# max_retention_days = 90
# traces = [0.05, 0.15, 0.25, 0.35, 0.5]

[memory.transform]
# interval_days = 7
# batch_size = 20
# daily_token_cap = 10000
# trigger = "manual"  # "manual" (需 /memory transform) | "auto" (定期自动)

[persona]
# auto_evolve = true
# max_trait_change = 0.1
# frozen = false
# export_format = "json"

[behavior_rules]
# enforce_safe_code = true
# enforce_lint = true
# enforce_format = true
# enforce_tests = true
# enforce_all = true

[safety]
# enable_sandbox = false
# confirm_dangerous_ops = true
# high_risk_apis = ["Command::new", "remove_dir_all", "fs::remove_dir_all", "os.remove", "os.rmdir"]
# custom_rules = []

[cost]
# max_monthly_cost = 5.0
# max_daily_cost = 2.0
# budget_alert_threshold = 0.8
# cost_transparency = true
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
    /// Whether to send stream_options for usage tracking.
    /// Default false. Set true only for official OpenAI API if you need usage stats.
    pub stream_usage: Option<bool>,
    /// Disable tools/function calling for this provider.
    /// Set true for providers like MiniMax that don't support tools.
    pub disable_tools: Option<bool>,
}

/// Configuration for embedding-based context compression (KadaneDial).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Enable embedding-based compression.
    pub enabled: bool,
    /// Path to BGE model directory (ModelScope format with safetensors).
    pub model_path: Option<String>,
    /// Z-score threshold for relevance filtering (higher = stricter).
    pub threshold: f32,
    /// Stop when cumulative gain drops below this threshold.
    pub stop_threshold: f32,
    /// Maximum number of segments to select.
    pub max_segments: usize,
    /// Minimum length of each segment (in message pairs).
    pub min_segment_len: usize,
    /// Always keep this many recent messages.
    pub keep_recent: usize,
    /// Token threshold for chunking: messages shorter than this are kept as single chunk.
    pub chunk_threshold_tokens: usize,
    /// Maximum tokens per chunk when splitting long messages.
    pub max_chunk_tokens: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_path: None,
            threshold: 0.0,
            stop_threshold: 0.5,
            max_segments: 5,
            min_segment_len: 2,
            keep_recent: 4,
            chunk_threshold_tokens: 256,
            max_chunk_tokens: 512,
        }
    }
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
    /// Fallback provider when prefix inference fails. Example: "openai".
    /// Set via `[models] default_provider = "openai"`.
    #[serde(default)]
    pub default_provider: String,
    /// Explicit model→provider mapping. Key = model name, Value = provider name.
    /// Takes priority over `resolve_provider_name()` prefix inference and `default_provider`.
    #[serde(default)]
    pub model_providers: HashMap<String, String>,
    /// Embedding-based compression configuration (KadaneDial).
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
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
            embedding: None,
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
                let entry = self
                    .providers
                    .entry(provider_name.to_string())
                    .or_default();
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CouncilConfig {
    pub default_rounds: u32,
    pub max_rounds: u32,
    pub max_participants: u32,
    pub participants: Vec<String>,
    pub arbiter_model: String,
    pub early_convergence_threshold: f64,
    pub verbose_by_default: bool,
    pub budget_warning: bool,
    pub council_memory_decay_factor: f64,
}

impl Default for CouncilConfig {
    fn default() -> Self {
        Self {
            default_rounds: 2,
            max_rounds: 3,
            max_participants: 4,
            participants: vec![
                "gpt-4o".into(),
                "claude-sonnet-4-20250514".into(),
                "deepseek-coder".into(),
            ],
            arbiter_model: "default".into(),
            early_convergence_threshold: 0.8,
            verbose_by_default: false,
            budget_warning: true,
            council_memory_decay_factor: 0.7,
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

// ──────────────────── Persona / Behavior / Safety / Cost ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonaConfig {
    pub auto_evolve: bool,
    pub max_trait_change: f64,
    pub frozen: bool,
    pub export_format: String,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            auto_evolve: true,
            max_trait_change: 0.1,
            frozen: false,
            export_format: "json".into(),
        }
    }
}

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
        assert_eq!(openai.api_key, "sk-new", "New config should not be overwritten by legacy");
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
