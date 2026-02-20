//! Rate limit information from Anthropic API response headers.
//!
//! Parses `anthropic-ratelimit-*` and `retry-after` headers into structured
//! types for use in retry logic and caller inspection.

use std::collections::HashMap;
use std::time::Duration;

/// Rate limit information parsed from Anthropic response headers.
///
/// Anthropic returns rate-limit state via HTTP response headers on every API
/// call. This struct captures those values for caller inspection and retry
/// decisions.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::RateLimitInfo;
///
/// let info = RateLimitInfo::default();
/// assert!(info.requests_limit.is_none());
/// assert!(info.retry_after.is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimitInfo {
    /// Maximum requests allowed in the current window.
    pub requests_limit: Option<u32>,
    /// Remaining requests in the current window.
    pub requests_remaining: Option<u32>,
    /// Timestamp when the request limit resets (ISO 8601).
    pub requests_reset: Option<String>,
    /// Maximum tokens allowed in the current window.
    pub tokens_limit: Option<u32>,
    /// Remaining tokens in the current window.
    pub tokens_remaining: Option<u32>,
    /// Timestamp when the token limit resets (ISO 8601).
    pub tokens_reset: Option<String>,
    /// Server-suggested retry delay from the `retry-after` header.
    pub retry_after: Option<Duration>,
}

impl RateLimitInfo {
    /// Parse rate-limit information from HTTP response headers.
    ///
    /// Accepts a map of lowercase header names to their string values.
    /// Extracts all `anthropic-ratelimit-*` headers and the `retry-after`
    /// header. Missing or unparseable headers are represented as `None`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::collections::HashMap;
    /// use adk_model::anthropic::RateLimitInfo;
    ///
    /// let mut headers = HashMap::new();
    /// headers.insert("anthropic-ratelimit-requests-remaining".to_string(), "5".to_string());
    /// headers.insert("retry-after".to_string(), "30".to_string());
    ///
    /// let info = RateLimitInfo::from_headers(&headers);
    /// assert_eq!(info.requests_remaining, Some(5));
    /// assert_eq!(info.retry_after, Some(std::time::Duration::from_secs(30)));
    /// ```
    pub fn from_headers(headers: &HashMap<String, String>) -> Self {
        Self {
            requests_limit: Self::parse_u32(headers, "anthropic-ratelimit-requests-limit"),
            requests_remaining: Self::parse_u32(headers, "anthropic-ratelimit-requests-remaining"),
            requests_reset: headers.get("anthropic-ratelimit-requests-reset").cloned(),
            tokens_limit: Self::parse_u32(headers, "anthropic-ratelimit-tokens-limit"),
            tokens_remaining: Self::parse_u32(headers, "anthropic-ratelimit-tokens-remaining"),
            tokens_reset: headers.get("anthropic-ratelimit-tokens-reset").cloned(),
            retry_after: Self::parse_retry_after(headers),
        }
    }

    /// Parse a header value as `u32`.
    fn parse_u32(headers: &HashMap<String, String>, name: &str) -> Option<u32> {
        headers.get(name).and_then(|v| v.trim().parse().ok())
    }

    /// Parse the `retry-after` header into a [`Duration`].
    ///
    /// Supports integer seconds format (e.g., `retry-after: 30`).
    fn parse_retry_after(headers: &HashMap<String, String>) -> Option<Duration> {
        headers
            .get("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_all_headers() {
        let headers = HashMap::from([
            ("anthropic-ratelimit-requests-limit".to_string(), "100".to_string()),
            ("anthropic-ratelimit-requests-remaining".to_string(), "95".to_string()),
            ("anthropic-ratelimit-requests-reset".to_string(), "2025-02-17T12:00:00Z".to_string()),
            ("anthropic-ratelimit-tokens-limit".to_string(), "100000".to_string()),
            ("anthropic-ratelimit-tokens-remaining".to_string(), "90000".to_string()),
            ("anthropic-ratelimit-tokens-reset".to_string(), "2025-02-17T12:00:00Z".to_string()),
            ("retry-after".to_string(), "30".to_string()),
        ]);

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.requests_limit, Some(100));
        assert_eq!(info.requests_remaining, Some(95));
        assert_eq!(info.requests_reset.as_deref(), Some("2025-02-17T12:00:00Z"));
        assert_eq!(info.tokens_limit, Some(100000));
        assert_eq!(info.tokens_remaining, Some(90000));
        assert_eq!(info.tokens_reset.as_deref(), Some("2025-02-17T12:00:00Z"));
        assert_eq!(info.retry_after, Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_parse_empty_headers() {
        let headers = HashMap::new();
        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info, RateLimitInfo::default());
    }

    #[test]
    fn test_parse_partial_headers() {
        let headers = HashMap::from([
            ("anthropic-ratelimit-requests-remaining".to_string(), "5".to_string()),
            ("anthropic-ratelimit-tokens-remaining".to_string(), "1000".to_string()),
        ]);

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.requests_limit, None);
        assert_eq!(info.requests_remaining, Some(5));
        assert_eq!(info.tokens_limit, None);
        assert_eq!(info.tokens_remaining, Some(1000));
        assert!(info.retry_after.is_none());
    }

    #[test]
    fn test_parse_invalid_numeric_headers() {
        let headers = HashMap::from([
            ("anthropic-ratelimit-requests-limit".to_string(), "not_a_number".to_string()),
            ("retry-after".to_string(), "invalid".to_string()),
        ]);

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.requests_limit, None);
        assert!(info.retry_after.is_none());
    }

    #[test]
    fn test_retry_after_with_whitespace() {
        let headers = HashMap::from([("retry-after".to_string(), " 60 ".to_string())]);

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.retry_after, Some(Duration::from_secs(60)));
    }
}
