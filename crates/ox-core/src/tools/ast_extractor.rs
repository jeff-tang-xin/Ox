//! Standalone AST symbol extractor — tree-sitter multi-language parsing.
//!
//! Independent of `knowledge/` so that `find_symbol` / `read_symbol` can work
//! with tree-sitter alone, without dragging in the embedding/graph stack.
//! Migrated from `knowledge/{extractor,language}.rs` on 2026-07-17.

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

// ── CodeSymbolInfo: flat DTO consumed by tools ──

#[derive(Debug, Clone)]
pub struct CodeSymbolInfo {
    pub name: String,
    pub fq_name: String,
    /// Human-readable kind: "function" | "struct" | "class" | ... (see extract_* fns)
    pub symbol_type: String,
    pub language: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub parent: Option<String>,
    /// Fully-qualified names of symbols called from within this one's body.
    pub calls: Vec<String>,
}

// ── SyntaxError: DTO from tree-sitter check_syntax ──

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub description: String,
}

// ── LanguageRegistry: extension → grammar ──

pub struct LanguageRegistry {
    parser: Parser,
    languages: HashMap<String, Language>,
    ext_map: HashMap<&'static str, &'static str>,
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn check_syntax(&mut self, code: &str, lang_name: &str) -> Result<Vec<SyntaxError>> {
        let tree = self.parse(code, lang_name)?;
        let total_lines = code.lines().count();
        let mut errors = Vec::new();
        Self::collect_errors(tree.root_node(), code, total_lines, &mut errors);
        Ok(errors)
    }

    fn collect_errors(
        node: Node,
        code: &str,
        total_lines: usize,
        errors: &mut Vec<SyntaxError>,
    ) {
        if node.is_error() || node.is_missing() {
            let line = node.start_position().row + 1;
            let col = node.start_position().column + 1;
            if node.is_missing() {
                return;
            }
            if line >= total_lines.saturating_sub(2) {
                let snippet = node.utf8_text(code.as_bytes()).unwrap_or("");
                if snippet.is_empty() || snippet.len() < 5 {
                    return;
                }
            }
            let snippet = node
                .utf8_text(code.as_bytes())
                .unwrap_or("<invalid>")
                .chars()
                .take(80)
                .collect::<String>();
            let trimmed = snippet.trim();
            if trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("#")
                || trimmed.starts_with('"')
            {
                return;
            }
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

// ── AstExtractor: produces CodeSymbolInfo per file ──

pub struct AstExtractor {
    registry: LanguageRegistry,
}

impl Default for AstExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl AstExtractor {
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
        }
    }

    pub fn detect_language<P: AsRef<Path>>(&self, path: P) -> Option<&str> {
        self.registry.detect_language(path)
    }

    /// Extract flat symbol records from a source file.
    pub fn extract_symbols<P: AsRef<Path>>(
        &mut self,
        path: P,
        code: &str,
    ) -> Result<Vec<CodeSymbolInfo>> {
        let path = path.as_ref();
        let lang_name = self
            .registry
            .detect_language(path)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type: {:?}", path))?
            .to_string();
        let tree = self.registry.parse(code, &lang_name)?;
        let mut out = Vec::new();
        self.walk(tree.root_node(), code, &lang_name, path, None, &mut out);
        self.resolve_calls(&mut out);
        Ok(out)
    }

    fn walk(
        &self,
        node: Node,
        code: &str,
        lang: &str,
        path: &Path,
        parent: Option<String>,
        out: &mut Vec<CodeSymbolInfo>,
    ) {
        if let Some(sym) = self.node_to_symbol(node, code, lang, path, parent.clone()) {
            out.push(sym);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_parent =
                if child.kind().ends_with("_item") || child.kind().ends_with("_declaration") {
                    parent.clone()
                } else {
                    self.get_node_name(node, code).or(parent.clone())
                };
            self.walk(child, code, lang, path, child_parent, out);
        }
    }

    fn node_to_symbol(
        &self,
        node: Node,
        code: &str,
        lang: &str,
        path: &Path,
        parent: Option<String>,
    ) -> Option<CodeSymbolInfo> {
        let (kind, name) = match lang {
            "rust" => extract_rust(node, code, self)?,
            "python" => extract_python(node, code, self)?,
            "javascript" => extract_js(node, code, self)?,
            "typescript" => extract_ts(node, code, self)?,
            "cpp" => extract_cpp(node, code, self)?,
            "go" => extract_go(node, code, self)?,
            "java" => extract_java(node, code, self)?,
            _ => return None,
        };
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let signature = self.get_declaration_header(node, code);
        let fq_name = if let Some(ref p) = parent {
            format!("{}::{}", p, name)
        } else {
            name.clone()
        };
        Some(CodeSymbolInfo {
            name,
            fq_name,
            symbol_type: kind.to_string(),
            language: lang.to_string(),
            file_path: path.to_string_lossy().to_string(),
            start_line,
            end_line,
            signature,
            parent,
            calls: Vec::new(),
        })
    }

    fn resolve_calls(&self, symbols: &mut [CodeSymbolInfo]) {
        let defined: Vec<String> = symbols.iter().map(|s| s.fq_name.clone()).collect();
        for sym in symbols.iter_mut() {
            let sig_lower = sym.signature.to_lowercase();
            for def in &defined {
                let short = def.rsplit("::").next().unwrap_or(def);
                if sig_lower.contains(&short.to_lowercase()) && !sym.calls.contains(def) {
                    sym.calls.push(def.clone());
                }
            }
        }
    }

    fn get_node_name(&self, node: Node, code: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let kind = child.kind();
            if kind.contains("identifier") || kind.contains("name") {
                return Some(self.get_node_text(child, code));
            }
        }
        None
    }

    fn get_node_text(&self, node: Node, code: &str) -> String {
        node.utf8_text(code.as_bytes())
            .unwrap_or_default()
            .to_string()
    }

    fn get_declaration_header(&self, node: Node, code: &str) -> String {
        let full = self.get_node_text(node, code);
        let node_start = node.start_byte();
        let body_kinds = [
            "class_body",
            "interface_body",
            "enum_body",
            "block",
            "declaration_list",
            "field_declaration_list",
            "statement_block",
            "compound_statement",
            "function_body",
            "method_body",
        ];
        let mut header_end = full.len();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if body_kinds.contains(&child.kind()) {
                let rel = child.start_byte().saturating_sub(node_start);
                if rel < header_end {
                    header_end = rel;
                }
            }
        }
        let header = if header_end < full.len() {
            full[..header_end].trim()
        } else if let Some(idx) = full.find('{') {
            full[..idx].trim()
        } else {
            full.trim()
        };
        header.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

// ── Language-specific: (node) → (kind_string, name) ──

fn extract_rust(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "function_item" => Some(("function", e.get_node_name(node, code)?)),
        "impl_item" => Some((
            "impl",
            e.get_node_name(node, code).unwrap_or_else(|| "<impl>".into()),
        )),
        "struct_item" => Some(("struct", e.get_node_name(node, code)?)),
        "enum_item" => Some(("enum", e.get_node_name(node, code)?)),
        "trait_item" => Some(("trait", e.get_node_name(node, code)?)),
        "type_alias" => Some(("type", e.get_node_name(node, code)?)),
        "const_item" => Some(("const", e.get_node_name(node, code)?)),
        "static_item" => Some(("static", e.get_node_name(node, code)?)),
        "mod_item" => Some(("module", e.get_node_name(node, code)?)),
        "macro_definition" => Some(("macro", e.get_node_name(node, code)?)),
        "enum_variant" => Some(("variant", e.get_node_name(node, code)?)),
        _ => None,
    }
}

fn extract_python(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "function_definition" => Some(("function", e.get_node_name(node, code)?)),
        "class_definition" => Some(("class", e.get_node_name(node, code)?)),
        _ => None,
    }
}

fn extract_js(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "function_declaration" => Some(("function", e.get_node_name(node, code)?)),
        "class_declaration" => Some(("class", e.get_node_name(node, code)?)),
        "method_definition" => Some(("method", e.get_node_name(node, code)?)),
        "arrow_function" => Some(("function", e.get_node_name(node, code)?)),
        _ => None,
    }
}

fn extract_ts(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "function_declaration" => Some(("function", e.get_node_name(node, code)?)),
        "class_declaration" => Some(("class", e.get_node_name(node, code)?)),
        "method_definition" => Some(("method", e.get_node_name(node, code)?)),
        "interface_declaration" => Some(("interface", e.get_node_name(node, code)?)),
        "type_alias_declaration" => Some(("type", e.get_node_name(node, code)?)),
        "enum_declaration" => Some(("enum", e.get_node_name(node, code)?)),
        _ => extract_js(node, code, e),
    }
}

fn extract_cpp(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "function_definition" => Some(("function", e.get_node_name(node, code)?)),
        "class_specifier" => Some(("class", e.get_node_name(node, code)?)),
        "struct_specifier" => Some(("struct", e.get_node_name(node, code)?)),
        "enum_specifier" => Some(("enum", e.get_node_name(node, code)?)),
        "namespace_definition" => Some(("namespace", e.get_node_name(node, code)?)),
        _ => None,
    }
}

fn extract_go(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "function_declaration" => Some(("function", e.get_node_name(node, code)?)),
        "method_declaration" => Some(("method", e.get_node_name(node, code)?)),
        "type_declaration" => Some(("type", e.get_node_name(node, code)?)),
        "interface_type" => Some(("interface", e.get_node_name(node, code)?)),
        "struct_type" => Some(("struct", e.get_node_name(node, code)?)),
        _ => None,
    }
}

fn extract_java(node: Node, code: &str, e: &AstExtractor) -> Option<(&'static str, String)> {
    match node.kind() {
        "class_declaration" => Some(("class", e.get_node_name(node, code)?)),
        "interface_declaration" => Some(("interface", e.get_node_name(node, code)?)),
        "method_declaration" => Some(("method", e.get_node_name(node, code)?)),
        "enum_declaration" => Some(("enum", e.get_node_name(node, code)?)),
        "package_declaration" => Some(("package", e.get_node_name(node, code)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let code = r#"
fn main() { println!("hi"); }
pub fn validate(t: &str) -> bool { true }
struct User { name: String }
impl User { fn new(name: String) -> Self { Self { name } } }
"#;
        let mut e = AstExtractor::new();
        let syms = e.extract_symbols("src/main.rs", code).unwrap();
        assert!(syms.iter().any(|s| s.symbol_type == "function" && s.name == "main"));
        assert!(syms.iter().any(|s| s.symbol_type == "struct" && s.name == "User"));
    }

    #[test]
    fn extracts_python_symbols() {
        let code = r#"
def hello(): print("hi")
class Calc:
    def add(self, a, b): return a + b
"#;
        let mut e = AstExtractor::new();
        let syms = e.extract_symbols("m.py", code).unwrap();
        assert!(syms.iter().any(|s| s.symbol_type == "function" && s.name == "hello"));
        assert!(syms.iter().any(|s| s.symbol_type == "class" && s.name == "Calc"));
    }

    #[test]
    fn declaration_header_excludes_body() {
        let code = r#"
public class BigService implements Runnable {
    private final String id;
    public void run() { }
}
"#;
        let mut e = AstExtractor::new();
        let syms = e.extract_symbols("BigService.java", code).unwrap();
        let cls = syms
            .iter()
            .find(|s| s.symbol_type == "class")
            .expect("class");
        assert!(cls.signature.contains("class BigService"));
        assert!(!cls.signature.contains("private final"));
        assert!(!cls.signature.contains("public void run"));
    }
}
