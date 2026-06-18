//! Concrete gates. Each reuses existing verification primitives so the new
//! engine inherits the battle-tested AST / verify / findings logic without a
//! rewrite — only the *control flow* around them is simplified.

use super::gate::{Gate, GateCtx, GateOutcome};
use crate::agent::{completion, findings, post_edit_verification as pev};

/// `## Done` must carry a real summary, not an empty acknowledgement.
pub struct FormatGate;

impl Gate for FormatGate {
    fn id(&self) -> &'static str {
        "format"
    }

    fn check(&self, ctx: &GateCtx) -> GateOutcome {
        let body = ctx
            .assistant_text
            .replace("## Done", "")
            .replace("##Done", "")
            .replace("【Done】", "");
        if body.trim().chars().count() < 8 {
            return GateOutcome::Fail {
                feedback: "❌ `## Done` 后必须用 1–3 行说明做了什么/验证结果，禁止空 Done。"
                    .to_string(),
            };
        }
        GateOutcome::Pass
    }
}

/// Edited files must parse — no lingering AST syntax errors.
pub struct SyntaxGate;

impl Gate for SyntaxGate {
    fn id(&self) -> &'static str {
        "syntax"
    }

    fn check(&self, ctx: &GateCtx) -> GateOutcome {
        let pending = pev::ast_pending_files(ctx.engine);
        if pending.is_empty() {
            return GateOutcome::Pass;
        }
        GateOutcome::Fail {
            feedback: format!(
                "❌ 以下文件仍有语法错误，必须先修复再 ## Done：\n{}",
                pending.join("\n")
            ),
        }
    }
}

/// If code changed, a project verify command must have run with exit 0.
/// This is the gate that structurally prevents a false `## Done` on a failed
/// build (the LLM cannot lie its way past a real shell exit code).
pub struct VerifyGate;

impl Gate for VerifyGate {
    fn id(&self) -> &'static str {
        "verify"
    }

    fn check(&self, ctx: &GateCtx) -> GateOutcome {
        if !ctx.had_code_changes {
            return GateOutcome::Pass;
        }
        if pev::verify_status_failed(ctx.engine) {
            let cmd = pev::verify_command(ctx.engine);
            return GateOutcome::Fail {
                feedback: format!(
                    "❌ 最近一次验证失败（{cmd}）。请根据报错修复后重新运行验证，exit 0 再 ## Done。"
                ),
            };
        }
        if pev::verify_status_blocks_done(ctx.engine) {
            let cmd = pev::verify_command(ctx.engine);
            return GateOutcome::Fail {
                feedback: format!(
                    "❌ 改了代码但尚未验证通过。请用 shell_exec 运行：\n```\n{cmd}\n```\nexit 0 后再 ## Done。"
                ),
            };
        }
        GateOutcome::Pass
    }
}

/// "If you made a plan, finish it." Encourages planfulness without forcing it:
/// when the Done message contains a checklist, every item must be checked.
/// No plan → passes (no loop risk). Combined with prompt discipline that asks
/// complex tasks to lay out a `## Plan`, this gives planning teeth cheaply.
pub struct PlanGate;

impl PlanGate {
    /// Count unchecked `- [ ]` / `* [ ]` checklist items.
    fn unchecked_items(text: &str) -> Vec<String> {
        text.lines()
            .map(str::trim_start)
            .filter(|l| {
                let l = l.trim_start_matches(['-', '*', ' ']);
                l.starts_with("[ ]") || l.starts_with("[]")
            })
            .map(|l| l.to_string())
            .collect()
    }
}

impl Gate for PlanGate {
    fn id(&self) -> &'static str {
        "plan"
    }

    fn check(&self, ctx: &GateCtx) -> GateOutcome {
        let pending = Self::unchecked_items(ctx.assistant_text);
        if pending.is_empty() {
            return GateOutcome::Pass;
        }
        GateOutcome::Fail {
            feedback: format!(
                "❌ 计划仍有 {} 项未完成，请逐项处理后再 ## Done（完成的标 `- [x]`）：\n{}",
                pending.len(),
                pending.join("\n")
            ),
        }
    }
}

/// If a findings store exists, the completion receipt must be present and
/// consistent (resolved items == in-scope items, every verify exit 0).
pub struct ScopeGate;

impl Gate for ScopeGate {
    fn id(&self) -> &'static str {
        "scope"
    }

    fn check(&self, ctx: &GateCtx) -> GateOutcome {
        let Some(store) = findings::load_or_migrate(ctx.engine) else {
            return GateOutcome::Pass;
        };
        if store.findings.is_empty() {
            return GateOutcome::Pass;
        }
        // Review-only ## Done: findings reported, no code edited — receipt not required yet.
        if !ctx.had_code_changes {
            return GateOutcome::Pass;
        }
        match completion::extract_from_text(ctx.assistant_text) {
            Some(receipt) => match completion::validate(ctx.engine, &receipt) {
                Ok(()) => GateOutcome::Pass,
                Err(e) => GateOutcome::Fail { feedback: e },
            },
            None => GateOutcome::Fail {
                feedback: format!(
                    "❌ 存在 {} 条审查项，## Done 必须附 completion_receipt JSON：\n{}",
                    store.findings.len(),
                    completion::COMPLETION_RECEIPT_SCHEMA
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::WorkflowEngine;
    use crate::agent::session::SessionState;
    use std::sync::Arc;

    fn engine() -> WorkflowEngine {
        let session = Arc::new(tokio::sync::Mutex::new(SessionState::new("t")));
        WorkflowEngine::new(session)
    }

    fn ctx<'a>(e: &'a WorkflowEngine, text: &'a str, changed: bool) -> GateCtx<'a> {
        GateCtx {
            engine: e,
            assistant_text: text,
            touched_files: &[],
            had_code_changes: changed,
        }
    }

    #[test]
    fn format_rejects_empty_done() {
        let e = engine();
        assert!(matches!(
            FormatGate.check(&ctx(&e, "## Done", false)),
            GateOutcome::Fail { .. }
        ));
        assert_eq!(
            FormatGate.check(&ctx(&e, "## Done\n修复了空指针并通过编译", false)),
            GateOutcome::Pass
        );
    }

    #[test]
    fn verify_blocks_done_when_shell_failed() {
        let e = engine();
        e.set_variable("_verify_command", "mvn compile".into());
        e.set_variable("_verify_status", "failed".into());
        assert!(matches!(
            VerifyGate.check(&ctx(&e, "## Done x", true)),
            GateOutcome::Fail { .. }
        ));
    }

    #[test]
    fn verify_passes_when_no_code_changes() {
        let e = engine();
        e.set_variable("_verify_command", "mvn compile".into());
        e.set_variable("_verify_status", "failed".into());
        assert_eq!(VerifyGate.check(&ctx(&e, "## Done x", false)), GateOutcome::Pass);
    }

    #[test]
    fn scope_passes_without_findings() {
        let e = engine();
        assert_eq!(ScopeGate.check(&ctx(&e, "## Done x", false)), GateOutcome::Pass);
    }

    #[test]
    fn scope_passes_review_done_without_receipt() {
        let e = engine();
        e.set_variable(
            "_findings_store",
            r#"{"summary":"ok","findings":[{"index":1,"severity":"high","file":"a.rs","symbol":"","issue":"x","recommendation":"","status":"open","user_notes":[],"dispute":null,"impl_log":[]}],"active_indices":[]}"#.into(),
        );
        assert_eq!(
            ScopeGate.check(&ctx(&e, "## Done\n审查完成，见上文", false)),
            GateOutcome::Pass
        );
    }

    #[test]
    fn plan_passes_without_checklist() {
        let e = engine();
        assert_eq!(
            PlanGate.check(&ctx(&e, "## Done\n改好了", false)),
            GateOutcome::Pass
        );
    }

    #[test]
    fn plan_blocks_unchecked_items() {
        let e = engine();
        let text = "## Done\n## Plan\n- [x] 改 A\n- [ ] 改 B\n- [ ] 验证";
        match PlanGate.check(&ctx(&e, text, false)) {
            GateOutcome::Fail { feedback } => assert!(feedback.contains("2 项未完成")),
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn plan_passes_all_checked() {
        let e = engine();
        let text = "## Done\n- [x] 改 A\n- [x] 改 B";
        assert_eq!(PlanGate.check(&ctx(&e, text, false)), GateOutcome::Pass);
    }
}
