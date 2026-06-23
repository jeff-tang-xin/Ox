//! User-request routing hints for step system prompts.

/// Phrasing that means read-only code audit (检查/审查), not implementation.
pub fn looks_like_read_only_audit(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    let has_audit = [
        "检查", "审查", "排查", "分析", "看看", "评估", "audit", "review", "inspect", "check",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k));
    let wants_modify = [
        "修改", "重构", "实现", "修复", "添加", "删除", "改写", "fix", "implement", "refactor",
    ]
    .iter()
    .any(|k| t.contains(k) || lower.contains(k));
    has_audit && !wants_modify
}

/// Optional prompt hint for {ROUTING_HINT} in step system prompt.
pub fn routing_hint_for_user(user_text: &str) -> String {
    if looks_like_read_only_audit(user_text) {
        "【路由提示】只读代码检查 → 必须 intent=exploring, pipeline=fast（跳过规划/审阅；人工确认后只读执行；禁止 modify/delete 计划）".to_string()
    } else {
        String::new()
    }
}
