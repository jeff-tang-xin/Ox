//! Keyword extraction payload from LLM responses.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordExtraction {
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub related_files: Vec<String>,
}
