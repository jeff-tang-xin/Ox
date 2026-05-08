use std::path::{Path, PathBuf};

/// Marker files/directories that indicate a project root.
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "CMakeLists.txt",
    ".oxroot", // explicit Ox project marker
];

/// Walk up from `start_dir` looking for any project marker.
/// Returns the first directory containing a marker, or `None`.
pub fn find_project_root(start_dir: &Path) -> Option<PathBuf> {
    let mut current = start_dir.to_path_buf();
    loop {
        for marker in PROJECT_MARKERS {
            if current.join(marker).exists() {
                return Some(current);
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Compute a deterministic project ID from the project root (or cwd if no root).
/// Uses blake3 hash of the canonical path, truncated to 16 hex chars.
pub fn compute_project_id(project_root: &Option<PathBuf>, cwd: &Path) -> String {
    let base = project_root.as_deref().unwrap_or(cwd);
    let canonical = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let hash = blake3::hash(canonical.to_string_lossy().as_bytes());
    hash.to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_project_root_finds_cargo_toml() {
        // This test runs from within the Ox project, so it should find the workspace root.
        let cwd = std::env::current_dir().unwrap();
        let root = find_project_root(&cwd);
        assert!(root.is_some(), "Should find project root");
        let root = root.unwrap();
        assert!(
            root.join("Cargo.toml").exists(),
            "Root should contain Cargo.toml"
        );
    }

    #[test]
    fn compute_project_id_is_deterministic() {
        let path = Some(PathBuf::from("."));
        let cwd = std::env::current_dir().unwrap();
        let id1 = compute_project_id(&path, &cwd);
        let id2 = compute_project_id(&path, &cwd);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 16);
    }
}
