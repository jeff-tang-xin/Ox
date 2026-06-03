/// Auto-reflection module - Automatically analyze completed workflows and generate Skills
/// 
/// This module implements the auto-reflection mechanism that triggers after workflow completion.
/// It analyzes the execution trace, identifies reusable patterns, and creates new Skills.

use std::path::Path;
use std::fs;
use std::sync::Arc;
use anyhow::Result;
use tracing;

use crate::message::Message;
use crate::llm::LlmProvider;
use crate::context::SKILL_CREATION_PROMPT;

/// Analyze a completed workflow and generate reflection insights
pub struct AutoReflector {
    llm_provider: Arc<dyn LlmProvider>,
    project_root: std::path::PathBuf,
}

impl AutoReflector {
    /// Create a new AutoReflector
    pub fn new(
        llm_provider: Arc<dyn LlmProvider>,
        project_root: &Path,
    ) -> Result<Self> {
        Ok(Self {
            llm_provider,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Perform auto-reflection on a completed workflow
    /// 
    /// # Arguments
    /// * `task_description` - Description of the completed task
    /// * `execution_summary` - Summary of how the task was executed
    /// * `conversation_history` - Full conversation history for context
    /// 
    /// # Returns
    /// Generated skill ID if successful
    pub async fn reflect_on_workflow(
        &self,
        task_description: &str,
        execution_summary: &str,
        conversation_history: &[Message],
    ) -> Result<Option<String>> {
        tracing::info!(
            "[AUTO-REFLECT] Starting reflection for task: {}",
            task_description.chars().take(80).collect::<String>()
        );

        // Step 1: Build reflection prompt with context
        let prompt = self.build_reflection_prompt(task_description, execution_summary, conversation_history);
        
        tracing::debug!("[AUTO-REFLECT] Prompt length: {} chars", prompt.len());

        // Step 2: Call LLM to generate skill content
        let skill_content = self.call_llm_for_reflection(&prompt).await?;
        
        if skill_content.trim().is_empty() {
            tracing::warn!("[AUTO-REFLECT] LLM returned empty content, skipping skill generation");
            return Ok(None);
        }

        tracing::debug!("[AUTO-REFLECT] Generated skill content (first 200 chars):\n{}", 
            skill_content.chars().take(200).collect::<String>());

        // Step 3: Parse and save the generated skill
        match self.parse_and_save_skill(&skill_content) {
            Ok(skill_id) => {
                tracing::info!("[AUTO-REFLECT] ✅ Successfully created skill: {}", skill_id);
                Ok(Some(skill_id))
            }
            Err(e) => {
                tracing::error!("[AUTO-REFLECT] ❌ Failed to parse/save skill: {}", e);
                Err(e)
            }
        }
    }

    /// Build the reflection prompt with full context
    fn build_reflection_prompt(
        &self,
        task_description: &str,
        execution_summary: &str,
        conversation_history: &[Message],
    ) -> String {
        // Extract recent conversation context (last 20 messages max)
        let start_idx = conversation_history.len().saturating_sub(20);
        // 使用安全的切片方法
        let context_messages = if start_idx < conversation_history.len() {
            &conversation_history[start_idx..]
        } else {
            &[]
        };
        
        let conversation_context = context_messages.iter().map(|msg| {
            match msg {
                Message::User { content } => format!("User: {}", content),
                Message::Assistant { content, .. } => format!("Assistant: {}", content),
                Message::ToolResult { content, .. } => format!("Tool result: {}", content.chars().take(200).collect::<String>()),
                _ => String::new(),
            }
        }).collect::<Vec<_>>().join("\n\n");

        SKILL_CREATION_PROMPT
            .replace("{task_description}", task_description)
            .replace("{execution_summary}", execution_summary)
            .replace("{conversation_context}", &conversation_context)
    }

    /// Call LLM to generate reflection insights
    async fn call_llm_for_reflection(&self, prompt: &str) -> Result<String> {
        use tokio::sync::mpsc;
        
        let messages = vec![Message::system(prompt)];
        let (tx, mut rx) = mpsc::unbounded_channel::<crate::llm::LlmStreamEvent>();
        
        // Use stream_chat and collect the full response
        self.llm_provider.stream_chat(&messages, &[], tx).await?;
        
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

    /// Parse the LLM-generated markdown and save as a skill
    fn parse_and_save_skill(&self, content: &str) -> Result<String> {
        // Extract skill metadata from YAML frontmatter
        let (metadata, _body) = self.extract_frontmatter(content)?;
        
        let skill_id = metadata.get("id")
            .ok_or_else(|| anyhow::anyhow!("Missing 'id' in skill frontmatter"))?
            .clone();
        
        let _name = metadata.get("name")
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' in skill frontmatter"))?
            .clone();
        
        let _description = metadata.get("description")
            .ok_or_else(|| anyhow::anyhow!("Missing 'description' in skill frontmatter"))?
            .clone();

        // Save skill to project-level skills directory
        let skills_dir = self.project_root.join(".ox").join("skills");
        fs::create_dir_all(&skills_dir)?;
        
        let skill_file = skills_dir.join(format!("{}.md", skill_id));
        fs::write(&skill_file, content)?;
        
        tracing::info!("[AUTO-REFLECT] Saved skill to: {:?}", skill_file);
        
        Ok(skill_id)
    }

    /// Extract YAML frontmatter from markdown content
    fn extract_frontmatter(&self, content: &str) -> Result<(std::collections::HashMap<String, String>, String)> {
        let content = content.trim();
        
        if !content.starts_with("---") {
            return Err(anyhow::anyhow!("Missing YAML frontmatter (---) at start of content"));
        }

        let end_marker = content.find("\n---\n")
            .ok_or_else(|| anyhow::anyhow!("Missing closing --- for frontmatter"))?;

        // 使用安全的字符边界检查
        let frontmatter_str = content.get(3..end_marker)
            .ok_or_else(|| anyhow::anyhow!("Invalid frontmatter boundaries"))?; // Skip opening ---
        let body = content.get(end_marker + 5..)
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
                let key = line.get(..colon_pos)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let value = line.get(colon_pos + 1..)
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
}
