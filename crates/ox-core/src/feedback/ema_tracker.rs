/// Exponential Moving Average (EMA) trend tracker
///
/// Tracks metrics over time using EMA to identify trends without requiring large datasets.
/// Suitable for personal CLI tools with limited evolution_log data (< 200 entries).
#[derive(Debug, Clone)]
pub struct EmatrendTracker {
    /// Current EMA value
    pub current_value: f64,
    /// Trend direction (-1.0 to 1.0)
    pub trend: f64,
    /// Number of samples processed
    pub sample_count: u32,
    /// EMA smoothing factor (alpha), typically 0.1-0.3
    alpha: f64,
}

impl EmatrendTracker {
    pub fn new(alpha: f64) -> Self {
        Self {
            current_value: 0.5, // Start at neutral
            trend: 0.0,
            sample_count: 0,
            alpha: alpha.clamp(0.01, 0.5), // Clamp to reasonable range
        }
    }

    /// Update EMA with a new observation
    pub fn update(&mut self, new_value: f64) {
        if self.sample_count == 0 {
            // First observation - initialize
            self.current_value = new_value;
            self.trend = 0.0;
        } else {
            let old_value = self.current_value;

            // Update EMA value
            self.current_value = old_value + self.alpha * (new_value - old_value);

            // Update trend (rate of change)
            self.trend = self.current_value - old_value;
        }

        self.sample_count += 1;
    }

    /// Check if trend is significant (above threshold)
    pub fn is_trend_significant(&self, threshold: f64) -> bool {
        self.trend.abs() > threshold
    }

    /// Get trend direction: -1 (decreasing), 0 (stable), 1 (increasing)
    pub fn trend_direction(&self, threshold: f64) -> i8 {
        if self.trend > threshold {
            1
        } else if self.trend < -threshold {
            -1
        } else {
            0
        }
    }

    /// Reset tracker to initial state
    pub fn reset(&mut self) {
        self.current_value = 0.5;
        self.trend = 0.0;
        self.sample_count = 0;
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EmaPersisted {
    value: f64,
    trend: f64,
    count: u32,
}

#[derive(Debug, Clone)]
pub struct Emamanager {
    trackers: std::collections::HashMap<String, EmatrendTracker>,
    default_alpha: f64,
}

impl Emamanager {
    pub fn new(default_alpha: f64) -> Self {
        Self {
            trackers: std::collections::HashMap::new(),
            default_alpha,
        }
    }

    /// Get or create a tracker for a metric
    pub fn get_tracker(&mut self, metric_name: &str) -> &mut EmatrendTracker {
        self.trackers
            .entry(metric_name.to_string())
            .or_insert_with(|| EmatrendTracker::new(self.default_alpha))
    }

    /// Update a metric
    pub fn update_metric(&mut self, metric_name: &str, value: f64) {
        let tracker = self.get_tracker(metric_name);
        tracker.update(value);
    }

    /// Get current value for a metric
    pub fn get_value(&self, metric_name: &str) -> Option<f64> {
        self.trackers.get(metric_name).map(|t| t.current_value)
    }

    /// Get trend for a metric
    pub fn get_trend(&self, metric_name: &str) -> Option<f64> {
        self.trackers.get(metric_name).map(|t| t.trend)
    }

    /// Check if a metric has significant trend
    pub fn has_significant_trend(&self, metric_name: &str, threshold: f64) -> bool {
        self.trackers
            .get(metric_name)
            .map(|t| t.is_trend_significant(threshold))
            .unwrap_or(false)
    }

    /// Load tracker state from a JSON metrics file (`~/.ox/ema_metrics.json`).
    pub fn load_from_file(
        &mut self,
        metric_name: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read_to_string(path)?;
        let map: std::collections::HashMap<String, EmaPersisted> = serde_json::from_str(&data)?;
        if let Some(p) = map.get(metric_name) {
            let tracker = self.get_tracker(metric_name);
            tracker.current_value = p.value;
            tracker.trend = p.trend;
            tracker.sample_count = p.count;
        }
        Ok(())
    }

    /// Persist tracker state to a JSON metrics file.
    pub fn persist_to_file(&self, metric_name: &str, path: &std::path::Path) -> anyhow::Result<()> {
        let mut map: std::collections::HashMap<String, EmaPersisted> = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(path)?).unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        if let Some(tracker) = self.trackers.get(metric_name) {
            map.insert(
                metric_name.to_string(),
                EmaPersisted {
                    value: tracker.current_value,
                    trend: tracker.trend,
                    count: tracker.sample_count,
                },
            );
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&map)?)?;
        Ok(())
    }

    /// Calculate code acceptance rate from override signals
    pub fn calculate_accept_rate(&mut self, total_writes: u32, accepted_writes: u32) -> f64 {
        if total_writes == 0 {
            return 1.0; // No writes yet, assume perfect acceptance
        }

        let rate = accepted_writes as f64 / total_writes as f64;

        // Update EMA tracker for accept_rate
        self.update_metric("code_accept_rate", rate);

        rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ema_initial_state() {
        let tracker = EmatrendTracker::new(0.2);
        assert_eq!(tracker.current_value, 0.5);
        assert_eq!(tracker.trend, 0.0);
        assert_eq!(tracker.sample_count, 0);
    }

    #[test]
    fn test_ema_first_update() {
        let mut tracker = EmatrendTracker::new(0.2);
        tracker.update(0.8);
        assert_eq!(tracker.current_value, 0.8);
        assert_eq!(tracker.trend, 0.0);
        assert_eq!(tracker.sample_count, 1);
    }

    #[test]
    fn test_ema_subsequent_updates() {
        let mut tracker = EmatrendTracker::new(0.2);
        tracker.update(0.8);
        tracker.update(0.6);

        // EMA should move towards 0.6 but not reach it immediately
        assert!(tracker.current_value < 0.8);
        assert!(tracker.current_value > 0.6);
        assert!(tracker.trend < 0.0); // Decreasing trend
        assert_eq!(tracker.sample_count, 2);
    }

    #[test]
    fn test_trend_direction() {
        let mut tracker = EmatrendTracker::new(0.2);
        tracker.update(0.9);
        tracker.update(0.7);
        tracker.update(0.5);

        assert_eq!(tracker.trend_direction(0.01), -1); // Decreasing
    }

    #[test]
    fn test_ema_manager_multiple_metrics() {
        let mut manager = Emamanager::new(0.2);

        manager.update_metric("accept_rate", 0.8);
        manager.update_metric("satisfaction", 0.9);

        assert!(manager.get_value("accept_rate").is_some());
        assert!(manager.get_value("satisfaction").is_some());
    }

    #[test]
    fn test_calculate_accept_rate() {
        let mut manager = Emamanager::new(0.2);
        let rate = manager.calculate_accept_rate(10, 7);

        assert!((rate - 0.7).abs() < 0.01);
    }
}
