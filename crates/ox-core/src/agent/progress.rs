use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Workflow progress tracking for a single requirement.
/// Stored in `.ox/<requirement_name>/progress.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowProgress {
    /// Requirement name (e.g., "order-optimization")
    pub requirement_name: String,
    /// Current workflow mode (free/spec/council)
    pub workflow_mode: String,
    /// Current workflow ID
    pub workflow_id: String,
    /// Current step index in the workflow
    pub workflow_step_index: usize,
    /// Last updated timestamp
    pub last_updated: String,
    /// Associated session file path (relative to project root)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
}

impl WorkflowProgress {
    /// Create a new progress tracker
    pub fn new(requirement_name: &str, workflow_mode: &str, workflow_id: &str) -> Self {
        Self {
            requirement_name: requirement_name.to_string(),
            workflow_mode: workflow_mode.to_string(),
            workflow_id: workflow_id.to_string(),
            workflow_step_index: 0,
            last_updated: Utc::now().to_rfc3339(),
            session_file: None,
        }
    }

    /// Load progress from `.ox/spec/<requirement_name>/progress.json`
    pub fn load(project_root: &Path, requirement_name: &str) -> Option<Self> {
        let progress_path = project_root
            .join(".ox")
            .join("spec")
            .join(requirement_name)
            .join("progress.json");

        if !progress_path.exists() {
            return None;
        }

        match fs::read_to_string(&progress_path) {
            Ok(content) => serde_json::from_str(&content).ok(),
            Err(_) => None,
        }
    }

    /// Save progress to `.ox/spec/<requirement_name>/progress.json`
    pub fn save(&self, project_root: &Path) -> anyhow::Result<()> {
        let progress_dir = project_root
            .join(".ox")
            .join("spec")
            .join(&self.requirement_name);
        fs::create_dir_all(&progress_dir)?;

        let progress_path = progress_dir.join("progress.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&progress_path, json)?;

        tracing::info!(
            "Saved workflow progress: {} at step {} ({})",
            self.requirement_name,
            self.workflow_step_index,
            self.workflow_mode
        );

        Ok(())
    }

    /// Update step index and save
    pub fn advance_step(&mut self, project_root: &Path) -> anyhow::Result<()> {
        self.workflow_step_index += 1;
        self.last_updated = Utc::now().to_rfc3339();
        self.save(project_root)
    }

    /// Get the directory path for this requirement
    pub fn get_requirement_dir(&self, project_root: &Path) -> PathBuf {
        project_root.join(".ox").join("spec").join(&self.requirement_name)
    }
}

/// Scan all requirements in `.ox/spec/` directory and return their progress.
pub fn scan_all_progress(project_root: &Path) -> Vec<WorkflowProgress> {
    let spec_dir = project_root.join(".ox").join("spec");
    if !spec_dir.exists() {
        return Vec::new();
    }

    let mut progresses = Vec::new();

    // Scan all subdirectories in .ox/spec/
    if let Ok(entries) = fs::read_dir(&spec_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Check if this directory has a progress.json
            let progress_path = path.join("progress.json");
            if progress_path.exists() {
                if let Ok(content) = fs::read_to_string(&progress_path) {
                    if let Ok(progress) = serde_json::from_str::<WorkflowProgress>(&content) {
                        progresses.push(progress);
                    }
                }
            }
        }
    }

    // Sort by last_updated (most recent first)
    progresses.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));

    progresses
}

/// List all incomplete requirements (workflow_step_index < total_steps)
pub fn list_incomplete_tasks(project_root: &Path, total_steps: usize) -> Vec<WorkflowProgress> {
    scan_all_progress(project_root)
        .into_iter()
        .filter(|p| p.workflow_step_index < total_steps && p.workflow_mode == "spec")
        .collect()
}
