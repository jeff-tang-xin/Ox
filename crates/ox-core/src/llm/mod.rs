pub mod anthropic;
pub mod openai;
pub mod openai_sse;
mod sse;
pub mod tokenizer;
pub mod universal_adapter;

use crate::message::{Message, TokenUsage};

/// Events emitted during LLM streaming.
#[derive(Debug, Clone)]
pub enum LlmStreamEvent {
    /// A chunk of text from the assistant.
    TextDelta(String),
    /// A chunk of reasoning/thinking content (DeepSeek reasoning_content).
    ReasoningDelta(String),
    /// A tool call has started.
    ToolCallStart { id: String, name: String },
    /// A chunk of tool call arguments JSON.
    ToolCallArgumentsDelta { id: String, delta: String },
    /// A tool call is complete.
    ToolCallEnd { id: String },
    /// Streaming is complete.
    Done { usage: TokenUsage },
    /// An error occurred.
    Error(String),
}

/// Per-request overrides for `stream_chat` (e.g. workflow Plan JSON step needs more headroom).
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamOptions {
    /// When set, overrides the provider config `max_tokens` for this call only.
    pub max_tokens: Option<u32>,
}

/// Trait for LLM providers (OpenAI, Anthropic, etc.).
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Stream a chat completion. Events are sent through `tx`.
    /// The function returns when streaming is complete or an error occurs.
    async fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tx: tokio::sync::mpsc::UnboundedSender<LlmStreamEvent>,
        opts: StreamOptions,
    ) -> anyhow::Result<()>;

    /// The model identifier string.
    fn model_name(&self) -> &str;

    /// Context window size in tokens for this model.
    fn context_window_size(&self) -> u32;
}

/// Schema for a tool that the LLM can call (JSON Schema format).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Lookup context window size by model name prefix.
/// Returns a reasonable default for unknown models.
pub fn context_window_for_model(model: &str) -> u32 {
    let m = model.to_lowercase();
    match () {
        // OpenAI models
        _ if m.starts_with("gpt-4o") => 128_000,
        _ if m.starts_with("gpt-4-turbo") => 128_000,
        _ if m.starts_with("gpt-4") => 8_192,
        _ if m.starts_with("gpt-3.5") => 16_385,
        _ if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") => 200_000,
        // Anthropic models
        _ if m.starts_with("claude-3")
            || m.starts_with("claude-opus")
            || m.starts_with("claude-sonnet") =>
        {
            200_000
        }
        _ if m.starts_with("claude-2") => 100_000,
        _ if m.starts_with("claude") => 200_000,
        // DeepSeek
        _ if m.starts_with("deepseek") => 64_000,
        // Fallback
        _ => 128_000,
    }
}

/// Resolve provider name from model string.
pub fn resolve_provider_name(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.starts_with("claude") {
        "anthropic"
    } else if m.starts_with("deepseek") {
        "deepseek"
    } else {
        "openai"
    }
}

/// Default base URL for each provider.
fn default_base_url(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "https://api.anthropic.com/v1",
        "deepseek" => "https://api.deepseek.com/v1",
        _ => "https://api.openai.com/v1",
    }
}

/// Resolve provider name from model string, using explicit mapping first.
/// Priority: config.model_providers exact match > config.default_provider > prefix inference.
pub fn resolve_provider_name_with_config<'a>(
    model: &str,
    config: &'a crate::config::ModelsConfig,
) -> &'a str {
    // Priority 1: explicit model→provider mapping
    if let Some(provider) = config.model_providers.get(model) {
        return provider.as_str();
    }
    // Priority 2: explicit default_provider overrides prefix inference
    if !config.default_provider.is_empty() {
        return config.default_provider.as_str();
    }
    // Priority 3: prefix inference (claude*/deepseek*/gpt*…)
    resolve_provider_name(model)
}

/// Source of the API key, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeySource {
    /// API key found in environment variable (named).
    EnvVar(String),
    /// API key found in config file.
    ConfigFile,
    /// API key not found.
    NotFound,
}

/// Source of the base URL, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseUrlSource {
    /// Base URL from config file.
    ConfigFile,
    /// Base URL from provider default.
    Default,
}

/// Provider resolution result with diagnostic information.
#[derive(Debug, Clone)]
pub struct ProviderResolveInfo {
    pub provider_name: String,
    pub api_key_source: ApiKeySource,
    pub base_url_source: BaseUrlSource,
}

/// Create the appropriate LLM provider based on model name and config,
/// returning both the provider and resolution diagnostics.
///
/// API key priority: env var `OX_{PROVIDER}_API_KEY` > config `[models.providers.{provider}] api_key`.
/// Provider resolution: `model_providers` explicit mapping > prefix inference.
/// base_url: config > provider default.
/// max_tokens: config > provider default (8192 for Anthropic, None/omit for OpenAI).
pub fn create_provider_with_info(
    model: &str,
    config: &crate::config::ModelsConfig,
) -> anyhow::Result<(Box<dyn LlmProvider>, ProviderResolveInfo)> {
    let provider_name = resolve_provider_name_with_config(model, config).to_string();
    let provider_cfg = config
        .providers
        .get(&provider_name)
        .cloned()
        .unwrap_or_default();

    // Resolve API key: env var overrides config.
    let env_key = format!("OX_{}_API_KEY", provider_name.to_uppercase());
    let (api_key, api_key_source) =
        if let Some(key) = std::env::var(&env_key).ok().filter(|s| !s.is_empty()) {
            (key, ApiKeySource::EnvVar(env_key))
        } else if !provider_cfg.api_key.is_empty() {
            (provider_cfg.api_key.clone(), ApiKeySource::ConfigFile)
        } else {
            return Err(anyhow::anyhow!(
                "{provider_name} API key not set. Set {env_key} env var or \
             [models.providers.{provider_name}] api_key in ~/.ox/config.toml"
            ));
        };

    // Resolve base_url: config > provider default.
    let (base_url, base_url_source) = if provider_cfg.base_url.is_empty() {
        (
            default_base_url(&provider_name).to_string(),
            BaseUrlSource::Default,
        )
    } else {
        (provider_cfg.base_url.clone(), BaseUrlSource::ConfigFile)
    };

    let resolve_info = ProviderResolveInfo {
        provider_name: provider_name.clone(),
        api_key_source,
        base_url_source,
    };
    // Check if tools should be disabled for this provider (e.g. MiniMax).
    let disable_tools = provider_cfg.disable_tools.unwrap_or(false);

    let provider = match provider_name.as_str() {
        "anthropic" => Box::new(anthropic::AnthropicProvider::new(
            model.to_string(),
            api_key,
            base_url,
            provider_cfg.max_tokens.unwrap_or(8192),
        )) as Box<dyn LlmProvider>,
        // OpenAI and DeepSeek (OpenAI-compatible) both use OpenAiProvider.
        _ => Box::new(openai::OpenAiProvider::new(
            model.to_string(),
            api_key,
            base_url,
            provider_cfg.max_tokens,
            disable_tools,
        )),
    };

    Ok((provider, resolve_info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_provider_name_openai() {
        assert_eq!(resolve_provider_name("gpt-4o"), "openai");
        assert_eq!(resolve_provider_name("gpt-3.5-turbo"), "openai");
        assert_eq!(resolve_provider_name("o1-preview"), "openai");
    }

    #[test]
    fn test_resolve_provider_name_anthropic() {
        assert_eq!(resolve_provider_name("claude-sonnet-4"), "anthropic");
        assert_eq!(resolve_provider_name("Claude-3-opus"), "anthropic");
    }

    #[test]
    fn test_resolve_provider_name_deepseek() {
        assert_eq!(resolve_provider_name("deepseek-coder"), "deepseek");
        assert_eq!(resolve_provider_name("DeepSeek-V2"), "deepseek");
    }

    #[test]
    fn test_default_base_url() {
        assert_eq!(default_base_url("openai"), "https://api.openai.com/v1");
        assert_eq!(
            default_base_url("anthropic"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(default_base_url("deepseek"), "https://api.deepseek.com/v1");
        assert_eq!(default_base_url("unknown"), "https://api.openai.com/v1");
    }

    #[test]
    fn test_resolve_provider_name_with_config_explicit_mapping() {
        use crate::config::ModelsConfig;
        let mut config = ModelsConfig::default();
        config
            .model_providers
            .insert("deepseek-v4-pro".to_string(), "openai".to_string());

        // Explicit mapping takes priority.
        assert_eq!(
            resolve_provider_name_with_config("deepseek-v4-pro", &config),
            "openai"
        );
        // Unmapped model falls back to prefix inference.
        assert_eq!(
            resolve_provider_name_with_config("gpt-4o", &config),
            "openai"
        );
        assert_eq!(
            resolve_provider_name_with_config("claude-sonnet-4", &config),
            "anthropic"
        );
    }

    #[test]
    fn test_resolve_provider_name_with_config_empty_mapping() {
        use crate::config::ModelsConfig;
        let config = ModelsConfig::default();
        assert_eq!(
            resolve_provider_name_with_config("deepseek-coder", &config),
            "deepseek"
        );
        assert_eq!(
            resolve_provider_name_with_config("gpt-4o", &config),
            "openai"
        );
    }

    #[test]
    fn test_resolve_provider_name_with_config_default_provider() {
        use crate::config::ModelsConfig;
        let mut config = ModelsConfig::default();
        config.default_provider = "openai".to_string();

        // default_provider overrides prefix inference for ALL models.
        assert_eq!(
            resolve_provider_name_with_config("my-custom-model", &config),
            "openai"
        );
        assert_eq!(
            resolve_provider_name_with_config("gpt-4o", &config),
            "openai"
        );
        assert_eq!(
            resolve_provider_name_with_config("claude-sonnet-4", &config),
            "openai"
        );
        assert_eq!(
            resolve_provider_name_with_config("deepseek-coder", &config),
            "openai"
        );
    }
}
