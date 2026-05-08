/// Effort level determines how much compute budget to allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    /// Simple Q&A, explanation, formatting.
    Low,
    /// Code completion, light modification.
    Medium,
    /// Standard code generation, refactoring.
    Standard,
    /// Complex architecture, multi-file changes, deep debugging.
    High,
}

impl EffortLevel {
    /// Coefficient multiplier for token cost estimation.
    pub fn coefficient(&self) -> f32 {
        match self {
            Self::Low => 0.2,
            Self::Medium => 0.5,
            Self::Standard => 1.0,
            Self::High => 1.5,
        }
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::Standard => write!(f, "standard"),
            Self::High => write!(f, "high"),
        }
    }
}

/// Estimate effort level from user input using local heuristics (zero token cost).
pub fn estimate_effort(input: &str, history_len: usize) -> EffortLevel {
    let input_tokens = input.len() / 4; // rough estimate
    let has_code_block = input.contains("```");
    let is_question = input.trim_end().ends_with('?')
        || input.to_lowercase().starts_with("what")
        || input.to_lowercase().starts_with("how")
        || input.to_lowercase().starts_with("why")
        || input.to_lowercase().starts_with("explain");

    let mentions_multiple_files = input.contains("files")
        || input.contains("project")
        || input.contains("refactor")
        || input.contains("architecture");

    match () {
        // Simple Q&A
        _ if is_question && input_tokens < 50 && !has_code_block => EffortLevel::Low,
        // Complex multi-file tasks
        _ if mentions_multiple_files || (has_code_block && input_tokens > 500) => EffortLevel::High,
        // Standard code tasks
        _ if input_tokens > 200 || has_code_block || history_len > 10 => EffortLevel::Standard,
        // Default to medium
        _ => EffortLevel::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_question_is_low() {
        assert_eq!(estimate_effort("What is a closure?", 0), EffortLevel::Low);
    }

    #[test]
    fn code_block_is_standard_or_higher() {
        let input = "Fix this:\n```rust\nfn main() {}\n```";
        let level = estimate_effort(input, 0);
        assert!(level == EffortLevel::Standard || level == EffortLevel::High);
    }

    #[test]
    fn refactor_is_high() {
        assert_eq!(
            estimate_effort("Refactor the authentication architecture", 0),
            EffortLevel::High
        );
    }

    #[test]
    fn short_request_is_medium() {
        assert_eq!(
            estimate_effort("Add a login button", 0),
            EffortLevel::Medium
        );
    }
}
