//! Cross-language source-code path detection (workflow gates, enforcer, snapshots).
//!
//! Central list — do not duplicate extension tables in agent/tools modules.

use std::path::Path;

/// File extensions treated as **source code** (editing blocked in read-only workflow steps).
/// Covers common compiled / scripted / markup-in-code languages — not Rust/Java-specific.
const SOURCE_EXTENSIONS: &[&str] = &[
    // Rust / systems
    "rs", "zig", "v", "nim", "cr", "odin", "asm", "s",
    // C / C++ / ObjC family
    "c", "cc", "cpp", "cxx", "h", "hpp", "hxx", "ino",
    // Go / D
    "go", "d",
    // JVM
    "java", "kt", "kts", "scala", "groovy",
    // .NET
    "cs", "fs", "fsx", "fsi", "vb",
    // JavaScript / TypeScript / front-end
    "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx",
    "vue", "svelte", "astro",
    // Styles / templates (code-like assets)
    "css", "scss", "sass", "less", "styl",
    "html", "htm", "xhtml",
    // Python / Ruby / Perl / Lua
    "py", "pyw", "pyi", "rb", "rake", "pl", "pm", "lua",
    // PHP / mobile / Dart
    "php", "phtml", "swift", "m", "mm", "dart",
    // Shell / automation
    "sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd",
    // Functional / BEAM / Lisp
    "hs", "lhs", "erl", "hrl", "ex", "exs", "clj", "cljs", "cljc", "edn",
    "ml", "mli", "rkt", "lisp", "cl",
    // R / Julia / MATLAB / Fortran / Pascal / Ada
    "r", "jl", "m", "f", "f90", "f95", "for", "pas", "pp", "ada", "adb", "ads",
    // SQL / API / schema
    "sql", "graphql", "gql", "proto", "thrift",
    // IaC / infra-as-code
    "tf", "hcl",
    // Blockchain / niche
    "sol", "move", "cairo",
];

/// Extensions commonly mentioned in user queries (source + docs + config).
const QUERY_PATH_EXTENSIONS: &[&str] = &[
    // source (subset + overlap ok)
    "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "kt", "scala", "cs",
    "cpp", "c", "h", "hpp", "rb", "php", "swift", "dart", "vue", "svelte",
    "sql", "sh", "ps1", "ex", "exs", "hs", "ml", "r", "jl", "zig", "nim",
    // docs / config / build
    "md", "txt", "rst", "adoc", "toml", "json", "yaml", "yml", "xml",
    "ini", "cfg", "properties", "gradle", "pom", "sbt", "cmake", "mk",
    "dockerfile", "mod", "sum", "lock", "env",
];

/// True when `path` points to a source-code file (any supported language).
pub fn is_source_code_path(path: impl AsRef<Path>) -> bool {
    extension_matches(path.as_ref(), SOURCE_EXTENSIONS)
}

/// True when `path` has an extension we recognize in user queries / lazy-index paths.
pub fn is_query_mentionable_path(path: impl AsRef<Path>) -> bool {
    extension_matches(path.as_ref(), QUERY_PATH_EXTENSIONS)
        || is_source_code_path(path.as_ref())
}

/// Human-readable hint for tool-guard error messages (language-neutral).
pub fn source_code_guard_hint() -> &'static str {
    "source code files in any language (not documentation-only files like .md / .txt)"
}

/// Build alternation for retrieval regex, e.g. `rs|py|java|...`.
pub fn query_path_extensions_regex() -> String {
    let mut exts: Vec<&str> = QUERY_PATH_EXTENSIONS.to_vec();
    for e in SOURCE_EXTENSIONS {
        if !exts.contains(e) {
            exts.push(e);
        }
    }
    exts.sort_unstable();
    exts.dedup();
    exts.join("|")
}

fn extension_matches(path: &Path, allowed: &[&str]) -> bool {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) if !e.is_empty() => e,
        _ => return false,
    };
    allowed.iter().any(|a| a.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_many_languages() {
        for p in [
            "src/main.rs",
            "app/App.java",
            "lib/foo.py",
            "cmd/main.go",
            "Program.cs",
            "page.tsx",
            "view.vue",
            "query.sql",
            "main.kt",
            "lib.swift",
            "app.dart",
            "mod.ex",
            "Main.hs",
            "script.pl",
            "main.cpp",
        ] {
            assert!(is_source_code_path(p), "expected source: {p}");
        }
    }

    #[test]
    fn docs_are_not_source() {
        for p in ["README.md", "config.yaml", "package.json", "notes.txt"] {
            assert!(!is_source_code_path(p), "expected non-source: {p}");
        }
    }
}
