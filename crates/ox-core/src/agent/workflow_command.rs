//! User workflow commands — findings scope & progress (single-step model).

use std::path::Path;

use super::engine::WorkflowEngine;
use super::findings::{self, Dispute, DisputeKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowCommand {
    SelectFindings(Vec<u32>),
    NewTask(String),
    SkipFinding(u32),
    ExtendScope(Vec<u32>),
    ShrinkScope(Vec<u32>),
    DisputeFinding {
        index: u32,
        kind: DisputeKind,
        reason: String,
    },
    ShowProgress,
    ShowFindings,
    ToggleFinding(u32),
    UndoFinding(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    Applied(Option<String>),
    Ignored,
}

/// Parse slash commands and `/fix 1,2` style input.
pub fn parse(input: &str) -> Option<WorkflowCommand> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_lowercase();
    if lower == "/progress" || lower == "/status" {
        return Some(WorkflowCommand::ShowProgress);
    }
    if lower == "/findings" || lower == "/list" {
        return Some(WorkflowCommand::ShowFindings);
    }
    if let Some(rest) = t.strip_prefix("/fix ").or_else(|| t.strip_prefix("/fix:")) {
        let indices = findings::parse_scope_indices(rest);
        if !indices.is_empty() {
            return Some(WorkflowCommand::SelectFindings(indices));
        }
    }
    if let Some(rest) = t
        .strip_prefix("/scope +")
        .or_else(|| t.strip_prefix("/scope+"))
    {
        let indices = findings::parse_scope_indices(rest);
        if !indices.is_empty() {
            return Some(WorkflowCommand::ExtendScope(indices));
        }
    }
    if let Some(rest) = t
        .strip_prefix("/scope -")
        .or_else(|| t.strip_prefix("/scope-"))
    {
        let indices = findings::parse_scope_indices(rest);
        if !indices.is_empty() {
            return Some(WorkflowCommand::ShrinkScope(indices));
        }
    }
    if let Some(rest) = t.strip_prefix("/skip ") {
        let indices = findings::parse_scope_indices(rest);
        if let Some(&n) = indices.first() {
            return Some(WorkflowCommand::SkipFinding(n));
        }
    }
    if let Some(rest) = t.strip_prefix("/new ") {
        return Some(WorkflowCommand::NewTask(rest.trim().to_string()));
    }
    if let Some(rest) = t.strip_prefix("/dispute ") {
        let indices = findings::parse_scope_indices(rest);
        if let Some(&n) = indices.first() {
            let reason = rest
                .trim_start_matches(|c: char| c.is_ascii_digit() || "、,， ".contains(c))
                .trim()
                .to_string();
            return Some(WorkflowCommand::DisputeFinding {
                index: n,
                kind: DisputeKind::FalsePositive,
                reason: if reason.is_empty() {
                    "用户标记误报".into()
                } else {
                    reason
                },
            });
        }
    }
    if let Some(rest) = t.strip_prefix("/toggle ") {
        let indices = findings::parse_scope_indices(rest);
        if let Some(&n) = indices.first() {
            return Some(WorkflowCommand::ToggleFinding(n));
        }
    }
    if let Some(rest) = t.strip_prefix("/undo ") {
        let indices = findings::parse_scope_indices(rest);
        if let Some(&n) = indices.first() {
            return Some(WorkflowCommand::UndoFinding(n));
        }
    }
    None
}

pub fn apply(engine: &mut WorkflowEngine, cmd: WorkflowCommand) -> CommandOutcome {
    apply_with_cwd(engine, cmd, None)
}

pub fn apply_with_cwd(
    engine: &mut WorkflowEngine,
    cmd: WorkflowCommand,
    working_dir: Option<&Path>,
) -> CommandOutcome {
    match cmd {
        WorkflowCommand::SelectFindings(indices) => apply_scope(engine, &indices),
        WorkflowCommand::NewTask(task) => {
            let _ = engine.finish_workflow_session();
            engine.begin_user_round(&task);
            CommandOutcome::Applied(Some(format!("🆕 新任务：{task}")))
        }
        WorkflowCommand::SkipFinding(n) => {
            if let Some(mut store) = findings::load_or_migrate(engine) {
                store.skip(n);
                findings::save(engine, &store);
                engine.sync_plan_from_findings();
                return CommandOutcome::Applied(Some(format!("已跳过 finding #{n}")));
            }
            CommandOutcome::Ignored
        }
        WorkflowCommand::ExtendScope(indices) => apply_scope(engine, &indices),
        WorkflowCommand::ShrinkScope(indices) => {
            if let Some(mut store) = findings::load_or_migrate(engine) {
                store.remove_scope(&indices);
                findings::save(engine, &store);
                engine.sync_plan_from_findings();
                return CommandOutcome::Applied(Some(store.scope_confirm_summary()));
            }
            CommandOutcome::Ignored
        }
        WorkflowCommand::DisputeFinding { index, kind, reason } => {
            if let Some(mut store) = findings::load_or_migrate(engine) {
                store.mark_dispute(
                    index,
                    Dispute {
                        kind,
                        reason: reason.clone(),
                    },
                );
                findings::save(engine, &store);
                return CommandOutcome::Applied(Some(format!(
                    "已标记 finding #{index} 为争议：{reason}"
                )));
            }
            CommandOutcome::Ignored
        }
        WorkflowCommand::ShowProgress => {
            CommandOutcome::Applied(Some(format_progress(engine)))
        }
        WorkflowCommand::ShowFindings => {
            let msg = if let Some(store) = findings::load_or_migrate(engine) {
                format!(
                    "{}\n\n{}",
                    crate::agent::presentation::format_executive(&store),
                    format_findings_list(engine)
                )
            } else {
                format_findings_list(engine)
            };
            CommandOutcome::Applied(Some(msg))
        }
        WorkflowCommand::ToggleFinding(n) => {
            if let Some(mut store) = findings::load_or_migrate(engine) {
                if store.active_indices.contains(&n) {
                    store.remove_scope(&[n]);
                } else {
                    store.add_scope(&[n]);
                }
                findings::save(engine, &store);
                engine.sync_plan_from_findings();
                return CommandOutcome::Applied(Some(store.scope_confirm_summary()));
            }
            CommandOutcome::Ignored
        }
        WorkflowCommand::UndoFinding(n) => {
            if let Some(cwd) = working_dir {
                match super::git_undo::undo_finding(engine, n, cwd) {
                    Ok(msg) => return CommandOutcome::Applied(Some(msg)),
                    Err(e) => {
                        if let Some(mut store) = findings::load_or_migrate(engine) {
                            if let Some(f) = store.get_mut(n) {
                                f.status = findings::FindingStatus::InProgress;
                            }
                            findings::save(engine, &store);
                            engine.sync_plan_from_findings();
                        }
                        return CommandOutcome::Applied(Some(format!(
                            "↩️ finding #{n} 已标为进行中。git 恢复失败: {e}"
                        )));
                    }
                }
            }
            if let Some(mut store) = findings::load_or_migrate(engine) {
                if let Some(f) = store.get_mut(n) {
                    f.status = findings::FindingStatus::InProgress;
                    for entry in &mut f.impl_log {
                        entry.detail.push_str(" [undo requested]");
                    }
                }
                findings::save(engine, &store);
                engine.sync_plan_from_findings();
                return CommandOutcome::Applied(Some(format!(
                    "↩️ finding #{n} 已标为进行中（无工作目录，未执行 git checkout）。"
                )));
            }
            CommandOutcome::Ignored
        }
    }
}

fn apply_scope(engine: &mut WorkflowEngine, indices: &[u32]) -> CommandOutcome {
    let mut store = match findings::load_or_migrate(engine) {
        Some(s) => s,
        None => return CommandOutcome::Ignored,
    };
    store.set_scope(indices);
    findings::save(engine, &store);
    engine.sync_plan_from_findings();
    CommandOutcome::Applied(Some(store.scope_confirm_summary()))
}

fn format_progress(engine: &WorkflowEngine) -> String {
    let Some(store) = findings::load_or_migrate(engine) else {
        return "（无 findings 进度）".to_string();
    };
    let mut lines = vec!["【Finding 进度】".to_string()];
    for row in store.progress_rows() {
        let icon = match row.status {
            findings::FindingStatus::Done => "✅",
            findings::FindingStatus::InProgress | findings::FindingStatus::AwaitingVerify => "🔄",
            findings::FindingStatus::Skipped | findings::FindingStatus::WontFix => "⏭",
            findings::FindingStatus::Disputed => "⚠️",
            _ if row.in_scope => "📌",
            _ => "⏸",
        };
        lines.push(format!(
            "{icon} #{} [{}] {} — {:?}",
            row.index, row.severity, row.issue, row.status
        ));
    }
    lines.join("\n")
}

fn format_findings_list(engine: &WorkflowEngine) -> String {
    let Some(store) = findings::load_or_migrate(engine) else {
        return "（无 findings）".to_string();
    };
    let mut lines = vec!["【Findings 列表 — /fix 1,2 选择范围】".to_string()];
    for f in &store.findings {
        let checked = if store.active_indices.contains(&f.index) {
            "☑"
        } else {
            "☐"
        };
        lines.push(format!(
            "{checked} #{} [{}] {} — {}",
            f.index,
            f.severity.label(),
            if f.file.is_empty() {
                f.symbol.clone()
            } else {
                f.file.clone()
            },
            f.issue
        ));
    }
    lines.push("\n命令：/fix 1,2 · /toggle 3 · /undo N · /progress".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fix_command() {
        assert_eq!(
            parse("/fix 1,2"),
            Some(WorkflowCommand::SelectFindings(vec![1, 2]))
        );
    }
}
