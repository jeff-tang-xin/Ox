use std::path::Path;
use tree_sitter::Node;
use crate::symbol::types::{Symbol, SymbolType};
use crate::symbol::language::{LanguageRegistry, SyntaxError};

pub struct AstExtractor {
    registry: LanguageRegistry,
}

impl AstExtractor {
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
        }
    }

    pub fn extract_symbols<P: AsRef<Path>>(
        &mut self,
        path: P,
        code: &str,
    ) -> anyhow::Result<Vec<Symbol>> {
        let path = path.as_ref();
        let lang_name = self.registry.detect_language(path)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type: {:?}", path))?
            .to_string();

        let tree = self.registry.parse(code, &lang_name)?;
        let mut symbols = Vec::new();

        self.extract_from_node(
            tree.root_node(),
            code,
            &lang_name,
            path,
            None,
            &mut symbols,
        );

        Ok(symbols)
    }

    /// Detect language from file extension.
    pub fn detect_language<P: AsRef<Path>>(&self, path: P) -> Option<&str> {
        self.registry.detect_language(path)
    }

    /// Check code for syntax errors without full symbol extraction.
    pub fn check_syntax(&mut self, code: &str, lang_name: &str) -> anyhow::Result<Vec<SyntaxError>> {
        self.registry.check_syntax(code, lang_name)
    }

    fn extract_from_node(
        &self,
        node: Node,
        code: &str,
        lang_name: &str,
        path: &Path,
        parent: Option<String>,
        symbols: &mut Vec<Symbol>,
    ) {
        if let Some(symbol) = self.node_to_symbol(node, code, lang_name, path, parent.clone()) {
            symbols.push(symbol);
        }

        // Determine parent for children
        let child_parent = if matches!(
            node.kind(),
            "function_item" | "function_signature_item" | "function_declaration"
                | "function_definition" | "class_declaration" | "class_definition"
                | "class_specifier" | "struct_item" | "struct_specifier"
                | "enum_item" | "enum_declaration" | "enum_specifier"
                | "trait_item" | "interface_declaration" | "interface_type"
                | "impl_item" | "namespace_definition" | "mod_item"
        ) {
            self.get_node_name(node, code)
        } else {
            parent
        };

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.extract_from_node(child, code, lang_name, path, child_parent.clone(), symbols);
        }
    }

    fn node_to_symbol(
        &self,
        node: Node,
        code: &str,
        lang_name: &str,
        path: &Path,
        parent: Option<String>,
    ) -> Option<Symbol> {
        let (kind, name) = match lang_name {
            "rust" => self.extract_rust_symbol(node, code)?,
            "python" => self.extract_python_symbol(node, code)?,
            "javascript" => self.extract_js_symbol(node, code)?,
            "typescript" => self.extract_ts_symbol(node, code)?,
            "cpp" => self.extract_cpp_symbol(node, code)?,
            "go" => self.extract_go_symbol(node, code)?,
            "java" => self.extract_java_symbol(node, code)?,
            _ => return None,
        };

        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        let signature = self.get_node_text(node, code);

        Some(Symbol {
            name,
            kind,
            start_line,
            end_line,
            file_path: path.to_string_lossy().to_string(),
            language: lang_name.to_string(),
            signature,
            parent,
        })
    }

    fn get_node_name(&self, node: Node, code: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let k = child.kind();
            if k.contains("identifier") || k.contains("name") || k == "field_identifier" {
                return Some(self.get_node_text(child, code));
            }
        }
        None
    }

    fn get_node_text(&self, node: Node, code: &str) -> String {
        node.utf8_text(code.as_bytes())
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect()
    }

    // ── Language-specific extractors ─────────────────────────────

    fn extract_rust_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_item" | "function_signature_item" => {
                Some((SymbolType::Function, self.get_node_name(node, code)?))
            }
            "struct_item" => Some((SymbolType::Struct, self.get_node_name(node, code)?)),
            "enum_item" => Some((SymbolType::Enum, self.get_node_name(node, code)?)),
            "trait_item" => Some((SymbolType::Trait, self.get_node_name(node, code)?)),
            "impl_item" => {
                let name = self.get_node_name(node, code).unwrap_or_else(|| "<impl>".to_string());
                Some((SymbolType::Impl, name))
            }
            "type_item" => Some((SymbolType::TypeAlias, self.get_node_name(node, code)?)),
            "const_item" => Some((SymbolType::Constant, self.get_node_name(node, code)?)),
            "static_item" => Some((SymbolType::Static, self.get_node_name(node, code)?)),
            "mod_item" => Some((SymbolType::Module, self.get_node_name(node, code)?)),
            "macro_definition" => Some((SymbolType::Macro, self.get_node_name(node, code)?)),
            "enum_variant" => Some((SymbolType::Variant, self.get_node_name(node, code)?)),
            _ => None,
        }
    }

    fn extract_python_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_definition" => Some((SymbolType::Function, self.get_node_name(node, code)?)),
            "class_definition" => Some((SymbolType::Class, self.get_node_name(node, code)?)),
            _ => None,
        }
    }

    fn extract_js_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_declaration" => Some((SymbolType::Function, self.get_node_name(node, code)?)),
            "class_declaration" => Some((SymbolType::Class, self.get_node_name(node, code)?)),
            "method_definition" => Some((SymbolType::Method, self.get_node_name(node, code)?)),
            "arrow_function" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            _ => None,
        }
    }

    fn extract_ts_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_declaration" => Some((SymbolType::Function, self.get_node_name(node, code)?)),
            "class_declaration" => Some((SymbolType::Class, self.get_node_name(node, code)?)),
            "method_definition" => Some((SymbolType::Method, self.get_node_name(node, code)?)),
            "interface_declaration" => Some((SymbolType::Interface, self.get_node_name(node, code)?)),
            "type_alias_declaration" => Some((SymbolType::TypeAlias, self.get_node_name(node, code)?)),
            "enum_declaration" => Some((SymbolType::Enum, self.get_node_name(node, code)?)),
            _ => self.extract_js_symbol(node, code),
        }
    }

    fn extract_cpp_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_definition" => Some((SymbolType::Function, self.get_node_name(node, code)?)),
            "class_specifier" => Some((SymbolType::Class, self.get_node_name(node, code)?)),
            "struct_specifier" => Some((SymbolType::Struct, self.get_node_name(node, code)?)),
            "enum_specifier" => Some((SymbolType::Enum, self.get_node_name(node, code)?)),
            "namespace_definition" => Some((SymbolType::Namespace, self.get_node_name(node, code)?)),
            _ => None,
        }
    }

    fn extract_go_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_declaration" => Some((SymbolType::Function, self.get_node_name(node, code)?)),
            "method_declaration" => Some((SymbolType::Method, self.get_node_name(node, code)?)),
            "type_declaration" => Some((SymbolType::TypeAlias, self.get_node_name(node, code)?)),
            "interface_type" => Some((SymbolType::Interface, self.get_node_name(node, code)?)),
            "struct_type" => Some((SymbolType::Struct, self.get_node_name(node, code)?)),
            _ => None,
        }
    }

    fn extract_java_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "class_declaration" => Some((SymbolType::Class, self.get_node_name(node, code)?)),
            "interface_declaration" => Some((SymbolType::Interface, self.get_node_name(node, code)?)),
            "method_declaration" => Some((SymbolType::Method, self.get_node_name(node, code)?)),
            "enum_declaration" => Some((SymbolType::Enum, self.get_node_name(node, code)?)),
            "package_declaration" => Some((SymbolType::Package, self.get_node_name(node, code)?)),
            _ => None,
        }
    }
}

impl Default for AstExtractor {
    fn default() -> Self {
        Self::new()
    }
}
