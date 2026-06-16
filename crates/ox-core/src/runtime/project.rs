use std::path::{Path, PathBuf};

/// Marker files/directories that indicate a project root.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    ".oxroot",
    // Rust
    "Cargo.toml",
    // JS / TS / Node (React, Vue, Angular, etc.)
    "package.json",
    "pnpm-workspace.yaml",
    // Go
    "go.mod",
    // Python
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
    "Pipfile",
    // Java / Kotlin
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    // C / C++
    "CMakeLists.txt",
    "Makefile",
    // Ruby / PHP / Dart
    "Gemfile",
    "composer.json",
    "pubspec.yaml",
    // .NET
    "global.json",
];

/// True if `dir` contains any known project marker (any language/ecosystem).
pub fn has_project_markers(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Project root for Ox features (skills, onboarding): detected root, else current working dir.
pub fn effective_project_root(project_root: &Option<PathBuf>, working_dir: &Path) -> PathBuf {
    project_root
        .clone()
        .unwrap_or_else(|| working_dir.to_path_buf())
}

/// Create `.oxroot` + `.ox/skills/` so empty dirs are treated as Ox projects.
pub fn ensure_ox_project_scaffold(dir: &Path) -> std::io::Result<()> {
    let ox = dir.join(".ox");
    std::fs::create_dir_all(ox.join("skills"))?;
    let marker = dir.join(".oxroot");
    if !marker.exists() {
        std::fs::write(
            &marker,
            "# Ox project root marker (any language/stack)\n",
        )?;
    }
    Ok(())
}

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
    let canonical = dunce::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
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
    fn has_project_markers_detects_package_json() {
        let tmp = std::env::temp_dir().join(format!("ox_marker_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(!has_project_markers(&tmp));
        std::fs::write(tmp.join("package.json"), "{}").unwrap();
        assert!(has_project_markers(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
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
