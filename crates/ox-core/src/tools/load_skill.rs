/// load_skill — LLM-callable tool to load the FULL content of a skill on demand.
///
/// At startup, only skill manifest (name + short description) is injected into
/// the system prompt. When the LLM needs to actually execute a skill's detailed
/// instructions, it calls `load_skill(skill_name)` to get the complete manual.
///
/// The full content is then injected as `<ACTIVE_SKILL_MANUAL>` in the next
/// system message by the agent loop, and removed after the task completes.

use serde_json::{Value, json};
use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct LoadSkillTool;

impl Default for LoadSkillTool {
    fn default() -> Self { Self }
}

#[async_trait::async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str { "load_skill" }

    fn description(&self) -> &str {
        "Load the FULL instructions for an on-demand skill (builtin / global / project extension). \
         Mandatory project skills (project-conventions, project-business-guide) are already injected \
         in Plan/Execute — do not reload them. Call this for other skills when you need their workflow."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "The skill ID (filename without .md) from the Loaded Skills list."
                }
            },
            "required": ["skill_name"]
        })
    }

    fn safety_level(&self) -> SafetyLevel { SafetyLevel::Safe }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return ToolOutput::error("Missing required parameter: 'skill_name'. Use the skill ID from the Loaded Skills list."),
        };

        // Search through known skill directories
        let search_dirs = [
            // Project skills (.ox/skills/)
            ctx.working_dir.join(".ox").join("skills"),
            // Global skills (~/.ox/skills/)
            dirs::home_dir().unwrap_or_default().join(".ox").join("skills"),
            // System skills (builtin)
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/skill/builtin"),
        ];

        for dir in &search_dirs {
            if !dir.exists() { continue; }
            let file_path = dir.join(format!("{}.md", skill_name));
            if file_path.exists() {
                match std::fs::read_to_string(&file_path) {
                    Ok(content) => {
                        // Strip YAML frontmatter if present
                        let body = strip_frontmatter(&content);
                        tracing::info!("[load_skill] Loaded full manual for '{}' ({} chars)", skill_name, body.len());
                        return ToolOutput::success(format!(
                            "<ACTIVE_SKILL_MANUAL>\n# {name}\n\n{body}\n\n</ACTIVE_SKILL_MANUAL>\n\n\
                             ✅ Full manual loaded. Follow the instructions above to complete the task. \
                             When done, output ## Done.",
                            name = skill_name,
                            body = body
                        ));
                    }
                    Err(e) => {
                        return ToolOutput::error(format!("Found skill file at {:?} but failed to read: {}", file_path, e));
                    }
                }
            }
        }

        ToolOutput::error(format!(
            "❌ Skill '{}' not found.\n\n\
             Available skills are listed in the system prompt under 【方法】.\n\
             Check the ID and try again.",
            skill_name
        ))
    }
}

/// Strip YAML frontmatter (--- ... ---) from markdown content.
fn strip_frontmatter(content: &str) -> String {
    if !content.starts_with("---") {
        return content.trim().to_string();
    }
    if let Some(end) = content[3..].find("---") {
        content[3 + end + 3..].trim().to_string()
    } else {
        content.trim().to_string()
    }
}
