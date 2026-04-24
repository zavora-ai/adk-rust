//! Requester type detection from HTTP headers.

use awp_types::RequesterType;
use axum::http::HeaderMap;

/// Known agent User-Agent patterns (case-insensitive).
const AGENT_PATTERNS: &[&str] = &[
    "bot",
    "crawler",
    "spider",
    "agent",
    "gpt",
    "claude",
    "gemini",
    "perplexity",
    "anthropic",
    "openai",
];

/// Detect whether a request comes from a human or an agent.
///
/// Detection priority:
/// 1. `X-AWP-Channel: agent` header overrides all other signals → [`RequesterType::Agent`]
/// 2. `Accept` contains `application/json`, `application/ld+json`, or `application/awp+json`
///    **and** `User-Agent` matches a known agent pattern → [`RequesterType::Agent`]
/// 3. Otherwise → [`RequesterType::Human`]
///
/// # Example
///
/// ```
/// use axum::http::HeaderMap;
/// use adk_awp::detect_requester_type;
/// use awp_types::RequesterType;
///
/// let mut headers = HeaderMap::new();
/// headers.insert("X-AWP-Channel", "agent".parse().unwrap());
/// assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
/// ```
pub fn detect_requester_type(headers: &HeaderMap) -> RequesterType {
    // 1. X-AWP-Channel override
    if let Some(channel) = headers.get("X-AWP-Channel") {
        if channel.to_str().unwrap_or("").eq_ignore_ascii_case("agent") {
            return RequesterType::Agent;
        }
    }

    // 2. Accept + User-Agent combination
    let accept = headers.get("Accept").and_then(|v| v.to_str().ok()).unwrap_or("");
    let ua = headers.get("User-Agent").and_then(|v| v.to_str().ok()).unwrap_or("");
    let ua_lower = ua.to_lowercase();

    let is_agent_accept = accept.contains("application/json")
        || accept.contains("application/ld+json")
        || accept.contains("application/awp+json");
    let is_agent_ua = AGENT_PATTERNS.iter().any(|p| ua_lower.contains(p));

    if is_agent_accept && is_agent_ua {
        return RequesterType::Agent;
    }

    RequesterType::Human
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_x_awp_channel_override() {
        let mut headers = HeaderMap::new();
        headers.insert("X-AWP-Channel", "agent".parse().unwrap());
        // Even with human-like Accept and UA, channel override wins
        headers.insert("Accept", "text/html".parse().unwrap());
        headers.insert("User-Agent", "Mozilla/5.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_x_awp_channel_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("X-AWP-Channel", "Agent".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_agent_accept_and_ua() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/json".parse().unwrap());
        headers.insert("User-Agent", "GPT-Agent/1.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_agent_ld_json_accept() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/ld+json".parse().unwrap());
        headers.insert("User-Agent", "ClaudeBot/1.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_agent_awp_json_accept() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/awp+json".parse().unwrap());
        headers.insert("User-Agent", "my-bot/1.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_human_html_accept() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "text/html".parse().unwrap());
        headers.insert("User-Agent", "Mozilla/5.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Human);
    }

    #[test]
    fn test_agent_ua_but_html_accept_is_human() {
        // Agent UA alone is not enough — need agent Accept too
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "text/html".parse().unwrap());
        headers.insert("User-Agent", "Googlebot/2.1".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Human);
    }

    #[test]
    fn test_json_accept_but_human_ua_is_human() {
        // JSON Accept alone is not enough — need agent UA too
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/json".parse().unwrap());
        headers.insert("User-Agent", "Mozilla/5.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Human);
    }

    #[test]
    fn test_no_headers_is_human() {
        let headers = HeaderMap::new();
        assert_eq!(detect_requester_type(&headers), RequesterType::Human);
    }

    #[test]
    fn test_anthropic_ua_pattern() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/json".parse().unwrap());
        headers.insert("User-Agent", "Anthropic-AI/1.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_openai_ua_pattern() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/json".parse().unwrap());
        headers.insert("User-Agent", "OpenAI-Agent/1.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }

    #[test]
    fn test_spider_ua_pattern() {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/json".parse().unwrap());
        headers.insert("User-Agent", "WebSpider/3.0".parse().unwrap());
        assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
    }
}
