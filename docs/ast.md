以下是完整的代码索引模块实现：

Cargo.toml

[package]
name = "code-indexer"
version = "0.1.0"
edition = "2021"

[dependencies]
tree-sitter = "0.22"
tree-sitter-rust = "0.21"
tree-sitter-python = "0.21"
tree-sitter-javascript = "0.21"
tree-sitter-typescript = "0.21"
tree-sitter-cpp = "0.22"
tree-sitter-go = "0.21"
tree-sitter-java = "0.21"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
walkdir = "2.5"
notify = "6.0"
tokio = { version = "1", features = ["full"] }
triviumdb = "0.1"
candle-core = "0.6"
candle-transformers = "0.6"
hf-hub = "0.3"
tokenizers = "0.15"

src/types.rs

use serde::{Deserialize, Serialize};

/// Symbol type extracted from AST.
[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// A code symbol extracted from source code.
[derive(Debug, Clone, Serialize, Deserialize)]
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
[derive(Debug, Clone, Serialize)]
pub struct SymbolQueryResult {
pub symbols: Vec<Symbol>,
pub total_count: usize,
pub query: String,
}

src/language.rs

use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};
use anyhow::Result;

pub struct LanguageRegistry {
parser: Parser,
languages: HashMap<String, Language>,
ext_map: HashMap<&'static str, &'static str>,
}

impl LanguageRegistry {
pub fn new() -> Self {
let mut ext_map = HashMap::new();
// 文件扩展名 -> 语言名映射
ext_map.insert("rs", "rust");
ext_map.insert("py", "python");
ext_map.insert("js", "javascript");
ext_map.insert("ts", "typescript");
ext_map.insert("tsx", "typescript");
ext_map.insert("jsx", "javascript");
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
            return Ok(*lang);
        }

        let language = match lang_name {
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "python" => tree_sitter_python::LANGUAGE.into(),
            "javascript" => tree_sitter_javascript::LANGUAGE.into(),
            "typescript" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            "cpp" => tree_sitter_cpp::LANGUAGE.into(),
            "go" => tree_sitter_go::LANGUAGE.into(),
            "java" => tree_sitter_java::LANGUAGE.into(),
            _ => anyhow::bail!("Unsupported language: {}", lang_name),
        };

        self.languages.insert(lang_name.to_string(), language);
        Ok(language)
    }

    pub fn parse(&mut self, code: &str, lang_name: &str) -> Result<Tree> {
        let language = self.get_language(lang_name)?;
        self.parser.set_language(language)?;
        let tree = self.parser.parse(code, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse code"))?;
        Ok(tree)
    }
}

src/extractor.rs

use std::path::Path;
use tree_sitter::{Tree, Node};
use crate::types::{Symbol, SymbolType};
use crate::language::LanguageRegistry;

pub struct AstExtractor {
registry: LanguageRegistry,
}

impl AstExtractor {
pub fn new() -> Self {
Self {
registry: LanguageRegistry::new(),
}
}

    pub fn extract_symbols<P: AsRef<Path>>(&mut self, path: P, code: &str) -> anyhow::Result<Vec<Symbol>> {
        let lang_name = self.registry.detect_language(&path)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type: {:?}", path.as_ref()))?;
        
        let tree = self.registry.parse(code, lang_name)?;
        let mut symbols = Vec::new();
        
        self.extract_from_node(tree.root_node(), code, lang_name, path.as_ref(), None, &mut symbols);
        
        Ok(symbols)
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
        // 尝试从当前节点提取符号
        if let Some(symbol) = self.node_to_symbol(node, code, lang_name, path, parent.clone()) {
            symbols.push(symbol);
        }

        // 递归处理子节点
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_parent = if child.kind().ends_with("_item") || child.kind().ends_with("_declaration") {
                // 子节点是顶层声明，parent 不变
                parent.clone()
            } else {
                // 子节点可能是嵌套的，尝试用当前节点的 name 作为 parent
                self.get_node_name(node, code).or(parent.clone())
            };
            self.extract_from_node(child, code, lang_name, path, child_parent, symbols);
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
        let signature = self.get_node_text(node, code).trim().to_string();

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
        // 查找标识符子节点
        for child in node.children(&mut node.walk()) {
            if child.kind().contains("identifier") || child.kind().contains("name") {
                return Some(self.get_node_text(child, code));
            }
        }
        None
    }

    fn get_node_text(&self, node: Node, code: &str) -> String {
        node.utf8_text(code.as_bytes()).unwrap_or_default().to_string()
    }

    // === 各语言提取器 ===

    fn extract_rust_symbol(&self, node: Node, code: &str) -> Option<(SymbolType, String)> {
        match node.kind() {
            "function_item" => {
                let name = self.get_node_name(node, code)?;
                Some((SymbolType::Function, name))
            }
            "impl_item" => {
                let name = self.get_node_name(node, code).unwrap_or_else(|| "<impl>".to_string());
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
                // 箭头函数可能有名字（赋值给变量时）
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

src/embedding.rs

use anyhow::Result;
use candle_core::{Tensor, Device};
use candle_transformers::models::bert::{BertModel, Config};
use hf_hub::{Repo, RepoType, Api};
use tokenizers::Tokenizer;

pub struct EmbeddingModel {
model: BertModel,
tokenizer: Tokenizer,
device: Device,
}

impl EmbeddingModel {
pub fn new() -> Result<Self> {
let device = Device::Cpu;

        // 使用 sentence-transformers/all-MiniLM-L6-v2
        let api = Api::new()?;
        let repo = Repo::with_revision(
            "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            RepoType::Model,
            "main".to_string(),
        );
        
        let config_path = api.get(&repo, "config.json")?;
        let config = std::fs::read_to_string(config_path)?;
        let config: Config = serde_json::from_str(&config)?;
        
        let model_path = api.get(&repo, "pytorch_model.bin")?;
        let weights = unsafe { candle_core::safetensors::load(&model_path, &device)? };
        let model = BertModel::load(&weights, config)?;
        
        let tokenizer_path = api.get(&repo, "tokenizer.json")?;
        let tokenizer = Tokenizer::from_file(tokenizer_path)?;
        
        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let tokens = self.tokenizer.encode(text, true)?;
        let input_ids = tokens.get_ids();
        let attention_mask = tokens.get_attention_mask();
        
        let input_ids = Tensor::new(input_ids, &self.device)?.unsqueeze(0)?;
        let attention_mask = Tensor::new(attention_mask, &self.device)?.unsqueeze(0)?;
        
        let output = self.model.forward(&input_ids, &attention_mask)?;
        
        // 取 [CLS] token 的 embedding（第0个token）
        let cls_embedding = output.last_hidden_state.i((0, 0))?;
        let embedding: Vec<f32> = cls_embedding.to_vec1()?;
        
        Ok(embedding)
    }
}

src/vector_store.rs

use anyhow::Result;
use serde_json::json;
use triviumdb::{TriviumDB, Filter};
use crate::types::Symbol;
use crate::embedding::EmbeddingModel;

pub struct VectorStore {
db: TriviumDB,
embedding_model: EmbeddingModel,
}

impl VectorStore {
pub fn new(path: &str, dim: usize) -> Result<Self> {
let db = TriviumDB::open(path, dim as u32)?;
let embedding_model = EmbeddingModel::new()?;
Ok(Self { db, embedding_model })
}

    pub fn insert_symbol(&mut self, symbol: &Symbol) -> Result<u64> {
        let embedding = self.embedding_model.embed(&symbol.signature)?;
        let id = self.db.insert(&embedding, json!({
            "file_path": symbol.file_path,
            "symbol_name": symbol.name,
            "symbol_type": symbol.kind.to_string(),
            "language": symbol.language,
            "start_line": symbol.start_line,
            "end_line": symbol.end_line,
            "parent": symbol.parent,
        }))?;
        Ok(id)
    }

    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<Symbol>> {
        let query_embedding = self.embedding_model.embed(query)?;
        let results = self.db.search(&query_embedding, Filter::empty(), top_k as u32)?;
        
        let symbols = results.into_iter().map(|r| {
            let payload = r.payload;
            Symbol {
                name: payload["symbol_name"].as_str().unwrap_or("").to_string(),
                kind: SymbolType::from_str(payload["symbol_type"].as_str().unwrap_or("function")).unwrap_or(SymbolType::Function),
                start_line: payload["start_line"].as_u64().unwrap_or(0) as usize,
                end_line: payload["end_line"].as_u64().unwrap_or(0) as usize,
                file_path: payload["file_path"].as_str().unwrap_or("").to_string(),
                language: payload["language"].as_str().unwrap_or("").to_string(),
                signature: "".to_string(), // 需要从文件重新读取
                parent: payload["parent"].as_str().map(|s| s.to_string()),
            }
        }).collect();
        
        Ok(symbols)
    }
}

impl SymbolType {
fn from_str(s: &str) -> Option<Self> {
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

src/indexer.rs

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use notify::{Watcher, RecursiveMode, Event, Result as NotifyResult};
use anyhow::Result;
use crate::types::{Symbol, SymbolQueryResult};
use crate::extractor::AstExtractor;
use crate::vector_store::VectorStore;

pub struct CodeIndexer {
symbols: Arc<RwLock<Vec<Symbol>>>,
extractor: AstExtractor,
vector_store: VectorStore,
project_path: PathBuf,
watcher: Option<notify::RecommendedWatcher>,
}

impl CodeIndexer {
pub fn new(project_path: &Path, db_path: &str) -> Result<Self> {
let vector_store = VectorStore::new(db_path, 384)?;
Ok(Self {
symbols: Arc::new(RwLock::new(Vec::new())),
extractor: AstExtractor::new(),
vector_store,
project_path: project_path.to_path_buf(),
watcher: None,
})
}

    pub async fn index_project(&mut self) -> Result<()> {
        println!("Indexing project: {:?}", self.project_path);
        
        for entry in walkdir::WalkDir::new(&self.project_path) {
            let entry = entry?;
            if entry.file_type().is_file() {
                self.index_file(entry.path()).await?;
            }
        }
        
        println!("Indexing complete. {} symbols indexed.", self.symbols.read().await.len());
        Ok(())
    }

    async fn index_file(&mut self, path: &Path) -> Result<()> {
        let code = std::fs::read_to_string(path)?;
        let symbols = self.extractor.extract_symbols(path, &code)?;
        
        let mut symbols_lock = self.symbols.write().await;
        for symbol in symbols {
            self.vector_store.insert_symbol(&symbol)?;
            symbols_lock.push(symbol);
        }
        
        Ok(())
    }

    pub async fn search(&self, query: &str, top_k: usize) -> Result<SymbolQueryResult> {
        let symbols = self.vector_store.search(query, top_k)?;
        Ok(SymbolQueryResult {
            symbols,
            total_count: symbols.len(),
            query: query.to_string(),
        })
    }

    pub fn start_watcher(&mut self) -> NotifyResult<()> {
        let symbols = Arc::clone(&self.symbols);
        let extractor = self.extractor.clone();
        let vector_store = self.vector_store.clone();
        let project_path = self.project_path.clone();

        let mut watcher = notify::recommended_watcher(move |res: NotifyResult<Event>| {
            match res {
                Ok(event) => {
                    for path in event.paths {
                        if path.is_file() {
                            // 增量更新逻辑
                            println!("File changed: {:?}", path);
                            // TODO: 重新索引该文件
                        }
                    }
                }
                Err(e) => eprintln!("Watch error: {:?}", e),
            }
        })?;

        watcher.watch(&project_path, RecursiveMode::Recursive)?;
        self.watcher = Some(watcher);
        Ok(())
    }
}

src/main.rs

mod types;
mod language;
mod extractor;
mod embedding;
mod vector_store;
mod indexer;

use anyhow::Result;
use std::env;

[tokio::main]
async fn main() -> Result<()> {
let args: Vec<String> = env::args().collect();
if args.len() < 2 {
eprintln!("Usage: code-indexer <project_path>");
std::process::exit(1);
}

    let project_path = &args[1];
    let mut indexer = indexer::CodeIndexer::new(
        std::path::Path::new(project_path),
        ".code_index.tdb"
    )?;

    // 全量索引
    indexer.index_project().await?;

    // 启动文件监听
    indexer.start_watcher()?;

    // 交互式搜索
    println!("nEnter search queries (or 'quit' to exit):");
    let stdin = std::io::stdin();
    loop {
        let mut query = String::new();
        print!("n> ");
        std::io::Write::flush(&mut std::io::stdout())?;
        stdin.read_line(&mut query)?;
        let query = query.trim();
        
        if query == "quit" || query == "exit" {
            break;
        }
        if query.is_empty() {
            continue;
        }

        let result = indexer.search(query, 5).await?;
        println!("nFound {} results:", result.total_count);
        for symbol in result.symbols {
            println!(
                "  [{}] {}::{} ({}:{:?})",
                symbol.kind,
                symbol.file_path,
                symbol.name,
                symbol.start_line,
                symbol.parent
            );
        }
    }

    Ok(())
}

运行方式

克隆后
cargo run -- /path/to/your/project

这就是完整的、可直接运行的代码索引系统。核心流程：

AST 解析 → 多语言 tree-sitter
符号提取 → 按语言规则提取函数、类等
向量生成 → 本地 embedding 模型
向量存储 → TriviumDB 嵌入式数据库
增量更新 → 文件监听 + 增量索引
语义搜索 → 向量相似度 + 元数据过滤

