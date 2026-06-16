//! Multi-round Skill reflection buffer — saves each workflow's draft to disk and
//! aggregates 5–10 rounds before prompting the user to confirm a save.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::skill::dedup::merge_skill_markdown;

const BUFFER_REL: &str = ".ox/skill-reflect-buffer.json";
const DRAFTS_DIR: &str = ".ox/skills/.drafts";
pub const DEFAULT_REFLECT_THRESHOLD: usize = 7;
pub const MIN_REFLECT_THRESHOLD: usize = 5;
pub const MAX_REFLECT_THRESHOLD: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectRound {
    pub task: String,
    pub skill_id: String,
    pub content: String,
    pub description: String,
    pub saved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillReflectBuffer {
    pub threshold: usize,
    pub rounds: Vec<ReflectRound>,
}

impl Default for SkillReflectBuffer {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_REFLECT_THRESHOLD,
            rounds: Vec::new(),
        }
    }
}

impl SkillReflectBuffer {
    pub fn clamp_threshold(n: usize) -> usize {
        n.clamp(MIN_REFLECT_THRESHOLD, MAX_REFLECT_THRESHOLD)
    }

    pub fn buffer_path(project_root: &Path) -> PathBuf {
        project_root.join(BUFFER_REL)
    }

    pub fn load(project_root: &Path, threshold: usize) -> Self {
        let threshold = Self::clamp_threshold(threshold);
        let path = Self::buffer_path(project_root);
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(mut buf) = serde_json::from_str::<Self>(&data) {
                buf.threshold = threshold;
                return buf;
            }
        }
        Self {
            threshold,
            rounds: Vec::new(),
        }
    }

    pub fn save(&self, project_root: &Path) -> Result<()> {
        if let Some(parent) = Self::buffer_path(project_root).parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(Self::buffer_path(project_root), json)?;
        Ok(())
    }

    pub fn clear_disk(project_root: &Path) -> Result<()> {
        let path = Self::buffer_path(project_root);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn round_count(&self) -> usize {
        self.rounds.len()
    }

    /// Append one reflection round, persist buffer + per-round draft file.
    /// Returns `(round_index, ready_for_review)`.
    pub fn append_round(
        &mut self,
        project_root: &Path,
        task: &str,
        skill_id: &str,
        content: &str,
        description: &str,
    ) -> Result<(usize, bool)> {
        let round = ReflectRound {
            task: task.to_string(),
            skill_id: skill_id.to_string(),
            content: content.to_string(),
            description: description.to_string(),
            saved_at: Utc::now(),
        };
        let idx = self.rounds.len() + 1;
        SkillReflectBuffer::save_round_draft(project_root, &round, idx)?;
        self.rounds.push(round);
        self.save(project_root)?;
        let ready = self.rounds.len() >= self.threshold;
        Ok((self.rounds.len(), ready))
    }

    fn save_round_draft(project_root: &Path, round: &ReflectRound, idx: usize) -> Result<()> {
        let drafts_dir = project_root.join(DRAFTS_DIR);
        fs::create_dir_all(&drafts_dir)?;
        let path = drafts_dir.join(format!("reflect-round-{idx:03}.md"));
        fs::write(&path, &round.content)?;
        tracing::info!("[SKILL-REFLECT] Round draft saved: {}", path.display());
        Ok(())
    }

    /// Merge all buffered rounds into one skill draft for user confirmation.
    pub fn build_merged_draft(&self) -> (String, String, String) {
        if self.rounds.is_empty() {
            return (
                "project-learnings".into(),
                String::new(),
                "Empty reflect buffer".into(),
            );
        }

        let skill_id = dominant_skill_id(&self.rounds);
        let mut merged = self.rounds[0].content.clone();
        for round in self.rounds.iter().skip(1) {
            merged = merge_skill_markdown(&merged, &round.content);
        }

        let description = format!(
            "聚合 {} 轮任务反思（{} …）",
            self.rounds.len(),
            self.rounds
                .last()
                .map(|r| r.task.chars().take(40).collect::<String>())
                .unwrap_or_default()
        );

        (skill_id, merged, description)
    }

    pub fn clear(&mut self, project_root: &Path) -> Result<()> {
        self.rounds.clear();
        self.save(project_root)
    }
}

fn dominant_skill_id(rounds: &[ReflectRound]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for r in rounds {
        *counts.entry(r.skill_id.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(id, _)| id.to_string())
        .unwrap_or_else(|| "project-learnings".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_root() -> PathBuf {
        let dir = env::temp_dir().join(format!("ox-reflect-{}", uuid_simple()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid_simple() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[test]
    fn append_and_merge() {
        let root = temp_root();
        let mut buf = SkillReflectBuffer::load(&root, 5);
        let c1 = "---\nname: a\ndescription: d\nscope: project\n---\n\nBody A";
        let c2 = "---\nname: b\ndescription: d\nscope: project\n---\n\nBody B";
        for i in 1..=4 {
            let (_, ready) = buf
                .append_round(&root, &format!("task{i}"), "foo", c1, "d1")
                .unwrap();
            assert!(!ready, "round {i} should not be ready yet");
        }
        let (n, ready) = buf
            .append_round(&root, "task5", "foo", c2, "d2")
            .unwrap();
        assert_eq!(n, 5);
        assert!(ready);
        let (id, merged, desc) = buf.build_merged_draft();
        assert_eq!(id, "foo");
        assert!(merged.contains("Body A"));
        assert!(merged.contains("Body B"));
        assert!(desc.contains('5'));
        let _ = fs::remove_dir_all(root);
    }
}
