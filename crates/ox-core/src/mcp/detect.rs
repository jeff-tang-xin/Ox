//! Startup detection for the GitNexus toolchain.
//!
//! Two independent things are probed:
//! 1. **Launchability** — can we resolve the configured `command`? (absolute
//!    path exists, or it resolves on `PATH`).
//! 2. **Node runtime** — is `node`/`npx` available? Only relevant when the
//!    launcher relies on Node (the default `npx` path). Used purely to produce
//!    an actionable hint when the launcher can't be found.
//!
//! Detection never fails the program: if GitNexus isn't launchable, Ox degrades
//! gracefully (the rest of the agent keeps working without the code graph).

use std::path::PathBuf;

use crate::config::GitNexusConfig;

/// Outcome of probing the GitNexus toolchain.
#[derive(Debug, Clone)]
pub struct GitNexusAvailability {
    /// Whether the integration is enabled in config.
    pub enabled: bool,
    /// The configured launcher command (e.g. `npx`, `gitnexus`, absolute path).
    pub command: String,
    /// Resolved absolute path to the launcher, if found.
    pub resolved_path: Option<PathBuf>,
    /// Whether the launcher could be resolved (path exists or found on PATH).
    pub command_found: bool,
    /// Whether `node` is on PATH.
    pub node_found: bool,
    /// Whether `npx` is on PATH.
    pub npx_found: bool,
}

impl GitNexusAvailability {
    /// Can we actually spawn GitNexus right now?
    pub fn is_launchable(&self) -> bool {
        self.enabled && self.command_found
    }

    /// One-line, user-facing status suitable for a startup banner.
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "GitNexus: disabled".into();
        }
        if self.is_launchable() {
            match &self.resolved_path {
                Some(p) => format!("GitNexus: ready ({})", p.display()),
                None => format!("GitNexus: ready ({})", self.command),
            }
        } else {
            "GitNexus: unavailable".into()
        }
    }

    /// Actionable hint when GitNexus can't be launched, else `None`.
    pub fn hint(&self) -> Option<String> {
        if !self.enabled || self.is_launchable() {
            return None;
        }
        // Launcher not found. Tailor the message to the likely cause.
        let needs_node = self.command == "npx" || self.command == "node";
        if needs_node && !self.node_found {
            Some(format!(
                "GitNexus launcher `{}` not found and Node.js is not installed. \
                 Install Node.js (which provides `npx`), or set [gitnexus] command \
                 to an installed `gitnexus` binary in ~/.ox/config.toml.",
                self.command
            ))
        } else {
            Some(format!(
                "GitNexus launcher `{}` could not be resolved. Check that it is on \
                 PATH or set an absolute path in [gitnexus] command (~/.ox/config.toml). \
                 Code-graph features will be unavailable this session.",
                self.command
            ))
        }
    }
}

/// Resolve a command to an absolute path: honor an explicit path, otherwise
/// search `PATH` (with platform extension handling via the `which` crate).
fn resolve_command(command: &str) -> Option<PathBuf> {
    let p = PathBuf::from(command);
    // Explicit path (absolute or contains a separator) → check existence.
    if p.is_absolute() || command.contains('/') || command.contains('\\') {
        return if p.exists() { Some(p) } else { None };
    }
    which::which(command).ok()
}

/// Probe the GitNexus toolchain without spawning anything.
pub fn detect(cfg: &GitNexusConfig) -> GitNexusAvailability {
    let node_found = which::which("node").is_ok();
    let npx_found = which::which("npx").is_ok();
    let resolved_path = if cfg.enabled {
        resolve_command(&cfg.command)
    } else {
        None
    };

    GitNexusAvailability {
        enabled: cfg.enabled,
        command: cfg.command.clone(),
        command_found: resolved_path.is_some(),
        resolved_path,
        node_found,
        npx_found,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_not_launchable() {
        let cfg = GitNexusConfig {
            enabled: false,
            ..Default::default()
        };
        let a = detect(&cfg);
        assert!(!a.is_launchable());
        assert!(a.hint().is_none());
        assert_eq!(a.summary(), "GitNexus: disabled");
    }

    #[test]
    fn absolute_nonexistent_path_is_not_found() {
        let cfg = GitNexusConfig {
            enabled: true,
            command: if cfg!(windows) {
                "Z:\\nope\\gitnexus.cmd".into()
            } else {
                "/nonexistent/bin/gitnexus".into()
            },
            ..Default::default()
        };
        let a = detect(&cfg);
        assert!(!a.command_found);
        assert!(!a.is_launchable());
        assert!(a.hint().is_some());
    }
}
