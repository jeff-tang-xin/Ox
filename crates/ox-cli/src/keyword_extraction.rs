use regex::Regex;
use ox_core::memory::semantic::KeywordExtraction;

/// 从 LLM 响应中提取关键词 JSON 块
pub fn extract_keywords_from_response(response: &str) -> Option<KeywordExtraction> {
    // 查找 ```json ... ``` 代码块
    let json_pattern = Regex::new(r"```json\s*([\s\S]*?)\s*```").ok()?;
    
    if let Some(caps) = json_pattern.captures(response) {
        let json_str = caps.get(1)?.as_str();
        
        // 尝试解析 JSON
        match serde_json::from_str::<KeywordExtraction>(json_str) {
            Ok(keywords) => {
                tracing::debug!(
                    "[KEYWORD EXTRACTION] Extracted {} keywords, {} topics",
                    keywords.keywords.len(),
                    keywords.topics.len()
                );
                return Some(keywords);
            }
            Err(e) => {
                tracing::warn!("[KEYWORD EXTRACTION] Failed to parse JSON: {}", e);
                return None;
            }
        }
    }
    
    None
}

/// 从响应中移除关键词 JSON 块（返回干净的文本）
pub fn remove_keyword_json_block(response: &str) -> String {
    let json_pattern = Regex::new(r"\n?```json\s*[\s\S]*?\s*```\n?").unwrap();
    json_pattern.replace_all(response, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords() {
        let response = r#"This is the main response text.

```json
{
  "keywords": ["authentication", "JWT", "login"],
  "topics": ["security", "api"],
  "related_files": ["src/auth.rs"]
}
```

Some trailing text."#;

        let extracted = extract_keywords_from_response(response).unwrap();
        assert_eq!(extracted.keywords.len(), 3);
        assert_eq!(extracted.topics.len(), 2);
        assert_eq!(extracted.related_files.len(), 1);
    }

    #[test]
    fn test_remove_json_block() {
        let response = r#"Main content here.

```json
{
  "keywords": ["test"],
  "topics": [],
  "related_files": []
}
```"#;

        let cleaned = remove_keyword_json_block(response);
        assert!(cleaned.contains("Main content here"));
        assert!(!cleaned.contains("```json"));
        assert!(!cleaned.contains("keywords"));
    }
}
