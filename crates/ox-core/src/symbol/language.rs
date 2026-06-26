use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

pub struct LanguageRegistry {
    parser: Parser,
    languages: HashMap<String, Language>,
    ext_map: HashMap<&'static str, &'static str>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        let mut ext_map = HashMap::new();
        ext_map.insert("rs", "rust");
        ext_map.insert("py", "python");
        ext_map.insert("js", "javascript");
        ext_map.insert("mjs", "javascript");
        ext_map.insert("cjs", "javascript");
        ext_map.insert("jsx", "javascript");
        ext_map.insert("ts", "typescript");
        ext_map.insert("tsx", "typescript");
        ext_map.insert("cpp", "cpp");
        ext_map.insert("cc", "cpp");
        ext_map.insert("cxx", "cpp");
        ext_map.insert("c", "cpp");
        ext_map.insert("h", "cpp");
        ext_map.insert("hpp", "cpp");
        ext_map.insert("hxx", "cpp");
        ext_map.insert("go", "go");
        ext_map.insert("java", "java");
        ext_map.insert("kt", "kotlin");
        ext_map.insert("rb", "ruby");
        ext_map.insert("php", "php");
        ext_map.insert("swift", "swift");
        ext_map.insert("scala", "scala");

        Self {
            parser: Parser::new(),
            languages: HashMap::new(),
            ext_map,
        }
    }

    pub fn detect_language<P: AsRef<Path>>(&self, path: P) -> Option<&str> {
        let ext = path.as_ref().extension()?.to_str()?;
        self.ext_map.get(ext).copied()
    }

    pub fn get_language(&mut self, lang_name: &str) -> Result<Language> {
        if let Some(lang) = self.languages.get(lang_name) {
            return Ok(lang.clone());
        }

        let language: Language = match lang_name {
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "python" => tree_sitter_python::LANGUAGE.into(),
            "javascript" => tree_sitter_javascript::LANGUAGE.into(),
            "typescript" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            "cpp" => tree_sitter_cpp::LANGUAGE.into(),
            "go" => tree_sitter_go::LANGUAGE.into(),
            "java" => tree_sitter_java::LANGUAGE.into(),
            _ => anyhow::bail!("Unsupported language: {}", lang_name),
        };

        self.languages
            .insert(lang_name.to_string(), language.clone());
        Ok(language)
    }

    pub fn parse(&mut self, code: &str, lang_name: &str) -> Result<Tree> {
        let language = self.get_language(lang_name)?;
        self.parser.set_language(&language)?;
        let tree = self
            .parser
            .parse(code, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse code"))?;
        Ok(tree)
    }

    /// Check code for syntax errors using tree-sitter.
    /// Returns a list of error descriptions with line numbers.
    /// Applies smart filtering to reduce false positives.
    pub fn check_syntax(&mut self, code: &str, lang_name: &str) -> Result<Vec<SyntaxError>> {
        let tree = self.parse(code, lang_name)?;
        let total_lines = code.lines().count();
        let mut errors = Vec::new();
        Self::collect_errors(tree.root_node(), code, total_lines, &mut errors);
        Ok(errors)
    }

    fn collect_errors(
        node: tree_sitter::Node,
        code: &str,
        total_lines: usize,
        errors: &mut Vec<SyntaxError>,
    ) {
        if node.is_error() || node.is_missing() {
            let line = node.start_position().row + 1;
            let col = node.start_position().column + 1;

            // ── Smart filtering: skip common false positives ──

            // 1. Skip "missing" tokens (often incomplete code during editing)
            if node.is_missing() {
                return;
            }

            // 2. Skip ERROR nodes at the very end of file (likely incomplete/truncated)
            if line >= total_lines.saturating_sub(2) {
                let snippet = node.utf8_text(code.as_bytes()).unwrap_or("");
                if snippet.is_empty() || snippet.len() < 5 {
                    return; // Tiny or empty error node at EOF
                }
            }

            // 3. Skip common false positive patterns
            let snippet = node
                .utf8_text(code.as_bytes())
                .unwrap_or("<invalid>")
                .chars()
                .take(80)
                .collect::<String>();

            // Filter: skip if it looks like a comment or string content
            let snippet_trimmed = snippet.trim();
            if snippet_trimmed.starts_with("//")
                || snippet_trimmed.starts_with("/*")
                || snippet_trimmed.starts_with("#")
                || snippet_trimmed.starts_with('"')
            {
                return;
            }

            // Filter: skip single-character error nodes (often whitespace/indent issues)
            if snippet.len() <= 1 {
                return;
            }

            let description = format!("Syntax error at line {}:{}: `{}`", line, col, snippet);
            errors.push(SyntaxError {
                line,
                column: col,
                description,
            });
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_errors(child, code, total_lines, errors);
        }
    }
}

/// A syntax error detected by tree-sitter parsing.
#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub description: String,
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
