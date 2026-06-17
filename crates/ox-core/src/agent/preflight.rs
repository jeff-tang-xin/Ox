//! Run read-only probe commands from plan JSON before execute confirmation.

use std::path::Path;
use std::process::Command;

use super::engine::WorkflowEngine;

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub id: String,
    pub command: String,
    pub output: String,
    pub success: bool,
}

/// Parse `probes` array from plan JSON and run each command in `working_dir`.
pub fn run_plan_probes(working_dir: &Path, plan_json: &str) -> Vec<ProbeResult> {
    let v: serde_json::Value = match serde_json::from_str(plan_json) {
        Ok(v) => v,
        Err(_) => {
            if let Some(block) = super::engine::extract_json_block(plan_json) {
                serde_json::from_str(&block).unwrap_or(serde_json::Value::Null)
            } else {
                return Vec::new();
            }
        }
    };

    let Some(probes) = v.get("probes").and_then(|p| p.as_array()) else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for probe in probes {
        let id = probe
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("probe")
            .to_string();
        let command = probe
            .get("command")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if command.is_empty() {
            continue;
        }
        let purpose = probe
            .get("purpose")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let output = run_shell_probe(working_dir, &command);
        let success = !output.starts_with("[exit ");
        results.push(ProbeResult {
            id: id.clone(),
            command: command.clone(),
            output: if purpose.is_empty() {
                output
            } else {
                format!("// {purpose}\n{output}")
            },
            success,
        });
    }
    results
}

fn run_shell_probe(working_dir: &Path, command: &str) -> String {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(working_dir);
    match cmd.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            let mut text = String::new();
            if !stdout.trim().is_empty() {
                text.push_str(stdout.trim());
            }
            if !stderr.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("stderr: {}", stderr.trim()));
            }
            if text.is_empty() {
                text = format!("(exit {code}, no output)");
            } else if code != 0 {
                text = format!("[exit {code}]\n{text}");
            }
            // Cap size for snapshot storage
            if text.chars().count() > 4000 {
                text = format!("{}…", text.chars().take(4000).collect::<String>());
            }
            text
        }
        Err(e) => format!("[preflight error] {e}"),
    }
}

/// Run probes and merge into workflow exploration snapshot.
pub fn run_and_store(engine: &WorkflowEngine, working_dir: &Path, plan_json: &str) -> bool {
    let results = run_plan_probes(working_dir, plan_json);
    if results.is_empty() {
        return false;
    }
    for r in results {
        let formatted = format!(
            "── DATA (preflight) ──\n$ {}\n{}\n── END DATA ──",
            r.command, r.output
        );
        let target = format!("probe:{}", r.id);
        engine.record_preflight_result(working_dir, &target, &formatted);
        tracing::info!(
            "[PREFLIGHT] {} `{}` → {} chars (ok={})",
            r.id,
            r.command.chars().take(60).collect::<String>(),
            r.output.chars().count(),
            r.success
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_probes_from_plan() {
        let plan = r#"{"probes":[{"id":"t","command":"echo hi","purpose":"test"}],"plan":[]}"#;
        let r = run_plan_probes(Path::new("."), plan);
        assert_eq!(r.len(), 1);
        assert!(r[0].output.contains("hi") || r[0].output.contains("error"));
    }
}
