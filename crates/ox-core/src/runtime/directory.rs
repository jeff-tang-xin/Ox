use std::path::{Path, PathBuf};

use super::{RuntimeEnvironment, compute_project_id, find_project_root};

/// Result of a directory change attempt.
#[derive(Debug)]
pub enum DirectoryChangeResult {
    /// Successfully changed to the new directory.
    Success {
        new_dir: PathBuf,
        project_changed: bool,
    },
    /// The target directory does not exist.
    NotFound(String),
    /// Error during directory change.
    Error(String),
}

/// Handle a `/cd` command: resolve the path, detect project boundary, update RuntimeEnvironment.
///
/// Returns the result and mutates `rt_env` in place on success.
pub fn change_directory(rt_env: &mut RuntimeEnvironment, target: &str) -> DirectoryChangeResult {
    let target_path = if target == "~" || target.starts_with("~/") {
        let home = &rt_env.home_dir;
        if target == "~" {
            home.clone()
        } else {
            home.join(&target[2..])
        }
    } else {
        let p = Path::new(target);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            rt_env.working_dir.join(p)
        }
    };

    // Canonicalize to resolve .. and symlinks.
    let canonical = match target_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return DirectoryChangeResult::NotFound(format!(
                "Cannot access '{}': {e}",
                target_path.display()
            ));
        }
    };

    if !canonical.is_dir() {
        return DirectoryChangeResult::NotFound(format!(
            "'{}' is not a directory",
            canonical.display()
        ));
    }

    // Detect if we crossed a project boundary.
    let new_project_root = find_project_root(&canonical);
    let project_changed = new_project_root != rt_env.project_root;

    // Update the runtime environment.
    rt_env.working_dir = canonical.clone();
    rt_env.project_root = new_project_root.clone();
    rt_env.project_id = compute_project_id(&rt_env.project_root, &rt_env.working_dir);
    rt_env.project_ox_dir = new_project_root.as_ref().map(|r| r.join(".ox"));

    // Also update the actual process working directory.
    if let Err(e) = std::env::set_current_dir(&canonical) {
        return DirectoryChangeResult::Error(format!(
            "Changed internal state but failed to set process cwd: {e}"
        ));
    }

    DirectoryChangeResult::Success {
        new_dir: canonical,
        project_changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::detect_runtime;

    #[test]
    fn change_to_parent_directory() {
        let mut rt_env = detect_runtime();
        let original = rt_env.working_dir.clone();

        let result = change_directory(&mut rt_env, "..");
        match result {
            DirectoryChangeResult::Success { new_dir, .. } => {
                assert_ne!(new_dir, original);
                assert_eq!(rt_env.working_dir, new_dir);
            }
            other => panic!("Expected Success, got: {other:?}"),
        }

        // Restore.
        let _ = std::env::set_current_dir(&original);
    }

    #[test]
    fn change_to_nonexistent_directory() {
        let mut rt_env = detect_runtime();
        let result = change_directory(&mut rt_env, "/nonexistent_dir_xyz_12345");
        assert!(matches!(result, DirectoryChangeResult::NotFound(_)));
    }
}
