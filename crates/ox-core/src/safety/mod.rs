use std::collections::HashSet;

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
                self.trusted_tools.contains(tool_name)
                    || self.trusted_tools.contains("__all__")
            }
            // Dangerous tools always require confirmation.
            SafetyLevel::Dangerous => false,
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
    pub fn trusted_list(&self) -> Vec<&str> {
        self.trusted_tools.iter().map(|s| s.as_str()).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_always_skips() {
        let tm = TrustManager::new();
        assert!(tm.can_skip_confirmation("file_read", SafetyLevel::Safe));
    }

    #[test]
    fn dangerous_never_skips() {
        let mut tm = TrustManager::new();
        tm.trust("shell_exec");
        tm.trust_all();
        assert!(!tm.can_skip_confirmation("shell_exec", SafetyLevel::Dangerous));
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
}
