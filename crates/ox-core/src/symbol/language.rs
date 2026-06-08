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

        self.languages.insert(lang_name.to_string(), language.clone());
        Ok(language)
    }

    pub fn parse(&mut self, code: &str, lang_name: &str) -> Result<Tree> {
        let language = self.get_language(lang_name)?;
        self.parser.set_language(&language)?;
        let tree = self.parser
            .parse(code, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse code"))?;
        Ok(tree)
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
