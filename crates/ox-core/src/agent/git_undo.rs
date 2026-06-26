//! Git-backed undo for finding-scoped file rollback.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::engine::WorkflowEngine;
use super::findings::{self, FindingStatus};

pub fn undo_finding(
    engine: &WorkflowEngine,
    finding_index: u32,
    working_dir: &Path,
) -> Result<String, String> {
    let store = findings::load_or_migrate(engine).ok_or_else(|| "无 findings store".to_string())?;
    let finding = store
        .get(finding_index)
        .ok_or_else(|| format!("finding #{finding_index} 不存在"))?;

    let mut paths: Vec<PathBuf> = Vec::new();
    if !finding.file.is_empty() {
        paths.push(PathBuf::from(&finding.file));
    }
    for step in store.to_plan_tracker(false).steps {
        if step.index == finding_index && !step.file.is_empty() {
            let p = PathBuf::from(&step.file);
            if !paths.iter().any(|x| x == &p) {
                paths.push(p);
            }
        }
    }
    if paths.is_empty() {
        return Err(format!(
            "finding #{finding_index} 无关联文件路径，无法 git checkout"
        ));
    }

    let rel_paths: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let output = git_checkout_paths(working_dir, &rel_paths)?;

    if let Some(mut store) = findings::load_or_migrate(engine) {
        if let Some(f) = store.get_mut(finding_index) {
            f.status = FindingStatus::InProgress;
            f.impl_log.push(findings::ImplAction {
                tool: "git_undo".into(),
                detail: format!("checkout -- {}", rel_paths.join(" ")),
            });
        }
        findings::save(engine, &store);
    }
    engine.sync_plan_from_findings();

    Ok(format!(
        "↩️ 已 git checkout -- {}\n{output}",
        rel_paths.join(" ")
    ))
}

pub fn git_checkout_paths(cwd: &Path, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Ok("（无路径）".to_string());
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd).arg("checkout").arg("--");
    for p in paths {
        cmd.arg(p.replace('\\', "/"));
    }
    let out = cmd
        .output()
        .map_err(|e| format!("git checkout 失败: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git checkout 退出码 {:?}: {stderr}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(if stdout.trim().is_empty() {
        "工作区已恢复".to_string()
    } else {
        stdout.trim().to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_path_arg() {
        let p = "src\\Foo.rs".replace('\\', "/");
        assert_eq!(p, "src/Foo.rs");
    }
}
