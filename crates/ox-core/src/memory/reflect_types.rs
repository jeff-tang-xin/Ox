use serde::{Deserialize, Serialize};

/// Step quality after reflection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepQuality {
    Correct,
    Wrong,
    Noise,
}

impl StepQuality {
    pub fn as_str(&self) -> &str {
        match self {
            StepQuality::Correct => "correct",
            StepQuality::Wrong => "wrong",
            StepQuality::Noise => "noise",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "correct" => Some(StepQuality::Correct),
            "wrong" => Some(StepQuality::Wrong),
            "noise" => Some(StepQuality::Noise),
            _ => None,
        }
    }
}

/// Keyword item with category
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionKeyword {
    pub word: String,
    pub category: String, // file | tool | decision | result | error
}

impl ReflectionKeyword {
    pub fn new(word: impl Into<String>, category: impl Into<String>) -> Self {
        Self {
            word: word.into(),
            category: category.into(),
        }
    }
}

/// Single step reflection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReflection {
    pub react_id: String,
    pub quality: StepQuality,
    pub keywords: Vec<ReflectionKeyword>,
    pub note: Option<String>,
}

/// Full reflection output for a batch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionOutput {
    pub analyzed_steps: Vec<StepReflection>,
    pub insights: Vec<String>,
}

/// Insight: a reusable piece of knowledge extracted from reflection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Insight {
    pub id: String,
    pub session_id: String,
    pub content: String,
    pub related_react_ids: Vec<String>,
    pub created_at: String,
}

impl Insight {
    pub fn new(id: impl Into<String>, session_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            content: content.into(),
            related_react_ids: Vec::new(),
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        }
    }

    pub fn with_related(mut self, ids: Vec<String>) -> Self {
        self.related_react_ids = ids;
        self
    }
}

/// Graph cluster generated from keywords
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCluster {
    pub id: String,
    pub topic: String,
    pub summary: String,
    pub react_ids: Vec<i64>,
    pub insights: Vec<String>,
    pub keywords: Vec<String>,
}

impl GraphCluster {
    pub fn new(id: impl Into<String>, topic: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            topic: topic.into(),
            summary: summary.into(),
            react_ids: Vec::new(),
            insights: Vec::new(),
            keywords: Vec::new(),
        }
    }
}
