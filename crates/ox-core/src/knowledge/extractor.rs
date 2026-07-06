use super::entity::{Entity, Relation, RelationType, SymbolType};
use super::language::LanguageRegistry;
/// AST-based code symbol extractor — produces `Entity::CodeSymbol` entities
/// from source files using tree-sitter multi-language parsing.
///
/// Migrated from `symbol/extractor.rs` — the extraction logic per language is the
/// same, but the output type is now `Entity` instead of the legacy `Symbol` struct.
use std::path::Path;
use tree_sitter::Node;

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

    /// Extract code symbols from a source file as `Entity::CodeSymbol` entities.
    pub fn extract_entities<P: AsRef<Path>>(
        &mut self,
        path: P,
        code: &str,
    ) -> anyhow::Result<Vec<Entity>> {
        let path = path.as_ref();
        let lang_name = self
            .registry
            .detect_language(path)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type: {:?}", path))?
            .to_string();

        let tree = self.registry.parse(code, &lang_name)?;
        let mut entities = Vec::new();

        self.extract_from_node(
            tree.root_node(),
            code,
            &lang_name,
            path,
            None,
            &mut entities,
        );

        // Second pass: resolve `calls` relations by matching function names
        // against all extracted symbols in the same file
        self.resolve_calls(&mut entities);
        // Third pass: metadata.calls → entity.relations (TriviumDB expand_depth + EntityGraph)
        self.wire_call_relations(&mut entities);

        Ok(entities)
    }

    /// Detect language from file extension.
    pub fn detect_language<P: AsRef<Path>>(&self, path: P) -> Option<&str> {
        self.registry.detect_language(path)
    }

    /// Check code for syntax errors via tree-sitter.
    pub fn check_syntax(
        &mut self,
        code: &str,
        lang_name: &str,
    ) -> anyhow::Result<Vec<super::language::SyntaxError>> {
        self.registry.check_syntax(code, lang_name)
    }

    fn extract_from_node(
        &self,
        node: Node,
        code: &str,
        lang_name: &str,
        path: &Path,
        parent: Option<String>,
        entities: &mut Vec<Entity>,
    ) {
        if let Some(entity) = self.node_to_entity(node, code, lang_name, path, parent.clone()) {
            entities.push(entity);
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_parent =
                if child.kind().ends_with("_item") || child.kind().ends_with("_declaration") {
                    parent.clone()
                } else {
                    self.get_node_name(node, code).or(parent.clone())
                };
            self.extract_from_node(child, code, lang_name, path, child_parent, entities);
        }
    }

    fn node_to_entity(
        &self,
        node: Node,
        code: &str,
        lang_name: &str,
        path: &Path,
        parent: Option<String>,
    ) -> Option<Entity> {
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
        let signature = self.get_declaration_header(node, code);

        // Build FQ name
        let fq_name = if let Some(ref p) = parent {
            format!("{}::{}", p, name)
        } else {
            name.clone()
        };

        Some(Entity::code_symbol(
            &name,
            &fq_name,
            kind,
            lang_name,
            &path.to_string_lossy(),
            start_line,
            end_line,
            &signature,
            parent.as_deref(),
        ))
    }

    // ── resolve_calls: populate the `calls` field on CodeSymbol entities ──

    /// Map `metadata.calls` (fq_name strings) to `entity.relations` (target entity IDs).
    fn wire_call_relations(&self, entities: &mut [Entity]) {
        use std::collections::HashMap;

        let fq_to_id: HashMap<String, String> = entities
            .iter()
            .filter_map(|e| {
                if let super::entity::EntityMetadata::CodeSymbol { fq_name, .. } = &e.metadata {
                    Some((fq_name.clone(), e.id.clone()))
                } else {
                    None
                }
            })
            .collect();

        for entity in entities.iter_mut() {
            let calls = match &entity.metadata {
                super::entity::EntityMetadata::CodeSymbol { calls, .. } => calls.clone(),
                _ => continue,
            };
            for callee_fq in calls {
                if let Some(target_id) = fq_to_id.get(&callee_fq) {
                    if target_id == &entity.id {
                        continue;
                    }
                    let dup = entity.relations.iter().any(|r| {
                        r.target_id == *target_id && r.relation_type == RelationType::Calls
                    });
                    if !dup {
                        entity.relations.push(Relation {
                            target_id: target_id.clone(),
                            relation_type: RelationType::Calls,
                            weight: 0.85,
                        });
                    }
                }
            }
        }
    }

    fn resolve_calls(&self, entities: &mut [Entity]) {
        // Collect all function/method names defined in this file
        let defined: Vec<String> = entities
            .iter()
            .filter_map(|e| {
                if e.kind == super::entity::EntityKind::CodeSymbol {
                    match &e.metadata {
                        super::entity::EntityMetadata::CodeSymbol { fq_name, .. } => {
                            Some(fq_name.clone())
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect();

        for entity in entities.iter_mut() {
            if let super::entity::EntityMetadata::CodeSymbol {
                ref mut calls,
                ref signature,
                ..
            } = entity.metadata
            {
                // Simple approach: scan the signature for names that match defined symbols
                let sig_lower = signature.to_lowercase();
                for def in &defined {
                    let short_name = def.rsplit("::").next().unwrap_or(def);
                    if sig_lower.contains(&short_name.to_lowercase()) && !calls.contains(def) {
                        calls.push(def.clone());
                    }
                }
            }
        }
    }

    // ── Helpers ──

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

    /// Declaration header only — excludes class/interface bodies (Java, TS, etc.).
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

        collapse_signature(header)
    }

    // ── Language-specific extractors ──

    fn extract_rust_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "impl_item" => {
                let name = self
                    .get_node_name(node, code)
                    .unwrap_or_else(|| "<impl>".to_string());
                Some((SymbolType::Impl, name))
            }
            "struct_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Struct, name))
            }
            "enum_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Enum, name))
            }
            "trait_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Trait, name))
            }
            "type_alias" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::TypeAlias, name))
            }
            "const_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Constant, name))
            }
            "static_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Static, name))
            }
            "mod_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Module, name))
            }
            "macro_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Macro, name))
            }
            "enum_variant" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Variant, name))
            }
            _ => None,
        }
    }

    fn extract_python_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "class_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Class, name))
            }
            _ => None,
        }
    }

    fn extract_js_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "class_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Class, name))
            }
            "method_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Method, name))
            }
            "arrow_function" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            _ => None,
        }
    }

    fn extract_ts_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "class_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Class, name))
            }
            "method_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Method, name))
            }
            "interface_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Interface, name))
            }
            "type_alias_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::TypeAlias, name))
            }
            "enum_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Enum, name))
            }
            _ => self.extract_js_symbol(node, code),
        }
    }

    fn extract_cpp_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "class_specifier" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Class, name))
            }
            "struct_specifier" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Struct, name))
            }
            "enum_specifier" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Enum, name))
            }
            "namespace_definition" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Namespace, name))
            }
            _ => None,
        }
    }

    fn extract_go_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "method_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Method, name))
            }
            "type_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::TypeAlias, name))
            }
            "interface_type" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Interface, name))
            }
            "struct_type" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Struct, name))
            }
            _ => None,
        }
    }

    fn extract_java_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "class_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Class, name))
            }
            "interface_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Interface, name))
            }
            "method_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Method, name))
            }
            "enum_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Enum, name))
            }
            "package_declaration" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Package, name))
            }
            _ => None,
        }
    }
}

/// Collapse whitespace; keep generics/annotations on one line for embedding.
fn collapse_signature(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::entity::{EntityMetadata, SymbolType};

    #[test]
    fn test_declaration_header_excludes_class_body() {
        let code = r#"
public class BigService implements Runnable {
    private final String id;
    public void run() { }
    public void other() { }
}
"#;
        let mut extractor = AstExtractor::new();
        let entities = extractor.extract_entities("BigService.java", code).unwrap();
        let class_entity = entities
            .iter()
            .find(|e| {
                matches!(
                    &e.metadata,
                    EntityMetadata::CodeSymbol {
                        symbol_type: SymbolType::Class,
                        ..
                    }
                )
            })
            .expect("class entity");
        if let EntityMetadata::CodeSymbol { signature, .. } = &class_entity.metadata {
            assert!(signature.contains("class BigService"));
            assert!(!signature.contains("private final"));
            assert!(!signature.contains("public void run"));
        } else {
            panic!("expected CodeSymbol metadata");
        }
    }

    #[test]
    fn test_extract_rust_symbols() {
        let code = r#"
fn main() {
    println!("hello");
}

pub fn validate_token(token: &str) -> bool {
    true
}

struct User {
    name: String,
}

impl User {
    fn new(name: String) -> Self {
        Self { name }
    }
}
"#;
        let mut extractor = AstExtractor::new();
        let entities = extractor.extract_entities("src/main.rs", code).unwrap();

        assert!(
            entities.len() >= 3,
            "Expected at least 3 symbols, got {}",
            entities.len()
        );

        let functions: Vec<_> = entities.iter()
            .filter(|e| matches!(&e.metadata, EntityMetadata::CodeSymbol { symbol_type, .. } if *symbol_type == SymbolType::Function))
            .collect();
        assert!(!functions.is_empty(), "Expected at least one function");

        let structs: Vec<_> = entities.iter()
            .filter(|e| matches!(&e.metadata, EntityMetadata::CodeSymbol { symbol_type, .. } if *symbol_type == SymbolType::Struct))
            .collect();
        assert!(!structs.is_empty(), "Expected at least one struct");
    }

    #[test]
    fn test_extract_python_symbols() {
        let code = r#"
def hello():
    print("hi")

class Calculator:
    def add(self, a, b):
        return a + b
"#;
        let mut extractor = AstExtractor::new();
        let entities = extractor.extract_entities("src/main.py", code).unwrap();

        assert!(entities.len() >= 2, "Expected at least 2 symbols");
    }
}
