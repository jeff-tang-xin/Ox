pub mod store;
pub mod semantic;  // 🆕 Semantic association manager
pub mod layering;  // 🆕 L0-L3 memory layering
pub mod hybrid_storage;  // 🆕 Hybrid SQLite + Markdown storage
pub mod memory_vector;  // 🆕 Vector-backed semantic memory search

use std::fmt;
use std::sync::Mutex;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default value for recent_scores array
fn default_recent_scores() -> [f32; 5] {
    [0.0; 5]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,
    pub content: String,
    pub node_type: MemoryNodeType,
    pub depth: u8,
    pub project_id: Option<String>,
    pub language: String,
    pub source: MemorySource,
    pub created_at: i64,
    pub last_accessed: i64,
    pub is_project_critical: bool,
    pub traces: [f32; 5],
    pub language_weight: f64,
    
    // 🆕 LLM Judge feedback tracking
    /// Average relevance score from LLM judge (0-10)
    #[serde(default)]
    pub avg_llm_score: f32,
    /// Number of times evaluated by LLM judge
    #[serde(default)]
    pub judge_eval_count: u32,
    /// Recent scores for trend analysis (last 5 evaluations)
    #[serde(default = "default_recent_scores")]
    pub recent_scores: [f32; 5],
    
    // 🆕 File association for context-aware retrieval
    /// Related file paths (e.g., ["src/auth.rs", "src/middleware/mod.rs"])
    #[serde(default)]
    pub related_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryNodeType {
    Fact,
    Style,
    Architectural,
    AntiPattern,
    Business,
    BestPractice,
    Pattern,
    MetaSkill,
}

impl MemoryNodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Style => "style",
            Self::Architectural => "architectural",
            Self::AntiPattern => "anti_pattern",
            Self::Business => "business",
            Self::BestPractice => "best_practice",
            Self::Pattern => "pattern",
            Self::MetaSkill => "meta_skill",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fact" => Some(Self::Fact),
            "style" => Some(Self::Style),
            "architectural" => Some(Self::Architectural),
            "anti_pattern" => Some(Self::AntiPattern),
            "business" => Some(Self::Business),
            "best_practice" => Some(Self::BestPractice),
            "pattern" => Some(Self::Pattern),
            "meta_skill" => Some(Self::MetaSkill),
            _ => None,
        }
    }

    pub fn default_depth(&self) -> u8 {
        match self {
            Self::Fact => 1,
            Self::Style => 3,
            Self::Architectural => 2,
            Self::AntiPattern => 2,
            Self::Business => 2,
            Self::BestPractice => 2,
            Self::Pattern => 2,
            Self::MetaSkill => 3,
        }
    }

    pub fn is_immediate_write(&self) -> bool {
        matches!(
            self,
            Self::Style | Self::Architectural | Self::AntiPattern | Self::MetaSkill
        )
    }

    pub fn is_long_term(&self) -> bool {
        matches!(self, Self::BestPractice | Self::Pattern | Self::MetaSkill)
    }
}

// ── Decay strategies ──

pub fn calculate_project_decay(node: &MemoryNode, base_half_life: u64) -> f32 {
    if node.is_project_critical {
        return 1.0;
    }
    let age_secs = (chrono::Utc::now().timestamp() - node.last_accessed).max(0);
    let age_days = age_secs as f32 / 86400.0;
    let short_term = (-age_days / (base_half_life as f32 * 0.3)).exp();
    let long_term = (-age_days / (base_half_life as f32 * 5.0)).exp();
    (0.7 * short_term + 0.3 * long_term).clamp(0.0, 1.0)
}

pub fn calculate_overall_decay(node: &MemoryNode, traces_config: &[f32]) -> f32 {
    let t = ((chrono::Utc::now().timestamp() - node.last_accessed).max(0) as f32) / 86400.0;
    let traces_sum: f32 = node
        .traces
        .iter()
        .zip(traces_config.iter())
        .map(|(trace, tau)| trace * (-t / tau).exp())
        .sum();
    let base = if traces_config.is_empty() {
        0.5
    } else {
        traces_sum / traces_config.len() as f32
    };
    (base * node.language_weight as f32 + node.depth as f32 * 0.5).clamp(0.0, 1.0)
}

pub fn power_law_decay(node: &MemoryNode, beta: f32) -> f32 {
    let age_days = ((chrono::Utc::now().timestamp() - node.last_accessed).max(1) as f32) / 86400.0;
    age_days.powf(-beta).clamp(0.0, 1.0)
}

pub fn composite_score(node: &MemoryNode, half_life: u64) -> f32 {
    let decay = if node.project_id.is_some() {
        calculate_project_decay(node, half_life)
    } else {
        calculate_overall_decay(node, &[0.1, 0.2, 0.3, 0.4, 0.5])
    };
    let now = chrono::Utc::now().timestamp();
    let recency = 1.0 - ((now - node.last_accessed) as f32 / 86400.0 / 30.0).min(1.0);
    node.depth as f32 * 0.5 + decay * 0.3 + recency * 0.2
}

// ── Memory chunking utilities ──

/// Split long text into overlapping chunks for better semantic preservation.
/// 
/// # Arguments
/// * `text` - The text to split
/// * `max_chunk_len` - Maximum length of each chunk (in characters)
/// * `overlap_ratio` - Overlap ratio between chunks (0.0-1.0, typically 0.15)
/// 
/// # Returns
/// Vector of chunk strings with overlap
fn split_with_overlap(text: &str, max_chunk_len: usize, overlap_ratio: f32) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let char_count = chars.len();
    
    if char_count <= max_chunk_len {
        return vec![text.to_string()];
    }
    
    let mut chunks = Vec::new();
    let step = (max_chunk_len as f32 * (1.0 - overlap_ratio)) as usize;
    let mut start = 0;
    
    while start < char_count {
        let end = (start + max_chunk_len).min(char_count);
        
        // Try to break at word boundary to avoid cutting words
        let chunk_end = if end < char_count {
            // Find last space before end (search in character slice)
            let chunk_chars = &chars[start..end];
            chunk_chars
                .iter()
                .rposition(|&c| c == ' ')
                .map(|pos| start + pos + 1)  // +1 to include the space
                .unwrap_or(end)
        } else {
            end
        };
        
        // Convert character indices back to string
        let chunk: String = chars[start..chunk_end].iter().collect();
        chunks.push(chunk);
        
        // Move to next chunk with overlap
        start = if chunk_end + step > char_count {
            break;  // Last chunk
        } else {
            chunk_end + step - (max_chunk_len as f32 * overlap_ratio) as usize
        };
    }
    
    chunks
}

// ── Janitor ──

impl MemoryManager {
    pub fn run_janitor(&self, _critical_threshold: f32, max_nodes: usize) {
        if let Some(ref store) = self.project_store {
            if let Ok(all) = store.query_by_project(
                "",
                &[
                    MemoryNodeType::Fact,
                    MemoryNodeType::Style,
                    MemoryNodeType::Architectural,
                    MemoryNodeType::Business,
                    MemoryNodeType::AntiPattern,
                ],
                max_nodes + 100,
            ) {
                let max_cleanup = (all.len() / 10).max(1);
                let mut expired = Vec::new();
                for node in &all {
                    if node.is_project_critical {
                        continue;
                    }
                    let days = (chrono::Utc::now().timestamp() - node.last_accessed).max(0) as f32
                        / 86400.0;
                    let should_cleanup = match node.depth {
                        0..=1 => days > 30.0,
                        2 => {
                            let decay = calculate_project_decay(node, 30);
                            days > 60.0 && decay < 0.3
                        }
                        _ => {
                            let decay = calculate_project_decay(node, 30);
                            decay < 0.1
                        }
                    };
                    if should_cleanup {
                        expired.push(&node.id);
                        if expired.len() >= max_cleanup {
                            break;
                        }
                    }
                }
                for id in &expired {
                    if let Err(e) = store.delete(id) {
                        tracing::warn!("Janitor failed to delete {}: {e}", id);
                    }
                }
                if !expired.is_empty() {
                    tracing::warn!("Janitor deleted {} expired memories", expired.len());
                }
            }
        }
    }
}

#[cfg(test)]
mod decay_tests {
    use super::*;

    #[test]
    fn project_decay_critical_is_one() {
        let node = MemoryNode::new(
            "test".into(),
            MemoryNodeType::Fact,
            Some("p".into()),
            "rust".into(),
            MemorySource::ToolObservation,
        )
        .with_critical();
        assert_eq!(calculate_project_decay(&node, 30), 1.0);
    }

    #[test]
    fn project_decay_fresh_is_high() {
        let node = MemoryNode::new(
            "test".into(),
            MemoryNodeType::Fact,
            Some("p".into()),
            "rust".into(),
            MemorySource::ToolObservation,
        );
        let decay = calculate_project_decay(&node, 30);
        assert!(decay > 0.9);
    }

    #[test]
    fn overall_decay_fresh_is_reasonable() {
        let node = MemoryNode::new(
            "test".into(),
            MemoryNodeType::BestPractice,
            None,
            "rust".into(),
            MemorySource::LlmExtraction,
        );
        let decay = calculate_overall_decay(&node, &[0.1, 0.2, 0.3, 0.4, 0.5]);
        assert!(decay > 0.0);
    }
    
    // 🆕 Test for enhanced file_read memory extraction
    #[test]
    fn test_file_read_memory_extraction_with_result() {
        let tool_args = r#"{"path": "src/main.rs"}"#;
        let tool_result = "     1\tfn main() {\n     2\t    println!(\"Hello, world!\");\n     3\t}\n     4\t\n     5\tstruct User {\n     6\t    name: String,\n     7\t}\n     8\t\n     9\timpl User {\n    10\t    fn new(name: String) -> Self {\n    11\t        Self { name }\n    12\t    }\n    13\t}";
        
        let node = MemoryNode::extract_from_tool_call_with_result(
            "file_read",
            tool_args,
            Some(tool_result),
            "test-project",
            "rust",
        );
        
        assert!(node.is_some());
        let node = node.unwrap();
        
        // Verify the memory contains rich information
        assert!(node.content.contains("src/main.rs"));
        assert!(node.content.contains("lines."));
        assert!(node.content.contains("Preview:"));
        assert!(node.content.contains("functions"));
        assert!(node.content.contains("structs/classes"));
        assert_eq!(node.related_files.len(), 1);
        assert_eq!(node.related_files[0], "src/main.rs");
        
        tracing::info!("Generated memory content:\n{}", node.content);
    }
    
    #[test]
    fn test_file_read_memory_without_result() {
        let tool_args = r#"{"path": "config.toml"}"#;
        
        let node = MemoryNode::extract_from_tool_call_with_result(
            "file_read",
            tool_args,
            None,  // No result available
            "test-project",
            "toml",
        );
        
        assert!(node.is_some());
        let node = node.unwrap();
        
        // Should have basic information
        assert!(node.content.contains("config.toml"));
        assert_eq!(node.related_files.len(), 1);
    }
}

impl fmt::Display for MemoryNodeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemorySource {
    UserExplicit,
    ToolObservation,
    LlmExtraction,
    Feedback,
    RefinedSummary,  // 🆕 Refined memory summaries from conversation turns
}

impl MemorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserExplicit => "user_explicit",
            Self::ToolObservation => "tool_observation",
            Self::LlmExtraction => "llm_extraction",
            Self::Feedback => "feedback",
            Self::RefinedSummary => "refined_summary",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user_explicit" => Some(Self::UserExplicit),
            "tool_observation" => Some(Self::ToolObservation),
            "llm_extraction" => Some(Self::LlmExtraction),
            "feedback" => Some(Self::Feedback),
            "refined_summary" => Some(Self::RefinedSummary),
            _ => None,
        }
    }
}

impl MemoryNode {
    pub fn new(
        content: String,
        node_type: MemoryNodeType,
        project_id: Option<String>,
        language: String,
        source: MemorySource,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            node_type,
            depth: node_type.default_depth(),
            project_id,
            language,
            source,
            created_at: now,
            last_accessed: now,
            is_project_critical: false,
            traces: [0.2, 0.2, 0.2, 0.2, 0.2],
            language_weight: 0.5,
            // 🆕 LLM Judge feedback fields
            avg_llm_score: 0.0,
            judge_eval_count: 0,
            recent_scores: [0.0; 5],
            // 🆕 File association
            related_files: Vec::new(),
        }
    }

    pub fn with_critical(mut self) -> Self {
        self.is_project_critical = true;
        self
    }

    pub fn with_language_weight(mut self, weight: f64) -> Self {
        self.language_weight = weight;
        self
    }

    /// Add a related file path to this memory
    pub fn with_related_file(mut self, file_path: &str) -> Self {
        if !self.related_files.contains(&file_path.to_string()) {
            self.related_files.push(file_path.to_string());
        }
        self
    }

    /// Add multiple related file paths
    pub fn with_related_files(mut self, file_paths: &[String]) -> Self {
        for path in file_paths {
            if !self.related_files.contains(path) {
                self.related_files.push(path.clone());
            }
        }
        self
    }
}

// ── WriteBuffer ──

const BUFFER_CAPACITY: usize = 10;
const BUFFER_FLUSH_SECS: i64 = 5;

#[derive(Clone)]
pub struct WriteBuffer {
    pending: Vec<MemoryNode>,
    last_flush: i64,
}

impl WriteBuffer {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            last_flush: chrono::Utc::now().timestamp(),
        }
    }

    pub fn buffer(&mut self, node: MemoryNode) -> bool {
        self.pending.push(node);
        let now = chrono::Utc::now().timestamp();
        self.pending.len() >= BUFFER_CAPACITY || (now - self.last_flush) >= BUFFER_FLUSH_SECS
    }

    pub fn drain(&mut self) -> Vec<MemoryNode> {
        self.last_flush = chrono::Utc::now().timestamp();
        std::mem::take(&mut self.pending)
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

// ── Extractor (启发式记忆提取) ──

impl MemoryNode {
    /// 🆕 Enhanced extraction with access to tool execution results
    pub fn extract_from_tool_call_with_result(
        tool_name: &str,
        tool_args: &str,
        tool_result: Option<&str>,
        project_id: &str,
        language: &str,
    ) -> Option<Self> {
        match tool_name {
            "file_read" => Self::extract_file_read_memory(tool_args, tool_result, project_id, language),
            "file_write" | "edit_file" => {
                if tool_args.len() < 20 {
                    return None;
                }
                
                // Extract file path and content summary
                let parsed = serde_json::from_str::<serde_json::Value>(tool_args).ok();
                let related_file = parsed.as_ref()
                    .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()));
                
                // Build richer content: file path + what changed + result summary
                let path_info = related_file.as_deref().unwrap_or("(unknown file)");
                let result_summary = tool_result
                    .map(|r| truncate_str(r, 200))
                    .unwrap_or("completed");
                
                let content = format!(
                    "[MODIFIED] {} | {} | Result: {}",
                    path_info,
                    tool_name,
                    result_summary
                );
                
                let mut node = if contains_architectural_keywords(tool_args) {
                    Self::new(content, MemoryNodeType::Architectural, Some(project_id.into()), language.into(), MemorySource::ToolObservation)
                } else if contains_business_keywords(tool_args) {
                    Self::new(content, MemoryNodeType::Business, Some(project_id.into()), language.into(), MemorySource::ToolObservation)
                } else {
                    Self::new(content, MemoryNodeType::Fact, Some(project_id.into()), language.into(), MemorySource::ToolObservation)
                };
                
                if let Some(fp) = related_file {
                    node = node.with_related_file(&fp);
                }
                
                Some(node)
            }
            "shell_exec" => {
                let is_error = tool_args.contains("error") || tool_args.contains("Error") || tool_args.contains("failed");
                let result_info = tool_result.unwrap_or("");
                let content = format!(
                    "[SHELL] {} | {} | {}",
                    truncate_str(tool_args, 200),
                    if is_error { "FAILED" } else { "OK" },
                    truncate_str(result_info, 300)
                );
                Some(Self::new(
                    content,
                    if is_error { MemoryNodeType::AntiPattern } else { MemoryNodeType::Fact },
                    Some(project_id.into()),
                    language.into(),
                    MemorySource::ToolObservation,
                ))
            }
            "code_search" | "file_search" => {
                // Remember what was searched for — helps with context
                let query_info = if tool_args.len() > 300 { &tool_args[..300] } else { tool_args };
                let result_count = tool_result.map(|r| r.lines().count()).unwrap_or(0);
                let content = format!("[SEARCH] {} | {} results", query_info, result_count);
                Some(Self::new(
                    content,
                    MemoryNodeType::Fact,
                    Some(project_id.into()),
                    language.into(),
                    MemorySource::ToolObservation,
                ))
            }
            _ => None,
        }
    }

    /// Legacy extraction without result access (kept for compatibility)
    pub fn extract_from_tool_call(
        tool_name: &str,
        tool_args: &str,
        project_id: &str,
        language: &str,
    ) -> Option<Self> {
        Self::extract_from_tool_call_with_result(tool_name, tool_args, None, project_id, language)
    }

    /// 🆕 Specialized memory extraction for file_read operations
    fn extract_file_read_memory(
        tool_args: &str,
        tool_result: Option<&str>,
        project_id: &str,
        language: &str,
    ) -> Option<Self> {
        // Extract file path from arguments
        let file_path = serde_json::from_str::<serde_json::Value>(tool_args)
            .ok()
            .and_then(|v| {
                v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string())
            })?;
        
        // Create rich memory content based on tool result
        let content = if let Some(result) = tool_result {
            // If we have the actual file content, create a meaningful summary
            let result_lines: Vec<&str> = result.lines().collect();
            let line_count = result_lines.len();
            
            // Extract key information from the file content (first 10 lines)
            let preview_lines: Vec<&str> = result_lines.iter().take(10).cloned().collect();
            let preview = preview_lines.join("\n");
            
            // Check for important patterns in the file
            let has_functions = result.contains("fn ") || result.contains("def ") || result.contains("function");
            let has_structs = result.contains("struct ") || result.contains("class ") || result.contains("type");
            let has_imports = result.contains("use ") || result.contains("import ") || result.contains("#include");
            
            let mut features = Vec::new();
            if has_functions { features.push("functions"); }
            if has_structs { features.push("structs/classes"); }
            if has_imports { features.push("imports/dependencies"); }
            
            let features_str = if features.is_empty() {
                String::new()
            } else {
                format!(" Contains: {}.", features.join(", "))
            };
            
            // 🚨 FIX: Use char-based truncation to avoid UTF-8 boundary errors
            let preview_truncated: String = preview.chars().take(300).collect();
            let preview_final = if preview.chars().count() > 300 {
                format!("{}...", preview_truncated)
            } else {
                preview
            };
            
            // 🆕 Enhanced format with searchable keywords at the beginning
            // This improves retrieval by putting key terms first
            let filename_only = file_path.split('/').last().unwrap_or(&file_path);
            format!(
                "[FILE] {} | Read src file with {} lines.{}\nFull path: {}\nPreview:\n{}",
                filename_only,  // Put filename first for better search matching
                line_count,
                features_str,
                file_path,
                preview_final
            )
        } else {
            // Fallback: just record that the file was read
            format!("Read file: {}", file_path)
        };
        
        let mut node = Self::new(
            content,
            MemoryNodeType::Fact,
            Some(project_id.into()),
            language.into(),
            MemorySource::ToolObservation,
        );
        node.depth = 3;  // Higher depth for better retrieval ranking
        node = node.with_related_file(&file_path);
        Some(node)
    }

    pub fn extract_from_conversation(
        assistant_content: &str,
        project_id: &str,
        language: &str,
    ) -> Option<Self> {
        // Take first 500 chars as summary — capture enough context
        let summary: String = assistant_content.chars().take(500).collect();
        
        if contains_architectural_keywords(assistant_content) {
            Some(Self::new(summary, MemoryNodeType::Architectural, Some(project_id.into()), language.into(), MemorySource::LlmExtraction))
        } else if contains_user_preference(assistant_content) {
            Some(Self::new(summary, MemoryNodeType::Style, Some(project_id.into()), language.into(), MemorySource::LlmExtraction))
        } else if contains_business_keywords(assistant_content) {
            Some(Self::new(summary, MemoryNodeType::Business, Some(project_id.into()), language.into(), MemorySource::LlmExtraction))
        } else if assistant_content.len() > 100 {
            // Store any substantial response as a general fact — captures requirements, explanations, etc.
            Some(Self::new(summary, MemoryNodeType::Fact, Some(project_id.into()), language.into(), MemorySource::LlmExtraction))
        } else {
            None
        }
    }
}

/// Calculate word overlap score between query and memory content.
/// Returns 0.0–0.3 based on shared terms (simple semantic relevance).
fn calculate_word_overlap(query: &str, content: &str) -> f32 {
    let query_lower = query.to_lowercase();
    let content_lower = content.to_lowercase();
    let query_words: std::collections::HashSet<&str> = query_lower.split_whitespace().collect();
    if query_words.is_empty() { return 0.0; }
    let content_words: std::collections::HashSet<&str> = content_lower.split_whitespace().collect();
    let overlap = query_words.intersection(&content_words).count() as f32;
    let score = overlap / query_words.len() as f32;
    (score * 0.3).min(0.3) // Cap at 30%
}

/// Safe string truncation that respects UTF-8 character boundaries.
fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn contains_architectural_keywords(text: &str) -> bool {
    let keywords = [
        "module", "struct ", "trait ", "interface", "abstract ", "impl ",
        "enum ", "protocol", "architecture", "design pattern",
        "middleware", "pipeline", "handler", "service", "repository",
        "dependency", "injection", "factory", "builder", "singleton",
        "microservice", "monolith", "layer", "component",
    ];
    keywords.iter().any(|k| text.to_lowercase().contains(k))
}

fn contains_business_keywords(text: &str) -> bool {
    let keywords = [
        "api", "endpoint", "model", "schema", "controller",
        "entity", "dto", "request", "response", "route",
        "auth", "login", "register", "user", "role", "permission",
        "order", "payment", "product", "inventory",
        "数据库", "表", "查询", "索引", "缓存",
    ];
    keywords.iter().any(|k| text.to_lowercase().contains(k))
}

fn contains_user_preference(text: &str) -> bool {
    let keywords = [
        "以后都", "不要用", "always use", "never use", "prefer", "avoid",
        "习惯", "偏好", "应该用", "选型", "用这个", "改成",
        "convention", "standard", "rule", "must", "should",
    ];
    keywords.iter().any(|k| text.to_lowercase().contains(k))
}

// ── MemoryManager (门面) ──

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::MemoryConfig;

pub struct MemoryManager {
    project_store: Option<store::MemoryStore>,
    overall_store: store::MemoryStore,
    write_buffer: Mutex<WriteBuffer>,
    // 🆕 Query cache for repeated searches
    query_cache: Mutex<HashMap<String, (Vec<MemoryNode>, chrono::DateTime<chrono::Utc>)>>,
    // 🆕 Semantic association manager for dynamic query expansion
    semantic_manager: Option<semantic::SemanticAssociationManager>,
    // 🆕 Hybrid storage for L0-L3 architecture (optional)
    hybrid_storage: Option<hybrid_storage::HybridStorage>,
    // 🆕 Vector-backed semantic memory search (optional, requires embedding model)
    memory_vector_store: Mutex<Option<memory_vector::MemoryVectorStore>>,
}

impl Clone for MemoryManager {
    fn clone(&self) -> Self {
        Self {
            project_store: self.project_store.clone(),
            overall_store: self.overall_store.clone(),
            write_buffer: Mutex::new(WriteBuffer::new()),
            query_cache: Mutex::new(HashMap::new()),  // Don't clone cache
            semantic_manager: self.semantic_manager.clone(),
            hybrid_storage: None,  // Don't clone hybrid storage (expensive)
            memory_vector_store: Mutex::new(None),  // Don't clone vector store
        }
    }
}

impl MemoryManager {
    pub fn init(
        runtime_ox_home: &PathBuf,
        project_id: &str,
        config: &MemoryConfig,
    ) -> anyhow::Result<Self> {
        let db_dir = runtime_ox_home.join("db");
        let overall_path = db_dir.join("memories_overall.db");
        let overall_store = store::MemoryStore::open(&overall_path)?;
        overall_store.checkpoint()?;

        let project_store = if !project_id.is_empty() {
            let project_path = db_dir.join(format!("memories_{}.db", project_id));
            let store = store::MemoryStore::open(&project_path)?;
            store.checkpoint()?;
            Some(store)
        } else {
            None
        };

        let _max_nodes = config.max_nodes;

        // Initialize semantic association manager (open new connection to same database)
        let overall_path = db_dir.join("memories_overall.db");
        let semantic_manager = if let Ok(conn) = rusqlite::Connection::open(&overall_path) {
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;").ok();
            Some(semantic::SemanticAssociationManager::new(
                Arc::new(std::sync::Mutex::new(conn)),
            ))
        } else {
            tracing::warn!("Failed to open semantic database");
            None
        };

        // 🆕 Initialize hybrid storage for L0-L3 architecture
        let hybrid_storage = match hybrid_storage::HybridStorage::new(runtime_ox_home) {
            Ok(storage) => {
                tracing::info!("✅ Hybrid storage initialized (L0-L3 architecture enabled)");
                Some(storage)
            }
            Err(e) => {
                tracing::warn!("⚠️ Failed to initialize hybrid storage: {}. Using legacy store only.", e);
                None
            }
        };

        Ok(Self {
            project_store,
            overall_store,
            write_buffer: Mutex::new(WriteBuffer::new()),
            query_cache: Mutex::new(HashMap::new()),  // Initialize empty cache
            semantic_manager,
            hybrid_storage,
            memory_vector_store: Mutex::new(None),  // Initialized separately via init_vector_store
        })
    }

    pub fn store(&self, mut node: MemoryNode) {
        node.content = crate::safety::sanitizer::DataSanitizer::sanitize_all(&node.content);
        let is_immediate = node.node_type.is_immediate_write();
        let is_long_term = node.node_type.is_long_term();

        // 🆕 Index into vector store for semantic search (before potential move)
        self.index_to_vector_store(&node);

        if is_immediate {
            if is_long_term || node.project_id.is_none() {
                if let Err(e) = self.overall_store.insert(&node) {
                    tracing::warn!("Failed to write memory (overall): {e}");
                }
            }
            if let Some(ref store) = self.project_store {
                if let Err(e) = store.insert(&node) {
                    tracing::warn!("Failed to write memory (project): {e}");
                }
            }
        } else {
            let should_flush = self.write_buffer.lock().unwrap().buffer(node);
            if should_flush {
                self.flush();
            }
        }
    }

    pub fn store_explicit(&self, content: &str, project_id: &str, language: &str) {
        let node = MemoryNode::new(
            content.to_string(),
            MemoryNodeType::Style,
            Some(project_id.into()),
            language.into(),
            MemorySource::UserExplicit,
        );
        if let Some(ref store) = self.project_store {
            if let Err(e) = store.insert(&node) {
                tracing::warn!("Failed to store explicit memory: {e}");
            }
        }
        // 🆕 Index into vector store
        self.index_to_vector_store(&node);
    }

    /// Initialize the memory vector store for semantic search.
    /// Call this in a background task after the embedding model is loaded.
    ///
    /// # Arguments
    /// * `embedding_config` - Embedding model configuration
    /// * `db_dir` - Directory for the TriviumDB file (e.g. `~/.ox/db/`)
    pub fn init_vector_store(&self, embedding_config: &crate::config::EmbeddingConfig, db_dir: &PathBuf) {
        if !embedding_config.enabled {
            tracing::info!("[MEMORY_VECTOR] Embedding disabled via config");
            return;
        }

        let tdb_path = db_dir.join("memories.tdb");
        let path_str = tdb_path.to_string_lossy().to_string();

        match memory_vector::MemoryVectorStore::open_standalone(&path_str, embedding_config) {
            Ok(store) => {
                tracing::info!(
                    "[MEMORY_VECTOR] ✅ Memory vector store initialized at {} (dim={})",
                    path_str, store.dimension()
                );
                *self.memory_vector_store.lock().unwrap() = Some(store);
            }
            Err(e) => {
                tracing::warn!(
                    "[MEMORY_VECTOR] ❌ Failed to initialize: {}. Semantic memory search disabled.",
                    e
                );
            }
        }
    }

    /// Index a single memory node into the vector store (if available).
    fn index_to_vector_store(&self, node: &MemoryNode) {
        let mut guard = self.memory_vector_store.lock().unwrap();
        if let Some(ref mut store) = *guard {
            if let Err(e) = store.index_node(node) {
                tracing::debug!("[MEMORY_VECTOR] Failed to index node {}: {}", node.id, e);
            }
        }
    }

    pub fn update_from_turn(
        &self,
        messages: &[crate::message::Message],
        project_id: &str,
        language: &str,
    ) {
        // Track successful tool calls for learning
        let mut successful_tools = Vec::new();
        
        // 🆕 Build a map of tool_call_id -> tool_result_content for accessing results
        let mut tool_results_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for msg in messages {
            if let crate::message::Message::ToolResult { tool_call_id, content } = msg {
                tool_results_map.insert(tool_call_id.clone(), content.clone());
            }
        }
        
        for msg in messages {
            if let crate::message::Message::Assistant {
                content,
                tool_calls,
                ..
            } = msg
            {
                // Extract knowledge from tool calls (with access to results)
                for tc in tool_calls {
                    // Get the corresponding tool result if available
                    let tool_result = tool_results_map.get(&tc.id).cloned();
                    
                    if let Some(node) = MemoryNode::extract_from_tool_call_with_result(
                        &tc.name,
                        &tc.arguments,
                        tool_result.as_deref(),
                        project_id,
                        language,
                    ) {
                        self.store(node);
                        successful_tools.push(tc.name.clone());
                    }
                }
                
                // Extract knowledge from conversation content
                if let Some(node) =
                    MemoryNode::extract_from_conversation(content, project_id, language)
                {
                    self.store(node);
                }
            }
        }
        
        // Log learning statistics for debugging
        if !successful_tools.is_empty() {
            tracing::debug!(
                "[LEARNING] Extracted {} memories from tools: {:?}",
                successful_tools.len(),
                successful_tools
            );
        }

        // 🧠 Requirement-Trace Memory: "user request → files changed → why"
        self.create_turn_summary(messages, project_id, language);
    }

    /// Create a structured memory node: what the user asked, what files changed, why.
    ///
    /// Format:
    /// ```text
    /// ## Turn Summary
    /// **Request**: (user's ask)
    /// **Files Changed**: path1, path2
    /// **Why**: (extracted reasoning from assistant)
    /// ```
    fn create_turn_summary(&self, messages: &[crate::message::Message], project_id: &str, language: &str) {
        use crate::message::Message;
        
        let user_request = messages.iter()
            .filter_map(|m| if let Message::User { content } = m { Some(content.as_str()) } else { None })
            .last()
            .unwrap_or("(no request)");
        
        // 🆕 Parse structured LLM output: ## Plan and ## Done blocks
        let mut plan_files: Vec<String> = Vec::new();
        let mut plan_reason = String::new();
        let mut done_created: Vec<String> = Vec::new();
        let mut done_modified: Vec<String> = Vec::new();
        let mut done_verified: String = String::new();
        
        for msg in messages {
            if let Message::Assistant { content, .. } = msg {
                // Parse ## Plan block
                if let Some(plan_start) = content.find("## Plan") {
                    let plan_text = &content[plan_start..];
                    let plan_end = plan_text.find("## Done").unwrap_or(plan_text.len());
                    let plan_block = &plan_text[..plan_end];
                    for line in plan_block.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("- File:") || trimmed.starts_with("- **File:**") {
                            let file = trimmed.trim_start_matches("- File:").trim_start_matches("- **File:**").trim();
                            let file = file.trim_matches('`').trim_matches('*');
                            if !file.is_empty() { plan_files.push(file.to_string()); }
                        }
                        if trimmed.starts_with("- Reason:") {
                            plan_reason = trimmed.trim_start_matches("- Reason:").trim().to_string();
                        }
                    }
                }
                // Parse ## Done block
                if let Some(done_start) = content.find("## Done") {
                    let done_block = &content[done_start..];
                    for line in done_block.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("- Created:") {
                            let entry = trimmed.trim_start_matches("- Created:").trim();
                            done_created.push(entry.to_string());
                        }
                        if trimmed.starts_with("- Modified:") {
                            let entry = trimmed.trim_start_matches("- Modified:").trim();
                            done_modified.push(entry.to_string());
                        }
                        if trimmed.starts_with("- Verified:") {
                            done_verified = trimmed.trim_start_matches("- Verified:").trim().to_string();
                        }
                    }
                }
            }
        }
        
        // Use Done block as primary source, fall back to heuristic
        if !done_created.is_empty() || !done_modified.is_empty() {
            let created = done_created.join("; ");
            let modified = done_modified.join("; ");
            let request = user_request.chars().take(200).collect::<String>();
            let why = if plan_reason.is_empty() { "(no reason given)" } else { &plan_reason };
            
            let content = format!(
                "## Turn Summary\n\
                 **Request**: {request}\n\
                 **Created**: {created}\n\
                 **Modified**: {modified}\n\
                 **Verified**: {done_verified}\n\
                 **Why**: {why}"
            );
            
            // Collect file paths from Done blocks
            let all_files: Vec<String> = done_created.iter()
                .chain(done_modified.iter())
                .filter_map(|e| {
                    // Extract path from format like "`src/auth.rs` — purpose"
                    let path = e.trim_matches('`').split('`').next().unwrap_or(e);
                    let path = path.split(" — ").next().unwrap_or(path).trim();
                    if path.is_empty() { None } else { Some(path.to_string()) }
                })
                .collect();
            
            let mut node = MemoryNode::new(
                content,
                MemoryNodeType::Pattern,
                Some(project_id.to_string()),
                language.to_string(),
                MemorySource::RefinedSummary,
            );
            if !all_files.is_empty() {
                node = node.with_related_files(&all_files);
            }
            node = node.with_critical();
            self.store(node);
            return;
        }
        
        // Fallback: heuristic extraction (legacy)
        let mut files_changed: Vec<String> = Vec::new();
        let mut operations: Vec<String> = Vec::new();
        let mut assistant_reasoning = String::new();
        
        for msg in messages {
            if let Message::Assistant { content, tool_calls, .. } = msg {
                if assistant_reasoning.is_empty() && !content.is_empty() {
                    assistant_reasoning = content.chars().take(300).collect();
                }
                for tc in tool_calls {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                            if !files_changed.contains(&path.to_string()) {
                                files_changed.push(path.to_string());
                            }
                            match tc.name.as_str() {
                                "file_write" => operations.push(format!("created {}", path)),
                                "edit_file" => operations.push(format!("patched {}", path)),
                                "delete_range" => operations.push(format!("deleted range in {}", path)),
                                _ => {}
                            }
                        }
                        if tc.name == "shell_exec" {
                            let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
                            operations.push(format!("ran `{}`", if cmd.len() > 60 { &cmd[..60] } else { cmd }));
                        }
                    }
                }
            }
        }
        
        if !files_changed.is_empty() || !operations.is_empty() {
            let request = user_request.chars().take(200).collect::<String>();
            let files = files_changed.join(", ");
            let ops = operations.join("; ");
            let why = if assistant_reasoning.is_empty() { "(no explanation)" } else { &assistant_reasoning };
            
            let content = format!(
                "## Turn Summary\n\
                 **Request**: {request}\n\
                 **Files Changed**: {files}\n\
                 **Operations**: {ops}\n\
                 **Why**: {why}"
            );
            
            let mut node = MemoryNode::new(
                content,
                MemoryNodeType::Pattern,
                Some(project_id.to_string()),
                language.to_string(),
                MemorySource::RefinedSummary,
            );
            node = node.with_related_files(&files_changed).with_critical();
            self.store(node);
        }

        // 🆕 Always create a lightweight Turn Summary, even for read-only turns.
        // This ensures code reading sessions are also remembered.
        let request = user_request.chars().take(150).collect::<String>();
        let content = format!("[TURN] Request: {request}");
        let mut node = MemoryNode::new(
            content,
            MemoryNodeType::Fact,
            Some(project_id.to_string()),
            language.to_string(),
            MemorySource::RefinedSummary,
        );
        // Boost depth so it ranks higher in retrieval
        node.depth = 2;
        self.store(node);
    }

    /// 🆕 Record LLM-extracted keywords for semantic learning (synchronous, fast)
    pub fn record_llm_keywords(
        &self,
        user_query: &str,
        extracted: semantic::KeywordExtraction,
    ) {
        if let Some(ref manager) = self.semantic_manager {
            match manager.record_llm_keywords(user_query, &extracted) {
                Ok(_) => {
                    tracing::info!(
                        "[SEMANTIC LEARNING] ✅ Recorded {} keywords, {} topics for query: '{}'",
                        extracted.keywords.len(),
                        extracted.topics.len(),
                        user_query.chars().take(50).collect::<String>()
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "[SEMANTIC LEARNING] ❌ Failed to record keywords: {}",
                        e
                    );
                }
            }
        } else {
            tracing::warn!(
                "[SEMANTIC LEARNING] ⚠️ Semantic manager not initialized - keywords will not be recorded"
            );
        }
    }

    /// Multi-path memory retrieval with entity-based expansion and optional re-ranking.
    /// 
    /// This enhanced version performs parallel searches across multiple paths:
    /// 1. Original query (semantic search)
    /// 2. Extracted entities (file names, function names, technical terms)
    /// 3. Type-specific queries (architecture, style, best practices)
    /// 
    /// Results are merged, deduplicated, ranked by composite score, and optionally
    /// re-ranked using cross-encoding for higher accuracy.
    /// 
    /// # Arguments
    /// * `query` - Search query
    /// * `project_id` - Optional project ID for scoped search
    /// Retrieve memories with optional re-ranking.
    /// Delegates to `retrieve()` — LLM Judge re-ranking is handled at config level.
    pub fn retrieve_with_rerank(
        &self,
        query: &str,
        project_id: &Option<&str>,
        limit: usize,
    ) -> Vec<MemoryNode> {
        self.retrieve(query, project_id, limit)
    }
    
    /// Legacy retrieve method (without re-ranking).
    /// Use retrieve_with_rerank for better accuracy.
    pub fn retrieve(
        &self,
        query: &str,
        project_id: &Option<&str>,
        limit: usize,
    ) -> Vec<MemoryNode> {
        // 🆕 Check cache first
        let cache_key = format!("{}:{:?}:{}", query, project_id, limit);
        if let Some((cached, timestamp)) = self.query_cache.lock().unwrap().get(&cache_key) {
            let age_secs = chrono::Utc::now().signed_duration_since(*timestamp).num_seconds();
            if age_secs < 300 {  // Cache TTL: 5 minutes
                tracing::debug!("[MEMORY CACHE] Hit for query: {}", query);
                return cached.clone();
            }
        }
        
        // 🆕 Step 1: Query expansion using semantic associations
        let mut expanded_queries = vec![query.to_string()];
        if let Some(ref manager) = self.semantic_manager {
            match manager.get_related_terms(query, 0.6) {
                Ok(related) => {
                    if !related.is_empty() {
                        tracing::info!(
                            "[SEMANTIC EXPANSION] ✅ '{}' → {} related terms: {:?}",
                            query,
                            related.len(),
                            related
                        );
                        expanded_queries.extend(related);
                    } else {
                        tracing::debug!(
                            "[SEMANTIC EXPANSION] ⚠️ No related terms found for '{}'",
                            query
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[SEMANTIC EXPANSION] ❌ Failed to get related terms: {}",
                        e
                    );
                }
            }
        } else {
            tracing::debug!(
                "[SEMANTIC EXPANSION] ⚠️ Semantic manager not initialized - skipping expansion"
            );
        }
        
        let mut all_results: std::collections::HashMap<String, (MemoryNode, f32)> = std::collections::HashMap::new();
        
        // 🎯 PATH 1: Original + expanded queries (weight: 1.0)
        for search_term in &expanded_queries {
            if !search_term.is_empty() {
                if let Some(pid) = project_id {
                    if let Ok(mems) = self
                        .project_store
                        .as_ref()
                        .unwrap_or_else(|| &self.overall_store)
                        .search(search_term, Some(pid), limit)
                    {
                        if !mems.is_empty() {
                            tracing::debug!(
                                "[MEMORY RETRIEVAL] PATH 1 (query): '{}' → {} results",
                                search_term,
                                mems.len()
                            );
                        }
                        for m in mems {
                            all_results.entry(m.id.clone())
                                .or_insert_with(|| (m, 1.0));
                        }
                    }
                }
            }
        }
        
        // 🎯 PATH 2: Entity-based retrieval (weight: 0.8)
        let entities = self.extract_query_entities(query);
        if !entities.is_empty() {
            tracing::debug!(
                "[MEMORY RETRIEVAL] Extracted {} entities: {:?}",
                entities.len(),
                entities
            );
        }
        for entity in &entities {
            if let Some(pid) = project_id {
                if let Ok(mems) = self
                    .project_store
                    .as_ref()
                    .unwrap_or_else(|| &self.overall_store)
                    .search(entity, Some(pid), 3)  // Limit per entity
                {
                    if !mems.is_empty() {
                        tracing::debug!(
                            "[MEMORY RETRIEVAL] PATH 2 (entity '{}'): {} results",
                            entity,
                            mems.len()
                        );
                    }
                    for m in mems {
                        all_results.entry(m.id.clone())
                            .and_modify(|(_, score)| *score = (*score + 0.8).min(2.0))
                            .or_insert_with(|| (m, 0.8));
                    }
                }
            }
        }
        
        // 🎯 PATH 3: Type-specific project memories (weight: 0.6)
        if let Some(pid) = project_id {
            if let Some(ref store) = self.project_store {
                if let Ok(mems) = store.query_by_project(
                    pid,
                    &[
                        MemoryNodeType::Architectural,
                        MemoryNodeType::Business,
                        MemoryNodeType::Style,
                    ],
                    limit / 2,  // Reduced limit for type-specific
                ) {
                    if !mems.is_empty() {
                        tracing::debug!(
                            "[MEMORY RETRIEVAL] PATH 3 (type-specific): {} results",
                            mems.len()
                        );
                    }
                    for m in mems {
                        all_results.entry(m.id.clone())
                            .and_modify(|(_, score)| *score = (*score + 0.6).min(2.0))
                            .or_insert_with(|| (m, 0.6));
                    }
                }
            }
        }
        
        // 🎯 PATH 4: Global best practices (weight: 0.5)
        if let Ok(mems) = self.overall_store.query_overall(
            &[
                MemoryNodeType::BestPractice,
                MemoryNodeType::Pattern,
                MemoryNodeType::MetaSkill,
            ],
            limit / 2,
        ) {
            if !mems.is_empty() {
                tracing::debug!(
                    "[MEMORY RETRIEVAL] PATH 4 (global): {} results",
                    mems.len()
                );
            }
            for m in mems {
                all_results.entry(m.id.clone())
                    .and_modify(|(_, score)| *score = (*score + 0.5).min(2.0))
                    .or_insert_with(|| (m, 0.5));
            }
        }

        // 🎯 PATH 5: Vector semantic search (weight: 0.9 — high because true semantic match)
        // This catches memories that are semantically similar but don't share exact keywords.
        {
            let guard = self.memory_vector_store.lock().unwrap();
            if let Some(ref vs) = *guard {
                match vs.search(query, limit) {
                    Ok(hits) if !hits.is_empty() => {
                        tracing::debug!(
                            "[MEMORY RETRIEVAL] PATH 5 (vector semantic): {} hits for '{}'",
                            hits.len(), query
                        );
                        for hit in hits {
                            // Only add if not already found by keyword paths (avoid duplicates)
                            if !all_results.contains_key(&hit.node_id) {
                                // Reconstruct a minimal MemoryNode from the vector hit
                                let node = MemoryNode::new(
                                    hit.content,
                                    hit.node_type,
                                    hit.project_id,
                                    String::new(),
                                    MemorySource::LlmExtraction,  // Best guess
                                );
                                let weight = 0.9 * hit.score.min(1.0);  // Scale by similarity
                                all_results.entry(hit.node_id)
                                    .or_insert_with(|| (node, weight));
                            } else {
                                // Boost existing keyword match with semantic confirmation
                                all_results.entry(hit.node_id)
                                    .and_modify(|(_, score)| {
                                        *score = (*score + 0.4 * hit.score).min(2.0);
                                    });
                            }
                        }
                    }
                    Ok(_) => {
                        tracing::debug!(
                            "[MEMORY RETRIEVAL] PATH 5 (vector semantic): no hits for '{}'",
                            query
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[MEMORY RETRIEVAL] PATH 5 (vector semantic) failed: {}",
                            e
                        );
                    }
                }
            }
        }
        
        // Log total candidates before filtering
        tracing::debug!(
            "[MEMORY RETRIEVAL] Total candidates before decay filter: {}",
            all_results.len()
        );
        
        // Convert to vector and apply decay filter
        let mut results: Vec<(MemoryNode, f32)> = all_results.into_values().collect();
        
        let before_filter_count = results.len();
        results.retain(|(n, _)| {
            let decay = if n.project_id.is_some() {
                calculate_project_decay(n, 30)
            } else {
                calculate_overall_decay(n, &[0.1, 0.2, 0.3, 0.4, 0.5])
            };
            decay > 0.3 || n.is_project_critical
        });
        
        let after_filter_count = results.len();
        if before_filter_count > 0 && after_filter_count < before_filter_count {
            tracing::info!(
                "[MEMORY RETRIEVAL] Decay filter: {} → {} memories (removed {})",
                before_filter_count,
                after_filter_count,
                before_filter_count - after_filter_count
            );
        } else if before_filter_count == 0 {
            tracing::info!(
                "[MEMORY RETRIEVAL] ⚠️ No candidates found - database may be empty or query doesn't match"
            );
        }
        
        // Sort by combined score (relevance + composite + recency + word overlap + LLM feedback)
        let now = chrono::Utc::now().timestamp();
        let query_for_sort = query.to_string();
        results.sort_by(|(a, weight_a), (b, weight_b)| {
            let base_score_a = composite_score(a, 30) * weight_a;
            let base_score_b = composite_score(b, 30) * weight_b;
            
            // Recency boost: memories accessed in last hour get +20%, last day +10%
            let recency_a = {
                let age_hours = (now - a.last_accessed) as f64 / 3600.0;
                if age_hours < 1.0 { 0.2 } else if age_hours < 24.0 { 0.1 } else { 0.0 }
            };
            let recency_b = {
                let age_hours = (now - b.last_accessed) as f64 / 3600.0;
                if age_hours < 1.0 { 0.2 } else if age_hours < 24.0 { 0.1 } else { 0.0 }
            };
            
            // File relevance boost: memories with related_files get +15%
            let file_boost_a = if !a.related_files.is_empty() { 0.15 } else { 0.0 };
            let file_boost_b = if !b.related_files.is_empty() { 0.15 } else { 0.0 };
            
            // Word overlap boost: simple semantic relevance via term overlap
            let word_overlap_a = calculate_word_overlap(&query_for_sort, &a.content);
            let word_overlap_b = calculate_word_overlap(&query_for_sort, &b.content);
            
            // LLM feedback boost
            let llm_boost_a = if a.avg_llm_score > 0.0 { (a.avg_llm_score / 10.0) * 0.3 } else { 0.0 };
            let llm_boost_b = if b.avg_llm_score > 0.0 { (b.avg_llm_score / 10.0) * 0.3 } else { 0.0 };
            
            let final_score_a = base_score_a * (1.0 + recency_a + file_boost_a + word_overlap_a + llm_boost_a);
            let final_score_b = base_score_b * (1.0 + recency_b + file_boost_b + word_overlap_b + llm_boost_b);
            
            final_score_b.partial_cmp(&final_score_a).unwrap_or(std::cmp::Ordering::Equal)
        });
        
        // Extract just the nodes, truncate to limit
        results.truncate(limit);
        let final_results: Vec<MemoryNode> = results.into_iter().map(|(node, _)| node).collect();
        
        // 🆕 Cache the results
        self.query_cache.lock().unwrap().insert(
            cache_key,
            (final_results.clone(), chrono::Utc::now())
        );
        
        final_results
    }

    /// Retrieve memories related to specific files.
    /// 
    /// This is useful when the user is working on specific files and wants
    /// context-aware memory retrieval.
    /// 
    /// # Arguments
    /// * `file_paths` - List of file paths to search for related memories
    /// * `project_id` - Optional project ID for scoped search
    /// * `limit` - Maximum number of results to return
    pub fn retrieve_by_files(
        &self,
        file_paths: &[String],
        project_id: &Option<&str>,
        limit: usize,
    ) -> Vec<MemoryNode> {
        if file_paths.is_empty() {
            return Vec::new();
        }

        let mut all_results: std::collections::HashMap<String, (MemoryNode, f32)> = std::collections::HashMap::new();

        // Search for memories that have related_files matching any of the provided paths
        for file_path in file_paths {
            let search_term = file_path.split('/').last().unwrap_or(file_path); // Also search by filename
            
            if let Some(pid) = project_id {
                if let Some(ref store) = self.project_store {
                    if let Ok(mems) = store.search(search_term, Some(pid), limit) {
                        for m in mems {
                            // Only include if it has related files
                            if !m.related_files.is_empty() {
                                // Boost score if file_path matches exactly
                                let boost = if m.related_files.contains(file_path) {
                                    1.5
                                } else {
                                    1.0
                                };
                                
                                all_results.entry(m.id.clone())
                                    .and_modify(|(_, score)| *score = (*score + boost).min(2.0))
                                    .or_insert_with(|| (m, boost));
                            }
                        }
                    }
                }
            }
        }

        // Convert to vector and sort by score
        let mut results: Vec<(MemoryNode, f32)> = all_results.into_values().collect();
        results.sort_by(|(_, score_a), (_, score_b)| {
            score_b.partial_cmp(score_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        results.truncate(limit);
        results.into_iter().map(|(node, _)| node).collect()
    }
    
    /// 🆕 Enhanced entity extraction for better file_read memory retrieval
    fn extract_query_entities(&self, query: &str) -> Vec<String> {
        let mut entities = Vec::new();
        
        // 1. Extract file paths and names (enhanced pattern)
        if let Ok(file_pattern) = regex::Regex::new(r"[\w./-]+\.(rs|toml|json|md|py|js|ts|go|jsx|tsx|java|cpp|c|h)") {
            for mat in file_pattern.find_iter(query) {
                let file_name = mat.as_str().to_string();
                if !entities.contains(&file_name) {
                    entities.push(file_name);
                }
            }
        }
        
        // 2. Extract code identifiers (backtick-wrapped)
        if let Ok(ident_pattern) = regex::Regex::new(r"`([\w_]+)`") {
            for mat in ident_pattern.find_iter(query) {
                let ident = mat.as_str().trim_matches('`').to_string();
                if ident.len() > 2 && !entities.contains(&ident) {
                    entities.push(ident);
                }
            }
        }
        
        // 3. Extract technical terms (expanded list)
        let tech_terms = [
            "authentication", "authorization", "database", "api", "http",
            "async", "await", "error handling", "testing", "deployment",
            "refactor", "optimize", "performance", "security",
            "function", "method", "class", "struct", "interface",
            "module", "component", "service", "controller"
        ];
        let query_lower = query.to_lowercase();
        for term in &tech_terms {
            if query_lower.contains(term) && !entities.iter().any(|e| e.to_lowercase() == *term) {
                entities.push(term.to_string());
            }
        }
        
        // Limit to top 5 most relevant entities
        entities.truncate(5);
        entities
    }

    pub fn flush(&self) {
        let batch = self.write_buffer.lock().unwrap().drain();
        if batch.is_empty() {
            return;
        }
        for node in &batch {
            if node.node_type.is_long_term() || node.project_id.is_none() {
                if let Err(e) = self.overall_store.insert(node) {
                    tracing::warn!("Failed to flush memory (overall): {e}");
                }
            }
            if let Some(ref store) = self.project_store {
                if let Err(e) = store.insert(node) {
                    tracing::warn!("Failed to flush memory (project): {e}");
                }
            }
            // 🆕 Index flushed nodes into vector store
            self.index_to_vector_store(node);
        }
    }

    pub fn format_memory_context(&self, nodes: &[MemoryNode], use_xml: bool) -> String {
        if nodes.is_empty() {
            return String::new();
        }

        // 🆕 Separate file_read memories from other memories
        let mut file_memories = Vec::new();
        let mut other_memories = Vec::new();
        
        for node in nodes {
            if node.content.starts_with("[FILE]") || node.content.starts_with("Read file:") {
                file_memories.push(node);
            } else {
                other_memories.push(node);
            }
        }
        
        let mut out = String::new();
        
        // 🆕 Format file memories with special warning header
        if !file_memories.is_empty() {
            out.push_str("\n## ⚠️ IMPORTANT: Files You've Already Read\n\n");
            out.push_str("You have recently read the following files. **DO NOT re-read them unless explicitly requested by the user.**\n");
            out.push_str("Use your memory of these files to answer questions directly.\n\n");
            
            for n in file_memories.iter().take(5) {
                // Extract filename and brief info
                let summary_line = if n.content.starts_with("[FILE]") {
                    // New format: "[FILE] main.rs | Read src file with 150 lines..."
                    n.content.lines().next().unwrap_or(&n.content).to_string()
                } else {
                    // Old format: "Read file: src/main.rs"
                    n.content.clone()
                };
                
                out.push_str(&format!("- {}\n", summary_line));
            }
            
            out.push_str("\n**Rule**: If the user asks about a file listed above, assume you already know its content. Only re-read if:\n");
            out.push_str("- The user explicitly says \"read the file again\"\n");
            out.push_str("- You suspect the file has been modified since you last read it\n");
            out.push_str("- You need to see a different part of the file (use offset/limit parameters)\n\n");
        }
        
        // Format other memories
        if !other_memories.is_empty() {
            if use_xml {
                // XML format for compatible APIs
                out.push_str("<relevant_memories>\n");
                for n in other_memories.iter().take(8) {
                    // 🆕 Dynamic truncation based on LLM score with chunking
                    let max_len = if n.avg_llm_score >= 8.0 {
                        350  // High-score memory: preserve more context
                    } else if n.avg_llm_score >= 6.0 {
                        250  // Medium-score memory: normal length
                    } else if n.avg_llm_score > 0.0 {
                        180  // Low-score memory: concise
                    } else {
                        250  // Unrated memory: default length
                    };
                    
                    // 🆕 Use sliding window chunking for long memories
                    let content = if n.content.len() > max_len * 2 {
                        // For very long memories, use first chunk with overlap
                        let chunks = split_with_overlap(&n.content, max_len, 0.15);
                        format!("{}...", chunks[0])
                    } else if n.content.len() > max_len {
                        // 🚨 FIX: Use char boundary to avoid slicing in the middle of UTF-8 characters
                        let char_boundary = n.content
                            .char_indices()
                            .nth(max_len)
                            .map(|(idx, _)| idx)
                            .unwrap_or(n.content.len());
                        format!("{}...", &n.content[..char_boundary])
                    } else {
                        n.content.clone()
                    };
                    out.push_str(&format!(
                        "  <memory depth=\"{}\" type=\"{}\">{}</memory>\n",
                        n.depth, n.node_type, content
                    ));
                }
                out.push_str("</relevant_memories>");
            } else {
                // Plain text format for MiniMax and similar APIs
                out.push_str("Relevant context:\n");
                for n in other_memories.iter().take(5) {
                    // 🆕 Dynamic truncation based on LLM score with chunking
                    let max_len = if n.avg_llm_score >= 8.0 {
                        400  // High-score memory: preserve more context
                    } else if n.avg_llm_score >= 6.0 {
                        280  // Medium-score memory: normal length
                    } else if n.avg_llm_score > 0.0 {
                        200  // Low-score memory: concise
                    } else {
                        280  // Unrated memory: default length
                    };
                    
                    // 🆕 Use sliding window chunking for long memories
                    let content = if n.content.len() > max_len * 2 {
                        // For very long memories, use first chunk with overlap
                        let chunks = split_with_overlap(&n.content, max_len, 0.15);
                        format!("{}...", chunks[0])
                    } else if n.content.len() > max_len {
                        // 🚨 FIX: Use char boundary to avoid slicing in the middle of UTF-8 characters
                        let char_boundary = n.content
                            .char_indices()
                            .nth(max_len)
                            .map(|(idx, _)| idx)
                            .unwrap_or(n.content.len());
                        format!("{}...", &n.content[..char_boundary])
                    } else {
                        n.content.clone()
                    };
                    out.push_str(&format!("- {}\n", content));
                }
            }
        }
        
        out
    }

    pub fn forget(&self, keyword: &str, project_id: &str) -> usize {
        let mut deleted = 0;
        if let Some(ref store) = self.project_store {
            if let Ok(results) = store.search(keyword, Some(project_id), 20) {
                for node in &results {
                    if let Ok(()) = store.delete(&node.id) {
                        deleted += 1;
                    }
                }
            }
        }
        deleted
    }

    pub fn stats(&self, project_id: &str) -> (usize, usize) {
        let project_count = self
            .project_store
            .as_ref()
            .and_then(|s| s.count_by_project(project_id).ok())
            .unwrap_or(0);
        let overall_count = self.overall_store.count_overall().unwrap_or(0);
        (project_count, overall_count)
    }

    /// 🆕 Get summary of recently read files from memory.
    /// 
    /// This is used to remind the LLM which files it has already read,
    /// preventing redundant file_read operations.
    /// 
    /// # Arguments
    /// * `limit` - Maximum number of recent files to return
    /// 
    /// # Returns
    /// A formatted string listing recently read files with brief descriptions
    pub fn get_recent_files_summary(&self, limit: usize) -> String {
        // Query recent file_read memories
        if let Some(ref store) = self.project_store {
            if let Ok(mems) = store.query_by_project(
                "",
                &[MemoryNodeType::Fact],
                limit + 10,  // Get extra to filter
            ) {
                // Filter for file_read memories and extract unique files
                let mut seen_files = std::collections::HashSet::new();
                let mut recent_files: Vec<(String, String)> = Vec::new();  // (file_path, summary)
                
                for mem in mems {
                    // Check if this is a file_read memory
                    if mem.content.starts_with("[FILE]") || mem.content.starts_with("Read file:") {
                        // Extract file path
                        let file_path = if mem.content.starts_with("[FILE]") {
                            // New format: "[FILE] main.rs | Read src file with 150 lines..."
                            mem.content.split('|').next()
                                .map(|s| s.replace("[FILE]", "").trim().to_string())
                                .unwrap_or_default()
                        } else {
                            // Old format: "Read file: src/main.rs"
                            mem.content.replace("Read file:", "").trim().to_string()
                        };
                        
                        if !file_path.is_empty() && !seen_files.contains(&file_path) {
                            seen_files.insert(file_path.clone());
                            
                            // Create a brief summary
                            let summary = if mem.content.len() > 200 {
                                format!("{}...", &mem.content[..200])
                            } else {
                                mem.content.clone()
                            };
                            
                            recent_files.push((file_path, summary));
                            
                            if recent_files.len() >= limit {
                                break;
                            }
                        }
                    }
                }
                
                // Format as a list
                if recent_files.is_empty() {
                    return String::new();
                }
                
                let mut output = String::from("You have recently read these files:\n\n");
                for (i, (path, summary)) in recent_files.iter().enumerate() {
                    output.push_str(&format!(
                        "{}. **{}**\n   {}\n\n",
                        i + 1,
                        path,
                        summary.lines().next().unwrap_or("")
                    ));
                }
                
                return output;
            }
        }
        
        String::new()
    }

    pub fn reinforce_accessed(&self, ids: &[&str]) {
        for id in ids {
            if let Some(ref store) = self.project_store {
                let _ = store.update_last_accessed(id);
                let _ = store.increment_depth(id);
            }
            let _ = self.overall_store.update_last_accessed(id);
            let _ = self.overall_store.increment_depth(id);
        }
    }
    
    /// 🆕 Update memory with LLM judge feedback.
    /// 
    /// This creates a feedback loop where high-scoring memories are reinforced
    /// and low-scoring memories are weakened or eventually deleted.
    /// 
    /// # Arguments
    /// * `memory_id` - ID of the memory to update
    /// * `llm_score` - Score from LLM judge (0-10)
    /// * `project_id` - Optional project ID for scoped update
    pub fn update_with_llm_feedback(&self, memory_id: &str, llm_score: f32, project_id: Option<&str>) {
        tracing::debug!(
            "[MEMORY FEEDBACK] Updating memory {} with LLM score: {:.1}",
            memory_id,
            llm_score
        );
        
        // Try to update in project store first, then overall store
        let stores = if let Some(pid) = project_id {
            vec![
                (self.project_store.as_ref(), Some(pid)),
                (Some(&self.overall_store), None),
            ]
        } else {
            vec![(Some(&self.overall_store), None)]
        };
        
        for (store_opt, _pid) in stores {
            if let Some(store) = store_opt {
                // Fetch current memory state
                if let Ok(mut node) = self.fetch_memory_by_id(memory_id, store) {
                    // Update recent scores (sliding window)
                    let mut scores = node.recent_scores;
                    scores.rotate_left(1);  // Shift left
                    scores[4] = llm_score;   // Add new score at end
                    node.recent_scores = scores;
                    
                    // Update eval count
                    node.judge_eval_count += 1;
                    
                    // Update average score (exponential moving average)
                    let alpha = 0.3;  // Weight for new score
                    node.avg_llm_score = node.avg_llm_score * (1.0 - alpha) + llm_score * alpha;
                    
                    // Adjust depth based on score
                    if llm_score >= 7.0 {
                        // High score: reinforce
                        node.depth = (node.depth + 1).min(10);
                        tracing::debug!("[MEMORY FEEDBACK] Reinforced memory {} (depth={})", memory_id, node.depth);
                    } else if llm_score < 5.0 {
                        // Low score: weaken
                        if node.depth > 0 {
                            node.depth -= 1;
                            tracing::debug!("[MEMORY FEEDBACK] Weakened memory {} (depth={})", memory_id, node.depth);
                        }
                        
                        // Check if should be deleted (consistently low scores)
                        let low_score_count = node.recent_scores.iter()
                            .filter(|&&s| s > 0.0 && s < 5.0)
                            .count();
                        
                        if low_score_count >= 3 && node.depth == 0 {
                            tracing::info!(
                                "[MEMORY FEEDBACK] Deleting consistently low-scoring memory {}",
                                memory_id
                            );
                            let _ = store.delete(memory_id);
                            return;
                        }
                    }
                    
                    // Save updated memory
                    if let Err(e) = store.insert(&node) {
                        tracing::warn!("[MEMORY FEEDBACK] Failed to save updated memory: {}", e);
                    }
                    
                    return;  // Successfully updated
                }
            }
        }
        
        tracing::warn!("[MEMORY FEEDBACK] Memory {} not found for update", memory_id);
    }
    
    /// 🆕 Batch update multiple memories with LLM feedback (more efficient)
    /// 
    /// This method uses a single transaction to update all memories,
    /// reducing database overhead significantly.
    pub fn update_with_llm_feedback_batch(
        &self,
        feedbacks: Vec<(String, f32)>,  // (memory_id, score)
        project_id: Option<&str>,
    ) {
        if feedbacks.is_empty() {
            return;
        }
        
        tracing::info!(
            "[MEMORY FEEDBACK BATCH] Updating {} memories with batch operation",
            feedbacks.len()
        );
        
        // Collect all updated nodes
        let mut updated_nodes = Vec::new();
        
        // Try to update in project store first, then overall store
        let stores = if let Some(pid) = project_id {
            vec![
                (self.project_store.as_ref(), Some(pid)),
                (Some(&self.overall_store), None),
            ]
        } else {
            vec![(Some(&self.overall_store), None)]
        };
        
        for (store_opt, _pid) in stores {
            if let Some(store) = store_opt {
                let mut updated_count = 0;
                
                for (memory_id, score) in &feedbacks {
                    // Fetch current memory state
                    if let Ok(mut node) = self.fetch_memory_by_id(memory_id, store) {
                        // Update recent scores (sliding window)
                        let mut scores = node.recent_scores;
                        scores.rotate_left(1);
                        scores[4] = *score;
                        node.recent_scores = scores;
                        
                        // Update eval count
                        node.judge_eval_count += 1;
                        
                        // Update average score (exponential moving average)
                        let alpha = 0.3;
                        node.avg_llm_score = node.avg_llm_score * (1.0 - alpha) + score * alpha;
                        
                        // Adjust depth based on score
                        if *score >= 7.0 {
                            node.depth = (node.depth + 1).min(10);
                        } else if *score < 5.0 && node.depth > 0 {
                            node.depth -= 1;
                            
                            // Check if should be deleted
                            let low_score_count = node.recent_scores.iter()
                                .filter(|&&s| s > 0.0 && s < 5.0)
                                .count();
                            
                            if low_score_count >= 3 && node.depth == 0 {
                                let _ = store.delete(memory_id);
                                updated_count += 1;
                                continue;
                            }
                        }
                        
                        updated_nodes.push(node);
                        updated_count += 1;
                    }
                }
                
                // Use insert_batch for efficient bulk update
                if !updated_nodes.is_empty() {
                    if let Err(e) = store.insert_batch(&updated_nodes) {
                        tracing::warn!("[MEMORY FEEDBACK BATCH] Failed to batch insert: {}", e);
                    } else {
                        tracing::info!(
                            "[MEMORY FEEDBACK BATCH] Successfully updated {} memories",
                            updated_count
                        );
                    }
                }
                
                return;  // Successfully processed in this store
            }
        }
    }
    
    /// Helper to fetch a memory by ID from a specific store
    fn fetch_memory_by_id(
        &self,
        memory_id: &str,
        store: &store::MemoryStore,
    ) -> anyhow::Result<MemoryNode> {
        // Query all types to find the memory
        let all_types = &[
            MemoryNodeType::Fact,
            MemoryNodeType::Style,
            MemoryNodeType::Architectural,
            MemoryNodeType::AntiPattern,
            MemoryNodeType::Business,
            MemoryNodeType::BestPractice,
            MemoryNodeType::Pattern,
            MemoryNodeType::MetaSkill,
        ];
        
        // Try project query first
        if let Ok(nodes) = store.query_by_project("", all_types, 1000) {
            if let Some(node) = nodes.into_iter().find(|n| n.id == memory_id) {
                return Ok(node);
            }
        }
        
        anyhow::bail!("Memory {} not found", memory_id)
    }

    /// Get a reference to the overall memory store
    pub fn overall_store(&self) -> &store::MemoryStore {
        &self.overall_store
    }
    
    /// Get learning statistics for a project
    pub fn get_learning_stats(&self, project_id: &str) -> LearningStats {
        let (project_count, overall_count) = self.stats(project_id);
        
        // Count memories by type
        let mut type_counts = std::collections::HashMap::new();
        
        if let Some(ref store) = self.project_store {
            if let Ok(nodes) = store.query_by_project(
                project_id,
                &[
                    MemoryNodeType::Fact,
                    MemoryNodeType::Style,
                    MemoryNodeType::Architectural,
                    MemoryNodeType::AntiPattern,
                    MemoryNodeType::Business,
                    MemoryNodeType::BestPractice,
                    MemoryNodeType::Pattern,
                    MemoryNodeType::MetaSkill,
                ],
                1000,
            ) {
                for node in &nodes {
                    *type_counts.entry(node.node_type.as_str()).or_insert(0) += 1;
                }
            }
        }
        
        LearningStats {
            project_memories: project_count,
            overall_memories: overall_count,
            memories_by_type: type_counts,
        }
    }

    /// 🆕 Run memory promotion pipeline (L0 → L1 → L2 → L3)
    /// 
    /// This triggers the four-tier architecture to distill raw conversations
    /// into high-level project personas.
    /// 
    /// # Arguments
    /// * `project_name` - Human-readable project name for persona generation
    /// 
    /// # Returns
    /// Promotion report or None if hybrid storage is not enabled
    pub fn run_promotion_pipeline(
        &self,
        project_id: &str,
        project_name: &str,
    ) -> Option<anyhow::Result<hybrid_storage::PromotionReport>> {
        if let Some(ref hybrid) = self.hybrid_storage {
            tracing::info!(
                "🚀 Starting memory promotion pipeline for project: {} ({})",
                project_name,
                project_id
            );
            
            let result = hybrid.promote_full_pipeline(project_id, project_name);
            
            if let Ok(ref report) = result {
                tracing::info!("✅ Promotion complete:\n{}", report);
            } else if let Err(ref e) = result {
                tracing::error!("❌ Promotion failed: {}", e);
            }
            
            Some(result)
        } else {
            tracing::debug!("Hybrid storage not enabled - skipping promotion pipeline");
            None
        }
    }

    /// 🆕 Get hybrid storage statistics
    pub fn get_hybrid_stats(&self) -> Option<String> {
        if let Some(ref hybrid) = self.hybrid_storage {
            hybrid.get_stats().ok().map(|stats| format!("{}", stats))
        } else {
            None
        }
    }
}

/// Statistics about learned knowledge
#[derive(Debug, Clone)]
pub struct LearningStats {
    pub project_memories: usize,
    pub overall_memories: usize,
    pub memories_by_type: std::collections::HashMap<&'static str, usize>,
}
