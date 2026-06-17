//! Frozen context at execute-confirmation time — what the user saw is what Execute gets.

use serde::{Deserialize, Serialize};

use super::engine::WorkflowEngine;

const HANDOFF_KEY: &str = "_execute_handoff";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteHandoff {
    pub confirmation_markdown: String,
    pub user_request: String,
    pub intent_output: Option<String>,
    pub plan_output: Option<String>,
    pub exploration_snapshot: String,
    pub from_step: usize,
    pub pipeline: String,
    pub preflight_done: bool,
}

impl ExecuteHandoff {
    pub fn freeze(
        engine: &WorkflowEngine,
        confirmation_markdown: &str,
        from_step: usize,
        preflight_done: bool,
    ) -> Self {
        let user_request = engine
            .get_variable("_current_user_request")
            .unwrap_or_default();
        let pipeline = WorkflowEngine::effective_routing(
            &user_request,
            engine.get_variable("_step0_output").as_deref(),
        )
        .map(|r| r.pipeline)
        .unwrap_or_else(|| "fast".to_string());

        Self {
            confirmation_markdown: confirmation_markdown.to_string(),
            user_request,
            intent_output: engine.get_variable("_step0_output"),
            plan_output: engine.get_variable("_step1_output"),
            exploration_snapshot: engine.exploration_snapshot_summary(),
            from_step,
            pipeline,
            preflight_done,
        }
    }

    pub fn save(&self, engine: &WorkflowEngine) {
        if let Ok(json) = serde_json::to_string(self) {
            engine.set_variable(HANDOFF_KEY, json);
        }
    }

    pub fn load(engine: &WorkflowEngine) -> Option<Self> {
        engine
            .get_variable(HANDOFF_KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn clear(engine: &WorkflowEngine) {
        engine.set_variable(HANDOFF_KEY, String::new());
    }

    /// High-priority block for Execute iteration 0 / system prompt substitution.
    pub fn format_for_execute(&self) -> String {
        let mut parts = vec![
            "【执行交接包 — 用户已确认以下内容，勿重复 preflight / 勿重新探索】".to_string(),
            self.confirmation_markdown.clone(),
        ];

        if !self.user_request.is_empty() {
            let req: String = self.user_request.chars().take(500).collect();
            parts.push(format!("【用户原话】\n{req}"));
        }

        if let Some(ref plan) = self.plan_output {
            let snippet: String = plan.chars().take(4000).collect();
            parts.push(format!("【计划 JSON】\n{snippet}"));
        }

        if !self.exploration_snapshot.is_empty() {
            let snap: String = self.exploration_snapshot.chars().take(8000).collect();
            parts.push(format!("【Preflight / 探索快照 — 已采集，勿重复相同命令】\n{snap}"));
        }

        if self.preflight_done {
            parts.push(
                "✅ 系统 Preflight 已在确认前完成。Execute 阶段：直接执行 plan 中的 shell/写操作步骤，\
                 不要再运行 git tag -l、git status 等探测命令（除非 plan 明确要求且命令不同）。"
                    .to_string(),
            );
        }

        parts.join("\n\n")
    }
}
