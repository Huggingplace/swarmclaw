use once_cell::sync::Lazy;
use regex::{Regex, RegexSet};

pub struct Redactor;

// Common secret patterns to scrub from logs and chat history
static PATTERNS: &[(&str, &str)] = &[
    (r"sk-[a-zA-Z0-9]{32,}", "[REDACTED_OPENAI_KEY]"),
    (
        r"xoxb-[0-9]{11,}-[0-9]{12,}-[a-zA-Z0-9]{24,}",
        "[REDACTED_SLACK_TOKEN]",
    ),
    (r"mship_[a-f0-9-]{36}", "[REDACTED_MOTHERSHIP_KEY]"),
    (r"AIza[0-9A-Za-z-_]{35}", "[REDACTED_GOOGLE_API_KEY]"),
    (
        r"(?:[A-Za-z0-9+/]{4}){10,}(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?",
        "[REDACTED_BASE64_DATA]",
    ),
    (r"ghp_[a-zA-Z0-9]{36}", "[REDACTED_GITHUB_TOKEN]"),
];

static REGEX_SET: Lazy<RegexSet> =
    Lazy::new(|| RegexSet::new(PATTERNS.iter().map(|(p, _)| p)).expect("Failed to build RegexSet"));

static INDIVIDUAL_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    PATTERNS
        .iter()
        .map(|(p, _)| Regex::new(p).expect("Failed to build individual Regex"))
        .collect()
});

impl Redactor {
    /// Scrub sensitive information from a string using high-performance regex matching.
    pub fn redact(text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }

        // Check if there are any matches at all first (very fast with RegexSet)
        if !REGEX_SET.is_match(text) {
            return text.to_string();
        }

        let mut redacted_text = text.to_string();

        // Perform replacements for each pattern that matched
        for (i, regex) in INDIVIDUAL_REGEXES.iter().enumerate() {
            if REGEX_SET.matches(text).matched(i) {
                redacted_text = regex.replace_all(&redacted_text, PATTERNS[i].1).to_string();
            }
        }

        redacted_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redaction() {
        let input = "My key is sk-12345678901234567890123456789012 and gh token is ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let output = Redactor::redact(input);
        assert!(output.contains("[REDACTED_OPENAI_KEY]"));
        assert!(output.contains("[REDACTED_GITHUB_TOKEN]"));
        assert!(!output.contains("sk-1234567890"));
    }
}
