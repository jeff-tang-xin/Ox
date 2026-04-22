/// Simple token estimator (whitespace-based, ~4 chars per token).
/// Sufficient for budget management; exact counts come from API responses.
pub fn estimate_tokens(text: &str) -> u32 {
    // Rough heuristic: 1 token ≈ 4 characters for English,
    // slightly less for CJK. Using 4 as a conservative estimate.
    let chars = text.len();
    chars.div_ceil(4) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hi"), 1);
        assert_eq!(estimate_tokens("hello world, this is a test"), 7);
    }
}
