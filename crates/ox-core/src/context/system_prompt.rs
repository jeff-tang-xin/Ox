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
    persona_vector: Option<&crate::persona::PersonaVector>,
    behavior_rules: Option<&crate::config::BehaviorRulesConfig>,
) -> String {
    let mut parts = Vec::new();

    // 1. Core persona.
    parts.push(persona.unwrap_or(DEFAULT_PERSONA).to_string());

    // 2. Core principles (P1-P4).
    parts.push(CORE_PRINCIPLES.to_string());

    // 3. PersonaVector (Phase 2).
    if let Some(pv) = persona_vector {
        parts.push(pv.generate_prompt_block());
    }

    // 4. Behavior rules.
    if let Some(br) = behavior_rules {
        if br.enforce_all {
            let mut rules = vec!["## Behavior Rules".to_string()];
            if br.enforce_safe_code { rules.push("- Never suggest code that bypasses safety checks".into()); }
            if br.enforce_lint { rules.push("- Always run lint before declaring code complete".into()); }
            if br.enforce_format { rules.push("- Always format code before writing files".into()); }
            if br.enforce_tests { rules.push("- Always write tests for new functions".into()); }
            if rules.len() > 1 { parts.push(rules.join("\n")); }
        }
    }

    // 5. Runtime environment.
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
- **P1: Intention Understanding** — Understand the full context before making changes.
- **P2: Simplicity First** — Minimum code that solves the problem, nothing speculative.
- **P3: Surgical Changes** — Touch only what you must, match existing style.
- **P4: Goal-Driven Execution** — Define success criteria, loop until verified.";
