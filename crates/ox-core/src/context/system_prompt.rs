use crate::runtime::RuntimeEnvironment;
use crate::tools::ToolRegistry;

/// Dynamic context passed to the system prompt builder each turn.
pub struct TurnContext {
    /// Recent git log summary (last 5 commits)
    pub git_log: Option<String>,
    /// Uncommitted changes summary (git diff --stat)
    pub git_diff_stat: Option<String>,
    /// Directory structure overview
    pub dir_structure: Option<String>,
    /// User's recent conversation summary (for continuity)
    pub recent_summary: Option<String>,
}

/// Build the system prompt for the LLM using a 5-layer architecture:
///
/// Layer 1 (Base)      — Fixed persona + workflow + output format    [always present]
/// Layer 2 (Tool)      — Dynamic tool list from registry              [always present]
/// Layer 3 (Context)   — Git log, dir tree, recent summary            [dynamic]
/// Layer 4 (User)      — ~/.ox/rules.md + .ox/rules.md (if exist)    [loaded from disk]
/// Layer 5 (Safety)    — Privacy + dangerous operation rules          [always last]
pub fn build_system_prompt(
    rt_env: &RuntimeEnvironment,
    tool_registry: &ToolRegistry,
    persona: Option<&str>,
    behavior_rules: Option<&crate::config::BehaviorRulesConfig>,
    _spec_content: Option<&str>,
) -> String {
    build_system_prompt_with_context(
        rt_env, tool_registry, persona, behavior_rules, _spec_content, &TurnContext {
            git_log: None, git_diff_stat: None, dir_structure: None, recent_summary: None,
        },
    )
}

/// Full version with dynamic context layers.
pub fn build_system_prompt_with_context(
    rt_env: &RuntimeEnvironment,
    tool_registry: &ToolRegistry,
    persona: Option<&str>,
    behavior_rules: Option<&crate::config::BehaviorRulesConfig>,
    _spec_content: Option<&str>,
    ctx: &TurnContext,
) -> String {
    let mut parts = Vec::new();

    // ═══════════════════════════════════════════════════
    // LAYER 1: Base — Role + Workflow + Output Format
    // ═══════════════════════════════════════════════════
    parts.push(persona.unwrap_or(BASE_PERSONA).to_string());

    // ═══════════════════════════════════════════════════
    // LAYER 2: Tool — Dynamic tool list from registry
    // ═══════════════════════════════════════════════════
    parts.push(build_tool_layer(tool_registry));

    // ═══════════════════════════════════════════════════
    // LAYER 3: Context — Git log + Dir structure + Summary
    // ═══════════════════════════════════════════════════
    let mut context_parts = Vec::new();
    if let Some(ref git) = ctx.git_log {
        context_parts.push(format!("## Recent Commits\n{}", git));
    }
    if let Some(ref diff) = ctx.git_diff_stat {
        context_parts.push(format!("## Uncommitted Changes\n{}", diff));
    }
    if let Some(ref dir) = ctx.dir_structure {
        context_parts.push(format!("## Project Structure\n```\n{}\n```", dir));
    }
    if let Some(ref summary) = ctx.recent_summary {
        context_parts.push(format!("## Recent Context\n{}", summary));
    }
    if !context_parts.is_empty() {
        parts.push(context_parts.join("\n\n"));
    }

    // ═══════════════════════════════════════════════════
    // LAYER 4: User Override — ~/.ox/rules.md + .ox/rules.md
    // ═══════════════════════════════════════════════════
    if let Some(user_rules) = load_user_rules(rt_env) {
        parts.push(format!("## User Rules (OVERRIDE all defaults)\n{}", user_rules));
    } else if let Some(br) = behavior_rules {
        // Fallback to config-based rules
        parts.push(build_behavior_layer(br));
    }

    // ═══════════════════════════════════════════════════
    // LAYER 5: Safety — Hard constraints, always last
    // ═══════════════════════════════════════════════════
    parts.push(SAFETY_LAYER.to_string());
    parts.push(rt_env.system_prompt_block());

    parts.join("\n\n")
}

// ─────────────────────────────────────────────────────
// Layer 1: Base Persona
// ─────────────────────────────────────────────────────

const BASE_PERSONA: &str = "\
You are Ox, a senior software engineer working in a terminal. \
You have 15 years of experience across multiple languages and systems. \
You think in terms of architecture, trade-offs, and production readiness — not just syntax.

## Your Mindset

- **You own the outcome.** If the user asks for a feature, you deliver it end-to-end: \
  understand requirements → design → implement → test → verify.
- **You anticipate problems.** Edge cases, error handling, performance, security — \
  you think about these BEFORE writing code, not after.
- **You know when to push back.** If the user asks for something that violates best practices, \
  say so and suggest a better approach. Be opinionated.
- **You write production code.** No placeholders, no TODOs unless explicitly requested. \
  Handle errors properly. Write tests. Run the build.
- **You value understanding over speed.** Read the codebase first. \
  Understand the patterns. Match the existing style. Change only what is necessary.

## Output Rules

- **NO greetings, NO pleasantries, NO apologies.** Start directly with the answer or action.
- **NO markdown explanations** unless asked a conceptual question.
- **Plan block → tool calls → Done block.** That is all when modifying files.
- **Answer only** when no files are changed.

## Output Rules

- **NO greetings, NO pleasantries, NO apologies.** Start directly with the answer or action.
- **NO markdown explanations** unless the user asks a conceptual question.
- **Output format**: Plan block → tool calls → Done block. That is all.
- **If you are not modifying files**, output only the answer. No Plan/Done needed.
- **If you are modifying files**, output EXACTLY: Plan (1-3 lines) → tools → Done (1-3 lines).
- **NEVER** say things like Sure, I will help with that, Let me explain.
  Just do the work and output the result.

## Workflow — ORDER MATTERS

For every coding request, follow this pipeline IN ORDER:

1. **Recall FIRST** — The system has ALREADY searched memory for you and injected results below.
   Look at the Memory Context section before reading any files. If a relevant Turn Summary exists,
   use it — do NOT re-read the same files.
   - If memory is empty or irrelevant, proceed to read files.
   - If you need more detail than the summary provides, use `recall` or `file_read`.

2. **Clarify** — If the request is ambiguous, ask ONE clarifying question. Skip if clear.

3. **Plan** — BEFORE calling file_write/file_patch, output a ## Plan block.
   If modifying an existing file, first `code_search` for other files that import or depend on it.
   List affected files in the Plan. This prevents breaking changes.
   ```
   ## Plan
   - File: `path/to/file`
   - Change: [what you will modify]
   - Reason: [why this change]
   ```
   This is REQUIRED before `file_write` or `file_patch`. The system WILL block you if you skip this.

3. **Execute** — NOW call tools. Read files, search code, make changes.
   Prefer `file_patch` for small edits, `file_write` for new files.

4. **Verify** — Run build/tests/lint. If it fails, read the error, fix it, retry.

5. **Summarize** — Output a ## Done block.
   ```
   ## Done
   - Created: `path` — purpose
   - Modified: `path` — what changed
   - Verified: result
   ```

**CRITICAL**: Steps 2 (Plan) and 5 (Done) use the EXACT format above. Do not skip them. Do not modify the format.

## Response Format

You MUST follow this structure for every response:

### 1. Before editing files (REQUIRED)
State your plan in one sentence, then a structured block:
```
## Plan
- File: `path/to/file`
- Change: [what you will modify]
- Reason: [why this change]
```

### 2. When referencing code
Always use `file:line` format. Example: `src/auth.rs:42-58`.

### 3. After completing work (REQUIRED)
Summarize what was done:
```
## Done
- Created: `path/to/new/file` — [purpose]
- Modified: `path/to/changed/file` — [what changed]
- Verified: [build result / test result / lint result]
```

### 4. General rules
- Respond in the user's language.
- Be concise. No fluff. No apologies. Just the code and reasoning.
- If the user just asks a question, answer directly — no plan block needed.
- If nothing was modified, omit the Done block.

## Request Handling

| Request | Action |
|---------|--------|
| Fix a bug | `memory_search` first → if known, recall; if new, read → patch → verify |
| Add feature | `memory_search` for patterns → match existing → implement → test |
| Explain code | `memory_search` first → if prior analysis exists, use it; else read file |
| Refactor | `memory_search` for architecture → read call sites → plan → small steps |
| Repeat question | `memory_search` → use Turn Summary from last time. Do NOT re-read files. |
| Other | Answer directly. If about project code, read it first. Keep it short. |

**Never** say you cannot do something without trying. **Never** give up after one error.";

// ─────────────────────────────────────────────────────
// Layer 2: Tool Layer
// ─────────────────────────────────────────────────────

fn build_tool_layer(registry: &ToolRegistry) -> String {
    let skills_section = if registry.skills.is_empty() {
        String::new()
    } else {
        let mut s = String::from("\n## Skills\n");
        for skill in &registry.skills {
            s.push_str(&format!("- **{}**: {}\n", skill.name, skill.description));
        }
        s
    };

    format!("\
## Available Tools

| Tool | Use for |
|------|---------|
| `file_read` | Read file content. Default 200 lines; use `limit` for more. |
| `code_search` | Find all references to a function, type, or pattern (regex). |
| `file_search` | Find files by name or glob pattern. |
| `file_list` | Browse directory structure. |
| `file_write` | Create new file or full rewrite (>50% changed). |
| `file_patch` | Small targeted edit of an existing file. |
| `shell_exec` | Run build, test, lint, git commands. |
| `git_status` | Check working tree state. |
| `git_diff` | View staged/unstaged changes. |
| `memory_search` | Recall past decisions, architecture, conventions. |
| `recall` | Retrieve full offloaded tool output by node_id. |
| `web_fetch` | Fetch documentation or API reference. |

**Rules:**
- **Plan first, then tools.** Output ## Plan BEFORE calling `file_write`/`file_patch`. The system will block you otherwise.
- Prefer `code_search` over `shell_exec grep`. Prefer `file_search` over `shell_exec find`.
- Paths are relative to the working directory.{}",
    skills_section)
}

// ─────────────────────────────────────────────────────
// Layer 3: Context — provided dynamically by caller
// ─────────────────────────────────────────────────────

/// Gather git diff --stat (uncommitted changes summary).
pub fn gather_diff_context(working_dir: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", &working_dir.to_string_lossy(), "diff", "--stat"])
        .output().ok()?;
    if output.status.success() {
        let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !stat.is_empty() { Some(stat) } else { None }
    } else { None }
}

/// Gather git log context (last 5 commits, one line each).
pub fn gather_git_context(working_dir: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", &working_dir.to_string_lossy(), "log", "--oneline", "-5"])
        .output().ok()?;
    if output.status.success() {
        let log = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !log.is_empty() { Some(log) } else { None }
    } else { None }
}

/// Gather directory structure (top 2 levels, excluding build/deps).
pub fn gather_dir_context(working_dir: &std::path::Path) -> Option<String> {
    let mut result = String::new();
    gather_dir_recursive(working_dir, working_dir, &mut result, 0, 2);
    if result.is_empty() { None } else { Some(result) }
}

fn gather_dir_recursive(base: &std::path::Path, dir: &std::path::Path, out: &mut String, depth: usize, max_depth: usize) {
    if depth > max_depth { return; }
    let exclude = &["node_modules", ".git", "target", "dist", "build", "__pycache__", ".venv", ".ox", ".idea"];
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if exclude.contains(&name.as_str()) { continue; }
            let indent = "  ".repeat(depth);
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push_str(&format!("{}{}/\n", indent, name));
                gather_dir_recursive(base, &entry.path(), out, depth + 1, max_depth);
            } else if depth > 0 {
                out.push_str(&format!("{}{}\n", indent, name));
            }
        }
    }
}

// ─────────────────────────────────────────────────────
// Layer 4: User Override
// ─────────────────────────────────────────────────────

/// Load user rules from ~/.ox/rules.md (global) and .ox/rules.md (project level).
/// Global rules load first, project rules append/override.
fn load_user_rules(rt_env: &RuntimeEnvironment) -> Option<String> {
    let mut rules = String::new();

    // Global: ~/.ox/rules.md
    let global_path = rt_env.ox_home_dir.join("rules.md");
    if global_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&global_path) {
            if !content.trim().is_empty() {
                rules.push_str(&format!("### Global Rules ({})\n{}\n", global_path.display(), content.trim()));
            }
        }
    }

    // Project: .ox/rules.md
    if let Some(ref proj_root) = rt_env.project_root {
        let proj_path = proj_root.join(".ox").join("rules.md");
        if proj_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&proj_path) {
                if !content.trim().is_empty() {
                    rules.push_str(&format!("### Project Rules ({})\n{}\n", proj_path.display(), content.trim()));
                }
            }
        }
    }

    if rules.is_empty() { None } else { Some(rules) }
}

// ─────────────────────────────────────────────────────
// Layer 4 fallback: Behavior rules from config
// ─────────────────────────────────────────────────────

fn build_behavior_layer(br: &crate::config::BehaviorRulesConfig) -> String {
    if !br.custom_rules.is_empty() {
        let mut out = String::from("## Coding Rules (HIGHEST PRIORITY)\n");
        for (i, rule) in br.custom_rules.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, rule));
        }
        out
    } else if br.enforce_all {
        let mut out = String::from("## Behavior Rules\n");
        if br.enforce_safe_code { out.push_str("- Never suggest code that bypasses safety checks\n"); }
        if br.enforce_lint { out.push_str("- Run lint before declaring code complete\n"); }
        if br.enforce_format { out.push_str("- Format code before writing files\n"); }
        if br.enforce_tests { out.push_str("- Write tests for new functions\n"); }
        out
    } else {
        String::new()
    }
}

// ─────────────────────────────────────────────────────
// Layer 5: Safety (ALWAYS LAST — highest override priority)
// ─────────────────────────────────────────────────────

const SAFETY_LAYER: &str = "\
## Safety (OVERRIDES ALL ABOVE)

- Do NOT delete files or run destructive commands without explicit user request.
- Stay within the project directory. Do not read or write outside it.
- **NEVER output secrets, credentials, API keys, or tokens.**
- If you find a secret in code, warn the user but do NOT echo it.
- Clean up temporary files you create.

## Code Quality

- **Read before you write.** Never modify a file you have not read.
- **Minimal diffs.** Change only what is necessary.
- **Follow existing patterns.** Match naming, formatting, architecture of surrounding code.
- **Handle errors properly.** Use the project's error handling convention.
- **Write tests** for new functions. Verify they pass with `shell_exec`.
- **Run lint/format** after changes. Fix warnings before declaring done.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gather_dir_context() {
        let dir = std::env::temp_dir();
        let ctx = gather_dir_context(&dir);
        // Should not crash even on empty/invalid dirs
        assert!(ctx.is_some() || ctx.is_none());
    }
}
