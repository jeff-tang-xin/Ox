//! Keyword JSON block extraction/stripping.
//!
//! Historically bound to `KnowledgeEngine::KeywordExtraction`; the type is now
//! self-contained here so this file can survive the KnowledgeEngine removal.
//! The functions are still referenced from `main.rs`; keep them wired so LLM
//! responses that emit ```json keyword blocks are cleaned before display.

use regex::Regex;

/// Simple JSON-parsed keyword extraction result.
#[derive(Debug, Clone, Default)]
pub struct KeywordExtraction {
    pub keywords: Vec<String>,
    pub topics: Vec<String>,
    pub related_files: Vec<String>,
}

fn parse_string_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract a ```json { keywords, topics, related_files } ``` block from an LLM response.
pub fn extract_keywords_from_response(response: &str) -> Option<KeywordExtraction> {
    let json_pattern = Regex::new(r"```json\s*([\s\S]*?)\s*```").ok()?;
    let caps = json_pattern.captures(response)?;
    let json_str = caps.get(1)?.as_str();

    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(v) => {
            let keywords = parse_string_array(&v["keywords"]);
            let topics = parse_string_array(&v["topics"]);
            let related_files = parse_string_array(&v["related_files"]);
            tracing::info!(
                "[KEYWORD EXTRACTION] ✅ Extracted {} keywords, {} topics, {} files",
                keywords.len(),
                topics.len(),
                related_files.len()
            );
            Some(KeywordExtraction {
                keywords,
                topics,
                related_files,
            })
        }
        Err(e) => {
            tracing::warn!(
                "[KEYWORD EXTRACTION] ❌ Failed to parse JSON: {}\nJSON content: {}",
                e,
                json_str.chars().take(200).collect::<String>()
            );
            None
        }
    }
}

/// Strip the ```json keyword block from a response so the user sees clean text.
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
