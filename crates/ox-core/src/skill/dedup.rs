//! Skill deduplication — canonical names, alias redirect, merge-on-update.

use super::policy::{PROJECT_ARCHITECTURE_LEGACY, PROJECT_BUSINESS, PROJECT_CONVENTIONS};
use std::path::{Path, PathBuf};

/// Max recommended project extension skills (excluding mandatory two + legacy).
pub const MAX_PROJECT_EXTENSION_SKILLS: usize = 3;

/// Known aliases that should map to `project-conventions`.
const CONVENTIONS_ALIASES: &[&str] = &[
    "project-coding-standards",
    "project-best-practices",
    "coding-standards",
    "project-standards",
];

/// Known aliases that should map to `project-business-guide`.
const BUSINESS_ALIASES: &[&str] = &[
    "project-architecture-patterns",
    "project-patterns",
    "project-domain-guide",
    "business-guide",
];

/// Result of resolving a write to `.ox/skills/*.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillWritePlan {
    /// New file — write as-is.
    CreateNew,
    /// Onboarding / rewrite of canonical mandatory file.
    OverwriteMandatory,
    /// Redirect write to canonical id (alias file must not be created).
    RedirectToCanonical {
        canonical_id: String,
        reason: String,
    },
    /// Append incoming body to existing skill.
    MergeIntoExisting {
        target_path: PathBuf,
        merged_markdown: String,
    },
    /// Block — use edit_file or explicit merge.
    RejectDuplicate { message: String },
}

/// Parse `.ox/skills/{id}.md` from a relative path; None if not a project skill path.
pub fn parse_project_skill_rel_path(rel: &str) -> Option<String> {
    let rel = rel.trim().replace('\\', "/");
    let rel = rel.strip_prefix("./").unwrap_or(&rel);
    if !rel.starts_with(".ox/skills/") || !rel.ends_with(".md") {
        return None;
    }
    let id = rel.strip_prefix(".ox/skills/")?.strip_suffix(".md")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(id.to_string())
}

/// Map alias id → canonical mandatory id, if applicable.
pub fn canonical_mandatory_id(skill_id: &str) -> Option<&'static str> {
    if skill_id == PROJECT_CONVENTIONS {
        return Some(PROJECT_CONVENTIONS);
    }
    if skill_id == PROJECT_BUSINESS || skill_id == PROJECT_ARCHITECTURE_LEGACY {
        return Some(PROJECT_BUSINESS);
    }
    if CONVENTIONS_ALIASES.contains(&skill_id) {
        return Some(PROJECT_CONVENTIONS);
    }
    if BUSINESS_ALIASES.contains(&skill_id) {
        return Some(PROJECT_BUSINESS);
    }
    None
}

pub fn is_mandatory_skill_file(skill_id: &str) -> bool {
    skill_id == PROJECT_CONVENTIONS
        || skill_id == PROJECT_BUSINESS
        || skill_id == PROJECT_ARCHITECTURE_LEGACY
}

/// List project skill ids (filename stems) under `.ox/skills/`.
pub fn list_project_skill_ids(project_root: &Path) -> Vec<String> {
    let dir = project_root.join(".ox").join("skills");
    let mut ids = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                ids.push(stem.to_string());
            }
        }
    }
    ids.sort();
    ids
}

/// Count extension skills (not mandatory canonical names).
pub fn count_extension_skills(ids: &[String]) -> usize {
    ids.iter()
        .filter(|id| {
            !is_mandatory_skill_file(id)
                && canonical_mandatory_id(id).is_none()
                && *id != PROJECT_ARCHITECTURE_LEGACY
        })
        .count()
}

/// Decide how to handle `file_write` to a project skill path.
pub fn plan_skill_write(
    project_root: &Path,
    skill_id: &str,
    new_content: &str,
    allow_merge: bool,
    onboarding_turn: bool,
) -> SkillWritePlan {
    let skills_dir = project_root.join(".ox").join("skills");

    // Alias → canonical mandatory file
    if let Some(canonical) = canonical_mandatory_id(skill_id)
        && skill_id != canonical
    {
        let canonical_path = skills_dir.join(format!("{canonical}.md"));
        if canonical_path.exists() || onboarding_turn {
            return SkillWritePlan::RedirectToCanonical {
                canonical_id: canonical.to_string(),
                reason: format!(
                    "Skill `{skill_id}` 与必填 Skill `{canonical}` 主题重复，禁止另建文件"
                ),
            };
        }
        // No canonical yet — still redirect name on first create
        return SkillWritePlan::RedirectToCanonical {
            canonical_id: canonical.to_string(),
            reason: format!("请使用标准名称 `{canonical}.md`，不要创建 `{skill_id}.md`"),
        };
    }

    let target = skills_dir.join(format!("{skill_id}.md"));

    if onboarding_turn && is_mandatory_skill_file(skill_id) {
        return SkillWritePlan::OverwriteMandatory;
    }

    if !target.exists() {
        let ids = list_project_skill_ids(project_root);
        if !is_mandatory_skill_file(skill_id)
            && canonical_mandatory_id(skill_id).is_none()
            && count_extension_skills(&ids) >= MAX_PROJECT_EXTENSION_SKILLS
        {
            return SkillWritePlan::RejectDuplicate {
                message: format!(
                    "项目扩展 Skill 已达上限（{MAX_PROJECT_EXTENSION_SKILLS} 个）。\
                     请用 edit_file 合并进已有 Skill，或 /skill delete 后再建。\n\
                     已有：{}",
                    ids.join(", ")
                ),
            };
        }
        return SkillWritePlan::CreateNew;
    }

    // File exists
    if allow_merge && let Ok(existing) = std::fs::read_to_string(&target) {
        let merged = merge_skill_markdown(&existing, new_content);
        return SkillWritePlan::MergeIntoExisting {
            target_path: target,
            merged_markdown: merged,
        };
    }

    if is_mandatory_skill_file(skill_id) {
        return SkillWritePlan::OverwriteMandatory;
    }

    SkillWritePlan::RejectDuplicate {
        message: format!(
            "Skill `{skill_id}` 已存在。禁止重复创建。\n\
             • 小改动：edit_file 更新 `.ox/skills/{skill_id}.md`\n\
             • 追加章节：file_write 同一 path 并设 `\"merge\": true`\n\
             • 主题重复：合并进 project-conventions 或 project-business-guide，不要新建相近 id"
        ),
    }
}

/// Merge new markdown into existing skill (keep original frontmatter).
pub fn merge_skill_markdown(existing: &str, incoming: &str) -> String {
    let (meta, existing_body) = split_frontmatter(existing);
    let (_, incoming_body) = split_frontmatter(incoming);
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let header = meta.unwrap_or_else(|| {
        "---\nname: merged-skill\ndescription: merged\nscope: project\n---".to_string()
    });
    format!(
        "{header}\n\n{}\n\n---\n## 更新 ({date})\n\n{}\n",
        existing_body.trim(),
        incoming_body.trim()
    )
}

fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let content = content.trim();
    if !content.starts_with("---") {
        return (None, content.to_string());
    }
    if let Some(end) = content[3..].find("\n---") {
        let yaml_end = 3 + end + 4;
        let header = content[..yaml_end].trim().to_string();
        let body = content[yaml_end..].trim().to_string();
        (Some(header), body)
    } else {
        (None, content.to_string())
    }
}

/// Build agent-facing dedup rules for system prompt.
pub fn skill_dedup_directive(project_root: &Path) -> Option<String> {
    let ids = list_project_skill_ids(project_root);
    if ids.is_empty() {
        return None;
    }
    Some(format!(
        "【Skill 去重规则】\n\
         • 项目必填（仅两个）：`project-conventions`、`project-business-guide`\n\
         • 禁止创建同义文件（如 project-coding-standards、project-architecture-patterns）\n\
         • 已存在则 edit_file 更新，或 file_write + `\"merge\": true` 追加章节\n\
         • 扩展 Skill 上限 {MAX_PROJECT_EXTENSION_SKILLS} 个（当前已有：{}）",
        ids.join(", ")
    ))
}

/// 检查是否需要提示用户合并相似 skill
/// 返回 Some((similar_skill_id, reason)) 如果有相似的，否则返回 None
pub fn check_similar_skills(
    project_root: &Path,
    new_skill_id: &str,
    new_description: &str,
) -> Option<(String, String)> {
    let skills_dir = project_root.join(".ox").join("skills");
    if !skills_dir.exists() {
        return None;
    }

    // 读取所有现有 skills 的信息
    let mut existing_skills: Vec<(String, String)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md")
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                // 解析 description
                let desc = extract_description(&content);
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if !id.is_empty() {
                    existing_skills.push((id, desc));
                }
            }
        }
    }

    // 检测相似性
    let new_words: std::collections::HashSet<String> = new_description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 2)
        .map(|s| s.to_string())
        .collect();

    for (existing_id, existing_desc) in &existing_skills {
        // 跳过同名
        if existing_id == new_skill_id {
            continue;
        }

        let existing_words: std::collections::HashSet<String> = existing_desc
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() > 2)
            .map(|s| s.to_string())
            .collect();

        let intersection: std::collections::HashSet<_> =
            new_words.intersection(&existing_words).collect();

        // 关键词重叠 >= 2
        if intersection.len() >= 2 {
            let overlap_list: Vec<String> = intersection
                .iter()
                .take(5)
                .map(|s| (*s).to_string())
                .collect();
            return Some((
                existing_id.clone(),
                format!(
                    "描述关键词与 '{}' 重叠: {}",
                    existing_id,
                    overlap_list.join(", ")
                ),
            ));
        }

        // 检查 ID 是否相似
        let new_id_parts: std::collections::HashSet<_> = new_skill_id
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() > 1)
            .collect();
        let existing_id_parts: std::collections::HashSet<_> = existing_id
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() > 1)
            .collect();

        let id_overlap: std::collections::HashSet<_> =
            new_id_parts.intersection(&existing_id_parts).collect();

        if !id_overlap.is_empty() {
            let overlap_list: Vec<String> = id_overlap
                .iter()
                .take(3)
                .map(|s| (*s).to_string())
                .collect();
            return Some((
                existing_id.clone(),
                format!(
                    "ID 与 '{}' 相似，都包含 '{}'",
                    existing_id,
                    overlap_list.join("', '")
                ),
            ));
        }
    }

    None
}

/// 从 skill markdown 中提取 description
fn extract_description(content: &str) -> String {
    // 尝试从 frontmatter 提取
    if let Some(start) = content.find("---")
        && let Some(end) = content[start + 3..].find("---")
    {
        let yaml = &content[start + 3..start + 3 + end];
        for line in yaml.lines() {
            let line = line.trim();
            if line.starts_with("description:") {
                let desc = line.trim_start_matches("description:").trim();
                return desc.trim_matches('"').trim_matches('\'').to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn alias_redirects_to_conventions() {
        assert_eq!(
            canonical_mandatory_id("project-coding-standards"),
            Some(PROJECT_CONVENTIONS)
        );
    }

    #[test]
    fn parse_skill_path() {
        assert_eq!(
            parse_project_skill_rel_path(".ox/skills/foo.md").as_deref(),
            Some("foo")
        );
        assert!(parse_project_skill_rel_path("src/main.rs").is_none());
    }

    #[test]
    fn merge_keeps_frontmatter() {
        let a = "---\nname: a\n---\n\nBody A";
        let b = "---\nname: b\n---\n\nBody B";
        let m = merge_skill_markdown(a, b);
        assert!(m.contains("name: a"));
        assert!(m.contains("Body A"));
        assert!(m.contains("Body B"));
        assert!(m.contains("## 更新"));
    }

    #[test]
    fn reject_duplicate_extension_without_merge() {
        let tmp = std::env::temp_dir().join(format!("ox_dedup_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join(".ox/skills")).unwrap();
        fs::write(tmp.join(".ox/skills/custom.md"), "---\nname: c\n---\n\nold").unwrap();
        let plan = plan_skill_write(&tmp, "custom", "---\nname: c\n---\n\nnew", false, false);
        assert!(matches!(plan, SkillWritePlan::RejectDuplicate { .. }));
        let _ = fs::remove_dir_all(&tmp);
    }
}
