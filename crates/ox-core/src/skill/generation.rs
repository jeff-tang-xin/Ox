/// Skill Generation Layering - Distill execution traces into reusable skills
/// 
/// Inspired by TencentDB-Agent-Memory's approach:
/// - L0: Raw Execution Traces (工具调用序列)
/// - L1: Pattern Extraction (识别重复模式)
/// - L2: Skill Templates (通用模板生成)
/// - L3: Meta-Skills (跨项目抽象)

use std::path::{Path, PathBuf};
use std::fs;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Skill generation layer levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillLayer {
    /// L0: Raw execution traces
    L0ExecutionTrace,
    /// L1: Extracted patterns
    L1Pattern,
    /// L2: Skill templates
    L2SkillTemplate,
    /// L3: Meta-skills (cross-project abstractions)
    L3MetaSkill,
}

impl SkillLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L0ExecutionTrace => "l0_trace",
            Self::L1Pattern => "l1_pattern",
            Self::L2SkillTemplate => "l2_template",
            Self::L3MetaSkill => "l3_meta",
        }
    }
}

/// L0: Raw execution trace from a task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTrace {
    pub id: String,
    pub task_description: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub messages: Vec<MessageRecord>,
    pub success: bool,
    pub duration_secs: u64,
    pub timestamp: i64,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub arguments: String,
    pub result_summary: String,
    pub is_error: bool,
    pub order: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub role: String, // "user" | "assistant" | "tool"
    pub content_preview: String,
    pub has_tool_calls: bool,
    pub order: usize,
}

impl ExecutionTrace {
    /// Convert to Markdown for inspection
    pub fn to_markdown(&self) -> String {
        let mut md = format!(
            "# Execution Trace\n\n\
             **ID**: {}\n\
             **Task**: {}\n\
             **Success**: {}\n\
             **Duration**: {}s\n\
             **Timestamp**: {}\n\n\
             ---\n\n\
             ## Tool Calls\n\n",
            self.id,
            self.task_description,
            if self.success { "✅" } else { "❌" },
            self.duration_secs,
            chrono::DateTime::<chrono::Utc>::from_timestamp(self.timestamp, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
        );

        for (i, call) in self.tool_calls.iter().enumerate() {
            md.push_str(&format!(
                "### {}. `{}` {}\n\n**Args**: {}\n\n**Result**: {}\n\n",
                i + 1,
                call.tool_name,
                if call.is_error { "❌" } else { "✅" },
                if call.arguments.len() > 200 {
                    let boundary = call.arguments.char_indices()
                        .take_while(|(i, _)| *i < 200)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(call.arguments.len());
                    format!("{}...", &call.arguments[..boundary])
                } else {
                    call.arguments.clone()
                },
                if call.result_summary.len() > 200 {
                    let boundary = call.result_summary.char_indices()
                        .take_while(|(i, _)| *i < 200)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(call.result_summary.len());
                    format!("{}...", &call.result_summary[..boundary])
                } else {
                    call.result_summary.clone()
                },
            ));
        }

        md
    }

    /// Save trace to file for debugging
    pub fn save_to_file(&self, dir: &Path) -> std::io::Result<PathBuf> {
        let traces_dir = dir.join(".ox").join("traces");
        fs::create_dir_all(&traces_dir)?;

        let filename = format!("trace_{}.md", self.id);
        let path = traces_dir.join(&filename);

        fs::write(&path, self.to_markdown())?;
        Ok(path)
    }
}

/// L1: Extracted pattern from multiple traces
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedPattern {
    pub id: String,
    pub pattern_name: String,
    pub description: String,
    pub source_traces: Vec<String>,
    pub common_tool_sequence: Vec<String>,
    pub frequency: u32,
    pub success_rate: f32,
    pub applicable_scenarios: Vec<String>,
    pub timestamp: i64,
}

impl ExtractedPattern {
    /// Convert to Markdown for review
    pub fn to_markdown(&self) -> String {
        format!(
            "# Pattern: {name}\n\n\
             **ID**: {id}\n\
             **Frequency**: {freq} times\n\
             **Success Rate**: {rate:.1}%\n\
             **Source Traces**: {traces}\n\n\
             ---\n\n\
             ## Description\n\n{desc}\n\n\
             ## Common Tool Sequence\n\n{sequence}\n\n\
             ## Applicable Scenarios\n\n{scenarios}\n",
            name = self.pattern_name,
            id = self.id,
            freq = self.frequency,
            rate = self.success_rate * 100.0,
            traces = self.source_traces.len(),
            desc = self.description,
            sequence = self.common_tool_sequence.iter()
                .enumerate()
                .map(|(i, t)| format!("{}. `{}`", i + 1, t))
                .collect::<Vec<_>>()
                .join("\n"),
            scenarios = self.applicable_scenarios.iter()
                .map(|s| format!("- {}", s))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// L2: Skill template ready for use
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTemplate {
    pub id: String,
    pub skill_name: String,
    pub description: String,
    pub source_pattern: String,
    pub trigger_conditions: Vec<String>,
    pub step_by_step_guide: Vec<String>,
    pub example_tool_calls: Vec<ToolCallRecord>,
    pub pitfalls: Vec<String>,
    pub timestamp: i64,
}

impl SkillTemplate {
    /// Convert to Ox Skill Markdown format
    pub fn to_skill_markdown(&self) -> String {
        let frontmatter = format!(
            "---\n\
             name: {name}\n\
             description: {desc}\n\
             ---\n\n",
            name = self.skill_name,
            desc = self.description,
        );

        let mut body = format!(
            "# {name}\n\n\
             ## When to Use\n\n\
             {triggers}\n\n\
             ## Step-by-Step Guide\n\n\
             {steps}\n\n",
            name = self.skill_name,
            triggers = self.trigger_conditions.iter()
                .map(|c| format!("- {}", c))
                .collect::<Vec<_>>()
                .join("\n"),
            steps = self.step_by_step_guide.iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n"),
        );

        if !self.pitfalls.is_empty() {
            body.push_str("## Common Pitfalls\n\n");
            body.push_str(&self.pitfalls.iter()
                .map(|p| format!("- ⚠️ {}", p))
                .collect::<Vec<_>>()
                .join("\n"));
            body.push_str("\n\n");
        }

        if !self.example_tool_calls.is_empty() {
            body.push_str("## Example Tool Calls\n\n");
            for (i, call) in self.example_tool_calls.iter().enumerate() {
                body.push_str(&format!(
                    "### Example {}\n\n```json\n{}\n```\n\n",
                    i + 1,
                    serde_json::to_string_pretty(&serde_json::json!({
                        "tool": call.tool_name,
                        "arguments": call.arguments
                    })).unwrap_or_default()
                ));
            }
        }

        frontmatter + &body
    }

    /// Save as Ox Skill file
    pub fn save_as_skill(&self, skills_dir: &Path) -> std::io::Result<PathBuf> {
        fs::create_dir_all(skills_dir)?;

        let filename = format!("{}.md", self.id);
        let path = skills_dir.join(&filename);

        fs::write(&path, self.to_skill_markdown())?;
        Ok(path)
    }
}

/// L3: Meta-skill - cross-project abstraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaSkill {
    pub id: String,
    pub name: String,
    pub abstract_description: String,
    pub source_skills: Vec<String>,
    pub universal_principles: Vec<String>,
    pub adaptation_guidelines: Vec<String>,
    pub timestamp: i64,
}

impl MetaSkill {
    /// Convert to Markdown
    pub fn to_markdown(&self) -> String {
        format!(
            "# Meta-Skill: {name}\n\n\
             **Abstract**: {abstract}\n\
             **Source Skills**: {sources}\n\n\
             ---\n\n\
             ## Universal Principles\n\n{principles}\n\n\
             ## Adaptation Guidelines\n\n{guidelines}\n",
            name = self.name,
            abstract = self.abstract_description,
            sources = self.source_skills.len(),
            principles = self.universal_principles.iter()
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n"),
            guidelines = self.adaptation_guidelines.iter()
                .map(|g| format!("- {}", g))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// Skill generation pipeline manager
pub struct SkillGenerator {
    base_path: PathBuf,
}

impl SkillGenerator {
    pub fn new(base_path: &Path) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
        }
    }

    /// Record an execution trace (L0)
    pub fn record_trace(
        &self,
        task_description: &str,
        messages: &[Message],
        success: bool,
        duration_secs: u64,
        project_id: Option<&str>,
    ) -> anyhow::Result<ExecutionTrace> {
        let tool_calls = extract_tool_calls(messages);
        let message_records = extract_message_records(messages);

        let trace = ExecutionTrace {
            id: format!("trace_{}", Utc::now().timestamp()),
            task_description: task_description.to_string(),
            tool_calls,
            messages: message_records,
            success,
            duration_secs,
            timestamp: Utc::now().timestamp(),
            project_id: project_id.map(|s| s.to_string()),
        };

        // Save trace to file
        trace.save_to_file(&self.base_path)?;
        tracing::info!("Recorded execution trace: {}", trace.id);

        Ok(trace)
    }

    /// Extract patterns from multiple traces (L0 → L1)
    pub fn extract_patterns(
        &self,
        traces: &[ExecutionTrace],
    ) -> Vec<ExtractedPattern> {
        // Simple pattern extraction based on tool sequence similarity
        let mut patterns_by_sequence: std::collections::HashMap<Vec<String>, Vec<&ExecutionTrace>> =
            std::collections::HashMap::new();

        for trace in traces {
            let sequence: Vec<String> = trace.tool_calls.iter()
                .map(|t| t.tool_name.clone())
                .collect();

            patterns_by_sequence
                .entry(sequence)
                .or_insert_with(Vec::new)
                .push(trace);
        }

        patterns_by_sequence
            .into_iter()
            .filter(|(_, group)| group.len() >= 2) // Only patterns that appear at least twice
            .enumerate()
            .map(|(i, (sequence, group))| {
                let success_count = group.iter().filter(|t| t.success).count();
                let success_rate = success_count as f32 / group.len() as f32;

                ExtractedPattern {
                    id: format!("pattern_{i}"),
                    pattern_name: format!("Pattern {}", i + 1),
                    description: format!(
                        "Common tool sequence appearing {} times",
                        group.len()
                    ),
                    source_traces: group.iter().map(|t| t.id.clone()).collect(),
                    common_tool_sequence: sequence,
                    frequency: group.len() as u32,
                    success_rate,
                    applicable_scenarios: vec![],
                    timestamp: Utc::now().timestamp(),
                }
            })
            .collect()
    }

    /// Generate skill templates from patterns (L1 → L2)
    pub fn generate_skill_templates(
        &self,
        patterns: &[ExtractedPattern],
    ) -> Vec<SkillTemplate> {
        patterns.iter().map(|pattern| {
            SkillTemplate {
                id: format!("skill_{}", pattern.id),
                skill_name: pattern.pattern_name.replace("Pattern", "Skill"),
                description: pattern.description.clone(),
                source_pattern: pattern.id.clone(),
                trigger_conditions: pattern.applicable_scenarios.clone(),
                step_by_step_guide: pattern.common_tool_sequence.iter()
                    .enumerate()
                    .map(|(_i, tool)| format!("Use `{}` tool", tool))
                    .collect(),
                example_tool_calls: vec![], // Would need to extract from traces
                pitfalls: vec![],
                timestamp: Utc::now().timestamp(),
            }
        }).collect()
    }

    /// Abstract meta-skills from multiple skill templates (L2 → L3)
    pub fn distill_meta_skills(
        &self,
        _templates: &[SkillTemplate],
    ) -> Vec<MetaSkill> {
        // Group similar skills and extract common principles
        // This is a simplified version - in production, would use LLM
        vec![]
    }

    /// Full pipeline: traces → patterns → skills → meta-skills
    pub fn run_full_pipeline(
        &self,
        traces: &[ExecutionTrace],
    ) -> anyhow::Result<SkillGenerationReport> {
        tracing::info!("Starting skill generation pipeline with {} traces", traces.len());

        // L0 → L1
        let patterns = self.extract_patterns(traces);
        tracing::info!("Extracted {} patterns", patterns.len());

        // L1 → L2
        let templates = self.generate_skill_templates(&patterns);
        tracing::info!("Generated {} skill templates", templates.len());

        // Save skill templates
        let skills_dir = self.base_path.join(".ox").join("skills").join("generated");
        let mut saved_paths = Vec::new();
        for template in &templates {
            if let Ok(path) = template.save_as_skill(&skills_dir) {
                saved_paths.push(path);
            }
        }

        // L2 → L3
        let meta_skills = self.distill_meta_skills(&templates);
        tracing::info!("Distilled {} meta-skills", meta_skills.len());

        Ok(SkillGenerationReport {
            trace_count: traces.len(),
            pattern_count: patterns.len(),
            template_count: templates.len(),
            meta_skill_count: meta_skills.len(),
            saved_skill_paths: saved_paths,
        })
    }
}

/// Report from skill generation pipeline
#[derive(Debug, Clone)]
pub struct SkillGenerationReport {
    pub trace_count: usize,
    pub pattern_count: usize,
    pub template_count: usize,
    pub meta_skill_count: usize,
    pub saved_skill_paths: Vec<PathBuf>,
}

impl std::fmt::Display for SkillGenerationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Skill Generation Report:\n\
             - Execution Traces: {}\n\
             - Patterns Extracted: {}\n\
             - Skill Templates: {}\n\
             - Meta-Skills: {}\n\
             - Saved Skills: {}",
            self.trace_count,
            self.pattern_count,
            self.template_count,
            self.meta_skill_count,
            self.saved_skill_paths.len()
        )
    }
}

/// Helper: Extract tool calls from messages
fn extract_tool_calls(messages: &[Message]) -> Vec<ToolCallRecord> {
    let mut tool_calls = Vec::new();
    let mut order = 0;

    for msg in messages {
        if let Message::Assistant { tool_calls: tc_list, .. } = msg {
            for tc in tc_list {
                tool_calls.push(ToolCallRecord {
                    tool_name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                    result_summary: String::new(), // Would need to match with ToolResult
                    is_error: false,
                    order,
                });
                order += 1;
            }
        }
    }

    tool_calls
}

/// Helper: Extract message records from messages
fn extract_message_records(messages: &[Message]) -> Vec<MessageRecord> {
    messages.iter().enumerate().map(|(i, msg)| {
        let (role, content_preview, has_tool_calls) = match msg {
            Message::User { content } => {
                ("user".to_string(), content.chars().take(100).collect(), false)
            }
            Message::Assistant { content, tool_calls, .. } => {
                ("assistant".to_string(), content.chars().take(100).collect(), !tool_calls.is_empty())
            }
            Message::ToolResult { content, .. } => {
                ("tool".to_string(), content.chars().take(100).collect(), false)
            }
            Message::System { content } => {
                ("system".to_string(), content.chars().take(100).collect(), false)
            }
        };

        MessageRecord {
            role,
            content_preview,
            has_tool_calls,
            order: i,
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_trace_markdown() {
        let trace = ExecutionTrace {
            id: "test_trace".to_string(),
            task_description: "Test task".to_string(),
            tool_calls: vec![ToolCallRecord {
                tool_name: "file_write".to_string(),
                arguments: "{}".to_string(),
                result_summary: "Success".to_string(),
                is_error: false,
                order: 0,
            }],
            messages: vec![],
            success: true,
            duration_secs: 5,
            timestamp: Utc::now().timestamp(),
            project_id: Some("test".to_string()),
        };

        let md = trace.to_markdown();
        assert!(md.contains("Test task"));
        assert!(md.contains("file_write"));
    }

    #[test]
    fn test_skill_template_markdown() {
        let template = SkillTemplate {
            id: "test_skill".to_string(),
            skill_name: "Test Skill".to_string(),
            description: "A test skill".to_string(),
            source_pattern: "pattern_1".to_string(),
            trigger_conditions: vec!["When doing X".to_string()],
            step_by_step_guide: vec!["Step 1".to_string()],
            example_tool_calls: vec![],
            pitfalls: vec![],
            timestamp: Utc::now().timestamp(),
        };

        let md = template.to_skill_markdown();
        assert!(md.contains("---"));
        assert!(md.contains("Test Skill"));
        assert!(md.contains("Step 1"));
    }
}
