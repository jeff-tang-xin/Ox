//! Onboarding — stub module for API compatibility.
//! Auto skill generation for new projects has been removed.
//! These functions now return defaults / no-ops.

use crate::message::Message;
use crate::runtime::has_project_markers;
use std::path::{Path, PathBuf};

pub const SKILL_CONVENTIONS: &str = "project-conventions.md";
pub const SKILL_BUSINESS: &str = "project-business-guide.md";
pub const SKILL_ARCHITECTURE_LEGACY: &str = "project-architecture.md";

pub fn skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ox").join("skills")
}

pub fn conventions_path(project_root: &Path) -> PathBuf {
    skills_dir(project_root).join(SKILL_CONVENTIONS)
}

pub fn business_guide_path(project_root: &Path) -> PathBuf {
    skills_dir(project_root).join(SKILL_BUSINESS)
}

pub fn legacy_architecture_path(project_root: &Path) -> PathBuf {
    skills_dir(project_root).join(SKILL_ARCHITECTURE_LEGACY)
}

pub fn needs_project_onboarding(project_root: &Path) -> bool {
    let dir = skills_dir(project_root);
    let has_conventions = dir.join(SKILL_CONVENTIONS).is_file();
    let has_business =
        dir.join(SKILL_BUSINESS).is_file() || dir.join(SKILL_ARCHITECTURE_LEGACY).is_file();
    !has_conventions || !has_business
}

pub fn is_greenfield_project(project_root: &Path) -> bool {
    !has_project_markers(project_root)
}

pub fn prepare_project_for_onboarding(project_root: &Path) -> std::io::Result<()> {
    let dir = skills_dir(project_root);
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(())
}

pub fn onboarding_files_complete(project_root: &Path) -> bool {
    conventions_path(project_root).is_file()
        && (business_guide_path(project_root).is_file()
            || legacy_architecture_path(project_root).is_file())
}

pub fn missing_onboarding_files(project_root: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    if !conventions_path(project_root).is_file() {
        missing.push(format!(".ox/skills/{SKILL_CONVENTIONS}"));
    }
    if !business_guide_path(project_root).is_file()
        && !legacy_architecture_path(project_root).is_file()
    {
        missing.push(format!(".ox/skills/{SKILL_BUSINESS}"));
    }
    missing
}

pub fn is_onboarding_turn(_messages: &[Message]) -> bool {
    false
}

pub fn turn_signals_onboarding_done(_new_messages: &[Message]) -> bool {
    false
}

pub fn extract_onboarding_task(messages: &[Message]) -> String {
    messages
        .iter()
        .find_map(|m| {
            if let Message::User { content } = m {
                Some(content.clone())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

pub fn finalize_cli_workflow_after_onboarding(
    _engine: &mut crate::agent::engine::WorkflowEngine,
    _session: &mut crate::message::Session,
    _task: &str,
) -> anyhow::Result<()> {
    Ok(())
}

pub fn onboarding_system_directive(_greenfield: bool) -> String {
    String::new()
}

pub fn build_onboarding_user_prompt(project_root: &Path) -> String {
    format!(
        "项目目录 `{}` 已就绪。请直接开始你的任务。",
        project_root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_onboarding_when_empty() {
        let tmp = std::env::temp_dir().join(format!("ox_onboard_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(skills_dir(&tmp)).unwrap();
        assert!(needs_project_onboarding(&tmp));
        assert!(is_greenfield_project(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prepare_creates_skills_dir() {
        let tmp = std::env::temp_dir().join(format!("ox_scaffold_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        prepare_project_for_onboarding(&tmp).unwrap();
        assert!(skills_dir(&tmp).is_dir());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn onboarding_never_active() {
        assert!(!is_onboarding_turn(&[Message::system("hello")]));
        assert!(!turn_signals_onboarding_done(&[Message::assistant("done")]));
    }

    #[test]
    fn build_prompt_is_minimal() {
        let dir = std::env::temp_dir().join(format!("ox_onboard_prompt_{}", std::process::id()));
        let prompt = build_onboarding_user_prompt(&dir);
        assert!(!prompt.is_empty());
    }
}
