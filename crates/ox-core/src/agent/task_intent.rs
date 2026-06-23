//! Classify user request intent for WORKSPACE routing and tool hints.

use super::engine::WorkflowEngine;
use super::findings;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskIntent {
    /// 审查 / 检查 / audit — read-only, findings + Done
    Review,
    /// 修复 / 改代码 / implement
    Fix,
    /// 解释 / 问答 / how does — answer directly, minimal exploration
    Qa,
    /// 一般编码任务
    General,
}

impl TaskIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Fix => "fix",
            Self::Qa => "qa",
            Self::General => "general",
        }
    }

    pub fn from_stored(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "review" => Self::Review,
            "fix" => Self::Fix,
            "qa" => Self::Qa,
            _ => Self::General,
        }
    }

    pub fn tool_hint(self) -> &'static str {
        match self {
            Self::Review => {
                "路径已知→file_read；符号名→find_symbol；文本片段→code_search。\
                 产出报告+findings JSON+## Done，禁止重复读同一文件。"
            }
            Self::Fix => {
                "按 findings 逐项：file_read(每文件1次)→edit_file→验证→下一项。\
                 Done 须附 completion_receipt。"
            }
            Self::Qa => "优先直接回答；需核对时单次 file_read/find_symbol，勿全面探索。",
            Self::General => {
                "路径→file_read；符号→find_symbol；改前先读。复杂任务先 ## Plan。"
            }
        }
    }

    pub fn authority_note(self) -> &'static str {
        match self {
            Self::Review => {
                "权威顺序：用户要求 > 实际代码 > 注释/文档。finding 须引用代码行，不能仅引用注释。"
            }
            _ => "未在本轮 tool result 中出现的代码行为不得断言；应调工具或标注「未验证」。",
        }
    }
}

/// Keyword-only classification (no session state).
pub fn classify(user_text: &str) -> TaskIntent {
    let t = user_text.trim();
    if t.is_empty() {
        return TaskIntent::General;
    }
    let lower = t.to_lowercase();

    if looks_like_fix(t, &lower) {
        return TaskIntent::Fix;
    }
    if looks_like_review(t, &lower) {
        return TaskIntent::Review;
    }
    if looks_like_qa(t, &lower) {
        return TaskIntent::Qa;
    }
    TaskIntent::General
}

/// Session-aware intent for a new user round (production routing).
pub fn resolve_for_round(engine: &WorkflowEngine, user_text: &str) -> TaskIntent {
    let t = user_text.trim();
    if t.is_empty() {
        return TaskIntent::General;
    }

    if t.starts_with("/fix") {
        return TaskIntent::Fix;
    }
    if t.starts_with("/review") {
        return TaskIntent::Review;
    }

    let has_findings = findings::load_or_migrate(engine)
        .map(|s| !s.findings.is_empty())
        .unwrap_or(false);
    let report_delivered = engine.execute_report_already_delivered();
    let raw = classify(t);

    // After review: fix only with findings or explicit greenfield implementation.
    if report_delivered || has_findings {
        if looks_like_fix_request(t) && (has_findings || looks_like_greenfield_impl(t)) {
            return TaskIntent::Fix;
        }
        if crate::agent::workflow_session::looks_like_review_follow_up(t) {
            return TaskIntent::Review;
        }
        if raw == TaskIntent::Qa {
            return TaskIntent::Qa;
        }
        if raw == TaskIntent::Fix && !has_findings {
            return TaskIntent::General;
        }
        if report_delivered && !looks_like_fix_request(t) {
            return if raw == TaskIntent::Review {
                TaskIntent::Review
            } else {
                TaskIntent::Qa
            };
        }
    }

    if raw == TaskIntent::Fix && !has_findings && !looks_like_greenfield_impl(t) {
        return if looks_like_review(t, &t.to_lowercase()) {
            TaskIntent::Review
        } else {
            TaskIntent::General
        };
    }

    raw
}

/// True when user wants to implement something new (not review-then-fix).
pub fn looks_like_greenfield_impl(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    let impl_signals = [
        "实现", "implement", "写一个", "创建", "添加功能", "新增", "开发",
        "build a", "create a", "add feature", "write a",
    ];
    let has_impl = impl_signals
        .iter()
        .any(|k| t.contains(k) || lower.contains(k));
    has_impl && !looks_like_review(t, &lower)
}

fn looks_like_fix_request(t: &str) -> bool {
    let lower = t.to_lowercase();
    looks_like_fix(t, &lower)
        || crate::agent::workflow_session::looks_like_post_failure_fix(t)
        || t.starts_with("/fix")
}

fn looks_like_fix(t: &str, lower: &str) -> bool {
    [
        "修复", "fix", "改掉", "改一下", "帮我改", "implement", "实现", "执行修复",
        "apply fix", "resolve finding", "/fix",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
}

fn looks_like_review(t: &str, lower: &str) -> bool {
    [
        "审查", "检查", "review", "audit", "代码审查", "走查", "对照", "是否符合",
        "有没有问题", "风险", "评估", "注释", "规范",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
        && !looks_like_fix(t, lower)
}

fn looks_like_qa(t: &str, lower: &str) -> bool {
    [
        "是什么", "为什么", "怎么", "如何", "explain", "what is", "how does", "什么意思",
        "帮我看", "介绍一下", "说明",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k))
        && !looks_like_review(t, lower)
        && !looks_like_fix(t, lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn review_intent() {
        assert_eq!(
            classify("请审查 MaintainDeliveryStrategy 是否符合注释"),
            TaskIntent::Review
        );
    }

    #[test]
    fn fix_intent() {
        assert_eq!(classify("修复 finding #2 的空指针"), TaskIntent::Fix);
    }

    #[test]
    fn qa_intent() {
        assert_eq!(classify("doHandle 方法是怎么工作的"), TaskIntent::Qa);
    }

    #[test]
    fn fix_without_findings_downgrades() {
        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        assert_eq!(
            resolve_for_round(&engine, "修复一下"),
            TaskIntent::General
        );
    }

    #[test]
    fn continue_after_review_is_not_fix() {
        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        engine.mark_execute_report_delivered();
        assert_eq!(resolve_for_round(&engine, "继续"), TaskIntent::Qa);
    }

    #[test]
    fn greenfield_impl_is_fix() {
        let engine = WorkflowEngine::new(Arc::new(Mutex::new(SessionState::new("t"))));
        assert_eq!(
            resolve_for_round(&engine, "帮我实现一个登录接口"),
            TaskIntent::Fix
        );
    }
}
