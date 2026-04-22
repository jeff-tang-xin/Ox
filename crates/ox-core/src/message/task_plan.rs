use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Status of a single task item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Blocked,
}

/// A single item in the task plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: u32,
    pub description: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A task plan tracks the agent's current work items.
/// Persisted to `.ox/task_plan.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskPlan {
    pub items: Vec<TaskItem>,
}

impl TaskPlan {
    /// Load from file or return empty plan.
    pub fn load_or_default(ox_dir: &Path) -> Self {
        let path = ox_dir.join("task_plan.json");
        if !path.exists() {
            return Self::default();
        }
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save to file.
    pub fn save(&self, ox_dir: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(ox_dir)?;
        let path = ox_dir.join("task_plan.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Format for display (e.g. /plan command).
    pub fn display(&self) -> String {
        if self.items.is_empty() {
            return "No active task plan.".to_string();
        }
        self.items
            .iter()
            .map(|item| {
                let icon = match item.status {
                    TaskStatus::Pending => " ",
                    TaskStatus::InProgress => ">",
                    TaskStatus::Done => "x",
                    TaskStatus::Blocked => "!",
                };
                format!("[{icon}] {}. {}", item.id, item.description)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
