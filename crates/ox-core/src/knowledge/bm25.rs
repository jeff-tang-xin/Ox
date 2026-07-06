//! Lightweight BM25 inverted index for hybrid retrieval (keyword + vector).
//!
//! Complements dense embedding search with exact token / identifier matching,
//! especially effective for code symbol names and file paths.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Persisted BM25 corpus.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Bm25Index {
    /// entity_id → token → term frequency
    docs: HashMap<String, HashMap<String, u32>>,
    /// entity_id → document length (token count)
    doc_lengths: HashMap<String, usize>,
    /// token → document frequency
    df: HashMap<String, usize>,
    avg_dl: f32,
}

impl Bm25Index {
    pub fn index_document(&mut self, doc_id: &str, text: &str) {
        self.remove_document(doc_id);
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return;
        }
        let mut tf: HashMap<String, u32> = HashMap::new();
        for t in &tokens {
            *tf.entry(t.clone()).or_insert(0) += 1;
        }
        let dl = tokens.len();
        for term in tf.keys() {
            *self.df.entry(term.clone()).or_insert(0) += 1;
        }
        self.doc_lengths.insert(doc_id.to_string(), dl);
        self.docs.insert(doc_id.to_string(), tf);
        self.recompute_avg_dl();
    }

    pub fn remove_document(&mut self, doc_id: &str) {
        if let Some(tf) = self.docs.remove(doc_id) {
            for term in tf.keys() {
                if let Some(df) = self.df.get_mut(term) {
                    *df = df.saturating_sub(1);
                    if *df == 0 {
                        self.df.remove(term);
                    }
                }
            }
        }
        self.doc_lengths.remove(doc_id);
        self.recompute_avg_dl();
    }

    /// Search corpus; returns (doc_id, normalized score 0..1).
    pub fn search(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        let q_tokens = tokenize(query);
        if q_tokens.is_empty() || self.docs.is_empty() {
            return Vec::new();
        }

        let n = self.docs.len() as f32;
        let mut scores: HashMap<String, f32> = HashMap::new();

        for qt in &q_tokens {
            let df = *self.df.get(qt).unwrap_or(&0) as f32;
            if df == 0.0 {
                continue;
            }
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

            for (doc_id, tf_map) in &self.docs {
                let tf = *tf_map.get(qt).unwrap_or(&0) as f32;
                if tf == 0.0 {
                    continue;
                }
                let dl = *self.doc_lengths.get(doc_id).unwrap_or(&1) as f32;
                let denom = tf + K1 * (1.0 - B + B * dl / self.avg_dl.max(1.0));
                let score = idf * (tf * (K1 + 1.0)) / denom;
                *scores.entry(doc_id.clone()).or_insert(0.0) += score;
            }
        }

        let mut ranked: Vec<(String, f32)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        if let Some(&max_score) = ranked.first().map(|(_, s)| s)
            && max_score > 0.0 {
                for (_, s) in &mut ranked {
                    *s /= max_score;
                }
            }

        ranked.truncate(top_k);
        ranked
    }

    pub fn load(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string(self)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    fn recompute_avg_dl(&mut self) {
        if self.doc_lengths.is_empty() {
            self.avg_dl = 0.0;
        } else {
            let sum: usize = self.doc_lengths.values().sum();
            self.avg_dl = sum as f32 / self.doc_lengths.len() as f32;
        }
    }
}

/// Tokenize for code + natural language (camelCase split, lowercase).
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '/') {
        if word.is_empty() {
            continue;
        }
        for part in split_camel_case(word) {
            let t = part.to_lowercase();
            if t.len() >= 2 {
                tokens.push(t);
            }
        }
    }
    tokens
}

fn split_camel_case(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if ch.is_uppercase() && !current.is_empty() {
            parts.push(current.clone());
            current.clear();
        }
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        parts.push(s.to_string());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_camel_case() {
        let t = tokenize("validateToken in src/auth.rs");
        assert!(t.iter().any(|x| x == "validate"));
        assert!(t.iter().any(|x| x == "token"));
        assert!(t.iter().any(|x| x.contains("auth")));
    }

    #[test]
    fn test_bm25_search_ranks_exact_match() {
        let mut idx = Bm25Index::default();
        idx.index_document("a", "fn validate_token checks jwt signature");
        idx.index_document("b", "unrelated database migration script");
        let hits = idx.search("validate_token", 2);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "a");
    }
}
