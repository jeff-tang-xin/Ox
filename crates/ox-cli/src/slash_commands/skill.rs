use std::path::PathBuf;
use crate::terminal::app::App as AppState;
use ox_core::runtime::RuntimeEnvironment;
use crate::slash_commands::{CommandResult, CommandMeta};
use ox_core::message::Session;

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
    _memory: &std::sync::Arc<ox_core::memory::MemoryManager>,
    _cost_tracker: &mut ox_core::cost::CostTracker,
    _trust_manager: &std::sync::Arc<std::sync::Mutex<ox_core::safety::TrustManager>>,
) -> CommandResult {
    let parts: Vec<&str> = args.split_whitespace().collect();
    
    if parts.is_empty() {
        show_skill_help(app);
        return CommandResult::Success;
    }
    
    match parts[0] {
        "list" => handle_skill_list(app, rt_env),
        "show" => {
            if parts.len() < 2 {
                app.output.push_system("Usage: /skill show <id>");
            } else {
                handle_skill_show(app, parts[1], rt_env);
            }
        }
        "create" => {
            if parts.len() < 2 {
                app.output.push_system("Usage: /skill create <id> [description]");
            } else {
                let id = parts[1];
                let desc = if parts.len() > 2 {
                    parts[2..].join(" ")
                } else {
                    "Custom skill".to_string()
                };
                handle_skill_create(app, id, &desc, rt_env);
            }
        }
        "create-llm" => {
            if parts.len() < 2 {
                app.output.push_system("Usage: /skill create-llm <description>");
                app.output.push_system("Example: /skill create-llm Rust error handling best practices");
            } else {
                let desc = parts[1..].join(" ");
                handle_skill_create_llm(app, &desc, rt_env);
            }
        }
        "reflect" => {
            handle_skill_reflect(app, rt_env);
        }
        "delete" => {
            if parts.len() < 2 {
                app.output.push_system("Usage: /skill delete <id>");
            } else {
                handle_skill_delete(app, parts[1], rt_env);
            }
        }
        _ => {
            show_skill_help(app);
        }
    }
    
    CommandResult::Success
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
         /skill delete <id>                   - Delete a skill\n\n\
         Examples:\n\
         /skill create rust-error-handling \"Best practices for error handling\"\n\
         /skill create-llm Rust async patterns\n\
         /skill reflect\n\
         /skill show think-before-coding"
    );
}

/// 列出所有 Skills
fn handle_skill_list(app: &mut AppState, rt_env: &RuntimeEnvironment) {
    use ox_core::skill::SkillLoader;
    
    let loader = SkillLoader::new(
        rt_env.ox_home_dir.join("skills"),
        rt_env.working_dir.join(".ox").join("skills")
    );
    
    match loader.load_enabled_skills() {
        Ok(skills) => {
            if skills.is_empty() {
                app.output.push_system("No skills available.");
                return;
            }
            
            let summary = skills.iter()
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
            app.output.push_system(&format!("❌ Failed to load skills: {}", e));
        }
    }
}

/// 显示 Skill 详情
fn handle_skill_show(app: &mut AppState, id: &str, rt_env: &RuntimeEnvironment) {
    let skill_path = find_skill_file(id, rt_env);
    
    match skill_path.and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(content) => {
            app.output.push_system(&format!("📄 Skill: {}\n\n{}", id, content));
        }
        None => {
            app.output.push_system(&format!("❌ Skill not found: {}", id));
        }
    }
}

/// 创建 Skill 模板
fn handle_skill_create(app: &mut AppState, id: &str, desc: &str, rt_env: &mut RuntimeEnvironment) {
    let skills_dir = rt_env.working_dir.join(".ox").join("skills");
    
    // 创建目录
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        app.output.push_system(&format!("❌ Failed to create skills directory: {}", e));
        return;
    }
    
    let skill_path = skills_dir.join(format!("{}.md", id));
    
    // 检查是否已存在
    if skill_path.exists() {
        app.output.push_system(&format!("❌ Skill already exists: {}", id));
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
        name.chars().next().unwrap_or(' ').to_uppercase().collect::<String>() + &name[1..]
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
            app.output.push_system(&format!("❌ Failed to create Skill: {}", e));
        }
    }
}

/// 删除 Skill
fn handle_skill_delete(app: &mut AppState, id: &str, rt_env: &RuntimeEnvironment) {
    let skill_path = find_skill_file(id, rt_env);
    
    match skill_path {
        Some(path) => {
            if let Err(e) = std::fs::remove_file(&path) {
                app.output.push_system(&format!("❌ Failed to delete: {}", e));
            } else {
                app.output.push_system(&format!("🗑️ Deleted Skill: {}\n\nNote: Skills will be reloaded on next agent run.", id));
            }
        }
        None => {
            app.output.push_system(&format!("❌ Skill not found: {}", id));
        }
    }
}

/// 使用 LLM 辅助创建 Skill
fn handle_skill_create_llm(app: &mut AppState, description: &str, rt_env: &RuntimeEnvironment) {
    // 生成 Skill ID
    let id = description
        .split_whitespace()
        .take(5)
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>();
    
    // 确定保存路径（默认项目级）
    let skills_dir = rt_env.working_dir.join(".ox").join("skills");
    let skill_path = skills_dir.join(format!("{}.md", id));
    
    // 检查是否已存在
    if skill_path.exists() {
        app.output.push_system(&format!("❌ Skill already exists: {}", id));
        return;
    }
    
    // 生成模板（让用户手动编辑）
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
        description,
        name.chars().next().unwrap_or(' ').to_uppercase().collect::<String>() + &name[1..]
    );
    
    // 保存文件
    match std::fs::write(&skill_path, &template) {
        Ok(_) => {
            app.output.push_system(&format!(
                "✅ Created Skill template: {}\n\nEdit the file to customize:\n{}",
                id,
                skill_path.display()
            ));
        }
        Err(e) => {
            app.output.push_system(&format!("❌ Failed to create Skill: {}", e));
        }
    }
}

/// 反思最近的任务并创建 Skill
fn handle_skill_reflect(app: &mut AppState, rt_env: &RuntimeEnvironment) {
    use ox_core::context::SKILL_CREATION_PROMPT;
    
    app.output.push_system("🤔 Analyzing conversation context for reusable patterns...");
    
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
    let project_path = rt_env.working_dir.join(".ox").join("skills").join(&filename);
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
