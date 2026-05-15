use crate::message::{Message, ToolCall};
use crate::config::rules::EnforcementRules;
use regex::Regex;
use lazy_static::lazy_static;

lazy_static! {
    // 计划意图的正则表达式模式（多语言支持）
    static ref PLAN_PATTERNS: Vec<Regex> = vec![
        // 英文模式
        Regex::new(r"(?i)(?:I plan|I will|I'm going|My plan|intend to|will modify|will change)").unwrap(),
        Regex::new(r"(?i)(?:plan to|going to|would like to|want to)").unwrap(),
        // 中文模式
        Regex::new(r"(我计划|我打算|我将要|我要|准备|想要修改|想要更改)").unwrap(),
        Regex::new(r"(计划[是:]|打算[是:]|将要[是:])").unwrap(),
        // 通用模式
        Regex::new(r"(Step|步骤|第一步|1\.|\*|-).*?(?:to|将|会|要)").unwrap(),
    ];
    
    // 步骤列表的正则表达式模式（多语言支持）
    static ref STEP_PATTERNS: Vec<Regex> = vec![
        // 英文步骤标识
        Regex::new(r"(?i)(?:step|phase|stage)\s*\d+").unwrap(),
        Regex::new(r"(?i)(?:first|second|third|finally|then|next)").unwrap(),
        // 中文步骤标识
        Regex::new(r"(第[一二三四五六七八九十百]+步|首先|然后|接着|最后|接下来)").unwrap(),
        // 数字列表格式
        Regex::new(r"^\s*\d+[\.、]\s*").unwrap(),
        Regex::new(r"^\s*[\*\-\+]\s+").unwrap(),
        // 冒号分隔的步骤
        Regex::new(r"(?:步骤|Step|阶段)[：:]\s*\d+").unwrap(),
    ];
}

/// Rule Enforcer - 强制执行系统级约束
/// 
/// 与 Skill 不同，Rules 是由代码直接校验的。如果违反，工具调用会被立即拦截，
/// 并将错误信息返回给 LLM，要求其修正行为。
pub struct RuleEnforcer;

impl RuleEnforcer {
    /// 执行所有启用的强制规则
    pub fn validate(
        rules: &EnforcementRules,
        tool_call: &ToolCall,
        messages: &[Message],
    ) -> Result<(), String> {
        if !rules.enabled {
            return Ok(());
        }

        // 1. 校验: 编辑前必须有计划 (Plan Before Edit)
        if rules.plan_before_edit {
            if let Err(e) = Self::check_plan_before_edit(tool_call, messages, &rules.custom_plan_patterns) {
                return Err(e);
            }
        }

        // 2. 校验: Shell 执行前必须有步骤列表 (Steps Before Shell)
        if rules.steps_before_shell {
            if let Err(e) = Self::check_steps_before_shell(tool_call, messages, &rules.custom_step_patterns) {
                return Err(e);
            }
        }

        Ok(())
    }

    /// 检查编辑操作前是否有明确的计划陈述
    fn check_plan_before_edit(tc: &ToolCall, messages: &[Message], custom_patterns: &[String]) -> Result<(), String> {
        if !matches!(tc.name.as_str(), "file_write" | "file_patch") {
            return Ok(());
        }

        let msgs = messages;
        // 扫描最近 5 条消息，寻找 LLM 提出的计划
        let has_plan = msgs.iter().rev().take(5).any(|msg| {
            if let Message::Assistant { content, .. } = msg {
                // 使用内置正则表达式进行多语言模式匹配
                let built_in_match = PLAN_PATTERNS.iter().any(|pattern| pattern.is_match(content));
                
                // 检查自定义模式
                let custom_match = custom_patterns.iter().any(|pattern_str| {
                    Regex::new(pattern_str)
                        .map(|pattern| pattern.is_match(content))
                        .unwrap_or(false)
                });
                
                built_in_match || custom_match
            } else {
                false
            }
        });

        if !has_plan {
            Err("🛑 RULE VIOLATION (coding-principles): You must propose a clear plan before editing files.\nExample: 'I plan to modify X to achieve Y.' or '我的计划是修改X以实现Y。'".to_string())
        } else {
            Ok(())
        }
    }

    /// 检查 Shell 命令执行前是否有步骤列表
    fn check_steps_before_shell(tc: &ToolCall, messages: &[Message], custom_patterns: &[String]) -> Result<(), String> {
        if tc.name != "shell_exec" {
            return Ok(());
        }
        
        // 🎯 核心逻辑：只检查最近一次用户消息之后是否有步骤列表
        let last_user_idx = messages.iter().rev()
            .position(|m| matches!(m, Message::User { .. }))
            .map(|pos| messages.len() - 1 - pos);
        
        let search_start = last_user_idx.unwrap_or(0);
        let recent_messages = &messages[search_start..];
        
        // 查找最近的任务列表
        let mut task_list_found = false;
        let mut has_pending_tasks = false;
        
        for msg in recent_messages.iter().rev() {
            if let Message::Assistant { content, .. } = msg {
                // 检查是否包含任务列表格式
                if STEP_PATTERNS.iter().any(|pattern| {
                    content.lines().any(|line| pattern.is_match(line.trim()))
                }) {
                    task_list_found = true;
                    
                    // 检查是否有未完成的任务（待办事项标记）
                    // 支持多种格式：- [ ], ☐, □, TODO, 待完成
                    let has_unfinished = content.contains("- [ ]") || 
                                        content.contains("☐") ||
                                        content.contains("□") ||
                                        content.to_lowercase().contains("todo") ||
                                        content.contains("待完成") ||
                                        content.contains("未完成");
                    
                    if has_unfinished {
                        has_pending_tasks = true;
                    }
                    
                    break; // 找到最近的任务列表即可
                }
            }
        }
        
        // 如果内置模式没找到，尝试自定义模式
        if !task_list_found && !custom_patterns.is_empty() {
            task_list_found = recent_messages.iter().any(|m| {
                if let Message::Assistant { content, .. } = m {
                    custom_patterns.iter().any(|pattern_str| {
                        Regex::new(pattern_str)
                            .map(|pattern| content.lines().any(|line| pattern.is_match(line.trim())))
                            .unwrap_or(false)
                    })
                } else {
                    false
                }
            });
            
            if task_list_found {
                has_pending_tasks = true; // 自定义模式默认认为有待办任务
            }
        }

        if !task_list_found {
            Err("🛑 RULE VIOLATION (engineering-practices): Before executing shell commands, you MUST provide a traceable task list.\n\nExample format:\n```\nTask List:\n- [ ] Step 1: Run git status to check current state\n- [ ] Step 2: Add changed files with git add .\n- [ ] Step 3: Commit changes with git commit\n```\n\nAfter each command execution, update the task list:\n- Mark completed items with ✓ or - [x]\n- Keep the full task list visible for tracking\n\n中文示例：\n```\n任务列表：\n- [ ] 第一步：运行 git status 查看状态\n- [ ] 第二步：使用 git add . 添加文件\n- [ ] 第三步：使用 git commit 提交更改\n```\n执行完每个命令后，更新任务列表标记已完成的项目：✓ 或 - [x]".to_string())
        } else if !has_pending_tasks {
            // 有任务列表但所有任务都已完成，提示创建新任务列表
            Err("⚠️ NOTICE: All tasks in your task list appear to be completed. For a new shell command, please create a new task list with pending items.".to_string())
        } else {
            Ok(())
        }
    }
}
