use std::path::{Path, PathBuf};
use chrono;

pub mod generation;  // 🆕 Skill generation layering

/// Skill 的作用域
#[derive(Debug, Clone, PartialEq)]
pub enum SkillScope {
    System,   // 系统级（硬编码）
    Global,   // 用户全局级 (~/.ox/skills/)
    Project,  // 项目级 (.ox/skills/)
}

impl std::fmt::Display for SkillScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillScope::System => write!(f, "system"),
            SkillScope::Global => write!(f, "global"),
            SkillScope::Project => write!(f, "project"),
        }
    }
}

/// Skill 数据结构
#[derive(Debug, Clone)]
pub struct Skill {
    /// Skill 唯一标识符（文件名不含 .md）
    pub id: String,
    
    /// 显示名称
    pub name: String,
    
    /// 简短描述（用于展示给 LLM）
    pub description: String,
    
    /// 详细内容（完整的 Markdown 内容）
    pub content: String,
    
    /// 来源层级
    pub scope: SkillScope,
    
    /// 创建时间
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Skill {
    /// 将 Skill 转换为 Markdown 字符串（用于保存）
    pub fn to_markdown(&self) -> String {
        self.content.clone()
    }
}

/// Skill 加载器
pub struct SkillLoader {
    global_skills_dir: PathBuf,
    project_skills_dir: PathBuf,
}

impl SkillLoader {
    pub fn new(global_dir: PathBuf, project_dir: PathBuf) -> Self {
        Self {
            global_skills_dir: global_dir,
            project_skills_dir: project_dir,
        }
    }
    
    /// 加载所有启用的 Skills（最多 10 个）
    pub fn load_enabled_skills(&self) -> anyhow::Result<Vec<Skill>> {
        let mut selected_skills = Vec::new();
        
        // 1. 始终加载系统级 Skills（核心原则，不可省略）
        let system_skills = get_system_skills();
        selected_skills.extend(system_skills);
        
        // 2. 收集所有用户 Skills（项目级 + 全局级）
        let mut user_skills = Vec::new();
        
        if self.project_skills_dir.exists() {
            user_skills.extend(
                self.load_skills_from_dir(&self.project_skills_dir, SkillScope::Project)?
            );
        }
        
        if self.global_skills_dir.exists() {
            user_skills.extend(
                self.load_skills_from_dir(&self.global_skills_dir, SkillScope::Global)?
            );
        }
        
        // 3. 用户 Skills 按优先级排序（项目级 > 全局级）
        user_skills.sort_by(|a, b| {
            let priority_a = match a.scope {
                SkillScope::Project => 2,
                SkillScope::Global => 1,
                SkillScope::System => 0,
            };
            let priority_b = match b.scope {
                SkillScope::Project => 2,
                SkillScope::Global => 1,
                SkillScope::System => 0,
            };
            priority_b.cmp(&priority_a)
        });
        
        // 4. 计算剩余槽位（最多 10 个 - 系统级数量）
        let remaining_slots = 10 - selected_skills.len();
        
        // 5. 填充用户 Skills
        selected_skills.extend(user_skills.into_iter().take(remaining_slots));
        
        Ok(selected_skills)
    }
    
    /// 从目录加载 Skills
    fn load_skills_from_dir(&self, dir: &Path, scope: SkillScope) -> anyhow::Result<Vec<Skill>> {
        let mut skills = Vec::new();
        
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(skill) = self.parse_skill_file(&path, scope.clone()) {
                    skills.push(skill);
                }
            }
        }
        
        Ok(skills)
    }
    
    /// 解析纯 Markdown Skill 文件
    fn parse_skill_file(&self, path: &Path, scope: SkillScope) -> anyhow::Result<Skill> {
        let content = std::fs::read_to_string(path)?;
        
        // 解析 YAML frontmatter
        let (metadata, body) = parse_frontmatter(&content)?;
        
        let id = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        
        // 从 frontmatter 提取 name 和 description
        let name = metadata.get("name")
            .cloned()
            .unwrap_or_else(|| id.replace('-', " ").to_string());
        
        let description = metadata.get("description")
            .cloned()
            .unwrap_or_else(|| "No description available".to_string());
        
        Ok(Skill {
            id,
            name,
            description,
            content: body,
            scope,
            created_at: chrono::Utc::now(),
        })
    }
}

/// 获取系统级 Skills（从 builtin 目录加载）
pub fn get_system_skills() -> Vec<Skill> {
    let builtin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/skill/builtin");
    
    if !builtin_dir.exists() {
        tracing::warn!("Builtin skills directory not found: {:?}", builtin_dir);
        return Vec::new();
    }
    
    let mut skills = Vec::new();
    
    for entry in std::fs::read_dir(&builtin_dir).ok().into_iter().flatten() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(skill) = parse_skill_file_static(&path, SkillScope::System) {
                    skills.push(skill);
                }
            }
        }
    }
    
    skills
}

/// 静态解析 Skill 文件（不依赖 SkillLoader）
fn parse_skill_file_static(path: &Path, scope: SkillScope) -> anyhow::Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    
    // 解析 YAML frontmatter
    let (metadata, body) = parse_frontmatter(&content)?;
    
    let id = path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    
    // 从 frontmatter 提取 name 和 description
    let name = metadata.get("name")
        .cloned()
        .unwrap_or_else(|| id.replace('-', " ").to_string());
    
    let description = metadata.get("description")
        .cloned()
        .unwrap_or_else(|| "No description available".to_string());
    
    Ok(Skill {
        id,
        name,
        description,
        content: body,
        scope,
        created_at: chrono::Utc::now(),
    })
}

/// 解析 YAML frontmatter
fn parse_frontmatter(content: &str) -> anyhow::Result<(std::collections::HashMap<String, String>, String)> {
    if !content.starts_with("---") {
        // 没有 frontmatter，返回空元数据和完整内容
        return Ok((std::collections::HashMap::new(), content.to_string()));
    }
    
    // 查找结束标记
    let end_marker = content[3..].find("---")
        .ok_or_else(|| anyhow::anyhow!("Invalid frontmatter: missing closing ---"))?;
    
    let yaml_content = &content[3..end_marker + 3];
    let body = content[end_marker + 6..].trim().to_string();
    
    // 简单 YAML 解析（只支持 key: value 格式）
    let mut metadata = std::collections::HashMap::new();
    for line in yaml_content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            metadata.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    
    Ok((metadata, body))
}
