//! Context building helpers with optional refinement support.

use ox_core::context::{ContextBuilder, build_refined_context};
use ox_core::message::Message;

/// Build context with optional refinement based on configuration.
/// 
/// If `use_refined` is true, uses the refined context format:
/// "User: ... Assistant: ... [tools]"
/// Otherwise uses the standard context format.
pub fn build_context_with_option(
    builder: &ContextBuilder,
    system_prompt: &str,
    memory_ctx: &str,
    messages: &[Message],
    max_tokens: u32,
    use_refined: bool,
) -> Vec<Message> {
    if use_refined {
        tracing::debug!("Using refined context format");
        // Use default max_turns of 10 for refined context
        builder.build_refined(system_prompt, memory_ctx, messages, max_tokens, 10)
    } else {
        tracing::debug!("Using standard context format");
        builder.build(system_prompt, memory_ctx, messages, max_tokens)
    }
}
