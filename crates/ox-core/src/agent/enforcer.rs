use crate::config::rules::EnforcementRules;
use crate::message::{Message, ToolCall};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json;

/// Check if a file path is source code (needs Plan before editing).
fn is_source_file(path: &str) -> bool {
    crate::source_paths::is_source_code_path(path)
}

lazy_static! {
    // 计划意图的正则表达式模式（多语言支持）
    static ref PLAN_PATTERNS: Vec<Regex> = vec![
        // Plan 块格式（LLM 实际按系统提示输出的格式）：## Plan / ▎ Plan / --- Plan ---
        Regex::new(r"(?im)^\s*#{1,3}\s+Plan\b").unwrap(),
        Regex::new(r"(?im)^\s*▎\s*Plan\b").unwrap(),
        Regex::new(r"(?im)^\s*-{3,}\s*Plan\b").unwrap(),
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
            Self::check_plan_before_edit(
                tool_call,
                messages,
                &rules.custom_plan_patterns,
                rules.trivial_edit_threshold,
            )?
        }

        // 2. 校验: Shell 执行前必须有步骤列表 (Steps Before Shell)
        if rules.steps_before_shell {
            Self::check_steps_before_shell(tool_call, messages, &rules.custom_step_patterns)?
        }

        // 3. 校验: 编辑前必须先读取文件 (Read Before Edit)
        if rules.read_before_edit {
            Self::check_read_before_edit(tool_call, messages, rules.trivial_edit_threshold)?
        }

        // 4. 校验: 修改定义前检查调用方 (Impact Analysis)
        if rules.impact_analysis {
            Self::check_impact_before_edit(tool_call, messages, rules.trivial_edit_threshold)?
        }

        Ok(())
    }

    /// 检查编辑操作前是否有明确的计划陈述
    fn check_plan_before_edit(
        tc: &ToolCall,
        messages: &[Message],
        custom_patterns: &[String],
        trivial_threshold: usize,
    ) -> Result<(), String> {
        if !matches!(
            tc.name.as_str(),
            "file_write" | "edit_file" | "delete_range"
        ) {
            return Ok(());
        }

        // Exception: Plan only needed for source code, not docs/config/system files
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
            && let Some(path) = args.get("path").and_then(|p| p.as_str())
            && !is_source_file(path)
        {
            return Ok(());
        }

        // 🎯 Trivial 编辑白名单：edit_file 的 old_string 很短时（如拼写修复），不需要 Plan
        // 只对 edit_file 生效（file_write 是全量写入，不适用）
        if tc.name == "edit_file"
            && trivial_threshold > 0
            && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        {
            // 检查单次编辑的 old_string 长度
            if let Some(old_str) = args.get("old_string").and_then(|v| v.as_str())
                && old_str.len() <= trivial_threshold
            {
                return Ok(());
            }
            // 检查批量编辑中是否所有 old_string 都很短
            if let Some(edits) = args.get("edits").and_then(|v| v.as_array()) {
                let all_trivial = edits.iter().all(|e| {
                    e.get("old_string")
                        .and_then(|v| v.as_str())
                        .map(|s| s.len() <= trivial_threshold)
                        .unwrap_or(false)
                });
                if all_trivial {
                    return Ok(());
                }
            }
        }

        // 🎯 只检查最近一次用户消息之后的消息（与 Steps Before Shell 逻辑一致）
        // 这样用户提一个问题 → LLM 计划一次 → 多次编辑操作都可通过
        let last_user_idx = messages
            .iter()
            .rev()
            .position(|m| matches!(m, Message::User { .. }))
            .map(|pos| messages.len() - 1 - pos);

        let search_start = last_user_idx.unwrap_or(0);
        let recent_messages = if search_start < messages.len() {
            &messages[search_start..]
        } else {
            &[]
        };

        // 在用户消息之后的消息中，寻找 LLM 提出的计划
        let has_plan = recent_messages.iter().any(|msg| {
            if let Message::Assistant { content, .. } = msg {
                // 使用内置正则表达式进行多语言模式匹配
                let built_in_match = PLAN_PATTERNS
                    .iter()
                    .any(|pattern| pattern.is_match(content));

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
    fn check_steps_before_shell(
        tc: &ToolCall,
        messages: &[Message],
        custom_patterns: &[String],
    ) -> Result<(), String> {
        if tc.name != "shell_exec" {
            return Ok(());
        }

        // 🎯 核心逻辑：只检查最近一次用户消息之后是否有步骤列表
        let last_user_idx = messages
            .iter()
            .rev()
            .position(|m| matches!(m, Message::User { .. }))
            .map(|pos| messages.len() - 1 - pos);

        let search_start = last_user_idx.unwrap_or(0);
        // 使用安全的切片方法
        let recent_messages = if search_start < messages.len() {
            &messages[search_start..]
        } else {
            &[]
        };

        // 查找最近的任务列表
        let mut task_list_found = false;
        let mut has_pending_tasks = false;

        for msg in recent_messages.iter().rev() {
            if let Message::Assistant { content, .. } = msg {
                // 检查是否包含任务列表格式
                if STEP_PATTERNS
                    .iter()
                    .any(|pattern| content.lines().any(|line| pattern.is_match(line.trim())))
                {
                    task_list_found = true;

                    // 检查是否有未完成的任务（待办事项标记）
                    // 支持多种格式：- [ ], ☐, □, TODO, 待完成
                    let has_unfinished = content.contains("- [ ]")
                        || content.contains("☐")
                        || content.contains("□")
                        || content.to_lowercase().contains("todo")
                        || content.contains("待完成")
                        || content.contains("未完成");

                    // 检查是否有已完成的标记
                    let has_completed = content.contains("- [x]")
                        || content.contains("- [X]")
                        || content.contains("✓")
                        || content.contains("✔")
                        || content.contains("✅");

                    // 默认认为有 pending 任务，除非检测到明确的完成标记且无未完成标记
                    has_pending_tasks = has_unfinished || !has_completed;

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
                            .map(|pattern| {
                                content.lines().any(|line| pattern.is_match(line.trim()))
                            })
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

    /// 检查编辑目标文件是否已被读取 (Read Before Edit)
    ///
    /// 在文件被修改前，确认 LLM 已经通过 file_read/recall 读取过它。
    /// 这防止 LLM"猜测"文件内容而不是先阅读。
    fn check_read_before_edit(
        tc: &ToolCall,
        messages: &[Message],
        trivial_threshold: usize,
    ) -> Result<(), String> {
        if !matches!(
            tc.name.as_str(),
            "file_write" | "edit_file" | "delete_range"
        ) {
            return Ok(());
        }

        // 只对源码文件强制（同 plan_before_edit）
        let target_path = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
            Ok(args) => args
                .get("path")
                .and_then(|p| p.as_str())
                .map(|s| s.trim().to_string()),
            Err(_) => return Ok(()),
        };
        let target_path = match target_path {
            Some(p) if !p.is_empty() => p,
            _ => return Ok(()),
        };
        if !is_source_file(&target_path) {
            return Ok(());
        }

        // 🎯 Trivial 编辑白名单：短修改不需要先读取（与 plan_before_edit 一致）
        if tc.name == "edit_file"
            && trivial_threshold > 0
            && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        {
            if let Some(old_str) = args.get("old_string").and_then(|v| v.as_str())
                && old_str.len() <= trivial_threshold
            {
                return Ok(());
            }
            if let Some(edits) = args.get("edits").and_then(|v| v.as_array()) {
                let all_trivial = edits.iter().all(|e| {
                    e.get("old_string")
                        .and_then(|v| v.as_str())
                        .map(|s| s.len() <= trivial_threshold)
                        .unwrap_or(false)
                });
                if all_trivial {
                    return Ok(());
                }
            }
        }

        // 🎯 扫描最近一次用户消息之后的消息中是否有对同一路径的 file_read
        let last_user_idx = messages
            .iter()
            .rev()
            .position(|m| matches!(m, Message::User { .. }))
            .map(|pos| messages.len() - 1 - pos);

        let search_start = last_user_idx.unwrap_or(0);
        let recent_messages = if search_start < messages.len() {
            &messages[search_start..]
        } else {
            &[]
        };

        // 提取目标路径的基础文件名（含扩展名，无目录）
        let target_basename = std::path::Path::new(&target_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let mut has_read = false;
        for msg in recent_messages.iter() {
            match msg {
                Message::Assistant { tool_calls, .. } => {
                    for tc in tool_calls {
                        if tc.name == "file_read"
                            && let Ok(args) =
                                serde_json::from_str::<serde_json::Value>(&tc.arguments)
                                && let Some(read_path) = args.get("path").and_then(|p| p.as_str()) {
                                    let read_basename = std::path::Path::new(read_path)
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    // 文件名本身匹配，或者读取路径包含目标路径
                                    if read_path.trim() == target_path
                                        || read_basename == target_basename
                                        || read_path.trim().ends_with(&target_path)
                                        || target_path.ends_with(read_path.trim())
                                    {
                                        has_read = true;
                                        break;
                                    }
                                }
                    }
                    if has_read {
                        break;
                    }
                }
                Message::ToolResult { content, .. }
                    // recall 的结果也视为"看过"——它恢复了 offloaded 的文件内容
                    if (content.contains(&target_basename) || content.contains(&target_path)) => {
                        // 不能单纯因为文本中出现文件名就认为已读取
                        // 需要是 recall 工具的结果
                    }
                _ => {}
            }
            if has_read {
                break;
            }
        }

        if !has_read {
            Err(format!(
                "🛑 RULE VIOLATION (read-before-edit): You edited `{path}` without reading it first!\n\n\
                 You MUST read a file before editing it. DO NOT guess the file content.\n\
                 The file may differ from what you expect.\n\n\
                 💡 Fix:\n\
                 1. Call `file_read` with `\"path\": \"{path}\"` to see the EXACT content\n\
                 2. Then call `edit_file` or `file_write` with the correct changes\n\n\
                 📝 Example:\n\
                 file_read(path=\"{path}\")\n\
                 ... then edit_file/path ...",
                path = target_path
            ))
        } else {
            Ok(())
        }
    }

    /// 检查修改源文件前是否搜索了调用方 (Impact Analysis)
    ///
    /// 当编辑已存在的源码文件时，确保 LLM 调用了 code_search 来检查依赖/调用方。
    /// 防止修改函数签名/接口后忘记更新调用处。
    fn check_impact_before_edit(
        tc: &ToolCall,
        messages: &[Message],
        trivial_threshold: usize,
    ) -> Result<(), String> {
        if !matches!(
            tc.name.as_str(),
            "file_write" | "edit_file" | "delete_range"
        ) {
            return Ok(());
        }

        // 只对已存在的源码文件强制
        let target_path = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
            Ok(args) => args
                .get("path")
                .and_then(|p| p.as_str())
                .map(|s| s.trim().to_string()),
            Err(_) => return Ok(()),
        };
        let target_path = match target_path {
            Some(p) if !p.is_empty() => p,
            _ => return Ok(()),
        };
        if !is_source_file(&target_path) {
            return Ok(());
        }

        // 🎯 Trivial 编辑白名单：短修改不需要 impact analysis
        if tc.name == "edit_file"
            && trivial_threshold > 0
            && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
        {
            if let Some(old_str) = args.get("old_string").and_then(|v| v.as_str())
                && old_str.len() <= trivial_threshold
            {
                return Ok(());
            }
            if let Some(edits) = args.get("edits").and_then(|v| v.as_array()) {
                let all_trivial = edits.iter().all(|e| {
                    e.get("old_string")
                        .and_then(|v| v.as_str())
                        .map(|s| s.len() <= trivial_threshold)
                        .unwrap_or(false)
                });
                if all_trivial {
                    return Ok(());
                }
            }
        }

        let target_basename = std::path::Path::new(&target_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // 🎯 检查最近一次用户消息之后，是否对同一文件执行过 code_search
        let last_user_idx = messages
            .iter()
            .rev()
            .position(|m| matches!(m, Message::User { .. }))
            .map(|pos| messages.len() - 1 - pos);

        let search_start = last_user_idx.unwrap_or(0);
        let recent_messages = if search_start < messages.len() {
            &messages[search_start..]
        } else {
            &[]
        };

        let has_impact_check = recent_messages.iter().any(|msg| {
            if let Message::Assistant { tool_calls, .. } = msg {
                tool_calls.iter().any(|tc| {
                    if tc.name == "code_search"
                        && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    {
                        let search_query = args.get("pattern").and_then(|q| q.as_str()).unwrap_or("");
                        // code_search was called with the file name or path as pattern
                        if search_query.contains(&target_basename)
                            || search_query.contains(&target_path)
                            || target_path.contains(search_query)
                        {
                            return true;
                        }
                    }
                    false
                })
            } else {
                false
            }
        });

        if !has_impact_check {
            Err(format!(
                "🛑 RULE VIOLATION (impact-analysis): You are editing `{path}` but haven't checked for callers or dependents!\n\n\
                 Before modifying an existing source file, you must check what depends on it:\n\
                 1. Call `code_search(pattern=\"{name}\")` to find all references and callers\n\
                 2. Review the impact\n\
                 3. Then proceed with the edit\n\n\
                 This prevents breaking changes. The system requires impact analysis before editing existing source files.\n\n\
                 📝 Example:\n\
                 code_search(pattern=\"{name}\")\n\
                 ... review results ...\n\
                 edit_file(path=\"{path}\")",
                path = target_path,
                name = target_basename,
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_edit_file_call(path: &str, old_string: &str, new_string: &str) -> ToolCall {
        ToolCall {
            id: "test".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "old_string": old_string,
                "new_string": new_string,
            })
            .to_string(),
        }
    }

    fn make_file_write_call(path: &str, content: &str) -> ToolCall {
        ToolCall {
            id: "test".to_string(),
            name: "file_write".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "content": content,
            })
            .to_string(),
        }
    }

    #[test]
    fn test_trivial_edit_bypasses_plan_requirement() {
        // 拼写修复：old_string 很短，应自动放行
        let tc = make_edit_file_call("src/main.rs", "teh", "the");
        let messages = vec![]; // 没有 plan，但 trivial 编辑应放行
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 50);
        assert!(
            result.is_ok(),
            "Trivial edit should bypass plan requirement"
        );
    }

    #[test]
    fn test_non_trivial_edit_requires_plan() {
        // 大段修改：old_string 超过阈值，没有 plan 应被拦截
        let long_old =
            "fn main() {\n    println!(\"Hello, world!\");\n    let x = 1;\n    let y = 2;\n}"
                .to_string();
        let tc = make_edit_file_call("src/main.rs", &long_old, "fn main() {}");
        let messages = vec![];
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 50);
        assert!(result.is_err(), "Non-trivial edit should require plan");
    }

    #[test]
    fn test_file_write_never_trivial() {
        // file_write 是全量写入，即使内容很短也不走 trivial 白名单
        let tc = make_file_write_call("src/main.rs", "fn main() {}");
        let messages = vec![];
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 50);
        assert!(
            result.is_err(),
            "file_write should never be considered trivial"
        );
    }

    #[test]
    fn test_trivial_threshold_zero_disables_whitelist() {
        // 阈值为 0 时，白名单禁用，所有编辑都需要 plan
        let tc = make_edit_file_call("src/main.rs", "teh", "the");
        let messages = vec![];
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 0);
        assert!(
            result.is_err(),
            "Threshold 0 should disable trivial whitelist"
        );
    }

    #[test]
    fn test_batch_edits_all_trivial_passes() {
        // 批量编辑中所有 old_string 都很短，应放行
        let tc = ToolCall {
            id: "test".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": "src/main.rs",
                "edits": [
                    { "old_string": "teh", "new_string": "the" },
                    { "old_string": "recieve", "new_string": "receive" },
                ]
            })
            .to_string(),
        };
        let messages = vec![];
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 50);
        assert!(result.is_ok(), "All-trivial batch edits should bypass plan");
    }

    #[test]
    fn test_batch_edits_mixed_requires_plan() {
        // 批量编辑中有一个 old_string 很长，需要 plan
        let long_old =
            "fn main() {\n    println!(\"Hello, world!\");\n    let x = 1;\n    let y = 2;\n}"
                .to_string();
        let tc = ToolCall {
            id: "test".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": "src/main.rs",
                "edits": [
                    { "old_string": "teh", "new_string": "the" },
                    { "old_string": long_old, "new_string": "fn main() {}" },
                ]
            })
            .to_string(),
        };
        let messages = vec![];
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 50);
        assert!(
            result.is_err(),
            "Mixed batch with non-trivial edit should require plan"
        );
    }

    #[test]
    fn test_non_source_file_always_passes() {
        // 非源码文件不受 plan 规则约束
        let tc = make_edit_file_call(
            "README.md",
            "some long text that exceeds threshold",
            "replacement",
        );
        let messages = vec![];
        let result = RuleEnforcer::check_plan_before_edit(&tc, &messages, &[], 50);
        assert!(result.is_ok(), "Non-source files should always pass");
    }
}
