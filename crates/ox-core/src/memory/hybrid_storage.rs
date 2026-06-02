/// Hybrid Storage Manager - SQLite for L0-L1, Markdown for L2-L3
/// 
/// Inspired by TencentDB-Agent-Memory's approach:
/// - SQLite: Fast indexed storage for raw conversations and atom facts (L0-L1)
/// - Markdown: Human-readable white-box files for scenarios and personas (L2-L3)
/// - Bidirectional traceability via node_id references

use std::path::{Path, PathBuf};
use std::fs;
use chrono::Utc;


use super::layering::*;
use super::store::MemoryStore;

/// Hybrid storage manager combining SQLite and Markdown
pub struct HybridStorage {
    /// SQLite store for L0-L1 (fast indexed queries)
    sqlite_store: MemoryStore,
    /// Base directory for Markdown white-box files (L2-L3)
    markdown_dir: PathBuf,
    /// Layer manager for promotions
    layer_manager: LayerManager,
    /// Base path reference
    base_path: PathBuf,
}

impl HybridStorage {
    /// Create new hybrid storage
    pub fn new(base_path: &Path) -> anyhow::Result<Self> {
        let sqlite_path = base_path.join(".ox").join("memory.db");
        let markdown_dir = base_path.join(".ox").join("knowledge");
        
        // Ensure directories exist
        fs::create_dir_all(&markdown_dir)?;
        
        let sqlite_store = MemoryStore::open(&sqlite_path)?;
        let layer_manager = LayerManager::new(base_path);
        
        Ok(Self {
            sqlite_store,
            markdown_dir,
            layer_manager,
            base_path: base_path.to_path_buf(),
        })
    }

    /// Store L0 conversation in SQLite
    pub fn store_l0(&self, conversation: &RawConversation) -> anyhow::Result<()> {
        let node = conversation.to_memory_node();
        self.sqlite_store.insert(&node)?;
        tracing::debug!("Stored L0 conversation in SQLite: {}", conversation.id);
        Ok(())
    }

    /// Store L1 atom fact in SQLite
    pub fn store_l1(&self, atom: &AtomFact) -> anyhow::Result<()> {
        let node = atom.to_memory_node();
        self.sqlite_store.insert(&node)?;
        tracing::debug!("Stored L1 atom fact in SQLite: {}", atom.id);
        Ok(())
    }

    /// Store L2 scenario as Markdown white-box file
    pub fn store_l2_markdown(&self, scenario: &ScenarioChunk) -> anyhow::Result<PathBuf> {
        let scenarios_dir = self.markdown_dir.join("scenarios");
        fs::create_dir_all(&scenarios_dir)?;
        
        let filename = format!("{}.md", scenario.id);
        let path = scenarios_dir.join(&filename);
        
        let content = format!(
            "# Scenario: {name}\n\n\
             **ID**: {id}\n\
             **Generated**: {timestamp}\n\
             **Project**: {project}\n\
             **Usage Count**: {usage}\n\n\
             ---\n\n\
             ## Description\n\n{desc}\n\n\
             ## Related Atoms\n\n{atoms}\n\n\
             ## Common Patterns\n\n{patterns}\n\n\
             ## Applicable Tools\n\n{tools}\n",
            name = scenario.scenario_name,
            id = scenario.id,
            timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp(scenario.timestamp, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            project = scenario.project_id.as_deref().unwrap_or("global"),
            usage = scenario.usage_count,
            desc = scenario.description,
            atoms = scenario.related_atoms.iter()
                .map(|id| format!("- `{}`", id))
                .collect::<Vec<_>>()
                .join("\n"),
            patterns = scenario.common_patterns.iter()
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n"),
            tools = scenario.applicable_tools.iter()
                .map(|t| format!("- `{}`", t))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        
        fs::write(&path, content)?;
        tracing::info!("Stored L2 scenario as Markdown: {}", path.display());
        
        Ok(path)
    }

    /// Load L2 scenario from Markdown white-box file
    pub fn load_l2_markdown(&self, scenario_id: &str) -> anyhow::Result<Option<ScenarioChunk>> {
        let scenarios_dir = self.markdown_dir.join("scenarios");
        let filename = format!("{}.md", scenario_id);
        let path = scenarios_dir.join(&filename);
        
        if !path.exists() {
            return Ok(None);
        }
        
        let content = fs::read_to_string(&path)?;
        
        // Simple parsing (in production, use a proper markdown parser)
        let mut scenario = ScenarioChunk {
            id: scenario_id.to_string(),
            scenario_name: "Unknown".to_string(),
            description: String::new(),
            related_atoms: vec![],
            common_patterns: vec![],
            applicable_tools: vec![],
            timestamp: Utc::now().timestamp(),
            project_id: None,
            usage_count: 0,
        };
        
        let mut current_section = "";
        for line in content.lines() {
            if line.starts_with("# Scenario:") {
                scenario.scenario_name = line.trim_start_matches("# Scenario:").trim().to_string();
            } else if line.starts_with("**ID**:") {
                // Already set
            } else if line.starts_with("**Project**:") {
                let proj = line.trim_start_matches("**Project**:").trim();
                if proj != "global" {
                    scenario.project_id = Some(proj.to_string());
                }
            } else if line.starts_with("**Usage Count**:") {
                if let Ok(count) = line.trim_start_matches("**Usage Count**:").trim().parse() {
                    scenario.usage_count = count;
                }
            } else if line.starts_with("## ") {
                current_section = line.trim_start_matches("## ").trim();
            } else if line.starts_with("- ") && !current_section.is_empty() {
                let item = line.trim_start_matches("- ").trim().to_string();
                match current_section {
                    "Related Atoms" => {
                        let atom_id = item.trim_start_matches('`').trim_end_matches('`').to_string();
                        scenario.related_atoms.push(atom_id);
                    }
                    "Common Patterns" => scenario.common_patterns.push(item),
                    "Applicable Tools" => {
                        let tool = item.trim_start_matches('`').trim_end_matches('`').to_string();
                        scenario.applicable_tools.push(tool);
                    }
                    "Description" => {
                        if scenario.description.is_empty() {
                            scenario.description = item;
                        } else {
                            scenario.description.push_str("\n");
                            scenario.description.push_str(&item);
                        }
                    }
                    _ => {}
                }
            }
        }
        
        Ok(Some(scenario))
    }

    /// Store L3 persona as Markdown white-box file (delegates to LayerManager)
    pub fn store_l3_persona(&self, persona: &ProjectPersona) -> anyhow::Result<PathBuf> {
        let path = self.layer_manager.save_persona_whitebox(persona)
            .map_err(|e| anyhow::anyhow!("Failed to save persona: {}", e))?;
        tracing::info!("Stored L3 persona as Markdown: {}", path.display());
        Ok(path)
    }

    /// Load L3 persona from Markdown white-box file (delegates to LayerManager)
    pub fn load_l3_persona(&self, project_id: &str) -> anyhow::Result<Option<ProjectPersona>> {
        self.layer_manager.load_persona_whitebox(project_id)
            .map_err(|e| anyhow::anyhow!("Failed to load persona: {}", e))
    }

    /// Promote L0 → L1 → L2 → L3 with full pipeline
    pub fn promote_full_pipeline(
        &self,
        project_id: &str,
        project_name: &str,
    ) -> anyhow::Result<PromotionReport> {
        tracing::info!("Starting full memory promotion pipeline for project: {}", project_id);
        
        // Step 1: Retrieve L0 conversations from SQLite
        let l0_conversations = self.retrieve_l0_conversations(project_id)?;
        tracing::info!("Retrieved {} L0 conversations", l0_conversations.len());
        
        // Step 2: Promote L0 → L1 (requires LLM in production)
        let l1_atoms = self.layer_manager.promote_l0_to_l1(&l0_conversations);
        tracing::info!("Promoted to {} L1 atoms", l1_atoms.len());
        
        // Store L1 atoms in SQLite
        for atom in &l1_atoms {
            self.store_l1(atom)?;
        }
        
        // Step 3: Aggregate L1 → L2
        let l2_scenarios = self.layer_manager.aggregate_l1_to_l2(&l1_atoms, project_id);
        tracing::info!("Aggregated to {} L2 scenarios", l2_scenarios.len());
        
        // Store L2 scenarios as Markdown
        let mut l2_paths = Vec::new();
        for scenario in &l2_scenarios {
            let path = self.store_l2_markdown(scenario)?;
            l2_paths.push(path);
        }
        
        // Step 4: Distill L2 → L3
        let l3_persona = self.layer_manager.distill_l2_to_l3(&l2_scenarios, project_name);
        tracing::info!("Distilled L3 project persona");
        
        // Store L3 persona as Markdown
        let l3_path = self.store_l3_persona(&l3_persona)?;
        
        Ok(PromotionReport {
            l0_count: l0_conversations.len(),
            l1_count: l1_atoms.len(),
            l2_count: l2_scenarios.len(),
            l3_generated: true,
            l2_paths,
            l3_path,
        })
    }

    /// Retrieve L0 conversations from SQLite for a project
    fn retrieve_l0_conversations(&self, project_id: &str) -> anyhow::Result<Vec<RawConversation>> {
        // In production, query SQLite for L0 nodes
        // For now, return empty - implementation requires extending MemoryStore
        tracing::warn!("retrieve_l0_conversations not yet implemented - returning empty");
        Ok(vec![])
    }

    /// Get storage statistics
    pub fn get_stats(&self) -> anyhow::Result<StorageStats> {
        // Count files in markdown directories
        let scenarios_dir = self.markdown_dir.join("scenarios");
        let personas_dir = self.markdown_dir.join("personas");
        
        let l2_count = if scenarios_dir.exists() {
            fs::read_dir(&scenarios_dir)?.count()
        } else {
            0
        };
        
        let l3_count = if personas_dir.exists() {
            fs::read_dir(&personas_dir)?.count()
        } else {
            0
        };
        
        // L0/L1 counts would come from SQLite (not implemented yet)
        Ok(StorageStats {
            l0_count: 0,
            l1_count: 0,
            l2_count,
            l3_count,
            markdown_dir: self.markdown_dir.clone(),
        })
    }

    /// Get base path reference
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }
}

/// Report from memory promotion pipeline
#[derive(Debug, Clone)]
pub struct PromotionReport {
    pub l0_count: usize,
    pub l1_count: usize,
    pub l2_count: usize,
    pub l3_generated: bool,
    pub l2_paths: Vec<PathBuf>,
    pub l3_path: PathBuf,
}

impl std::fmt::Display for PromotionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Promotion Report:\n\
             - L0 Conversations: {}\n\
             - L1 Atoms: {}\n\
             - L2 Scenarios: {}\n\
             - L3 Persona: {}\n\
             - L2 Files: {}\n\
             - L3 File: {}",
            self.l0_count,
            self.l1_count,
            self.l2_count,
            if self.l3_generated { "Generated" } else { "Not generated" },
            self.l2_paths.len(),
            self.l3_path.display()
        )
    }
}

/// Storage statistics
#[derive(Debug, Clone)]
pub struct StorageStats {
    pub l0_count: usize,
    pub l1_count: usize,
    pub l2_count: usize,
    pub l3_count: usize,
    pub markdown_dir: PathBuf,
}

impl std::fmt::Display for StorageStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Storage Stats:\n\
             - L0 (SQLite): {}\n\
             - L1 (SQLite): {}\n\
             - L2 (Markdown): {}\n\
             - L3 (Markdown): {}\n\
             - Markdown Dir: {}",
            self.l0_count,
            self.l1_count,
            self.l2_count,
            self.l3_count,
            self.markdown_dir.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_hybrid_storage_creation() {
        let temp_dir = TempDir::new().unwrap();
        let storage = HybridStorage::new(temp_dir.path()).unwrap();
        
        assert!(temp_dir.path().join(".ox").join("memory.db").exists());
        assert!(temp_dir.path().join(".ox").join("knowledge").exists());
    }

    #[test]
    fn test_store_and_load_l2_scenario() {
        let temp_dir = TempDir::new().unwrap();
        let storage = HybridStorage::new(temp_dir.path()).unwrap();
        
        let scenario = ScenarioChunk {
            id: "test_scenario".to_string(),
            scenario_name: "Test Scenario".to_string(),
            description: "A test scenario".to_string(),
            related_atoms: vec!["atom1".to_string()],
            common_patterns: vec!["Pattern 1".to_string()],
            applicable_tools: vec!["file_write".to_string()],
            timestamp: Utc::now().timestamp(),
            project_id: Some("test_project".to_string()),
            usage_count: 3,
        };
        
        let path = storage.store_l2_markdown(&scenario).unwrap();
        assert!(path.exists());
        
        let loaded = storage.load_l2_markdown("test_scenario").unwrap().unwrap();
        assert_eq!(loaded.scenario_name, "Test Scenario");
        assert_eq!(loaded.related_atoms.len(), 1);
    }

    #[test]
    fn test_store_and_load_l3_persona() {
        let temp_dir = TempDir::new().unwrap();
        let storage = HybridStorage::new(temp_dir.path()).unwrap();
        
        let persona = ProjectPersona {
            id: "persona_test_proj".to_string(),
            project_name: "Test Project".to_string(),
            tech_stack: vec!["Rust".to_string()],
            coding_conventions: vec!["Use snake_case".to_string()],
            architectural_patterns: vec![],
            common_pitfalls: vec![],
            preferred_workflows: vec![],
            team_preferences: vec![],
            generated_at: Utc::now().timestamp(),
            last_updated: Utc::now().timestamp(),
            version: 1,
        };
        
        let path = storage.store_l3_persona(&persona).unwrap();
        assert!(path.exists());
        
        let loaded = storage.load_l3_persona("test_proj").unwrap().unwrap();
        assert_eq!(loaded.project_name, "Test Project");
        assert_eq!(loaded.tech_stack.len(), 1);
    }
}
