use anyhow::Result;
use std::fs;
/// Auto-reflection module - Automatically analyze completed workflows and generate Skills
///
/// This module implements the auto-reflection mechanism that triggers after workflow completion.
/// It analyzes the execution trace, identifies reusable patterns, and creates new Skills.
use std::path::Path;
use std::sync::Arc;
use tracing;

use crate::context::SKILL_CREATION_PROMPT;
use crate::llm::LlmProvider;
use crate::message::Message;

/// Outcome of auto-reflection — draft for user confirmation or skip.
#[derive(Debug, Clone)]
pub enum ReflectOutcome {
    /// Skill generated; awaiting user confirmation before save.
    Draft {
        skill_id: String,
        content: String,
        description: String,
    },
    /// Reflection skipped (quality gate).
    Skipped { reason: String },
}

const MIN_TASK_LEN: usize = 12;

/// Analyze a completed workflow and generate reflection insights
pub struct AutoReflector {
    llm_provider: Arc<dyn LlmProvider>,
    project_root: std::path::PathBuf,
}

impl AutoReflector {
    /// Create a new AutoReflector
    pub fn new(llm_provider: Arc<dyn LlmProvider>, project_root: &Path) -> Result<Self> {
        Ok(Self {
            llm_provider,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Perform auto-reflection on a completed workflow.
    /// Returns a draft for user confirmation (does not save to disk).
    pub async fn reflect_on_workflow(
        &self,
        task_description: &str,
        execution_summary: &str,
        conversation_history: &[Message],
    ) -> Result<ReflectOutcome> {
        if let Some(reason) =
            Self::quality_gate(task_description, execution_summary, conversation_history)
        {
            tracing::info!("[AUTO-REFLECT] Skipped: {reason}");
            return Ok(ReflectOutcome::Skipped { reason });
        }

        tracing::info!(
            "[AUTO-REFLECT] Starting reflection for task: {}",
            task_description.chars().take(80).collect::<String>()
        );

        let existing_skills = crate::skill::dedup::list_project_skill_ids(&self.project_root);

        let prompt =
            self.build_reflection_prompt(task_description, execution_summary, conversation_history, &existing_skills);
        let skill_content = self.call_llm_for_reflection(&prompt).await?;

        if skill_content.trim().is_empty() {
            return Ok(ReflectOutcome::Skipped {
                reason: "LLM returned empty skill content".into(),
            });
        }

        let (skill_id, description) = self.parse_draft_metadata(&skill_content);
        let canonical =
            crate::skill::dedup::canonical_mandatory_id(&skill_id).unwrap_or(skill_id.as_str());
        let skill_id = canonical.to_string();

        if self.skill_exists(&skill_id) {
            tracing::info!("[AUTO-REFLECT] Skill `{skill_id}` exists — will merge on save");
        }

        Ok(ReflectOutcome::Draft {
            skill_id,
            content: skill_content,
            description,
        })
    }

    /// Save a confirmed skill draft to `.ox/skills/`.
    pub fn save_skill_draft(&self, content: &str) -> Result<String> {
        Self::save_content_to_project(&self.project_root, content)
    }

    /// Save skill markdown to project `.ox/skills/` (no LLM required).
    pub fn save_content_to_project(project_root: &Path, content: &str) -> Result<String> {
        Self::write_skill_file(project_root, content)
    }

    /// Quality gates — return skip reason if reflection should not run.
    fn quality_gate(
        task_description: &str,
        execution_summary: &str,
        conversation_history: &[Message],
    ) -> Option<String> {
        if task_description.trim().len() < MIN_TASK_LEN {
            return Some("Task too short for skill extraction".into());
        }
        // Substance = real edits happened. Detect case-insensitively and via the
        // unified tool-result envelope markers (`✓ edit_file` / `✓ file_write` /
        // `✓ delete_range`), so this no longer depends on the free-text summary or
        // on a case-sensitive match (edit_file emits "✅ Patched", capital P, which
        // the old lowercase `contains("patched")` never matched → reflection was
        // wrongly skipped for edit_file-only turns).
        let summary_lc = execution_summary.to_ascii_lowercase();
        let has_substance = summary_lc.contains("## done")
            || summary_lc.contains("modify")
            || summary_lc.contains("file_write")
            || summary_lc.contains("edit_file")
            || conversation_history.iter().any(|m| match m {
                Message::ToolResult { content, .. } => {
                    let c = content.to_ascii_lowercase();
                    c.contains("successfully")
                        || c.contains("patched")
                        || c.contains("edit_file")
                        || c.contains("file_write")
                        || c.contains("delete_range")
                }
                Message::Assistant { tool_calls, .. } => tool_calls.iter().any(|tc| {
                    let a = tc.arguments.to_ascii_lowercase();
                    a.contains("edit_file")
                        || a.contains("file_write")
                        || a.contains("delete_range")
                }),
                _ => false,
            });
        if !has_substance {
            return Some("No substantive code changes detected".into());
        }
        if conversation_history.len() < 4 {
            return Some("Conversation too short for reliable skill extraction".into());
        }
        None
    }

    fn skill_exists(&self, skill_id: &str) -> bool {
        let skills_dir = self.project_root.join(".ox").join("skills");
        skills_dir.join(format!("{skill_id}.md")).exists()
    }

    fn parse_draft_metadata(&self, content: &str) -> (String, String) {
        if let Ok((meta, body)) = self.extract_frontmatter(content) {
            let id = meta
                .get("id")
                .cloned()
                .or_else(|| {
                    body.lines()
                        .next()
                        .and_then(|l| l.strip_prefix("# "))
                        .map(|s| s.to_lowercase().replace(' ', "-"))
                })
                .unwrap_or_else(|| "generated-skill".into());
            let desc = meta
                .get("description")
                .cloned()
                .unwrap_or_else(|| "AI generated skill".into());
            return (id, desc);
        }
        let title = content
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("# "))
            .unwrap_or("generated-skill");
        (
            title.to_lowercase().replace(' ', "-"),
            "AI generated skill".into(),
        )
    }

    /// Legacy entry — kept for tests; prefer reflect_on_workflow + save_skill_draft.
    pub async fn reflect_and_save(
        &self,
        task_description: &str,
        execution_summary: &str,
        conversation_history: &[Message],
    ) -> Result<Option<String>> {
        match self
            .reflect_on_workflow(task_description, execution_summary, conversation_history)
            .await?
        {
            ReflectOutcome::Draft { content, .. } => Ok(Some(self.save_skill_draft(&content)?)),
            ReflectOutcome::Skipped { .. } => Ok(None),
        }
    }

    /// Build the reflection prompt with full context
    fn build_reflection_prompt(
        &self,
        task_description: &str,
        execution_summary: &str,
        conversation_history: &[Message],
        existing_skills: &[String],
    ) -> String {
        // Extract recent conversation context (last 20 messages max)
        let start_idx = conversation_history.len().saturating_sub(20);
        // 使用安全的切片方法
        let context_messages = if start_idx < conversation_history.len() {
            &conversation_history[start_idx..]
        } else {
            &[]
        };

        let conversation_context = context_messages
            .iter()
            .map(|msg| match msg {
                Message::User { content } => format!("User: {}", content),
                Message::Assistant { content, .. } => format!("Assistant: {}", content),
                Message::ToolResult { content, .. } => format!(
                    "Tool result: {}",
                    content.chars().take(200).collect::<String>()
                ),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let existing_skills_str = if existing_skills.is_empty() {
            "(none)".to_string()
        } else {
            existing_skills.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n")
        };

        SKILL_CREATION_PROMPT
            .replace("{task_description}", task_description)
            .replace("{execution_summary}", execution_summary)
            .replace("{conversation_context}", &conversation_context)
            .replace("{existing_skills}", &existing_skills_str)
    }

    /// Call LLM to generate reflection insights
    async fn call_llm_for_reflection(&self, prompt: &str) -> Result<String> {
        use tokio::sync::mpsc;

        let messages = vec![Message::system(prompt)];
        let (tx, mut rx) = mpsc::unbounded_channel::<crate::llm::LlmStreamEvent>();

        // Use stream_chat and collect the full response
        self.llm_provider
            .stream_chat(&messages, &[], tx, crate::llm::StreamOptions::default())
            .await?;

        let mut full_content = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                crate::llm::LlmStreamEvent::TextDelta(text) => {
                    full_content.push_str(&text);
                }
                crate::llm::LlmStreamEvent::Done { .. } => {
                    break;
                }
                crate::llm::LlmStreamEvent::Error(e) => {
                    return Err(anyhow::anyhow!("LLM streaming error: {}", e));
                }
                _ => {}
            }
        }

        Ok(full_content)
    }

    /// Parse the LLM-generated markdown and save as a skill.
    fn parse_and_save_skill(&self, content: &str) -> Result<String> {
        Self::write_skill_file(&self.project_root, content)
    }

    /// Write skill markdown to disk via dedup::plan_skill_write (shared by instance and static callers).
    fn write_skill_file(project_root: &Path, content: &str) -> Result<String> {
        let (repaired_content, skill_id, scope) = match Self::extract_frontmatter_static(content) {
            Ok((metadata, body)) => {
                let id = metadata.get("id").cloned().unwrap_or_else(|| {
                    body.lines()
                        .next()
                        .and_then(|l| l.strip_prefix("# "))
                        .unwrap_or("generated-skill")
                        .to_lowercase()
                        .replace(' ', "-")
                });
                let name = metadata
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| id.replace('-', " "));
                let description = metadata
                    .get("description")
                    .cloned()
                    .unwrap_or_else(|| "AI generated skill".to_string());
                let scope = metadata
                    .get("scope")
                    .cloned()
                    .unwrap_or_else(|| "project".to_string());
                let scope = match scope.as_str() {
                    "project" | "global" | "system" => scope,
                    _ => "project".to_string(),
                };
                let fixed = format!(
                    "---\nname: \"{}\"\ndescription: \"{}\"\nscope: \"{}\"\n---\n\n{}",
                    name,
                    description,
                    scope,
                    body.trim()
                );
                (fixed, id, scope)
            }
            Err(_) => {
                let body = content.trim();
                let title = body
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("# "))
                    .unwrap_or("generated-skill");
                let id = title.to_lowercase().replace(' ', "-");
                let fixed = format!(
                    "---\nname: \"{}\"\ndescription: \"AI generated skill\"\nscope: \"project\"\n---\n\n{}",
                    title, body
                );
                (fixed, id, "project".to_string())
            }
        };

        let resolved_id = crate::skill::dedup::canonical_mandatory_id(&skill_id)
            .unwrap_or(skill_id.as_str())
            .to_string();

        // Global-scope skills go to ~/.ox/skills/ — no dedup policy needed.
        if scope == "global" {
            let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
            let skills_dir = home.join(".ox").join("skills");
            fs::create_dir_all(&skills_dir)?;
            let path = skills_dir.join(format!("{resolved_id}.md"));
            fs::write(&path, &repaired_content)?;
            tracing::info!("[AUTO-REFLECT] Saved global skill to: {:?}", path);
            return Ok(resolved_id);
        }

        // Project-scope skills go through the full dedup decision chain.
        let plan = crate::skill::dedup::plan_skill_write(
            project_root,
            &resolved_id,
            &repaired_content,
            true,  // allow_merge
            false, // not onboarding
        );

        match plan {
            crate::skill::dedup::SkillWritePlan::CreateNew => {
                let skills_dir = project_root.join(".ox").join("skills");
                fs::create_dir_all(&skills_dir)?;
                let path = skills_dir.join(format!("{resolved_id}.md"));
                fs::write(&path, &repaired_content)?;
                tracing::info!("[AUTO-REFLECT] Saved skill to: {:?}", path);
                Ok(resolved_id)
            }
            crate::skill::dedup::SkillWritePlan::OverwriteMandatory => {
                let path = project_root.join(".ox").join("skills").join(format!("{resolved_id}.md"));
                fs::write(&path, &repaired_content)?;
                tracing::info!("[AUTO-REFLECT] Overwrote mandatory skill: {:?}", path);
                Ok(format!("{resolved_id} (overwritten)"))
            }
            crate::skill::dedup::SkillWritePlan::RedirectToCanonical {
                canonical_id,
                reason,
            } => {
                tracing::info!("[AUTO-REFLECT] {reason}");
                let path = project_root
                    .join(".ox")
                    .join("skills")
                    .join(format!("{canonical_id}.md"));
                if path.exists() {
                    let existing = fs::read_to_string(&path)?;
                    let merged =
                        crate::skill::dedup::merge_skill_markdown(&existing, &repaired_content);
                    fs::write(&path, &merged)?;
                    Ok(format!("{canonical_id} (merged via redirect)"))
                } else {
                    fs::create_dir_all(path.parent().unwrap())?;
                    fs::write(&path, &repaired_content)?;
                    Ok(format!("{canonical_id} (created via redirect)"))
                }
            }
            crate::skill::dedup::SkillWritePlan::MergeIntoExisting {
                target_path,
                merged_markdown,
            } => {
                fs::write(&target_path, &merged_markdown)?;
                tracing::info!("[AUTO-REFLECT] Merged into existing skill: {:?}", target_path);
                Ok(format!("{resolved_id} (merged)"))
            }
            crate::skill::dedup::SkillWritePlan::RejectDuplicate { message } => {
                tracing::warn!("[AUTO-REFLECT] Skill rejected: {message}");
                Ok(format!("{resolved_id} (rejected: duplicate)"))
            }
        }
    }

    /// Extract YAML frontmatter from markdown content
    fn extract_frontmatter(
        &self,
        content: &str,
    ) -> Result<(std::collections::HashMap<String, String>, String)> {
        Self::extract_frontmatter_static(content)
    }

    fn extract_frontmatter_static(
        content: &str,
    ) -> Result<(std::collections::HashMap<String, String>, String)> {
        let content = content.trim();

        if !content.starts_with("---") {
            return Err(anyhow::anyhow!(
                "Missing YAML frontmatter (---) at start of content"
            ));
        }

        let end_marker = content
            .find("\n---\n")
            .ok_or_else(|| anyhow::anyhow!("Missing closing --- for frontmatter"))?;

        // 使用安全的字符边界检查
        let frontmatter_str = content
            .get(3..end_marker)
            .ok_or_else(|| anyhow::anyhow!("Invalid frontmatter boundaries"))?; // Skip opening ---
        let body = content
            .get(end_marker + 5..)
            .map(|s| s.trim())
            .unwrap_or(""); // Skip closing ---\n

        // Simple YAML parser for key-value pairs
        let mut metadata = std::collections::HashMap::new();
        for line in frontmatter_str.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(colon_pos) = line.find(':') {
                // 使用安全的字符边界检查
                let key = line
                    .get(..colon_pos)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let value = line
                    .get(colon_pos + 1..)
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .unwrap_or_default();
                metadata.insert(key, value);
            }
        }

        Ok((metadata, body.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_frontmatter() {
        let content = r#"---
id: rust_async_best_practices
name: Rust Async Best Practices
description: Patterns for effective async/await usage in Rust
---

# Content here
Some markdown content."#;

        // This is just a compile check - actual test would need mock providers
        assert!(content.contains("rust_async_best_practices"));
    }

    fn convo(tool_result: &str) -> Vec<Message> {
        vec![
            Message::user("请修复登录逻辑"),
            Message::assistant("好的，我来改"),
            Message::ToolResult {
                tool_call_id: "t1".into(),
                content: tool_result.into(),
            },
            Message::assistant("完成"),
        ]
    }

    #[test]
    fn quality_gate_detects_edit_file_patched_case_insensitive() {
        // edit_file emits "✅ Patched" (capital P) wrapped by the unified
        // envelope as "✓ edit_file\n✅ Patched ...". Must count as substance.
        let convo = convo("✓ edit_file\n✅ Patched src/auth.rs (3 → 5 lines)");
        let skip = AutoReflector::quality_gate("修复登录鉴权逻辑的边界问题", "改完了", &convo);
        assert!(
            skip.is_none(),
            "edit_file change should pass the gate, got {skip:?}"
        );
    }

    #[test]
    fn quality_gate_detects_file_write_success() {
        let convo = convo("✓ file_write\n✅ Successfully written 200 bytes to a.rs");
        let skip = AutoReflector::quality_gate("新增配置文件加载逻辑", "done", &convo);
        assert!(skip.is_none());
    }

    #[test]
    fn quality_gate_skips_pure_readonly_turn() {
        let convo = convo("✓ file_read\nfn main() {}");
        let skip = AutoReflector::quality_gate("看看这个函数是干嘛的", "这是入口", &convo);
        assert!(skip.is_some(), "read-only Q&A should be skipped");
    }
}
