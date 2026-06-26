//! Skill loading policy — three tiers (system / global / project) and mandatory injection.

use super::{Skill, SkillScope, skill_applies_to_phase};

pub const OUTPUT_DISCIPLINE_SKILL_ID: &str = "ox-output-discipline";

/// System skills always relevant during agent turns (listed first in on-demand manifest).
pub fn priority_system_skill_ids() -> &'static [&'static str] {
    &[
        OUTPUT_DISCIPLINE_SKILL_ID,
        "ox-systemic-comprehension",
        "concise-direct",
        "coding-principles",
    ]
}
pub const PROJECT_CONVENTIONS: &str = "project-conventions";
pub const PROJECT_BUSINESS: &str = "project-business-guide";
pub const PROJECT_ARCHITECTURE_LEGACY: &str = "project-architecture";

/// Project skills that must be injected in Plan + Execute when present.
pub fn mandatory_project_skill_ids() -> &'static [&'static str] {
    &[PROJECT_CONVENTIONS, PROJECT_BUSINESS]
}

pub fn is_mandatory_project_skill(id: &str) -> bool {
    id == PROJECT_CONVENTIONS || id == PROJECT_BUSINESS
}

/// Find a loaded skill by id, with legacy architecture as fallback for business guide.
fn find_mandatory_skill<'a>(skills: &'a [Skill], id: &str) -> Option<&'a Skill> {
    if id == PROJECT_BUSINESS {
        skills
            .iter()
            .find(|s| s.id == PROJECT_BUSINESS)
            .or_else(|| skills.iter().find(|s| s.id == PROJECT_ARCHITECTURE_LEGACY))
    } else {
        skills.iter().find(|s| s.id == id)
    }
}

/// Full-text block injected at Plan / Execute (project conventions + business guide).
pub fn build_mandatory_injection(skills: &[Skill]) -> Option<String> {
    let mut parts = Vec::new();
    for id in mandatory_project_skill_ids() {
        if let Some(skill) = find_mandatory_skill(skills, id) {
            parts.push(format!(
                "### `{}` [{} — 必读]\n{}\n",
                skill.id,
                skill.scope,
                skill.content.trim()
            ));
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "【项目 Skill — 必读（Plan/Execute 已自动注入，遵守后再改代码）】\n\
         以下内容是本项目规范与业务指导，**优先级高于通用编程习惯**。\n\n\
         {}",
        parts.join("\n")
    ))
}

/// Manifest for skills loaded on demand via `load_skill`.
pub fn build_on_demand_manifest(skills: &[Skill]) -> Option<String> {
    let on_demand: Vec<_> = skills
        .iter()
        .filter(|s| !is_mandatory_project_skill(&s.id) && s.id != PROJECT_ARCHITECTURE_LEGACY)
        .collect();
    if on_demand.is_empty() {
        return None;
    }

    let mut system = Vec::new();
    let mut global = Vec::new();
    let mut project = Vec::new();
    for s in on_demand {
        let phase_tag = if s.phases.is_empty() {
            String::new()
        } else {
            format!(" [{}]", s.phases.join(","))
        };
        let line = format!("- `{}`{}: {}", s.id, phase_tag, s.description.trim());
        match s.scope {
            SkillScope::System => system.push(line),
            SkillScope::Global => global.push(line),
            SkillScope::Project => project.push(line),
        }
    }

    let mut out = String::from("【Skill — load_skill(name) 加载完整手册】\n");
    if !system.is_empty() {
        out.push_str("\n**内置**（~ox-core/skill/builtin，跨项目通用原则）\n");
        for l in system {
            out.push_str(&l);
            out.push('\n');
        }
    }
    if !global.is_empty() {
        out.push_str("\n**全局**（~/.ox/skills/，你的跨项目习惯）\n");
        for l in global {
            out.push_str(&l);
            out.push('\n');
        }
    }
    if !project.is_empty() {
        out.push_str("\n**项目扩展**（.ox/skills/，本项目其他专题）\n");
        for l in project {
            out.push_str(&l);
            out.push('\n');
        }
    }
    Some(out)
}

pub const SKILL_ROUTE_TAG: &str = "[SKILL_ROUTE]";

/// Phase-filtered skill manifest for per-iteration injection.
pub fn build_skill_route(skills: &[Skill], phase: &str) -> Option<String> {
    let mut mandatory = Vec::new();
    let mut on_demand = Vec::new();
    for s in skills {
        if !skill_applies_to_phase(s, phase) {
            continue;
        }
        let line = format!("- `{}` [{}]: {}", s.id, s.scope, s.description.trim());
        if is_mandatory_project_skill(&s.id) {
            mandatory.push(line);
        } else if s.id != PROJECT_ARCHITECTURE_LEGACY {
            on_demand.push(line);
        }
    }
    if mandatory.is_empty() && on_demand.is_empty() {
        return None;
    }
    let mut out = format!(
        "{SKILL_ROUTE_TAG}\nphase={phase} | 必读已注入 system prompt 的 project skill 除外\n"
    );
    if !mandatory.is_empty() {
        out.push_str(&format!("本阶段必读: {}\n", mandatory.join("\n")));
    }
    if !on_demand.is_empty() {
        out.push_str(&format!("按需 load_skill:\n{}\n", on_demand.join("\n")));
    }
    Some(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn skill(id: &str, scope: SkillScope, body: &str) -> Skill {
        Skill {
            id: id.to_string(),
            name: id.to_string(),
            description: "d".into(),
            content: body.into(),
            scope,
            created_at: Utc::now(),
            phases: Vec::new(),
        }
    }

    #[test]
    fn mandatory_injection_includes_both_project_skills() {
        let skills = vec![
            skill(PROJECT_CONVENTIONS, SkillScope::Project, "use cargo fmt"),
            skill(PROJECT_BUSINESS, SkillScope::Project, "Ox is a CLI agent"),
        ];
        let block = build_mandatory_injection(&skills).unwrap();
        assert!(block.contains("cargo fmt"));
        assert!(block.contains("CLI agent"));
    }

    #[test]
    fn legacy_architecture_substitutes_business() {
        let skills = vec![
            skill(PROJECT_CONVENTIONS, SkillScope::Project, "rules"),
            skill(PROJECT_ARCHITECTURE_LEGACY, SkillScope::Project, "domain"),
        ];
        let block = build_mandatory_injection(&skills).unwrap();
        assert!(block.contains("domain"));
    }

    #[test]
    fn manifest_excludes_mandatory_project_skills() {
        let skills = vec![
            skill(PROJECT_CONVENTIONS, SkillScope::Project, "x"),
            skill("coding-principles", SkillScope::System, "y"),
        ];
        let block = build_on_demand_manifest(&skills).unwrap();
        assert!(block.contains("coding-principles"));
        assert!(!block.contains("project-conventions"));
    }
}
