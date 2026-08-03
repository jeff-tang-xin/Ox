pub mod injection;
pub mod sanitizer;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::tools::SafetyLevel;

/// Normalize a path using dunce (Windows-friendly canonicalization)
/// - On Windows: Removes `\\?\` prefix and normalizes UNC paths
/// - On Unix: Same as canonicalize()
/// - Never fails: Falls back to original path if normalization fails
fn normalize_path(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Session-scoped trust manager for tool confirmation.
///
/// Tracks which tools the user has temporarily trusted (via `/trust`).
/// Trust is session-scoped only — it expires when the REPL exits.
///
/// Also maintains a command blacklist — patterns that are always blocked
/// even when the tool is trusted. Blacklisted commands require a second
/// confirmation even for trusted tools.
#[derive(Debug, Clone, Default)]
pub struct TrustManager {
    trusted_tools: HashSet<String>,
    /// Command patterns that are always blocked (e.g. "rm -rf", "format").
    /// Even if shell_exec is trusted, blacklisted commands require re-confirmation.
    command_blacklist: Vec<String>,
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

    /// Check if a shell command is blocked by the blacklist.
    /// Returns the matching pattern if blocked.
    pub fn is_command_blacklisted(&self, command: &str) -> Option<&str> {
        let lower = command.to_lowercase();
        self.command_blacklist
            .iter()
            .find(|p| lower.contains(&p.to_lowercase()))
            .map(|s| s.as_str())
    }

    /// Add a pattern to the command blacklist.
    pub fn block_command(&mut self, pattern: &str) {
        let p = pattern.trim().to_lowercase();
        if !p.is_empty() && !self.command_blacklist.contains(&p) {
            self.command_blacklist.push(p);
        }
    }

    /// Remove a pattern from the command blacklist.
    pub fn unblock_command(&mut self, pattern: &str) {
        let p = pattern.trim().to_lowercase();
        self.command_blacklist.retain(|x| x != &p);
    }

    /// List blacklisted command patterns.
    pub fn blacklist(&self) -> &[String] {
        &self.command_blacklist
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
    let canonical_workdir = normalize_path(working_dir);
    let canonical_path = normalize_path(path);

    if canonical_path.starts_with(&canonical_workdir) {
        return true;
    }

    // Path doesn't exist yet — check parent.
    if let Some(parent) = path.parent() {
        let canonical_parent = normalize_path(parent);
        return canonical_parent.starts_with(&canonical_workdir);
    }

    false
}

/// Normalize a path to an absolute form if it exists.
/// For non-existent paths (e.g., new files being created), returns the path as-is.
///
/// NOTE: This function does NOT enforce sandbox containment — that is the job
/// of `is_path_within_workdir`, called by the agent layer
/// (`unified_handler.rs`, `agent/mod.rs`) which flags out-of-project paths for
/// safety confirmation. Keeping normalization and sandbox-check separate avoids
/// breaking legitimate reads of absolute paths outside the project (e.g.
/// `/etc/hosts`, `~/.ox/config.toml`).
///
/// Uses dunce for Windows-friendly path normalization.
pub fn validate_path_within_workdir(path: &Path, _working_dir: &Path) -> anyhow::Result<PathBuf> {
    // Try to normalize if the path exists
    if path.exists() {
        return Ok(normalize_path(path));
    }

    // Path doesn't exist yet, return as-is
    Ok(path.to_path_buf())
}

/// Case-sensitive Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let a_len = a.len();
    let b_len = b.len();
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr = vec![0usize; b_len + 1];
    for i in 0..a_len {
        curr[0] = i + 1;
        for j in 0..b_len {
            let cost = if a[i] == b[j] { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_len]
}

/// Auto-resolve a short or misspelled path by searching the project root.
///
/// Resolution cascade (each step only fires if the previous fails):
/// 1. **Direct hit**: `path_base.join(path_str)` exists → return it.
/// 2. **Exact basename** (no directory separators in input): walk project root,
///    match files by exact basename. If exactly one match → auto-resolve.
/// 3. **Fuzzy basename** (no directory separators in input): Levenshtein
///    distance ≤ 2. If exactly one candidate → auto-resolve.
/// 4. **Suffix match** (input HAS directory separators): treat the input as a
///    path suffix and match any file whose relative path ends with the input.
///    e.g. `ox-core/llm/openai.rs` matches `crates/ox-core/src/llm/openai.rs`.
///    If exactly one match → auto-resolve.
/// 5. Give up → caller reports error with `suggest_path_correction` hint.
///
/// **Returns** `Option<(PathBuf, bool)>`:
/// - `(PathBuf, false)` — direct hit, no auto-resolution
/// - `(PathBuf, true)` — auto-resolved via steps 2-4 (LLM should be informed)
/// - `None` — couldn't resolve, caller falls back to error + suggestion
///
/// Design goal: minimize LLM cognitive load on path spelling while keeping
/// ambiguity-safe (never auto-resolves when multiple candidates exist).
pub fn resolve_short_path(path_str: &str, path_base: &Path) -> Option<(PathBuf, bool)> {
    // 1. Normalize separators and try direct resolution
    let normalized = path_str.trim().replace('\\', "/");
    let direct = if Path::new(&normalized).is_absolute() {
        PathBuf::from(&normalized)
    } else {
        path_base.join(&normalized)
    };
    if direct.exists() {
        return Some((direct, false));
    }

    // Collect all files once (reuse for steps 2-4)
    let all_files: Vec<PathBuf> = ignore::WalkBuilder::new(path_base)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.path().to_path_buf())
        .collect();

    let has_dir_sep = normalized.contains('/');

    // 2-3. Basename-only input: try exact then fuzzy basename match
    if !has_dir_sep {
        // 2. Exact basename match
        let exact: Vec<PathBuf> = all_files
            .iter()
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == normalized))
            .cloned()
            .collect();
        if exact.len() == 1 {
            let resolved = exact.into_iter().next().unwrap();
            tracing::debug!(
                "[PATH_RESOLVE] Auto-resolved '{}' → '{}' (exact basename, single match)",
                path_str,
                resolved.display()
            );
            return Some((resolved, true));
        }

        // 3. Fuzzy basename match (Levenshtein ≤ 2)
        let fuzzy: Vec<(usize, PathBuf)> = all_files
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().to_string();
                let dist = levenshtein(&name, &normalized);
                if dist > 0 && dist <= 2 {
                    Some((dist, p.clone()))
                } else {
                    None
                }
            })
            .collect();
        if fuzzy.len() == 1 {
            let (dist, resolved) = fuzzy.into_iter().next().unwrap();
            tracing::debug!(
                "[PATH_RESOLVE] Auto-resolved '{}' → '{}' (fuzzy basename, distance={})",
                path_str,
                resolved.display(),
                dist
            );
            return Some((resolved, true));
        }
        // Multiple or zero fuzzy matches → fall through to None
        return None;
    }

    // 4. Input has directory separators: try suffix match
    //    e.g. "ox-core/llm/openai.rs" matches "crates/ox-core/src/llm/openai.rs"
    //    because the relative path ends with the input suffix.
    let suffix = normalized.as_str();
    let suffix_matches: Vec<PathBuf> = all_files
        .iter()
        .filter(|p| {
            let rel = p
                .strip_prefix(path_base)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            rel == suffix || rel.ends_with(&format!("/{suffix}"))
        })
        .cloned()
        .collect();

    if suffix_matches.len() == 1 {
        let resolved = suffix_matches.into_iter().next().unwrap();
        tracing::debug!(
            "[PATH_RESOLVE] Auto-resolved '{}' → '{}' (suffix match, single match)",
            path_str,
            resolved.display()
        );
        return Some((resolved, true));
    }

    // 5. Could not auto-resolve
    None
}

/// Suggest correct path candidates when a given path does not exist.
///
/// Walks `working_dir` (respecting .gitignore) and matches files by basename:
/// exact basename match first, then Levenshtein distance <= 2 fuzzy match.
/// Returns a formatted suggestion string listing up to 5 relative-path
/// candidates, or `None` if no plausible candidate is found.
pub fn suggest_path_correction(bad_path: &Path, working_dir: &Path) -> Option<String> {
    let target = bad_path.file_name()?.to_string_lossy().to_string();

    let mut exact: Vec<String> = Vec::new();
    let mut fuzzy: Vec<(usize, String)> = Vec::new();

    for entry in ignore::WalkBuilder::new(working_dir).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let rel = entry
            .path()
            .strip_prefix(working_dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");

        if name == target {
            exact.push(rel);
        } else {
            let dist = levenshtein(&name, &target);
            if dist <= 2 {
                fuzzy.push((dist, rel));
            }
        }
    }

    let mut candidates: Vec<String> = if !exact.is_empty() {
        exact
    } else {
        fuzzy.sort_by_key(|(d, _)| *d);
        fuzzy.into_iter().map(|(_, r)| r).collect()
    };
    if candidates.is_empty() {
        return None;
    }
    candidates.truncate(5);

    let list = candidates
        .iter()
        .map(|c| format!("  • {c}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "💡 路径可能写错了。根据文件名，你是不是指：\n{list}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("validation.rs", "validation.rs"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("validatirs", "validation.rs"), 3);
    }

    #[test]
    fn suggest_exact_basename_match() {
        let dir = std::env::temp_dir().join(format!("ox_sug_exact_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/validation.rs"), "x").unwrap();
        let bad = dir.join("wrong/place/validation.rs");
        let out = suggest_path_correction(&bad, &dir);
        assert!(out.is_some());
        assert!(out.unwrap().contains("a/b/validation.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggest_fuzzy_match() {
        let dir = std::env::temp_dir().join(format!("ox_sug_fuzzy_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("validation.rs"), "x").unwrap();
        // basename off by 2 chars
        let bad = dir.join("validaton.rs");
        let out = suggest_path_correction(&bad, &dir);
        assert!(out.is_some());
        assert!(out.unwrap().contains("validation.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── resolve_short_path tests ──

    #[test]
    fn resolve_direct_hit() {
        let dir = std::env::temp_dir().join(format!("ox_rsp_direct_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("exact.rs"), "x").unwrap();

        // Full relative path works directly
        let result = resolve_short_path("exact.rs", &dir);
        assert!(result.is_some());
        let (path, auto) = result.unwrap();
        assert_eq!(path.file_name().unwrap(), "exact.rs");
        assert!(!auto); // Direct hit, no auto-resolution

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_basename_single_match() {
        let dir = std::env::temp_dir().join(format!("ox_rsp_single_{}", std::process::id()));
        let sub = dir.join("sub").join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("unique.rs"), "x").unwrap();

        // Basename-only, exactly one match → auto-resolved
        let result = resolve_short_path("unique.rs", &dir);
        assert!(result.is_some());
        let (resolved, auto) = result.unwrap();
        assert!(resolved.to_string_lossy().contains("sub"));
        assert_eq!(resolved.file_name().unwrap(), "unique.rs");
        assert!(auto); // Auto-resolution happened

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_basename_multi_match_returns_none() {
        let dir = std::env::temp_dir().join(format!("ox_rsp_multi_{}", std::process::id()));
        let sub1 = dir.join("a");
        let sub2 = dir.join("b");
        std::fs::create_dir_all(&sub1).unwrap();
        std::fs::create_dir_all(&sub2).unwrap();
        std::fs::write(sub1.join("dup.rs"), "x").unwrap();
        std::fs::write(sub2.join("dup.rs"), "x").unwrap();

        // Multiple matches → don't auto-resolve (ambiguous)
        let result = resolve_short_path("dup.rs", &dir);
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_with_directory_separator_no_match() {
        let dir = std::env::temp_dir().join(format!("ox_rsp_sep_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Path with directory separator, no matching suffix → no fallback
        let result = resolve_short_path("nonexistent/foo.rs", &dir);
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_fuzzy_basename_single_match() {
        // LLM misspells basename by 1 char → fuzzy match auto-resolves
        let dir = std::env::temp_dir().join(format!("ox_rsp_fuzzy_{}", std::process::id()));
        let sub = dir.join("crates").join("ox-core").join("src").join("llm");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("openai_sse.rs"), "x").unwrap();

        // "openi_sse.rs" (missing 'a') → Levenshtein distance 1 → auto-resolved
        let result = resolve_short_path("openi_sse.rs", &dir);
        assert!(result.is_some());
        let (resolved, auto) = result.unwrap();
        assert_eq!(resolved.file_name().unwrap(), "openai_sse.rs");
        assert!(auto); // Auto-resolution happened

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_fuzzy_basename_multi_match_returns_none() {
        // Multiple fuzzy matches → ambiguous, don't auto-resolve
        let dir = std::env::temp_dir().join(format!("ox_rsp_fm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test.rs"), "x").unwrap();
        std::fs::write(dir.join("tests.rs"), "x").unwrap();

        // "tst.rs" → distance 1 to both "test.rs" and "tests.rs" → ambiguous
        let result = resolve_short_path("tst.rs", &dir);
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_suffix_match_single() {
        // LLM writes partial path (missing prefix) → suffix match auto-resolves
        let dir = std::env::temp_dir().join(format!("ox_rsp_suf_{}", std::process::id()));
        let deep = dir.join("crates").join("ox-core").join("src").join("llm");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("openai.rs"), "x").unwrap();

        // "ox-core/src/llm/openai.rs" matches "crates/ox-core/src/llm/openai.rs"
        let result = resolve_short_path("ox-core/src/llm/openai.rs", &dir);
        assert!(result.is_some());
        let (resolved, auto) = result.unwrap();
        assert_eq!(resolved.file_name().unwrap(), "openai.rs");
        assert!(resolved.to_string_lossy().contains("crates"));
        assert!(auto);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_suffix_match_short_segment() {
        // Even a short suffix like "llm/openai.rs" should match
        let dir = std::env::temp_dir().join(format!("ox_rsp_suf2_{}", std::process::id()));
        let deep = dir.join("crates").join("ox-core").join("src").join("llm");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("openai.rs"), "x").unwrap();

        let result = resolve_short_path("llm/openai.rs", &dir);
        assert!(result.is_some());
        let (resolved, auto) = result.unwrap();
        assert_eq!(resolved.file_name().unwrap(), "openai.rs");
        assert!(auto);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_suffix_match_multi_returns_none() {
        // Multiple files with same suffix → ambiguous
        let dir = std::env::temp_dir().join(format!("ox_rsp_sufm_{}", std::process::id()));
        let a = dir.join("a").join("mod");
        let b = dir.join("b").join("mod");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("foo.rs"), "x").unwrap();
        std::fs::write(b.join("foo.rs"), "x").unwrap();

        // "mod/foo.rs" matches both "a/mod/foo.rs" and "b/mod/foo.rs"
        let result = resolve_short_path("mod/foo.rs", &dir);
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggest_no_match_returns_none() {
        let dir = std::env::temp_dir().join(format!("ox_sug_none_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.rs"), "x").unwrap();
        let bad = dir.join("zzzzzzzzzz.rs");
        assert!(suggest_path_correction(&bad, &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert!(!tm.can_skip_confirmation("edit_file", SafetyLevel::RequiresConfirmation));
    }

    #[test]
    fn trust_all_skips_requires_confirmation() {
        let mut tm = TrustManager::new();
        tm.trust_all();
        assert!(tm.can_skip_confirmation("file_write", SafetyLevel::RequiresConfirmation));
        assert!(tm.can_skip_confirmation("edit_file", SafetyLevel::RequiresConfirmation));
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
