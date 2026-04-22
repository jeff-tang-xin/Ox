pub mod anthropic;
pub mod openai;
pub mod tokenizer;

use crate::message::{Message, TokenUsage};

/// Events emitted during LLM streaming.
#[derive(Debug, Clone)]
pub enum LlmStreamEvent {
    /// A chunk of text from the assistant.
    TextDelta(String),
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
        _ if m.starts_with("claude-3") || m.starts_with("claude-opus") || m.starts_with("claude-sonnet") => 200_000,
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

/// Create the appropriate LLM provider based on model name and config.
///
/// API key priority: env var `OX_{PROVIDER}_API_KEY` > config `[models.providers.{provider}] api_key`.
/// base_url: config > provider default.
/// max_tokens: config > provider default (8192 for Anthropic, None/omit for OpenAI).
pub fn create_provider(
    model: &str,
    config: &crate::config::ModelsConfig,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    let provider_name = resolve_provider_name(model);
    let provider_cfg = config
        .providers
        .get(provider_name)
        .cloned()
        .unwrap_or_default();

    // Resolve API key: env var overrides config.
    let env_key = format!("OX_{}_API_KEY", provider_name.to_uppercase());
    let api_key = std::env::var(&env_key)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if provider_cfg.api_key.is_empty() {
                None
            } else {
                Some(provider_cfg.api_key.clone())
            }
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{provider_name} API key not set. Set {env_key} env var or \
                 [models.providers.{provider_name}] api_key in ~/.ox/config.toml"
            )
        })?;

    // Resolve base_url: config > provider default.
    let base_url = if provider_cfg.base_url.is_empty() {
        default_base_url(provider_name).to_string()
    } else {
        provider_cfg.base_url.clone()
    };

    match provider_name {
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider::new(
            model.to_string(),
            api_key,
            base_url,
            provider_cfg.max_tokens.unwrap_or(8192),
        ))),
        // OpenAI and DeepSeek (OpenAI-compatible) both use OpenAiProvider.
        _ => Ok(Box::new(openai::OpenAiProvider::new(
            model.to_string(),
            api_key,
            base_url,
            provider_cfg.max_tokens,
        ))),
    }
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
        assert_eq!(default_base_url("anthropic"), "https://api.anthropic.com/v1");
        assert_eq!(default_base_url("deepseek"), "https://api.deepseek.com/v1");
        assert_eq!(default_base_url("unknown"), "https://api.openai.com/v1");
    }
}
