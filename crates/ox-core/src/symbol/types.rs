use serde::{Deserialize, Serialize};

/// Symbol type extracted from AST.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolType {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Trait,
    Interface,
    Impl,
    TypeAlias,
    Constant,
    Static,
    Module,
    Namespace,
    Package,
    Macro,
    Variant,
}

impl std::fmt::Display for SymbolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SymbolType::Function => "function",
            SymbolType::Method => "method",
            SymbolType::Struct => "struct",
            SymbolType::Class => "class",
            SymbolType::Enum => "enum",
            SymbolType::Trait => "trait",
            SymbolType::Interface => "interface",
            SymbolType::Impl => "impl",
            SymbolType::TypeAlias => "type",
            SymbolType::Constant => "const",
            SymbolType::Static => "static",
            SymbolType::Module => "module",
            SymbolType::Namespace => "namespace",
            SymbolType::Package => "package",
            SymbolType::Macro => "macro",
            SymbolType::Variant => "variant",
        };
        write!(f, "{}", s)
    }
}

impl SymbolType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(SymbolType::Function),
            "method" => Some(SymbolType::Method),
            "struct" => Some(SymbolType::Struct),
            "class" => Some(SymbolType::Class),
            "enum" => Some(SymbolType::Enum),
            "trait" => Some(SymbolType::Trait),
            "interface" => Some(SymbolType::Interface),
            "impl" => Some(SymbolType::Impl),
            "type" => Some(SymbolType::TypeAlias),
            "const" => Some(SymbolType::Constant),
            "static" => Some(SymbolType::Static),
            "module" => Some(SymbolType::Module),
            "namespace" => Some(SymbolType::Namespace),
            "package" => Some(SymbolType::Package),
            "macro" => Some(SymbolType::Macro),
            "variant" => Some(SymbolType::Variant),
            _ => None,
        }
    }
}

/// A code symbol extracted from source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolType,
    pub start_line: usize,
    pub end_line: usize,
    pub file_path: String,
    pub language: String,
    pub signature: String,
    pub parent: Option<String>,
}

/// Result of a symbol query.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolQueryResult {
    pub symbols: Vec<Symbol>,
    pub total_count: usize,
    pub query: String,
}
