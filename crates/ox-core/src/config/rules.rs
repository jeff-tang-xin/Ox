use serde::{Deserialize, Serialize};

/// 强制执行的规则配置 (Enforcement Rules)
/// 
/// 这些规则由系统代码直接校验，违反时将直接拦截工具调用并反馈给 LLM。
/// 它们是从系统级 Skills 中提取的“硬约束”。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnforcementRules {
    /// 是否启用全局强制校验
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 规则 1: 编辑前必须有计划 (Extracted from coding-principles)
    /// 检查 LLM 在调用 file_write/edit_file 前是否在对话中提出了计划。
    #[serde(default = "default_true")]
    pub plan_before_edit: bool,

    /// 规则 2: 复杂任务前必须有步骤列表 (Extracted from engineering-practices)
    /// 检查 LLM 在调用 shell_exec 前是否列出了 Steps。
    #[serde(default = "default_true")]
    pub steps_before_shell: bool,

    /// 规则 3: 编辑前必须先读取文件 (Read Before Edit)
    /// 检查 LLM 在调用 file_write/edit_file 前是否通过 file_read 读取过目标文件。
    /// 防止 LLM 在没有阅读的情况下猜测文件内容。
    #[serde(default = "default_true")]
    pub read_before_edit: bool,

    /// 规则 4: 修改前检查调用方 (Impact Analysis)
    /// 编辑已存在的源码文件时，检查是否通过 code_search 搜索了依赖/调用方。
    /// 防止修改后忘记更新调用处。
    #[serde(default = "default_true")]
    pub impact_analysis: bool,

    /// Trivial 编辑阈值（字符数）
    /// 当 edit_file 的 old_string 长度不超过此值时，视为 trivial 修改，
    /// 自动跳过 plan_before_edit 规则。设为 0 可禁用此白名单。
    #[serde(default = "default_trivial_threshold")]
    pub trivial_edit_threshold: usize,
    
    /// 自定义计划检测模式（可选）
    /// 用户可以添加额外的正则表达式模式来检测计划意图
    #[serde(default)]
    pub custom_plan_patterns: Vec<String>,
    
    /// 自定义步骤检测模式（可选）
    /// 用户可以添加额外的正则表达式模式来检测步骤列表
    #[serde(default)]
    pub custom_step_patterns: Vec<String>,
}

fn default_true() -> bool { true }
fn default_trivial_threshold() -> usize { 50 }

impl Default for EnforcementRules {
    fn default() -> Self {
        Self {
            enabled: true,
            plan_before_edit: true,
            steps_before_shell: true,
            read_before_edit: true,
            impact_analysis: true,
            trivial_edit_threshold: default_trivial_threshold(),
            custom_plan_patterns: vec![],
            custom_step_patterns: vec![],
        }
    }
}
