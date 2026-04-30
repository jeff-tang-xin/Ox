use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Record of a file written by Ox
#[derive(Debug, Clone)]
pub struct WriteRecord {
    pub content_hash: u64,
    pub line_count: usize,
    pub timestamp: Instant,
}

/// Signal indicating user modified an Ox-written file
#[derive(Debug, Clone)]
pub struct OverrideSignal {
    pub path: PathBuf,
    pub change_ratio: f64,  // 0.0~1.0, proportion of changes
    pub time_elapsed: Duration,
}

/// Detects when users override code written by Ox
pub struct CodeOverrideDetector {
    /// Recently written files and their content hashes
    recent_writes: HashMap<PathBuf, WriteRecord>,
    /// Detection window (default 5 minutes)
    detection_window: Duration,
}

impl CodeOverrideDetector {
    pub fn new(detection_window_secs: u64) -> Self {
        Self {
            recent_writes: HashMap::new(),
            detection_window: Duration::from_secs(detection_window_secs),
        }
    }

    /// Register a file written by Ox
    pub fn register_write(&mut self, path: PathBuf, content: &str) {
        let content_hash = hash_content(content);
        let line_count = content.lines().count();
        
        self.recent_writes.insert(path, WriteRecord {
            content_hash,
            line_count,
            timestamp: Instant::now(),
        });
    }

    /// Detect overrides before next user input
    pub fn detect_overrides(&mut self) -> Vec<OverrideSignal> {
        let mut signals = vec![];
        let mut to_remove = vec![];

        for (path, record) in &self.recent_writes {
            // Skip if outside detection window
            if record.timestamp.elapsed() > self.detection_window {
                to_remove.push(path.clone());
                continue;
            }

            // Check if file still exists
            if !path.exists() {
                // File was deleted - strong negative signal
                signals.push(OverrideSignal {
                    path: path.clone(),
                    change_ratio: 1.0,
                    time_elapsed: record.timestamp.elapsed(),
                });
                to_remove.push(path.clone());
                continue;
            }

            // Compare current content with Ox's version
            if let Ok(current_content) = std::fs::read_to_string(path) {
                let current_hash = hash_content(&current_content);
                
                if current_hash == record.content_hash {
                    // No changes - accepted
                    continue;
                }

                // Calculate change ratio
                let current_lines = current_content.lines().count();
                let change_ratio = calculate_diff_ratio(record.content_hash, record.line_count, 
                                                        current_hash, current_lines);

                signals.push(OverrideSignal {
                    path: path.clone(),
                    change_ratio,
                    time_elapsed: record.timestamp.elapsed(),
                });
            }
        }

        // Clean up expired records
        for path in to_remove {
            self.recent_writes.remove(&path);
        }

        signals
    }

    /// Get the number of tracked files
    pub fn tracked_count(&self) -> usize {
        self.recent_writes.len()
    }
}

/// Simple hash function for content comparison
fn hash_content(content: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Calculate diff ratio between two versions
/// Returns a value between 0.0 (no change) and 1.0 (completely different)
fn calculate_diff_ratio(old_hash: u64, old_lines: usize, new_hash: u64, new_lines: usize) -> f64 {
    if old_hash == new_hash {
        return 0.0;
    }

    // If we can't read the actual files, estimate based on line count difference
    if old_lines == 0 && new_lines == 0 {
        return 1.0;
    }

    let max_lines = old_lines.max(new_lines);
    if max_lines == 0 {
        return 1.0;
    }

    let line_diff = (old_lines as i64 - new_lines as i64).abs() as f64;
    let ratio = line_diff / max_lines as f64;
    
    // Clamp to [0.0, 1.0]
    ratio.min(1.0)
}

/// Map change ratio to implicit feedback signal
pub fn map_override_to_feedback(change_ratio: f64) -> Option<ImplicitFeedback> {
    if change_ratio < 0.05 {
        // Minor tweaks (formatting, variable names) - ignore
        None
    } else if change_ratio < 0.30 {
        // Partial correction - weak negative signal
        Some(ImplicitFeedback::WeakNegative)
    } else if change_ratio < 1.0 {
        // Major rewrite - strong negative signal
        Some(ImplicitFeedback::StrongNegative)
    } else {
        // File deleted - very strong negative signal
        Some(ImplicitFeedback::VeryStrongNegative)
    }
}

/// Implicit feedback signal types
#[derive(Debug, Clone, Copy)]
pub enum ImplicitFeedback {
    WeakNegative,      // weight: 0.3
    StrongNegative,    // weight: 0.8
    VeryStrongNegative, // weight: 1.0
}

impl ImplicitFeedback {
    pub fn weight(&self) -> f64 {
        match self {
            Self::WeakNegative => 0.3,
            Self::StrongNegative => 0.8,
            Self::VeryStrongNegative => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_content_consistency() {
        let content = "hello world";
        let hash1 = hash_content(content);
        let hash2 = hash_content(content);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_content_different() {
        let hash1 = hash_content("hello");
        let hash2 = hash_content("world");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_calculate_diff_ratio_no_change() {
        let ratio = calculate_diff_ratio(12345, 100, 12345, 100);
        assert_eq!(ratio, 0.0);
    }

    #[test]
    fn test_calculate_diff_ratio_line_change() {
        let ratio = calculate_diff_ratio(12345, 100, 67890, 80);
        assert!(ratio > 0.0 && ratio <= 1.0);
    }

    #[test]
    fn test_map_override_to_feedback() {
        assert!(map_override_to_feedback(0.02).is_none());
        assert!(matches!(map_override_to_feedback(0.15), Some(ImplicitFeedback::WeakNegative)));
        assert!(matches!(map_override_to_feedback(0.50), Some(ImplicitFeedback::StrongNegative)));
        assert!(matches!(map_override_to_feedback(1.0), Some(ImplicitFeedback::VeryStrongNegative)));
    }

    #[test]
    fn test_implicit_feedback_weights() {
        assert!((ImplicitFeedback::WeakNegative.weight() - 0.3).abs() < 0.01);
        assert!((ImplicitFeedback::StrongNegative.weight() - 0.8).abs() < 0.01);
        assert!((ImplicitFeedback::VeryStrongNegative.weight() - 1.0).abs() < 0.01);
    }
}
