/// Memory Layering - Four-tier progressive memory architecture
/// 
/// Inspired by TencentDB-Agent-Memory's approach:
/// - L0: Raw Conversations (原始对话)
/// - L1: Atom Facts (原子事实)  
/// - L2: Scenario Chunks (场景分块)
/// - L3: Project Persona (项目画像)
///
/// This module provides the layering abstraction on top of existing MemoryStore.

use std::path::Path;
use serde::{Deserialize, Serialize};
use chrono::Utc;

use super::{MemoryNode, MemoryNodeType, MemorySource};

/// Memory layer levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryLayer {
    /// L0: Raw conversation logs
    L0RawConversation,
    /// L1: Refined atomic facts
    L1AtomFact,
    /// L2: Scenario-based chunks
    L2ScenarioChunk,
    /// L3: Project persona/profile
    L3ProjectPersona,
}

impl MemoryLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L0RawConversation => "l0_raw",
            Self::L1AtomFact => "l1_atom",
            Self::L2ScenarioChunk => "l2_scenario",
            Self::L3ProjectPersona => "l3_persona",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "l0_raw" => Some(Self::L0RawConversation),
            "l1_atom" => Some(Self::L1AtomFact),
            "l2_scenario" => Some(Self::L2ScenarioChunk),
            "l3_persona" => Some(Self::L3ProjectPersona),
            _ => None,
        }
    }

    /// Get typical depth for this layer
    pub fn default_depth(&self) -> u8 {
        match self {
            Self::L0RawConversation => 0,
            Self::L1AtomFact => 1,
            Self::L2ScenarioChunk => 2,
            Self::L3ProjectPersona => 3,
        }
    }

    /// Whether this layer should be persisted long-term
    pub fn is_long_term(&self) -> bool {
        matches!(self, Self::L2ScenarioChunk | Self::L3ProjectPersona)
    }

    /// Whether this layer can be safely cleaned up
    pub fn is_cleanup_candidate(&self) -> bool {
        matches!(self, Self::L0RawConversation | Self::L1AtomFact)
    }
}

/// L0 Raw Conversation entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawConversation {
    pub id: String,
    pub session_id: String,
    pub user_message: String,
    pub assistant_response: String,
    pub timestamp: i64,
    pub project_id: Option<String>,
    pub tools_used: Vec<String>,
    pub success: bool,
}

impl RawConversation {
    pub fn to_memory_node(&self) -> MemoryNode {
        let content = format!(
            "User: {}\n\nAssistant: {}",
            self.user_message, self.assistant_response
        );
        
        MemoryNode {
            id: format!("l0_{}", self.id),
            content,
            node_type: MemoryNodeType::Fact,
            depth: MemoryLayer::L0RawConversation.default_depth(),
            project_id: self.project_id.clone(),
            language: "en".to_string(),
            source: MemorySource::ToolObservation,
            created_at: self.timestamp,
            last_accessed: self.timestamp,
            is_project_critical: false,
            traces: [0.0; 5],
            language_weight: 0.5,
            avg_llm_score: 0.0,
            judge_eval_count: 0,
            recent_scores: [0.0; 5],
            related_files: vec![],
        }
    }
}

/// L1 Atom Fact - distilled from conversations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomFact {
    pub id: String,
    pub fact: String,
    pub category: String, // e.g., "coding_style", "architecture", "tool_preference"
    pub confidence: f32,
    pub source_conversation_ids: Vec<String>,
    pub timestamp: i64,
    pub project_id: Option<String>,
}

impl AtomFact {
    pub fn to_memory_node(&self) -> MemoryNode {
        let node_type = match self.category.as_str() {
            "coding_style" => MemoryNodeType::Style,
            "architecture" => MemoryNodeType::Architectural,
            "anti_pattern" => MemoryNodeType::AntiPattern,
            "best_practice" => MemoryNodeType::BestPractice,
            _ => MemoryNodeType::Fact,
        };

        MemoryNode {
            id: format!("l1_{}", self.id),
            content: self.fact.clone(),
            node_type,
            depth: MemoryLayer::L1AtomFact.default_depth(),
            project_id: self.project_id.clone(),
            language: "en".to_string(),
            source: MemorySource::LlmExtraction,
            created_at: self.timestamp,
            last_accessed: self.timestamp,
            is_project_critical: self.confidence > 0.9,
            traces: [self.confidence; 5],
            language_weight: 0.5,
            avg_llm_score: self.confidence,
            judge_eval_count: 1,
            recent_scores: [self.confidence; 5],
            related_files: vec![],
        }
    }
}

/// L2 Scenario Chunk - aggregated patterns from multiple atoms
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioChunk {
    pub id: String,
    pub scenario_name: String,
    pub description: String,
    pub related_atoms: Vec<String>,
    pub common_patterns: Vec<String>,
    pub applicable_tools: Vec<String>,
    pub timestamp: i64,
    pub project_id: Option<String>,
    pub usage_count: u32,
}

impl ScenarioChunk {
    pub fn to_memory_node(&self) -> MemoryNode {
        let content = format!(
            "Scenario: {}\n\nDescription: {}\n\nPatterns:\n{}\n\nTools: {}",
            self.scenario_name,
            self.description,
            self.common_patterns.join("\n- "),
            self.applicable_tools.join(", ")
        );

        MemoryNode {
            id: format!("l2_{}", self.id),
            content,
            node_type: MemoryNodeType::Pattern,
            depth: MemoryLayer::L2ScenarioChunk.default_depth(),
            project_id: self.project_id.clone(),
            language: "en".to_string(),
            source: MemorySource::RefinedSummary,
            created_at: self.timestamp,
            last_accessed: self.timestamp,
            is_project_critical: self.usage_count > 5,
            traces: [1.0; 5],
            language_weight: 0.7,
            avg_llm_score: 0.8,
            judge_eval_count: self.usage_count,
            recent_scores: [0.8; 5],
            related_files: vec![],
        }
    }
}

/// L3 Project Persona - high-level profile distilled from scenarios
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPersona {
    pub id: String,
    pub project_name: String,
    pub tech_stack: Vec<String>,
    pub coding_conventions: Vec<String>,
    pub architectural_patterns: Vec<String>,
    pub common_pitfalls: Vec<String>,
    pub preferred_workflows: Vec<String>,
    pub team_preferences: Vec<String>,
    pub generated_at: i64,
    pub last_updated: i64,
    pub version: u32,
}

impl ProjectPersona {
    pub fn to_markdown(&self) -> String {
        format!(
            "# Project Persona: {}\n\n\
             **Generated**: {}\n\
             **Version**: {}\n\n\
             ## Tech Stack\n{}\n\n\
             ## Coding Conventions\n{}\n\n\
             ## Architectural Patterns\n{}\n\n\
             ## Common Pitfalls\n{}\n\n\
             ## Preferred Workflows\n{}\n\n\
             ## Team Preferences\n{}\n",
            self.project_name,
            chrono::DateTime::<chrono::Utc>::from_timestamp(self.generated_at, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            self.version,
            self.tech_stack.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n"),
            self.coding_conventions.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n"),
            self.architectural_patterns.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n"),
            self.common_pitfalls.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n"),
            self.preferred_workflows.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n"),
            self.team_preferences.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n"),
        )
    }

    pub fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_markdown())
    }

    pub fn load_from_file(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        // Simple parsing - in production, use a proper markdown parser
        let mut persona = ProjectPersona {
            id: "loaded".to_string(),
            project_name: "Unknown".to_string(),
            tech_stack: vec![],
            coding_conventions: vec![],
            architectural_patterns: vec![],
            common_pitfalls: vec![],
            preferred_workflows: vec![],
            team_preferences: vec![],
            generated_at: Utc::now().timestamp(),
            last_updated: Utc::now().timestamp(),
            version: 1,
        };

        // Parse sections (simplified)
        let mut current_section = "";
        for line in content.lines() {
            if line.starts_with("# Project Persona:") {
                persona.project_name = line.trim_start_matches("# Project Persona:").trim().to_string();
            } else if line.starts_with("## ") {
                current_section = line.trim_start_matches("## ").trim();
            } else if line.starts_with("- ") && !current_section.is_empty() {
                let item = line.trim_start_matches("- ").trim().to_string();
                match current_section {
                    "Tech Stack" => persona.tech_stack.push(item),
                    "Coding Conventions" => persona.coding_conventions.push(item),
                    "Architectural Patterns" => persona.architectural_patterns.push(item),
                    "Common Pitfalls" => persona.common_pitfalls.push(item),
                    "Preferred Workflows" => persona.preferred_workflows.push(item),
                    "Team Preferences" => persona.team_preferences.push(item),
                    _ => {}
                }
            }
        }

        Ok(persona)
    }
}

/// Layer manager - handles promotion between layers
pub struct LayerManager {
    base_path: std::path::PathBuf,
}

impl LayerManager {
    pub fn new(base_path: &Path) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
        }
    }

    /// Promote L0 conversations to L1 atoms (requires LLM distillation)
    pub fn promote_l0_to_l1(
        &self,
        conversations: &[RawConversation],
    ) -> Vec<AtomFact> {
        // In production, this would call LLM to extract facts
        // For now, return empty - implementation requires LLM integration
        tracing::info!("Promoting {} L0 conversations to L1 atoms", conversations.len());
        vec![]
    }

    /// Aggregate L1 atoms into L2 scenarios
    pub fn aggregate_l1_to_l2(
        &self,
        atoms: &[AtomFact],
        project_id: &str,
    ) -> Vec<ScenarioChunk> {
        // Group atoms by category and create scenarios
        let mut scenarios_by_category: std::collections::HashMap<String, Vec<&AtomFact>> = 
            std::collections::HashMap::new();

        for atom in atoms {
            scenarios_by_category
                .entry(atom.category.clone())
                .or_insert_with(Vec::new)
                .push(atom);
        }

        scenarios_by_category
            .into_iter()
            .enumerate()
            .map(|(i, (category, atoms_in_cat))| {
                ScenarioChunk {
                    id: format!("scenario_{}_{}", project_id, i),
                    scenario_name: format!("{} Pattern", category.replace('_', " ").to_uppercase()),
                    description: format!("Common patterns in {} for this project", category),
                    related_atoms: atoms_in_cat.iter().map(|a| a.id.clone()).collect(),
                    common_patterns: atoms_in_cat.iter().map(|a| a.fact.clone()).collect(),
                    applicable_tools: vec![],
                    timestamp: Utc::now().timestamp(),
                    project_id: Some(project_id.to_string()),
                    usage_count: 0,
                }
            })
            .collect()
    }

    /// Distill L2 scenarios into L3 project persona
    pub fn distill_l2_to_l3(
        &self,
        scenarios: &[ScenarioChunk],
        project_name: &str,
    ) -> ProjectPersona {
        let mut persona = ProjectPersona {
            id: format!("persona_{}", project_name.replace(' ', "_").to_lowercase()),
            project_name: project_name.to_string(),
            tech_stack: vec![],
            coding_conventions: vec![],
            architectural_patterns: vec![],
            common_pitfalls: vec![],
            preferred_workflows: vec![],
            team_preferences: vec![],
            generated_at: Utc::now().timestamp(),
            last_updated: Utc::now().timestamp(),
            version: 1,
        };

        // Extract information from scenarios
        for scenario in scenarios {
            if scenario.scenario_name.contains("TECH") {
                persona.tech_stack.extend(scenario.common_patterns.clone());
            } else if scenario.scenario_name.contains("CODING") {
                persona.coding_conventions.extend(scenario.common_patterns.clone());
            } else if scenario.scenario_name.contains("ARCHITECTURE") {
                persona.architectural_patterns.extend(scenario.common_patterns.clone());
            } else if scenario.scenario_name.contains("PITFALL") {
                persona.common_pitfalls.extend(scenario.common_patterns.clone());
            } else if scenario.scenario_name.contains("WORKFLOW") {
                persona.preferred_workflows.extend(scenario.common_patterns.clone());
            }
        }

        persona
    }

    /// Save L3 persona to white-box Markdown file
    pub fn save_persona_whitebox(&self, persona: &ProjectPersona) -> std::io::Result<std::path::PathBuf> {
        let personas_dir = self.base_path.join(".ox").join("personas");
        std::fs::create_dir_all(&personas_dir)?;
        
        let filename = format!("{}.md", persona.id);
        let path = personas_dir.join(&filename);
        
        persona.save_to_file(&path)?;
        tracing::info!("Saved project persona to: {}", path.display());
        
        Ok(path)
    }

    /// Load L3 persona from white-box Markdown file
    pub fn load_persona_whitebox(&self, project_id: &str) -> std::io::Result<Option<ProjectPersona>> {
        let personas_dir = self.base_path.join(".ox").join("personas");
        let filename = format!("persona_{}.md", project_id.replace(' ', "_").to_lowercase());
        let path = personas_dir.join(&filename);
        
        if path.exists() {
            ProjectPersona::load_from_file(&path).map(Some)
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_conversion() {
        let conv = RawConversation {
            id: "test_1".to_string(),
            session_id: "sess_1".to_string(),
            user_message: "How do I write a Rust function?".to_string(),
            assistant_response: "Use fn keyword...".to_string(),
            timestamp: Utc::now().timestamp(),
            project_id: Some("test_project".to_string()),
            tools_used: vec!["file_write".to_string()],
            success: true,
        };

        let node = conv.to_memory_node();
        assert_eq!(node.depth, MemoryLayer::L0RawConversation.default_depth());
        assert!(node.content.contains("User:"));
        assert!(node.content.contains("Assistant:"));
    }

    #[test]
    fn test_persona_markdown_roundtrip() {
        let persona = ProjectPersona {
            id: "test_proj".to_string(),
            project_name: "Test Project".to_string(),
            tech_stack: vec!["Rust".to_string(), "Tokio".to_string()],
            coding_conventions: vec!["Use snake_case".to_string()],
            architectural_patterns: vec!["Repository pattern".to_string()],
            common_pitfalls: vec![],
            preferred_workflows: vec![],
            team_preferences: vec![],
            generated_at: Utc::now().timestamp(),
            last_updated: Utc::now().timestamp(),
            version: 1,
        };

        let md = persona.to_markdown();
        assert!(md.contains("Test Project"));
        assert!(md.contains("Rust"));
        assert!(md.contains("snake_case"));
    }
}
