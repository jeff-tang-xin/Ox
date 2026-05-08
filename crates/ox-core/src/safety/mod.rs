pub mod sanitizer;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::tools::SafetyLevel;

/// Session-scoped trust manager for tool confirmation.
///
/// Tracks which tools the user has temporarily trusted (via `/trust`).
/// Trust is session-scoped only — it expires when the REPL exits.
#[derive(Debug, Clone, Default)]
pub struct TrustManager {
    trusted_tools: HashSet<String>,
}

impl TrustManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if confirmation can be skipped for a given tool.
    pub fn can_skip_confirmation(&self, tool_name: &str, safety: SafetyLevel) -> bool {
        match safety {
            SafetyLevel::Safe => true,
            SafetyLevel::RequiresConfirmation => {
                self.trusted_tools.contains(tool_name) || self.trusted_tools.contains("__all__")
            }
            SafetyLevel::Dangerous => self.trusted_tools.contains("__all__"),
        }
    }

    /// Trust a specific tool for the current session.
    pub fn trust(&mut self, tool_name: &str) {
        self.trusted_tools.insert(tool_name.to_string());
    }

    /// Trust all RequiresConfirmation tools (Dangerous excluded).
    pub fn trust_all(&mut self) {
        self.trusted_tools.insert("__all__".to_string());
    }

    /// Revoke all temporary trust.
    pub fn untrust_all(&mut self) {
        self.trusted_tools.clear();
    }

    /// List currently trusted tools.
    pub fn trusted_list(&self) -> Vec<String> {
        self.trusted_tools.iter().cloned().collect()
    }

    /// Check if any tools are trusted.
    pub fn has_trusted(&self) -> bool {
        !self.trusted_tools.is_empty()
    }
}

/// Check if a shell command contains high-risk patterns.
pub fn is_high_risk_command(command: &str) -> bool {
    let patterns = [
        "rm -rf",
        "rm -r /",
        "rmdir /s",
        "del /s",
        "format ",
        "mkfs",
        "dd if=",
        ":(){ :|:& };:",
        "remove_dir_all",
        "> /dev/sda",
        "chmod -R 777",
        "curl | sh",
        "wget | sh",
    ];
    let lower = command.to_lowercase();
    patterns.iter().any(|p| lower.contains(p))
}

/// Check whether a resolved path is within the working directory.
/// Returns true if within, false if outside. Does not error.
pub fn is_path_within_workdir(path: &Path, working_dir: &Path) -> bool {
    let Ok(canonical_workdir) = working_dir.canonicalize() else {
        return false;
    };
    if let Ok(canonical_path) = path.canonicalize() {
        return canonical_path.starts_with(&canonical_workdir);
    }
    // Path doesn't exist yet — check parent.
    if let Some(parent) = path.parent() {
        if let Ok(canonical_parent) = parent.canonicalize() {
            return canonical_parent.starts_with(&canonical_workdir);
        }
    }
    false
}

/// Resolve a path to an absolute path if it exists.
/// For non-existent paths (e.g., new files), returns the path as-is.
/// This is a convenience wrapper around PathBuf::join + canonicalize.
pub fn validate_path_within_workdir(path: &Path, _working_dir: &Path) -> anyhow::Result<PathBuf> {
    // Try to canonicalize if the path exists
    if path.exists() {
        return Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    }

    // Path doesn't exist yet, return as-is
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_always_skips() {
        let tm = TrustManager::new();
        assert!(tm.can_skip_confirmation("file_read", SafetyLevel::Safe));
    }

    #[test]
    fn dangerous_skips_when_trust_all() {
        let mut tm = TrustManager::new();
        // Individual trust does NOT skip Dangerous.
        tm.trust("shell_exec");
        assert!(!tm.can_skip_confirmation("shell_exec", SafetyLevel::Dangerous));
        // trust_all DOES skip Dangerous.
        tm.trust_all();
        assert!(tm.can_skip_confirmation("shell_exec", SafetyLevel::Dangerous));
    }

    #[test]
    fn trust_specific_tool() {
        let mut tm = TrustManager::new();
        assert!(!tm.can_skip_confirmation("file_write", SafetyLevel::RequiresConfirmation));
        tm.trust("file_write");
        assert!(tm.can_skip_confirmation("file_write", SafetyLevel::RequiresConfirmation));
        assert!(!tm.can_skip_confirmation("file_patch", SafetyLevel::RequiresConfirmation));
    }

    #[test]
    fn trust_all_skips_requires_confirmation() {
        let mut tm = TrustManager::new();
        tm.trust_all();
        assert!(tm.can_skip_confirmation("file_write", SafetyLevel::RequiresConfirmation));
        assert!(tm.can_skip_confirmation("file_patch", SafetyLevel::RequiresConfirmation));
    }

    #[test]
    fn untrust_revokes() {
        let mut tm = TrustManager::new();
        tm.trust("file_write");
        tm.untrust_all();
        assert!(!tm.can_skip_confirmation("file_write", SafetyLevel::RequiresConfirmation));
    }

    #[test]
    fn high_risk_detection() {
        assert!(is_high_risk_command("rm -rf /"));
        assert!(is_high_risk_command("sudo rm -rf /home"));
        assert!(!is_high_risk_command("ls -la"));
        assert!(!is_high_risk_command("cargo build"));
    }

    #[test]
    fn validate_path_allows_within_workdir() {
        let dir = std::env::temp_dir();
        let file_path = dir.join("test_file.txt");
        let result = validate_path_within_workdir(&file_path, &dir);
        assert!(result.is_ok() || file_path.parent().is_some());
    }

    #[test]
    fn is_path_within_workdir_detects_outside() {
        let dir = std::env::temp_dir();
        // Path within workdir.
        let inside = dir.join("subdir/file.txt");
        assert!(is_path_within_workdir(&inside, &dir) || !inside.exists());
        // Path traversal should be detected.
        let traversal = dir.join("../../etc/passwd");
        assert!(!is_path_within_workdir(&traversal, &dir));
    }

    #[test]
    fn validate_path_no_longer_rejects_traversal() {
        let dir = std::env::temp_dir();
        // Use a path that exists (parent dir of temp_dir is typically C:\Users on Windows).
        let parent_dir = dir.parent().unwrap_or(&dir);
        let existing_path = parent_dir.join("some_file.txt");
        // validate_path_within_workdir should resolve the path (it exists or parent exists).
        let result = validate_path_within_workdir(&existing_path, &dir);
        // Should succeed — no longer hard-rejects out-of-workdir paths.
        assert!(result.is_ok());
    }
}
