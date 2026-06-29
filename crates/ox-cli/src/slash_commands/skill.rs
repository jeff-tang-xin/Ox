use crate::slash_commands::CommandMeta;
use crate::terminal::app::App as AppState;
use ox_core::message::Session;
use ox_core::runtime::RuntimeEnvironment;
use std::path::PathBuf;

/// /skill 命令元数据
pub const SKILL_COMMAND: CommandMeta = CommandMeta {
    name: "skill",
    aliases: &["skills"],
    description: "Manage Skills (reusable patterns and best practices)",
    handler: handle_skill_command,
};

/// 处理 /skill 命令
pub fn handle_skill_command(
    app: &mut AppState,
    args: &str,
    _session: &mut Session,
    rt_env: &mut RuntimeEnvironment,
    _config: &ox_core::config::OxConfig,
    _cost_tracker: &mut ox_core::cost::CostTracker,
    _trust_manager: &std::sync::Arc<std::sync::Mutex<ox_core::safety::TrustManager>>,
) -> crate::slash_commands::CommandResult {
    let parts: Vec<&str> = args.split_whitespace().collect();

    if parts.is_empty() {
        show_skill_help(app);
        return crate::slash_commands::CommandResult::Success;
    }

    match parts[0] {
        "list" => {
            handle_skill_list(app, rt_env);
            crate::slash_commands::CommandResult::Success
        }
        "show" => {
            if parts.len() < 2 {
                app.output.push_system("Usage: /skill show <id>");
            } else {
                handle_skill_show(app, parts[1], rt_env);
            }
            crate::slash_commands::CommandResult::Success
        }
        "create" => {
            if parts.len() < 2 {
                app.output
                    .push_system("Usage: /skill create <id> [description]");
            } else {
                let id = parts[1];
                let desc = if parts.len() > 2 {
                    // 使用安全的字符边界检查
                    parts
                        .get(2..)
                        .map(|p| p.join(" "))
                        .unwrap_or_else(|| "Custom skill".to_string())
                } else {
                    "Custom skill".to_string()
                };
                handle_skill_create(app, id, &desc, rt_env);
            }
            crate::slash_commands::CommandResult::Success
        }
        "create-llm" => {
            if parts.len() < 2 {
                app.output
                    .push_system("Usage: /skill create-llm <description>");
                app.output
                    .push_system("Example: /skill create-llm Rust error handling best practices");
                crate::slash_commands::CommandResult::Success
            } else {
                let desc = parts.get(1..).map(|p| p.join(" ")).unwrap_or_default();
                handle_skill_create_llm(app, &desc, rt_env)
            }
        }
        "reflect" => {
            handle_skill_reflect(app, rt_env);
            crate::slash_commands::CommandResult::Success
        }
        "delete" => {
            if parts.len() < 2 {
                app.output.push_system("Usage: /skill delete <id>");
            } else {
                handle_skill_delete(app, parts[1], rt_env);
            }
            crate::slash_commands::CommandResult::Success
        }
        "audit" => {
            handle_skill_audit(app, rt_env);
            crate::slash_commands::CommandResult::Success
        }
        _ => {
            show_skill_help(app);
            crate::slash_commands::CommandResult::Success
        }
    }
}

/// 显示帮助信息
fn show_skill_help(app: &mut AppState) {
    app.output.push_system(
        "📚 Skill Management Commands:\n\n\
         /skill list                          - List all available skills\n\
         /skill show <id>                     - Show skill content\n\
         /skill create <id> [desc]            - Create skill template\n\
         /skill create-llm <desc>             - Use LLM to generate skill\n\
         /skill reflect                       - Reflect on recent task and create skill\n\
         /skill delete <id>                   - Delete a skill\n\
         /skill audit                         - Audit skills for potential merges\n\n\
         Examples:\n\
         /skill create rust-error-handling \"Best practices for error handling\"\n\
         /skill create-llm Rust async patterns\n\
         /skill reflect\n\
         /skill audit\n\
         /skill show think-before-coding",
    );
}

/// 审计 Skills，检测可能需要合并的相似项
fn handle_skill_audit(app: &mut AppState, rt_env: &RuntimeEnvironment) {
    use ox_core::skill::SkillLoader;

    let loader = SkillLoader::new(
        rt_env.ox_home_dir.join("skills"),
        rt_env.working_dir.join(".ox").join("skills"),
    );

    match loader.load_enabled_skills() {
        Ok(skills) => {
            if skills.is_empty() {
                app.output.push_system("No skills to audit.");
                return;
            }

            // 按 scope 分组
            let mut global_skills = Vec::new();
            let mut project_skills = Vec::new();
            let mut mandatory_skills = Vec::new();

            for skill in &skills {
                let scope_str = match skill.scope {
                    ox_core::skill::SkillScope::System => "system",
                    ox_core::skill::SkillScope::Global => "global",
                    ox_core::skill::SkillScope::Project => "project",
                };
                match scope_str {
                    "global" => global_skills.push(skill),
                    "project" => project_skills.push(skill),
                    "mandatory" => mandatory_skills.push(skill),
                    _ => project_skills.push(skill),
                }
            }

            let mut output = String::new();
            output.push_str("🔍 **Skill Audit Report**\n\n");

            // 统计信息
            output.push_str(&format!(
                "📊 Total: {} skills ({} global, {} project, {} mandatory)\n\n",
                skills.len(),
                global_skills.len(),
                project_skills.len(),
                mandatory_skills.len()
            ));

            // 检测可能的重复/相似
            let similar = find_similar_skills(&skills);
            if !similar.is_empty() {
                output.push_str("⚠️ **可能需要合并的相似 Skills:**\n\n");
                for (s1, s2, reason) in &similar {
                    output.push_str(&format!("  • `{}` ↔ `{}`\n    原因: {}\n\n", s1, s2, reason));
                }
            } else {
                output.push_str("✅ 未发现明显相似的 skills\n\n");
            }

            // 检测超过上限的项目技能
            const MAX_PROJECT_SKILLS: usize = 3;
            if project_skills.len() > MAX_PROJECT_SKILLS {
                output.push_str(&format!(
                    "⚠️ 项目技能超限: {} 个 (上限 {})\n",
                    project_skills.len(),
                    MAX_PROJECT_SKILLS
                ));
                output.push_str("  建议合并到 project-conventions 或 project-business-guide\n\n");
            }

            // 列出所有 skills
            if !global_skills.is_empty() {
                output.push_str("**Global Skills:**\n");
                for s in &global_skills {
                    output.push_str(&format!("  • {} — {}\n", s.id, s.description));
                }
                output.push_str("\n");
            }

            if !project_skills.is_empty() {
                output.push_str("**Project Skills:**\n");
                for s in &project_skills {
                    output.push_str(&format!("  • {} — {}\n", s.id, s.description));
                }
                output.push_str("\n");
            }

            app.output.push_system(&output);
        }
        Err(e) => {
            app.output
                .push_system(&format!("❌ Failed to load skills: {}", e));
        }
    }
}

/// 基于描述相似性检测可能需要合并的 skills
fn find_similar_skills(
    skills: &[ox_core::skill::Skill],
) -> Vec<(String, String, String)> {
    let mut similar = Vec::new();

    // 简单的关键词重叠检测
    for i in 0..skills.len() {
        for j in (i + 1)..skills.len() {
            let s1 = &skills[i];
            let s2 = &skills[j];

            // 提取关键词（简单按空格分词）
            let words1: std::collections::HashSet<String> = s1
                .description
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| s.len() > 2)
                .map(|s| s.to_string())
                .collect();

            let words2: std::collections::HashSet<String> = s2
                .description
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| s.len() > 2)
                .map(|s| s.to_string())
                .collect();

            // 计算交集
            let intersection: std::collections::HashSet<_> =
                words1.intersection(&words2).collect();

            // 如果有 2+ 个共同关键词，认为可能相似
            if intersection.len() >= 2 {
                let overlap_list: Vec<String> = intersection.iter().take(5).map(|s| (*s).to_string()).collect();
                let reason = format!(
                    "描述关键词重叠: {}",
                    overlap_list.join(", ")
                );
                similar.push((s1.id.clone(), s2.id.clone(), reason));
            }

            // 检查 id 是否相似（包含共同单词）
            let id_parts1: std::collections::HashSet<_> = s1
                .id
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| s.len() > 1)
                .collect();
            let id_parts2: std::collections::HashSet<_> = s2
                .id
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| s.len() > 1)
                .collect();

            let id_overlap: std::collections::HashSet<_> =
                id_parts1.intersection(&id_parts2).collect();

            if !id_overlap.is_empty() && id_overlap.len() >= 1 {
                // 已经通过关键词检测了，这里再加一个 id 相似检测
                let overlap_list: Vec<String> = id_overlap.iter().take(3).map(|s| (*s).to_string()).collect();
                let reason = format!(
                    "ID 相似: 都包含 '{}'",
                    overlap_list.join("', '")
                );
                // 检查是否已经添加过
                let already_added = similar.iter().any(|(a, b, _)| {
                    (a == &s1.id && b == &s2.id) || (a == &s2.id && b == &s1.id)
                });
                if !already_added {
                    similar.push((s1.id.clone(), s2.id.clone(), reason));
                }
            }
        }
    }

    similar
}

/// 列出所有 Skills
fn handle_skill_list(app: &mut AppState, rt_env: &RuntimeEnvironment) {
    use ox_core::skill::SkillLoader;

    let loader = SkillLoader::new(
        rt_env.ox_home_dir.join("skills"),
        rt_env.working_dir.join(".ox").join("skills"),
    );

    match loader.load_enabled_skills() {
        Ok(skills) => {
            if skills.is_empty() {
                app.output.push_system("No skills available.");
                return;
            }

            let skills = loader.load_enabled_skills().unwrap_or_default();
            let summary = skills
                .iter()
                .map(|s| format!("  - {} [{}] - {}", s.id, s.scope, s.description))
                .collect::<Vec<_>>()
                .join("\n");

            app.output.push_system(&format!(
                "📚 Available Skills ({} total):\n\n{}",
                skills.len(),
                summary
            ));
        }
        Err(e) => {
            app.output
                .push_system(&format!("❌ Failed to load skills: {}", e));
        }
    }
}

/// 显示 Skill 详情
fn handle_skill_show(app: &mut AppState, id: &str, rt_env: &RuntimeEnvironment) {
    let skill_path = find_skill_file(id, rt_env);

    match skill_path.and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(content) => {
            app.output
                .push_system(&format!("📄 Skill: {}\n\n{}", id, content));
        }
        None => {
            app.output
                .push_system(&format!("❌ Skill not found: {}", id));
        }
    }
}

/// 创建 Skill 模板
fn handle_skill_create(app: &mut AppState, id: &str, desc: &str, rt_env: &mut RuntimeEnvironment) {
    let skills_dir = rt_env.working_dir.join(".ox").join("skills");

    // 创建目录
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        app.output
            .push_system(&format!("❌ Failed to create skills directory: {}", e));
        return;
    }

    let skill_path = skills_dir.join(format!("{}.md", id));

    // 检查是否已存在
    if skill_path.exists() {
        app.output.push_system(&format!(
            "❌ Skill already exists: {}\n\
             • 编辑：直接改 {}\n\
             • 追加：在 Agent 中用 file_write + \"merge\": true\n\
             • 删除后重建：/skill delete {}",
            id,
            skill_path.display(),
            id
        ));
        return;
    }

    // 生成模板
    let name = id.replace('-', " ");
    let template = format!(
        "---\n\
         name: {}\n\
         description: {}\n\
         ---\n\n\
         # {}\n\n\
         ## When to Use\n\
         - \n\n\
         ## Steps\n\
         1. \n\n\
         ## Anti-patterns\n\
         - \n\n\
         ## Example\n\
         ```\n\
         // Add your example here\n\
         ```\n",
        id,
        desc,
        name.chars()
            .next()
            .unwrap_or(' ')
            .to_uppercase()
            .collect::<String>()
            + name.get(1..).unwrap_or("")
    );

    // 保存文件
    match std::fs::write(&skill_path, &template) {
        Ok(_) => {
            app.output.push_system(&format!(
                "✅ Created Skill template: {}\n\nEdit the file to customize:\n{}\n\nNote: Skills will be reloaded on next agent run.",
                id,
                skill_path.display()
            ));
        }
        Err(e) => {
            app.output
                .push_system(&format!("❌ Failed to create Skill: {}", e));
        }
    }
}

/// 删除 Skill
fn handle_skill_delete(app: &mut AppState, id: &str, rt_env: &RuntimeEnvironment) {
    let skill_path = find_skill_file(id, rt_env);

    match skill_path {
        Some(path) => {
            if let Err(e) = std::fs::remove_file(&path) {
                app.output
                    .push_system(&format!("❌ Failed to delete: {}", e));
            } else {
                app.output.push_system(&format!(
                    "🗑️ Deleted Skill: {}\n\nNote: Skills will be reloaded on next agent run.",
                    id
                ));
            }
        }
        None => {
            app.output
                .push_system(&format!("❌ Skill not found: {}", id));
        }
    }
}

/// 使用 LLM 辅助创建 Skill
fn handle_skill_create_llm(
    app: &mut AppState,
    description: &str,
    _rt_env: &RuntimeEnvironment,
) -> crate::slash_commands::CommandResult {
    use ox_core::context::SKILL_CREATION_PROMPT;

    // 构建 prompt
    let prompt = SKILL_CREATION_PROMPT.replace("{task_description}", description);

    // 显示提示信息
    app.output.push_system(&format!(
        "🤖 Generating Skill for: {}\n\nPlease wait for LLM to generate content...",
        description
    ));

    // 返回 LLM 请求，由主流程处理
    crate::slash_commands::CommandResult::LlmRequest {
        prompt,
        description: format!("Create skill: {}", description),
        skip_workflow: false,
    }
}

/// 反思最近的任务并创建 Skill
fn handle_skill_reflect(app: &mut AppState, _rt_env: &RuntimeEnvironment) {
    use ox_core::context::SKILL_CREATION_PROMPT;

    app.output
        .push_system("🤔 Analyzing conversation context for reusable patterns...");

    // TODO: 完整的 LLM 集成逻辑
    //
    // 设计原则：让 LLM 自主决定需要什么上下文
    //
    // 方案 A: 提供 session 历史，让 LLM 自己分析
    //   - 优点：简单直接
    //   - 缺点：可能 token 过多
    //
    // 方案 B: 先调用 memory_search 获取相关记忆，再让 LLM 总结
    //   - 优点：更精准，token 更少
    //   - 缺点：需要额外调用
    //
    // 推荐：方案 A（简单优先），如果 token 超限再考虑方案 B

    let task_description = "Review the conversation context and identify any reusable patterns, best practices, or lessons learned that could be valuable as a Skill. Focus on what worked well and could help in future similar tasks.";

    let prompt = SKILL_CREATION_PROMPT.replace("{task_description}", task_description);

    // 4. 调用 LLM (需要访问 llm_provider 和 session)
    // 5. 解析返回的 Markdown 内容
    // 6. 提取 Skill ID、name、description
    // 7. 保存到 .ox/skills/

    app.output.push_system(&format!(
        "⚠️ Reflection logic pending full implementation.\n\
         Prompt template ready (first 5 lines):\n\
         {}\n\
         Next step: Integrate with LlmProvider to generate Skill content.",
        prompt.lines().take(5).collect::<Vec<_>>().join("\n")
    ));
}

/// 查找 Skill 文件（优先项目级，其次全局级）
fn find_skill_file(id: &str, rt_env: &RuntimeEnvironment) -> Option<PathBuf> {
    let filename = format!("{}.md", id);

    // 先查项目级
    let project_path = rt_env
        .working_dir
        .join(".ox")
        .join("skills")
        .join(&filename);
    if project_path.exists() {
        return Some(project_path);
    }

    // 再查全局级
    let global_path = rt_env.ox_home_dir.join("skills").join(&filename);
    if global_path.exists() {
        return Some(global_path);
    }

    None
}
