pub mod store;

use std::fmt;

use serde::{Deserialize, Serialize};

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
    Council,
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
            Self::Council => "council",
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
            "council" => Some(Self::Council),
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
            Self::Council => 3,
        }
    }

    pub fn is_immediate_write(&self) -> bool {
        matches!(self, Self::Style | Self::Architectural | Self::AntiPattern | Self::MetaSkill | Self::Council)
    }

    pub fn is_long_term(&self) -> bool {
        matches!(self, Self::BestPractice | Self::Pattern | Self::MetaSkill)
    }
}

// ── Decay strategies ──

pub fn calculate_project_decay(node: &MemoryNode, base_half_life: u64) -> f32 {
    if node.is_project_critical { return 1.0; }
    let age_secs = (chrono::Utc::now().timestamp() - node.last_accessed).max(0);
    let age_days = age_secs as f32 / 86400.0;
    let short_term = (-age_days / (base_half_life as f32 * 0.3)).exp();
    let long_term  = (-age_days / (base_half_life as f32 * 5.0)).exp();
    (0.7 * short_term + 0.3 * long_term).clamp(0.0, 1.0)
}

pub fn calculate_overall_decay(node: &MemoryNode, traces_config: &[f32]) -> f32 {
    let t = ((chrono::Utc::now().timestamp() - node.last_accessed).max(0) as f32) / 86400.0;
    let traces_sum: f32 = node.traces.iter()
        .zip(traces_config.iter())
        .map(|(trace, tau)| trace * (-t / tau).exp())
        .sum();
    let base = if traces_config.is_empty() { 0.5 } else { traces_sum / traces_config.len() as f32 };
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

// ── Janitor ──

impl MemoryManager {
    pub fn run_janitor(&self, _critical_threshold: f32, max_nodes: usize) {
        if let Some(ref store) = self.project_store {
            if let Ok(all) = store.query_by_project(
                "",
                &[MemoryNodeType::Fact, MemoryNodeType::Style, MemoryNodeType::Architectural, MemoryNodeType::Business, MemoryNodeType::AntiPattern],
                max_nodes + 100,
            ) {
                let max_cleanup = (all.len() / 10).max(1);
                let mut expired = Vec::new();
                for node in &all {
                    if node.is_project_critical { continue; }
                    let days = (chrono::Utc::now().timestamp() - node.last_accessed).max(0) as f32 / 86400.0;
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
                        if expired.len() >= max_cleanup { break; }
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
        let node = MemoryNode::new("test".into(), MemoryNodeType::Fact, Some("p".into()), "rust".into(), MemorySource::ToolObservation).with_critical();
        assert_eq!(calculate_project_decay(&node, 30), 1.0);
    }

    #[test]
    fn project_decay_fresh_is_high() {
        let node = MemoryNode::new("test".into(), MemoryNodeType::Fact, Some("p".into()), "rust".into(), MemorySource::ToolObservation);
        let decay = calculate_project_decay(&node, 30);
        assert!(decay > 0.9);
    }

    #[test]
    fn overall_decay_fresh_is_reasonable() {
        let node = MemoryNode::new("test".into(), MemoryNodeType::BestPractice, None, "rust".into(), MemorySource::LlmExtraction);
        let decay = calculate_overall_decay(&node, &[0.1, 0.2, 0.3, 0.4, 0.5]);
        assert!(decay > 0.0);
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
    CouncilConclusion,
    Feedback,
}

impl MemorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserExplicit => "user_explicit",
            Self::ToolObservation => "tool_observation",
            Self::LlmExtraction => "llm_extraction",
            Self::CouncilConclusion => "council_conclusion",
            Self::Feedback => "feedback",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user_explicit" => Some(Self::UserExplicit),
            "tool_observation" => Some(Self::ToolObservation),
            "llm_extraction" => Some(Self::LlmExtraction),
            "council_conclusion" => Some(Self::CouncilConclusion),
            "feedback" => Some(Self::Feedback),
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
    pub fn extract_from_tool_call(tool_name: &str, tool_args: &str, project_id: &str, language: &str) -> Option<Self> {
        match tool_name {
            "file_write" | "file_patch" => {
                if tool_args.len() < 20 { return None; }
                let content = if tool_args.len() > 400 {
                    format!("{}...", &tool_args[..tool_args.char_indices().take(400).last().map(|(i,_)| i).unwrap_or(0)])
                } else {
                    tool_args.to_string()
                };
                if contains_architectural_keywords(&content) {
                    Some(Self::new(content, MemoryNodeType::Architectural, Some(project_id.into()), language.into(), MemorySource::ToolObservation))
                } else if contains_business_keywords(&content) {
                    Some(Self::new(content, MemoryNodeType::Business, Some(project_id.into()), language.into(), MemorySource::ToolObservation))
                } else {
                    Some(Self::new(content, MemoryNodeType::Fact, Some(project_id.into()), language.into(), MemorySource::ToolObservation))
                }
            }
            "shell_exec" => {
                if tool_args.contains("error") || tool_args.contains("Error") || tool_args.contains("failed") {
                    let content = if tool_args.len() > 400 {
                        format!("{}...", &tool_args[..tool_args.char_indices().take(400).last().map(|(i,_)| i).unwrap_or(0)])
                    } else {
                        tool_args.to_string()
                    };
                    Some(Self::new(content, MemoryNodeType::AntiPattern, Some(project_id.into()), language.into(), MemorySource::ToolObservation))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn extract_from_conversation(assistant_content: &str, project_id: &str, language: &str) -> Option<Self> {
        if contains_architectural_keywords(assistant_content) {
            let content: String = assistant_content.chars().take(300).collect();
            Some(Self::new(content, MemoryNodeType::Architectural, Some(project_id.into()), language.into(), MemorySource::LlmExtraction))
        } else if contains_user_preference(assistant_content) {
            let content: String = assistant_content.chars().take(200).collect();
            Some(Self::new(content, MemoryNodeType::Style, Some(project_id.into()), language.into(), MemorySource::LlmExtraction))
        } else {
            None
        }
    }
}

fn contains_architectural_keywords(text: &str) -> bool {
    let keywords = ["module", "struct ", "trait ", "interface", "abstract ", "impl ", "enum ", "protocol", "architecture", "design pattern"];
    keywords.iter().any(|k| text.to_lowercase().contains(k))
}

fn contains_business_keywords(text: &str) -> bool {
    let keywords = ["api", "endpoint", "model", "schema", "service", "handler", "controller", "repository", "entity"];
    keywords.iter().any(|k| text.to_lowercase().contains(k))
}

fn contains_user_preference(text: &str) -> bool {
    let keywords = ["以后都", "不要用", "always use", "never use", "prefer", "avoid", "习惯", "偏好", "应该用", "选型"];
    keywords.iter().any(|k| text.to_lowercase().contains(k))
}

// ── MemoryManager (门面) ──

use std::path::PathBuf;

use crate::config::MemoryConfig;

#[derive(Clone)]
pub struct MemoryManager {
    project_store: Option<store::MemoryStore>,
    overall_store: store::MemoryStore,
    write_buffer: WriteBuffer,
}

impl MemoryManager {
    pub fn init(runtime_ox_home: &PathBuf, project_id: &str, config: &MemoryConfig) -> anyhow::Result<Self> {
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

        Ok(Self {
            project_store,
            overall_store,
            write_buffer: WriteBuffer::new(),
        })
    }

    pub fn store(&mut self, mut node: MemoryNode) {
        node.content = crate::safety::sanitizer::DataSanitizer::sanitize_all(&node.content);
        let is_immediate = node.node_type.is_immediate_write();
        let is_long_term = node.node_type.is_long_term();

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
            let should_flush = self.write_buffer.buffer(node);
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
    }

    pub fn update_from_turn(&mut self, messages: &[crate::message::Message], project_id: &str, language: &str) {
        for msg in messages {
            if let crate::message::Message::Assistant { content, tool_calls } = msg {
                for tc in tool_calls {
                    if let Some(node) = MemoryNode::extract_from_tool_call(&tc.name, &tc.arguments, project_id, language) {
                        self.store(node);
                    }
                }
                if let Some(node) = MemoryNode::extract_from_conversation(content, project_id, language) {
                    self.store(node);
                }
            }
        }
    }

    pub fn retrieve(&self, query: &str, project_id: &Option<&str>, limit: usize) -> Vec<MemoryNode> {
        let mut results = Vec::new();

        if let Some(pid) = project_id {
            if let Some(ref store) = self.project_store {
                if let Ok(mems) = store.query_by_project(pid, &[MemoryNodeType::Architectural, MemoryNodeType::Business, MemoryNodeType::Style], limit) {
                    results.extend(mems);
                }
            }
        }

        if let Ok(mems) = self.overall_store.query_overall(&[MemoryNodeType::BestPractice, MemoryNodeType::Pattern, MemoryNodeType::MetaSkill], limit) {
            results.extend(mems);
        }

        if !query.is_empty() {
            if let Some(pid) = project_id {
                if let Ok(mems) = self.project_store.as_ref().unwrap_or_else(|| &self.overall_store).search(query, Some(pid), limit) {
                    for m in mems {
                        if !results.iter().any(|r| r.id == m.id) {
                            results.push(m);
                        }
                    }
                }
            }
        }

        results.retain(|n| {
            let decay = if n.project_id.is_some() {
                calculate_project_decay(n, 30)
            } else {
                calculate_overall_decay(n, &[0.1, 0.2, 0.3, 0.4, 0.5])
            };
            decay > 0.3 || n.is_project_critical
        });

        results.sort_by(|a, b| {
            composite_score(b, 30).partial_cmp(&composite_score(a, 30)).unwrap_or(std::cmp::Ordering::Equal)
        });

        results.truncate(limit);
        results
    }

    pub fn flush(&mut self) {
        let batch = self.write_buffer.drain();
        if batch.is_empty() { return; }
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
        }
    }

    pub fn format_memory_context(&self, nodes: &[MemoryNode], use_xml: bool) -> String {
        if nodes.is_empty() { return String::new(); }

        if use_xml {
            // XML format for compatible APIs
            let mut out = String::from("<relevant_memories>\n");
            for n in nodes.iter().take(8) {
                let content: String = n.content.chars().take(120).collect();
                out.push_str(&format!("  <memory depth=\"{}\" type=\"{}\">{}</memory>\n", n.depth, n.node_type, content));
            }
            out.push_str("</relevant_memories>");
            out
        } else {
            // Plain text format for MiniMax and similar APIs
            let mut out = String::from("Relevant context:\n");
            for n in nodes.iter().take(5) {
                let content: String = n.content.chars().take(150).collect();
                out.push_str(&format!("- {}\n", content));
            }
            out
        }
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
        let project_count = self.project_store.as_ref().and_then(|s| s.count_by_project(project_id).ok()).unwrap_or(0);
        let overall_count = self.overall_store.count_overall().unwrap_or(0);
        (project_count, overall_count)
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
}
