use regex::Regex;

pub struct DataSanitizer {
    patterns: Vec<(Regex, &'static str)>,
}

impl DataSanitizer {
    pub fn new() -> Self {
        let patterns: Vec<(Regex, &'static str)> = vec![
            (Regex::new(r"\b\d{11}\b").unwrap(), "[PHONE]"),
            (Regex::new(r"[\w.+-]+@[\w-]+\.[\w.]+").unwrap(), "[EMAIL]"),
            (Regex::new(r"\b\d{17}[\dXx]\b").unwrap(), "[ID_CARD]"),
            (Regex::new(r"\b\d{16,19}\b").unwrap(), "[BANK_CARD]"),
            (
                Regex::new(r"(?i)(password|passwd|pwd|secret|token|api_key|apikey)\s*[:=]\s*\S+")
                    .unwrap(),
                "[REDACTED]",
            ),
        ];
        Self { patterns }
    }

    pub fn sanitize(&self, text: &str) -> String {
        let mut result = text.to_string();
        for (re, replacement) in &self.patterns {
            result = re.replace_all(&result, *replacement).to_string();
        }
        result
    }

    pub fn should_sanitize(&self, text: &str) -> bool {
        self.patterns.iter().any(|(re, _)| re.is_match(text))
    }

    pub fn sanitize_all(text: &str) -> String {
        Self::new().sanitize(text)
    }
}

impl Default for DataSanitizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_phone() {
        let s = DataSanitizer::new();
        assert_eq!(s.sanitize("Call 13912345678"), "Call [PHONE]");
    }

    #[test]
    fn sanitize_email() {
        let s = DataSanitizer::new();
        assert_eq!(s.sanitize("Email: user@example.com"), "Email: [EMAIL]");
    }

    #[test]
    fn sanitize_password() {
        let s = DataSanitizer::new();
        let result = s.sanitize("password=secret123");
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("secret123"));
    }

    #[test]
    fn sanitize_preserves_safe_text() {
        let s = DataSanitizer::new();
        assert_eq!(s.sanitize("Hello world"), "Hello world");
    }
}
