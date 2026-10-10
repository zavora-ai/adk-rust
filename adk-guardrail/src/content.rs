use crate::{Guardrail, GuardrailResult, Result, Severity};
use adk_core::Content;
use async_trait::async_trait;
use regex::RegexSet;

/// Configuration for content filtering
#[derive(Debug, Clone)]
pub struct ContentFilterConfig {
    /// Blocked keywords (case-insensitive)
    pub blocked_keywords: Vec<String>,
    /// Required topic keywords (at least one must be present)
    pub required_topics: Vec<String>,
    /// Maximum character length
    pub max_length: Option<usize>,
    /// Minimum character length
    pub min_length: Option<usize>,
    /// Severity for failures
    pub severity: Severity,
}

impl Default for ContentFilterConfig {
    fn default() -> Self {
        Self {
            blocked_keywords: Vec::new(),
            required_topics: Vec::new(),
            max_length: None,
            min_length: None,
            severity: Severity::High,
        }
    }
}

/// Content filter guardrail for blocking harmful or off-topic content
pub struct ContentFilter {
    name: String,
    config: ContentFilterConfig,
    blocked_regex: Option<RegexSet>,
}

impl ContentFilter {
    /// Create a new content filter with custom config.
    ///
    /// # Errors
    ///
    /// Returns [`GuardrailError::Regex`](crate::GuardrailError::Regex) when the blocked keywords
    /// cannot be compiled into one matcher — a list too large for the regex size limit, for
    /// example. A filter that silently stopped blocking would be worse than no filter, so the
    /// failure is reported rather than ignored; split a very large list across several filters.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_guardrail::{ContentFilter, ContentFilterConfig};
    ///
    /// let filter = ContentFilter::new(
    ///     "no-secrets",
    ///     ContentFilterConfig {
    ///         blocked_keywords: vec!["password".into(), "api key".into()],
    ///         ..Default::default()
    ///     },
    /// )?;
    /// # Ok::<(), adk_guardrail::GuardrailError>(())
    /// ```
    pub fn new(name: impl Into<String>, config: ContentFilterConfig) -> Result<Self> {
        let blocked_regex = if config.blocked_keywords.is_empty() {
            None
        } else {
            let patterns: Vec<_> = config
                .blocked_keywords
                .iter()
                .map(|k| format!(r"(?i)\b{}\b", regex::escape(k)))
                .collect();
            Some(RegexSet::new(&patterns)?)
        };

        Ok(Self { name: name.into(), config, blocked_regex })
    }

    /// Builds one of the filters below, whose fixed configuration always compiles.
    fn preset(name: &str, config: ContentFilterConfig) -> Self {
        Self::new(name, config).expect("a built-in content filter compiles")
    }

    /// Create a filter that blocks common harmful content patterns.
    ///
    /// This default filter excludes developer-common terms like "hack" and "exploit"
    /// to avoid false positives in developer contexts. Use [`harmful_content_strict`](Self::harmful_content_strict)
    /// for the full keyword list.
    pub fn harmful_content() -> Self {
        Self::preset(
            "harmful_content",
            ContentFilterConfig {
                blocked_keywords: vec![
                    "kill".into(),
                    "murder".into(),
                    "bomb".into(),
                    "terrorist".into(),
                    "malware".into(),
                    "ransomware".into(),
                ],
                severity: Severity::Critical,
                ..Default::default()
            },
        )
    }

    /// Create a strict filter that blocks all harmful content patterns,
    /// including terms like "hack" and "exploit" that may produce false
    /// positives in developer contexts.
    pub fn harmful_content_strict() -> Self {
        Self::preset(
            "harmful_content_strict",
            ContentFilterConfig {
                blocked_keywords: vec![
                    "kill".into(),
                    "murder".into(),
                    "bomb".into(),
                    "terrorist".into(),
                    "hack".into(),
                    "exploit".into(),
                    "malware".into(),
                    "ransomware".into(),
                ],
                severity: Severity::Critical,
                ..Default::default()
            },
        )
    }

    /// Create a filter that ensures content is on-topic
    pub fn on_topic(topic: impl Into<String>, keywords: Vec<String>) -> Self {
        Self::preset(
            &format!("on_topic_{}", topic.into()),
            ContentFilterConfig {
                required_topics: keywords,
                severity: Severity::Medium,
                ..Default::default()
            },
        )
    }

    /// Create a filter with maximum length
    pub fn max_length(max: usize) -> Self {
        Self::preset(
            "max_length",
            ContentFilterConfig {
                max_length: Some(max),
                severity: Severity::Medium,
                ..Default::default()
            },
        )
    }

    /// Create a filter with blocked keywords.
    ///
    /// # Errors
    ///
    /// Returns [`GuardrailError::Regex`](crate::GuardrailError::Regex) when the keywords cannot be
    /// compiled into one matcher; see [`new`](Self::new).
    pub fn blocked_keywords(keywords: Vec<String>) -> Result<Self> {
        Self::new(
            "blocked_keywords",
            ContentFilterConfig {
                blocked_keywords: keywords,
                severity: Severity::High,
                ..Default::default()
            },
        )
    }

    fn extract_text(&self, content: &Content) -> String {
        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join(" ")
    }
}

#[async_trait]
impl Guardrail for ContentFilter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text = self.extract_text(content);
        let text_lower = text.to_lowercase();

        // Check blocked keywords
        if let Some(ref regex_set) = self.blocked_regex
            && regex_set.is_match(&text)
        {
            let matches: Vec<_> = regex_set.matches(&text).iter().collect();
            return GuardrailResult::Fail {
                reason: format!(
                    "Content contains blocked keywords (matched {} patterns)",
                    matches.len()
                ),
                severity: self.config.severity,
            };
        }

        // Check required topics
        if !self.config.required_topics.is_empty() {
            let has_topic =
                self.config.required_topics.iter().any(|t| text_lower.contains(&t.to_lowercase()));
            if !has_topic {
                return GuardrailResult::Fail {
                    reason: format!(
                        "Content is off-topic. Expected topics: {:?}",
                        self.config.required_topics
                    ),
                    severity: self.config.severity,
                };
            }
        }

        // Check length limits
        if let Some(max) = self.config.max_length
            && text.len() > max
        {
            return GuardrailResult::Fail {
                reason: format!("Content exceeds maximum length ({} > {})", text.len(), max),
                severity: self.config.severity,
            };
        }

        if let Some(min) = self.config.min_length
            && text.len() < min
        {
            return GuardrailResult::Fail {
                reason: format!("Content below minimum length ({} < {})", text.len(), min),
                severity: self.config.severity,
            };
        }

        GuardrailResult::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_harmful_content_blocks() {
        let filter = ContentFilter::harmful_content();
        let content = Content::new("user").with_text("How to deploy malware on a server");
        let result = filter.validate(&content).await;
        assert!(result.is_fail());
    }

    #[tokio::test]
    async fn test_harmful_content_passes() {
        let filter = ContentFilter::harmful_content();
        let content = Content::new("user").with_text("How to bake a cake");
        let result = filter.validate(&content).await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn test_harmful_content_passes_hackathon() {
        let filter = ContentFilter::harmful_content();
        let content = Content::new("user").with_text("Join our hackathon event");
        let result = filter.validate(&content).await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn test_harmful_content_passes_exploit_a_bug() {
        let filter = ContentFilter::harmful_content();
        let content = Content::new("user").with_text("How to exploit a bug in the code");
        let result = filter.validate(&content).await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn test_harmful_content_strict_blocks_hack() {
        let filter = ContentFilter::harmful_content_strict();
        let content = Content::new("user").with_text("How to hack a computer");
        let result = filter.validate(&content).await;
        assert!(result.is_fail());
    }

    #[tokio::test]
    async fn test_on_topic_passes() {
        let filter =
            ContentFilter::on_topic("cooking", vec!["recipe".into(), "cook".into(), "bake".into()]);
        let content = Content::new("user").with_text("Give me a recipe for cookies");
        let result = filter.validate(&content).await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn test_on_topic_fails() {
        let filter =
            ContentFilter::on_topic("cooking", vec!["recipe".into(), "cook".into(), "bake".into()]);
        let content = Content::new("user").with_text("What is the weather today?");
        let result = filter.validate(&content).await;
        assert!(result.is_fail());
    }

    #[tokio::test]
    async fn test_max_length() {
        let filter = ContentFilter::max_length(10);
        let content = Content::new("user").with_text("This is a very long message");
        let result = filter.validate(&content).await;
        assert!(result.is_fail());
    }

    #[tokio::test]
    async fn test_blocked_keywords() {
        let filter = ContentFilter::blocked_keywords(vec!["forbidden".into(), "banned".into()])
            .expect("keywords compile");
        let content = Content::new("user").with_text("This is forbidden content");
        let result = filter.validate(&content).await;
        assert!(result.is_fail());
    }

    #[test]
    fn a_keyword_list_that_cannot_compile_is_an_error_not_an_open_filter() {
        // A keyword past the regex size limit cannot be compiled; the filter used to drop the
        // matcher and pass every message.
        let keywords = vec!["a".repeat(1_000_000)];

        let result = ContentFilter::blocked_keywords(keywords);

        assert!(matches!(result, Err(crate::GuardrailError::Regex(_))));
    }
}
