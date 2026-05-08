/// Shared content validation utilities for file write/patch operations.
///
/// This module provides consistent validation logic across all file modification tools
/// to prevent garbled/corrupted text from being written.

/// Validate file content to prevent garbled/corrupted text from being written.
///
/// This is the unified validation function used by both `file_write` and `file_patch`.
///
/// # Validation Checks
/// 1. **Null bytes** - Definite corruption indicator (always rejected)
/// 2. **Replacement characters (U+FFFD)** - Warn if >5% and >10 chars (not blocked)
/// 3. **Control characters** - Reject if >10% of content (relaxed for code)
///
/// # Returns
/// - `Ok(())` if content passes all checks
/// - `Err(String)` if content contains definite corruption (null bytes or excessive control chars)
///
/// # Examples
/// ```
/// use ox_core::tools::content_validation::validate_content;
///
/// // Valid content
/// assert!(validate_content("Hello World").is_ok());
///
/// // Content with emoji (allowed)
/// assert!(validate_content("Hello 😀 World").is_ok());
///
/// // Content with null bytes (rejected)
/// assert!(validate_content("Hello\x00World").is_err());
/// ```
pub fn validate_content(content: &str) -> Result<(), String> {
    let total_chars = content.chars().count();

    // Check 1: Detect null bytes (definite corruption indicator)
    if content.contains('\x00') {
        return Err("❌ Corrupted Content: File contains null bytes (\\x00)\n\n\
                    💡 This indicates:\n\
                    • Binary data mixed with text\n\
                    • Severe encoding errors\n\n\
                     Please verify and regenerate the content."
            .to_string());
    }

    // Check 2: Detect excessive replacement characters (U+FFFD - relaxed threshold)
    if total_chars > 0 {
        let fffd_count = content.matches('\u{FFFD}').count();
        let fffd_ratio = fffd_count as f64 / total_chars as f64;

        // Only reject if >5% of content is replacement characters AND count > 10
        if fffd_ratio > 0.05 && fffd_count > 10 {
            tracing::warn!(
                "[CONTENT_VALIDATION] High replacement character ratio: {} chars ({:.1}%)",
                fffd_count,
                fffd_ratio * 100.0
            );
            // Don't block, just warn - LLM might generate special symbols
        }
    }

    // Check 3: Detect suspicious non-printable character ratio
    if total_chars > 100 {
        let non_printable_count = content
            .chars()
            .filter(|c| {
                !c.is_whitespace()
                    && !c.is_ascii_graphic()
                    && !c.is_ascii_punctuation()
                    && !matches!(*c, '\n' | '\r' | '\t')
                    && (*c as u32) < 0x20 // Control characters only
            })
            .count();

        let ratio = non_printable_count as f64 / total_chars as f64;
        if ratio > 0.10 {
            // >10% non-printable
            return Err(format!(
                "❌ Suspicious Content: {:.1}% control characters detected\n\n\
                 💡 This suggests:\n\
                 • Binary data mixed with text\n\
                 • Severe encoding corruption\n\n\
                 📝 Please verify content integrity.",
                ratio * 100.0
            ));
        }
    }

    // All checks passed or warnings only - allow write
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_text() {
        assert!(validate_content("Hello World").is_ok());
        assert!(validate_content("你好世界").is_ok());
        assert!(validate_content("Mixed: Hello 世界 🌍").is_ok());
    }

    #[test]
    fn test_emoji_allowed() {
        assert!(validate_content("Hello 😀 World 🚀").is_ok());
    }

    #[test]
    fn test_special_unicode_allowed() {
        assert!(validate_content("Math: ∑∫∞ ≈≠≤≥").is_ok());
        assert!(validate_content("Arrows: →←↑↓").is_ok());
    }

    #[test]
    fn test_null_bytes_rejected() {
        assert!(validate_content("Hello\x00World").is_err());
    }

    #[test]
    fn test_few_fffd_allowed() {
        // Small number of replacement characters should be allowed
        let content = "Some text \u{FFFD} here";
        assert!(validate_content(content).is_ok());
    }

    #[test]
    fn test_many_fffd_warned_but_allowed() {
        // Create content with >5% and >10 replacement chars
        let mut content = String::new();
        for _ in 0..100 {
            content.push('a');
        }
        for _ in 0..15 {
            content.push('\u{FFFD}');
        }
        // Should warn but allow
        assert!(validate_content(&content).is_ok());
    }

    #[test]
    fn test_control_characters_rejected() {
        // Create content with >10% control characters (need >100 chars total)
        let mut content = String::new();
        for _ in 0..200 {
            content.push('a');
        }
        for _ in 0..30 {
            content.push('\x01'); // Control character (>10% of 230)
        }
        assert!(validate_content(&content).is_err());
    }

    #[test]
    fn test_normal_whitespace_allowed() {
        let content = "Line 1\nLine 2\r\nLine 3\tTabbed";
        assert!(validate_content(content).is_ok());
    }

    #[test]
    fn test_code_with_special_chars() {
        let content = r#"fn main() {
    println!("Hello, {}!", name);
    let path = "C:\\Users\\test";
}"#;
        assert!(validate_content(content).is_ok());
    }
}
