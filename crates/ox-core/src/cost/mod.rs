use std::path::{Path, PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::message::TokenUsage;

/// A single API call cost record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    pub timestamp: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
}

/// Persistent cost tracking across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostTracker {
    /// Date string "YYYY-MM-DD" for daily tracking.
    pub current_date: String,
    /// Total tokens used today.
    pub daily_prompt_tokens: u64,
    pub daily_completion_tokens: u64,
    /// Total cost today (USD).
    pub daily_cost_usd: f64,
    /// Total cost this month (USD).
    pub monthly_cost_usd: f64,
    /// Month string "YYYY-MM" for monthly tracking.
    pub current_month: String,
    /// Total session count.
    pub total_api_calls: u64,
    /// Persistence path.
    #[serde(skip)]
    file_path: PathBuf,
}

impl CostTracker {
    /// Load or create a cost tracker from the given directory.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        let file_path = dir.join("cost_tracking.json");
        if file_path.exists() {
            let content = std::fs::read_to_string(&file_path)?;
            let mut tracker: CostTracker = serde_json::from_str(&content)?;
            tracker.file_path = file_path;
            tracker.roll_date();
            Ok(tracker)
        } else {
            let today = Local::now().format("%Y-%m-%d").to_string();
            let month = Local::now().format("%Y-%m").to_string();
            Ok(Self {
                current_date: today,
                daily_prompt_tokens: 0,
                daily_completion_tokens: 0,
                daily_cost_usd: 0.0,
                monthly_cost_usd: 0.0,
                current_month: month,
                total_api_calls: 0,
                file_path,
            })
        }
    }

    /// Roll over daily/monthly counters if the date has changed.
    fn roll_date(&mut self) {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let month = Local::now().format("%Y-%m").to_string();

        if self.current_date != today {
            self.daily_prompt_tokens = 0;
            self.daily_completion_tokens = 0;
            self.daily_cost_usd = 0.0;
            self.current_date = today;
        }

        if self.current_month != month {
            self.monthly_cost_usd = 0.0;
            self.current_month = month;
        }
    }

    /// Record a completed API call.
    pub fn record(&mut self, model: &str, usage: &TokenUsage) {
        self.roll_date();

        let cost = estimate_cost(model, usage);
        self.daily_prompt_tokens += usage.prompt_tokens as u64;
        self.daily_completion_tokens += usage.completion_tokens as u64;
        self.daily_cost_usd += cost;
        self.monthly_cost_usd += cost;
        self.total_api_calls += 1;

        if let Err(e) = self.save() {
            tracing::error!("Failed to save cost tracking: {e}");
        }
    }

    /// Check if daily budget is exceeded.
    pub fn daily_exceeded(&self, limit: f64) -> bool {
        self.daily_cost_usd >= limit
    }

    /// Check if monthly budget is exceeded.
    pub fn monthly_exceeded(&self, limit: f64) -> bool {
        self.monthly_cost_usd >= limit
    }

    /// Check if approaching budget threshold (e.g. 80%).
    pub fn approaching_limit(&self, monthly_limit: f64, threshold: f64) -> bool {
        self.monthly_cost_usd >= monthly_limit * threshold
    }

    /// Format a human-readable cost summary.
    pub fn summary(&self) -> String {
        format!(
            "Cost Summary:\n\
             Today: {:.4} USD ({} prompt + {} completion tokens, {} calls)\n\
             This month: {:.4} USD",
            self.daily_cost_usd,
            self.daily_prompt_tokens,
            self.daily_completion_tokens,
            self.total_api_calls,
            self.monthly_cost_usd,
        )
    }

    /// Short one-line summary for the status bar.
    pub fn summary_short(&self) -> String {
        let daily_tokens = self.daily_prompt_tokens + self.daily_completion_tokens;
        if self.monthly_cost_usd > 0.0 {
            format!(
                "${:.2}/mo · {}tk today",
                self.monthly_cost_usd,
                daily_tokens / 1000
            )
        } else {
            format!("{}tk today", daily_tokens / 1000)
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&self.file_path, json)?;
        Ok(())
    }
}

/// Estimate USD cost for an API call based on model and token usage.
/// Uses approximate per-token pricing (as of 2024-2025).
pub fn estimate_cost(model: &str, usage: &TokenUsage) -> f64 {
    let m = model.to_lowercase();
    let (prompt_rate, completion_rate) = match () {
        _ if m.starts_with("gpt-4o") => (2.50, 10.00), // per 1M tokens
        _ if m.starts_with("gpt-4-turbo") => (10.00, 30.00),
        _ if m.starts_with("gpt-4") => (30.00, 60.00),
        _ if m.starts_with("gpt-3.5") => (0.50, 1.50),
        _ if m.starts_with("o1") => (15.00, 60.00),
        _ if m.starts_with("o3") || m.starts_with("o4") => (10.00, 40.00),
        _ if m.contains("claude-3-5-sonnet") || m.contains("claude-sonnet") => (3.00, 15.00),
        _ if m.contains("claude-3-opus") || m.contains("claude-opus") => (15.00, 75.00),
        _ if m.contains("claude-3-haiku") || m.contains("claude-haiku") => (0.25, 1.25),
        _ if m.starts_with("claude") => (3.00, 15.00),
        _ if m.starts_with("deepseek") => (0.14, 0.28),
        _ => (5.00, 15.00), // conservative fallback
    };

    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * prompt_rate;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * completion_rate;
    prompt_cost + completion_cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_cost_gpt4o() {
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        let cost = estimate_cost("gpt-4o", &usage);
        // 1000/1M * 2.5 + 500/1M * 10.0 = 0.0025 + 0.005 = 0.0075
        assert!((cost - 0.0075).abs() < 0.0001);
    }

    #[test]
    fn cost_tracker_summary_format() {
        let tracker = CostTracker {
            current_date: "2025-01-01".to_string(),
            daily_prompt_tokens: 5000,
            daily_completion_tokens: 2000,
            daily_cost_usd: 0.05,
            monthly_cost_usd: 1.23,
            current_month: "2025-01".to_string(),
            total_api_calls: 10,
            file_path: PathBuf::new(),
        };
        let summary = tracker.summary();
        assert!(summary.contains("0.0500 USD"));
        assert!(summary.contains("5000 prompt"));
        assert!(summary.contains("10 calls"));
    }
}
