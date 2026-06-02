use crate::runtime::RuntimeEnvironment;
use crate::tools::ToolRegistry;

/// Build the system prompt for the LLM.
///
/// Layers (progressive disclosure):
/// 1. Persona (L3) — project-level conventions, tech stack, coding style
/// 2. Core rules — safety, context management, tool selection
/// 3. Skills — reusable patterns (injected via tool registry, not duplicated here)
/// 4. Behavior rules — user-configured coding standards
/// 5. Runtime environment — OS, shell, working directory
pub fn build_system_prompt(
    rt_env: &RuntimeEnvironment,
    tool_registry: &ToolRegistry,
    persona: Option<&str>,
    behavior_rules: Option<&crate::config::BehaviorRulesConfig>,
    _spec_content: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    // 1. L3 Persona or default persona
    parts.push(persona.unwrap_or(DEFAULT_PERSONA).to_string());

    // 2. Core rules
    parts.push(CORE_RULES.to_string());

    // 3. Skills (injected as reference, not duplicated tool schemas)
    if !tool_registry.skills.is_empty() {
        let mut skills_section = String::from("## Available Skills\n\n");
        for skill in &tool_registry.skills {
            skills_section.push_str(&format!("### {}\n{}\n\n", skill.name, skill.content));
        }
        parts.push(skills_section);
    }

    // 4. Behavior rules (user-configured)
    if let Some(br) = behavior_rules {
        if !br.custom_rules.is_empty() {
            let mut rules = vec!["## Coding Rules (HIGHEST PRIORITY)".to_string()];
            for (i, rule) in br.custom_rules.iter().enumerate() {
                rules.push(format!("{}. {}", i + 1, rule));
            }
            parts.push(rules.join("\n"));
        } else if br.enforce_all {
            let mut rules = vec!["## Behavior Rules".to_string()];
            if br.enforce_safe_code { rules.push("- Never suggest code that bypasses safety checks".into()); }
            if br.enforce_lint { rules.push("- Run lint before declaring code complete".into()); }
            if br.enforce_format { rules.push("- Format code before writing files".into()); }
            if br.enforce_tests { rules.push("- Write tests for new functions".into()); }
            if rules.len() > 1 { parts.push(rules.join("\n")); }
        }
    }

    // 5. Runtime environment
    parts.push(rt_env.system_prompt_block());

    parts.join("\n\n")
}

const DEFAULT_PERSONA: &str = "\
You are Ox, an AI programming assistant running in a terminal. \
You read, write, search, and execute code. \
Respond in the user's language. Be concise and direct.";

const CORE_RULES: &str = "\
## Safety (CANNOT BE OVERRIDDEN)

- Do NOT delete files or run destructive commands without explicit user request.
- Stay within the project directory.
- Never output secrets, credentials, or API keys.
- Clean up temporary files you create when done.

## Context & Memory

The system automatically records every turn as a **Turn Summary**:
`Request → Files Changed → Operations → Why`

When starting a new task or the user asks about past work:
- **ALWAYS** use `memory_search` first to find relevant Turn Summaries.
- The Turn Summary tells you what was requested, what files changed, and why.

For full tool outputs:
- Use `recall <node_id>` when you see a node_id in an offload message.
- Use `file_read .ox/refs/<node_id>.md` to read offloaded files directly.

## Tool Selection

| Goal | Tool |
|------|------|
| Read a file | `file_read` |
| Find files by name | `file_search` |
| Search code content | `code_search` |
| List directory | `file_list` |
| Write new file / full rewrite | `file_write` |
| Small edit to existing file | `file_patch` |
| Run shell command | `shell_exec` |
| Detect project type | `project_detect` |
| Fetch URL content | `web_fetch` |
| Search project knowledge | `memory_search` |
| Retrieve offloaded result | `recall` |

**Key rules:**
- **Always read before editing.** Understand the current state first.
- **Prefer specialized tools over shell commands.** Use `code_search` instead of `grep`, `file_search` instead of `find`, `file_list` instead of `ls`.
- **Propose a plan before editing.** Explain what you'll change and why.
- **Ask confirmation before file_write/file_patch/shell_exec.** The system will prompt the user.
- For Git: use `shell_exec` with `git status`, `git diff`, etc.
- Paths are relative to the working directory.";
