//! Lightweight memory node types for CLI display and context formatting.
//!
//! Replaces the legacy `memory::MemoryNode` after MemoryManager removal.

use serde::{Deserialize, Serialize};

fn default_recent_scores() -> [f32; 5] {
    [0.0; 5]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,
    pub content: String,
    pub node_type: MemoryNodeType,
    pub depth: u8,
    pub project_id: Option<String>,
    pub language: String,
    pub source: MemorySource,
    pub created_at: i64,
    pub last_accessed: i64,
    pub is_project_critical: bool,
    pub traces: [f32; 5],
    pub language_weight: f64,
    #[serde(default)]
    pub avg_llm_score: f32,
    #[serde(default)]
    pub judge_eval_count: u32,
    #[serde(default = "default_recent_scores")]
    pub recent_scores: [f32; 5],
    #[serde(default)]
    pub related_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryNodeType {
    Fact,
    Style,
    Architectural,
    AntiPattern,
    Business,
    BestPractice,
    Pattern,
    MetaSkill,
}

impl MemoryNodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Style => "style",
            Self::Architectural => "architectural",
            Self::AntiPattern => "anti_pattern",
            Self::Business => "business",
            Self::BestPractice => "best_practice",
            Self::Pattern => "pattern",
            Self::MetaSkill => "meta_skill",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fact" => Some(Self::Fact),
            "style" => Some(Self::Style),
            "architectural" => Some(Self::Architectural),
            "anti_pattern" => Some(Self::AntiPattern),
            "business" => Some(Self::Business),
            "best_practice" => Some(Self::BestPractice),
            "pattern" => Some(Self::Pattern),
            "meta_skill" => Some(Self::MetaSkill),
            _ => None,
        }
    }
}

impl std::fmt::Display for MemoryNodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemorySource {
    UserExplicit,
    ToolObservation,
    LlmExtraction,
    Feedback,
    RefinedSummary,
}

impl MemorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserExplicit => "user_explicit",
            Self::ToolObservation => "tool_observation",
            Self::LlmExtraction => "llm_extraction",
            Self::Feedback => "feedback",
            Self::RefinedSummary => "refined_summary",
        }
    }
}

/// Format retrieved memory nodes for system prompt injection.
pub fn format_memory_context(nodes: &[MemoryNode], use_xml: bool) -> String {
    if nodes.is_empty() {
        return String::new();
    }

    let mut file_memories = Vec::new();
    let mut other_memories = Vec::new();

    for node in nodes {
        if node.content.starts_with("[FILE]") || node.content.starts_with("Read file:") {
            file_memories.push(node);
        } else {
            other_memories.push(node);
        }
    }

    let mut out = String::new();

    if !file_memories.is_empty() {
        out.push_str("\n## Files Already Read\n\n");
        for n in file_memories.iter().take(5) {
            let summary_line = if n.content.starts_with("[FILE]") {
                n.content.lines().next().unwrap_or(&n.content).to_string()
            } else {
                n.content.clone()
            };
            out.push_str(&format!("- {summary_line}\n"));
        }
        out.push('\n');
    }

    if !other_memories.is_empty() {
        if use_xml {
            out.push_str("<relevant_memories>\n");
            for n in other_memories.iter().take(8) {
                let max_len = 250;
                let content = truncate_chars(&n.content, max_len);
                out.push_str(&format!(
                    "  <memory depth=\"{}\" type=\"{}\">{}</memory>\n",
                    n.depth, n.node_type, content
                ));
            }
            out.push_str("</relevant_memories>");
        } else {
            out.push_str("Relevant context:\n");
            for n in other_memories.iter().take(5) {
                let content = truncate_chars(&n.content, 250);
                out.push_str(&format!(
                    "- [{}] (depth {}) {}\n",
                    n.node_type, n.depth, content
                ));
            }
        }
    }

    out
}

fn truncate_chars(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_len).collect();
    format!("{truncated}...")
}
