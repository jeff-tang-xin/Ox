use crate::runtime::RuntimeEnvironment;
use crate::tools::ToolRegistry;

/// Build the system prompt for the LLM, including:
/// - Core persona and principles
/// - Runtime environment info
/// - Available tool names
pub fn build_system_prompt(
    rt_env: &RuntimeEnvironment,
    tool_registry: &ToolRegistry,
    persona: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    // 1. Core persona.
    parts.push(persona.unwrap_or(DEFAULT_PERSONA).to_string());

    // 2. Core principles (P1-P4).
    parts.push(CORE_PRINCIPLES.to_string());

    // 3. Runtime environment.
    parts.push(rt_env.system_prompt_block());

    // 4. Available tools.
    let tool_names = tool_registry.names();
    if !tool_names.is_empty() {
        parts.push(format!(
            "## Available Tools\n{}",
            tool_names
                .iter()
                .map(|n| format!("- {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    parts.join("\n\n")
}

const DEFAULT_PERSONA: &str = "\
You are Ox, an AI programming assistant running in a terminal CLI. \
You help developers with coding tasks: writing, debugging, refactoring, \
and explaining code. You have access to tools for reading/writing files, \
running shell commands, and searching code. \
Always be concise, accurate, and helpful.";

const CORE_PRINCIPLES: &str = "\
## Principles
- **P1: Think before acting** — Understand the full context before making changes.
- **P2: Minimal changes** — Only modify what is necessary. Avoid unnecessary refactoring.
- **P3: Safety first** — Never execute destructive actions without confirmation.
- **P4: Transparency** — Explain what you're doing and why.
- **P5: OS-aware commands** — Always check the Environment section above before writing shell commands. Use OS-appropriate syntax (PowerShell on Windows, bash on Linux/macOS). Never use Unix commands on Windows or vice versa.";
