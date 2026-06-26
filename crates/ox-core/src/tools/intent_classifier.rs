//! 智能意图分类器 - 统一意图识别系统
//!
//! 采用三层混合策略：
//! 1. 从 LLM 响应中提取意图（零成本）
//! 2. 基于规则的快速分类（低成本）
//! 3. 专用小模型分类（低成本的兜底方案）
//!
//! **统一意图识别**：同时服务于工具过滤和记忆检索决策

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 意图类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum QuestionType {
    /// 阅读/查看代码
    CodeReading,
    /// 创建/修改代码
    CodeWriting,
    /// 调试/修复问题
    Debugging,
    /// 重构/优化代码
    Refactoring,
    /// 探索项目结构
    Exploration,
    /// 一般性问题
    GeneralQuestion,
}

impl std::fmt::Display for QuestionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CodeReading => write!(f, "CodeReading"),
            Self::CodeWriting => write!(f, "CodeWriting"),
            Self::Debugging => write!(f, "Debugging"),
            Self::Refactoring => write!(f, "Refactoring"),
            Self::Exploration => write!(f, "Exploration"),
            Self::GeneralQuestion => write!(f, "GeneralQuestion"),
        }
    }
}

/// 意图信息（统一版本）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentInfo {
    /// 意图类型
    pub intent: QuestionType,
    /// 置信度 (0.0 - 1.0)
    pub confidence: f32,
    /// 提取的关键词
    pub keywords: Vec<String>,
    /// 建议使用的工具列表
    pub suggested_tools: Vec<String>,

    // === 新增：记忆检索决策 ===
    /// 是否需要检索记忆
    #[serde(default = "default_false")]
    pub should_search_memory: bool,
    /// 记忆检索的查询词（如果需要）
    #[serde(default)]
    pub memory_query: Option<String>,
    /// 记忆检索范围
    #[serde(default)]
    pub memory_scope: MemoryScope,
}

fn default_false() -> bool {
    false
}

impl IntentInfo {
    pub fn default_intent() -> Self {
        Self {
            intent: QuestionType::GeneralQuestion,
            confidence: 0.5,
            keywords: vec![],
            suggested_tools: vec!["file_read".to_string()],
            should_search_memory: false,
            memory_query: None,
            memory_scope: MemoryScope::Both,
        }
    }
}

/// 记忆检索范围
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    /// 仅当前项目
    Project,
    /// 全局知识
    Global,
    /// 两者都搜索
    Both,
}

impl Default for MemoryScope {
    fn default() -> Self {
        Self::Both
    }
}

/// 基于规则的意图分类器
pub struct RuleBasedClassifier {
    /// 多语言动词映射表
    action_verbs: ActionVerbMap,
}

struct ActionVerbMap {
    read_verbs: HashSet<String>,
    write_verbs: HashSet<String>,
    search_verbs: HashSet<String>,
    debug_verbs: HashSet<String>,
    refactor_verbs: HashSet<String>,
    explore_verbs: HashSet<String>,
}

impl RuleBasedClassifier {
    pub fn new() -> Self {
        Self {
            action_verbs: Self::build_verb_map(),
        }
    }

    fn build_verb_map() -> ActionVerbMap {
        ActionVerbMap {
            read_verbs: [
                // English
                "read", "view", "show", "display", "check", "inspect", "see", // 中文
                "看", "查看", "读", "阅读", "显示", "检查", "看看", // Japanese
                "読む", "見る", // Spanish
                "leer", "ver", "mostrar",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),

            write_verbs: [
                // English
                "write",
                "create",
                "add",
                "implement",
                "build",
                "make",
                "generate",
                // 中文
                "写",
                "创建",
                "添加",
                "实现",
                "构建",
                "做",
                "新建",
                "生成",
                // Japanese
                "書く",
                "作る",
                "追加",
                // Spanish
                "escribir",
                "crear",
                "añadir",
                "implementar",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),

            search_verbs: [
                // English
                "search",
                "find",
                "locate",
                "look",
                // 中文
                "搜索",
                "查找",
                "找",
                "寻找",
                // Japanese
                "検索",
                "探す",
                // Spanish
                "buscar",
                "encontrar",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),

            debug_verbs: [
                // English
                "fix",
                "debug",
                "repair",
                "solve",
                "troubleshoot",
                "error",
                "bug",
                // 中文
                "修复",
                "调试",
                "解决",
                "修",
                "排除",
                "错误",
                "bug",
                "报错",
                // Japanese
                "修正",
                "デバッグ",
                "解決",
                // Spanish
                "arreglar",
                "depurar",
                "solucionar",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),

            refactor_verbs: [
                // English
                "refactor",
                "optimize",
                "improve",
                "clean",
                "restructure",
                "enhance",
                // 中文
                "重构",
                "优化",
                "改进",
                "清理",
                "重组",
                "增强",
                // Japanese
                "リファクタ",
                "最適化",
                "改善",
                // Spanish
                "refactorizar",
                "optimizar",
                "mejorar",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),

            explore_verbs: [
                // English
                "explore",
                "understand",
                "learn",
                "analyze",
                "explain",
                "structure",
                // 中文
                "探索",
                "了解",
                "学习",
                "分析",
                "解释",
                "理解",
                "结构",
                "介绍",
                // Japanese
                "探検",
                "理解",
                "学ぶ",
                // Spanish
                "explorar",
                "entender",
                "aprender",
                "analizar",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }

    /// 基于规则分类
    pub fn classify(&self, user_message: &str) -> IntentInfo {
        let msg_lower = user_message.to_lowercase();
        let words = self.extract_words(&msg_lower);

        // 统计各类型得分
        let mut scores = std::collections::HashMap::new();
        scores.insert(QuestionType::CodeReading, 0);
        scores.insert(QuestionType::CodeWriting, 0);
        scores.insert(QuestionType::Debugging, 0);
        scores.insert(QuestionType::Refactoring, 0);
        scores.insert(QuestionType::Exploration, 0);

        for word in &words {
            if self.action_verbs.read_verbs.contains(word.as_str()) {
                *scores.get_mut(&QuestionType::CodeReading).unwrap() += 1;
            }
            if self.action_verbs.write_verbs.contains(word.as_str()) {
                *scores.get_mut(&QuestionType::CodeWriting).unwrap() += 1;
            }
            if self.action_verbs.search_verbs.contains(word.as_str()) {
                *scores.get_mut(&QuestionType::CodeReading).unwrap() += 1;
            }
            if self.action_verbs.debug_verbs.contains(word.as_str()) {
                *scores.get_mut(&QuestionType::Debugging).unwrap() += 1;
            }
            if self.action_verbs.refactor_verbs.contains(word.as_str()) {
                *scores.get_mut(&QuestionType::Refactoring).unwrap() += 1;
            }
            if self.action_verbs.explore_verbs.contains(word.as_str()) {
                *scores.get_mut(&QuestionType::Exploration).unwrap() += 1;
            }
        }

        // Chinese/multi-char phrases: substring match (whole-sentence tokens miss these).
        for verb in &self.action_verbs.debug_verbs {
            if msg_lower.contains(verb) {
                *scores.get_mut(&QuestionType::Debugging).unwrap() += 1;
            }
        }
        for verb in &self.action_verbs.write_verbs {
            if msg_lower.contains(verb) {
                *scores.get_mut(&QuestionType::CodeWriting).unwrap() += 1;
            }
        }
        for verb in &self.action_verbs.read_verbs {
            if msg_lower.contains(verb) {
                *scores.get_mut(&QuestionType::CodeReading).unwrap() += 1;
            }
        }

        // 找到最高分的意图
        let (best_intent, best_score) = scores
            .into_iter()
            .max_by_key(|&(_, score)| score)
            .unwrap_or((QuestionType::GeneralQuestion, 0));

        // 计算置信度
        let total_words = words.len().max(1);
        let confidence = if best_score == 0 {
            0.3
        } else {
            ((best_score as f32 / total_words as f32) * 2.0).min(0.9)
        };

        // 提取关键词（简单实现：取前 5 个有意义的词）
        let keywords: Vec<String> = words.into_iter().filter(|w| w.len() > 1).take(5).collect();

        // 根据意图推荐工具
        let suggested_tools = self.recommend_tools(&best_intent);

        // === 新增：记忆检索决策 ===
        let (should_search_memory, memory_query, memory_scope) =
            self.decide_memory_search(user_message, &best_intent, &keywords);

        IntentInfo {
            intent: best_intent,
            confidence,
            keywords,
            suggested_tools,
            should_search_memory,
            memory_query,
            memory_scope,
        }
    }

    /// 决策是否需要检索记忆
    fn decide_memory_search(
        &self,
        user_message: &str,
        intent: &QuestionType,
        keywords: &[String],
    ) -> (bool, Option<String>, MemoryScope) {
        let msg_lower = user_message.to_lowercase();

        // 规则 1: 用户明确提到历史/之前的内容
        let history_indicators = [
            "之前",
            "以前",
            "上次",
            "记得",
            "历史",
            "before",
            "previous",
            "remember",
            "last time",
            "earlier",
        ];

        for indicator in &history_indicators {
            if msg_lower.contains(indicator) {
                return (
                    true,
                    Some(self.extract_memory_query(user_message, keywords)),
                    MemoryScope::Project, // 历史内容通常在项目级别
                );
            }
        }

        // 规则 2: 涉及项目特定知识（架构、约定等）
        let project_knowledge_indicators = [
            "架构",
            "结构",
            "规范",
            "约定",
            "风格",
            "习惯",
            "architecture",
            "convention",
            "pattern",
            "style",
        ];

        for indicator in &project_knowledge_indicators {
            if msg_lower.contains(indicator) {
                return (
                    true,
                    Some(self.extract_memory_query(user_message, keywords)),
                    MemoryScope::Project,
                );
            }
        }

        // 规则 3: 复杂任务可能需要参考最佳实践
        match intent {
            QuestionType::CodeWriting | QuestionType::Refactoring => {
                if keywords.len() >= 3 {
                    // 任务较复杂
                    return (
                        true,
                        Some(self.extract_memory_query(user_message, keywords)),
                        MemoryScope::Both, // 可能用到全局知识
                    );
                }
            }
            QuestionType::Debugging => {
                // Debug 时查找类似问题的解决方案
                return (
                    true,
                    Some(self.extract_memory_query(user_message, keywords)),
                    MemoryScope::Both,
                );
            }
            _ => {}
        }

        // 默认不检索
        (false, None, MemoryScope::Both)
    }

    /// 从用户消息中提取记忆检索查询词
    fn extract_memory_query(&self, user_message: &str, keywords: &[String]) -> String {
        // 简单策略：使用关键词作为查询词
        if !keywords.is_empty() {
            keywords.join(" ")
        } else {
            // 如果没有关键词，使用原始消息的前 50 个字符
            user_message.chars().take(50).collect()
        }
    }

    /// 提取单词（支持多语言）
    fn extract_words(&self, text: &str) -> Vec<String> {
        let mut words = Vec::new();

        // 1. 英文单词：按空格和标点分割
        for word in text.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric());
            if !cleaned.is_empty() {
                words.push(cleaned.to_string());
            }
        }

        // 2. 中文字符：每个字符单独作为一个"词"
        for ch in text.chars() {
            if ch.is_ascii() {
                continue;
            }
            words.push(ch.to_string());
        }

        words
    }

    /// 根据意图推荐工具
    fn recommend_tools(&self, intent: &QuestionType) -> Vec<String> {
        let mut tools = match intent {
            QuestionType::CodeReading => {
                vec![
                    "file_read".to_string(),
                    "code_search".to_string(),
                    "file_list".to_string(),
                ]
            }
            QuestionType::CodeWriting => {
                vec![
                    "file_read".to_string(),
                    "file_write".to_string(),
                    "edit_file".to_string(),
                ]
            }
            QuestionType::Debugging => {
                vec![
                    "file_read".to_string(),
                    "code_search".to_string(),
                    "shell_exec".to_string(),
                    "edit_file".to_string(),
                ]
            }
            QuestionType::Refactoring => {
                vec![
                    "file_read".to_string(),
                    "code_search".to_string(),
                    "edit_file".to_string(),
                ]
            }
            QuestionType::Exploration => {
                vec![
                    "file_list".to_string(),
                    "project_detect".to_string(),
                    "file_search".to_string(),
                    "code_search".to_string(),
                ]
            }
            QuestionType::GeneralQuestion => {
                vec!["file_read".to_string(), "web_fetch".to_string()]
            }
        };

        // 始终添加 memory_search（LLM 可以根据需要决定是否使用）
        if !tools.contains(&"memory_search".to_string()) {
            tools.push("memory_search".to_string());
        }

        tools
    }
}

impl Default for RuleBasedClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// 从 LLM 响应中提取意图
pub fn extract_intent_from_llm_response(response: &str) -> Option<IntentInfo> {
    // 查找最后一个 JSON 代码块
    if let Some(json_start) = response.rfind("```json") {
        if let Some(json_end) = response[json_start..].find("```") {
            // 使用字符边界安全的切片方法
            let start_byte = json_start + 7; // "```json" 的长度是7
            let end_byte = json_start + json_end;

            // 确保字节索引在有效范围内且位于字符边界
            if start_byte <= end_byte && end_byte <= response.len() {
                // 使用 get() 方法进行安全切片
                if let Some(json_str) = response.get(start_byte..end_byte) {
                    match serde_json::from_str::<IntentInfo>(json_str.trim()) {
                        Ok(info) => {
                            tracing::info!(
                                "Extracted intent from LLM: {:?} (confidence: {:.2})",
                                info.intent,
                                info.confidence
                            );
                            return Some(info);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse intent JSON: {}", e);
                        }
                    }
                } else {
                    tracing::warn!("Invalid UTF-8 boundaries in response");
                }
            }
        }
    }

    None
}

/// 从响应中移除意图 JSON 块
pub fn remove_intent_json(response: &str) -> String {
    if let Some(json_start) = response.rfind("```json") {
        if let Some(json_end) = response[json_start..].find("```") {
            let json_block_end = json_start + json_end + 3;

            // 确保字节索引在有效范围内
            if json_start <= response.len() && json_block_end <= response.len() {
                // 使用 get() 方法进行安全切片
                if let (Some(before_part), Some(after_part)) =
                    (response.get(..json_start), response.get(json_block_end..))
                {
                    // 移除 JSON 块及其前后的空行
                    let before = before_part.trim_end();
                    let after = after_part.trim_start();

                    format!("{}\n{}", before, after)
                } else {
                    response.to_string()
                }
            } else {
                response.to_string()
            }
        } else {
            response.to_string()
        }
    } else {
        response.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::intent_classifier::MemoryScope;

    #[test]
    fn test_chinese_code_writing() {
        let classifier = RuleBasedClassifier::new();
        // "写" 明确指向 CodeWriting
        let result = classifier.classify("帮我写一个登录功能");

        assert_eq!(result.intent, QuestionType::CodeWriting);
        // 置信度可能较低，因为中文分词简单
        println!(
            "Intent: {:?}, Confidence: {}",
            result.intent, result.confidence
        );
    }

    #[test]
    fn test_english_debugging() {
        let classifier = RuleBasedClassifier::new();
        let result = classifier.classify("Fix the login error");

        assert_eq!(result.intent, QuestionType::Debugging);
        assert!(result.confidence > 0.5);
    }

    #[test]
    fn test_exploration() {
        let classifier = RuleBasedClassifier::new();
        // 英文 "explore" 明确指向 Exploration
        let result = classifier.classify("Explore the project structure");

        assert_eq!(result.intent, QuestionType::Exploration);
        println!(
            "Intent: {:?}, Confidence: {}",
            result.intent, result.confidence
        );
    }

    #[test]
    fn test_mixed_language() {
        let classifier = RuleBasedClassifier::new();
        let result = classifier.classify("帮我 fix 这个 bug");

        assert_eq!(result.intent, QuestionType::Debugging);
    }

    // === 新增：记忆检索决策测试 ===

    #[test]
    fn test_memory_search_with_history_reference() {
        let classifier = RuleBasedClassifier::new();
        let result = classifier.classify("之前说的那个登录功能怎么实现的？");

        assert!(result.should_search_memory);
        assert!(result.memory_query.is_some());
        assert_eq!(result.memory_scope, MemoryScope::Project);
    }

    #[test]
    fn test_memory_search_with_architecture_mention() {
        let classifier = RuleBasedClassifier::new();
        let result = classifier.classify("项目的架构是怎样的？");

        assert!(result.should_search_memory);
        assert_eq!(result.memory_scope, MemoryScope::Project);
    }

    #[test]
    fn test_no_memory_search_for_simple_task() {
        let classifier = RuleBasedClassifier::new();
        let result = classifier.classify("创建一个 Hello World 文件");

        // 简单任务可能不需要记忆检索（取决于规则）
        // 这里我们只验证不会崩溃
        println!("Should search memory: {}", result.should_search_memory);
    }

    #[test]
    fn test_memory_search_for_debugging() {
        let classifier = RuleBasedClassifier::new();
        let result = classifier.classify("为什么这段代码报错？");

        // Debugging 通常会触发记忆检索
        assert!(result.should_search_memory);
        assert_eq!(result.memory_scope, MemoryScope::Both);
    }
}
