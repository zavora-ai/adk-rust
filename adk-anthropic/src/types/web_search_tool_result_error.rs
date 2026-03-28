use serde::{Deserialize, Serialize};
use std::fmt;

/// Error codes that can be returned when a web search tool operation fails.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchErrorCode {
    /// The input provided to the web search tool is invalid.
    InvalidToolInput,

    /// The web search service is currently unavailable.
    Unavailable,

    /// The maximum number of uses for the web search tool has been exceeded.
    MaxUsesExceeded,

    /// Too many requests have been made to the web search service.
    TooManyRequests,

    /// The query provided to the web search tool is too long.
    QueryTooLong,
}

impl fmt::Display for WebSearchErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebSearchErrorCode::InvalidToolInput => write!(f, "invalid_tool_input"),
            WebSearchErrorCode::Unavailable => write!(f, "unavailable"),
            WebSearchErrorCode::MaxUsesExceeded => write!(f, "max_uses_exceeded"),
            WebSearchErrorCode::TooManyRequests => write!(f, "too_many_requests"),
            WebSearchErrorCode::QueryTooLong => write!(f, "query_too_long"),
        }
    }
}

/// An error that occurred when using the web search tool.
///
/// This struct represents various failure conditions that can occur during
/// web search operations, from input validation errors to service availability issues.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchToolResultError {
    /// The specific error code indicating the type of failure.
    ///
    /// This code can be used to programmatically handle different error scenarios
    /// and provide appropriate user feedback or retry logic.
    pub error_code: WebSearchErrorCode,
}

impl WebSearchToolResultError {
    /// Creates a new WebSearchToolResultError with the specified error code.
    pub fn new(error_code: WebSearchErrorCode) -> Self {
        Self { error_code }
    }

    /// Returns true if the error is due to an invalid tool input.
    pub fn is_invalid_input(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::InvalidToolInput)
    }

    /// Returns true if the error is due to the service being unavailable.
    pub fn is_unavailable(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::Unavailable)
    }

    /// Returns true if the error is due to exceeding the maximum number of uses.
    pub fn is_max_uses_exceeded(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::MaxUsesExceeded)
    }

    /// Returns true if the error is due to too many requests.
    pub fn is_too_many_requests(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::TooManyRequests)
    }

    /// Returns true if the error is due to a query that is too long.
    pub fn is_query_too_long(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::QueryTooLong)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let error = WebSearchToolResultError { error_code: WebSearchErrorCode::InvalidToolInput };

        let json = serde_json::to_string(&error).unwrap();
        let expected = r#"{"error_code":"invalid_tool_input"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"error_code":"max_uses_exceeded"}"#;
        let error: WebSearchToolResultError = serde_json::from_str(json).unwrap();

        assert_eq!(error.error_code, WebSearchErrorCode::MaxUsesExceeded);
    }

    #[test]
    fn error_code_helpers() {
        let error = WebSearchToolResultError::new(WebSearchErrorCode::InvalidToolInput);
        assert!(error.is_invalid_input());
        assert!(!error.is_unavailable());
        assert!(!error.is_max_uses_exceeded());
        assert!(!error.is_too_many_requests());
        assert!(!error.is_query_too_long());

        let error = WebSearchToolResultError::new(WebSearchErrorCode::Unavailable);
        assert!(!error.is_invalid_input());
        assert!(error.is_unavailable());
        assert!(!error.is_max_uses_exceeded());
        assert!(!error.is_too_many_requests());
        assert!(!error.is_query_too_long());
    }
}
