use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Error codes that can be returned when a web search tool operation fails.
///
/// Codes this crate does not know are kept verbatim in [`WebSearchErrorCode::Unknown`],
/// so a result replayed to the API carries the code the API sent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    /// The request exceeded the web search tool's size limit.
    RequestTooLarge,

    /// An error code from a newer API version, kept verbatim.
    Unknown(String),
}

impl WebSearchErrorCode {
    /// Returns the wire value of the code, for example `"query_too_long"`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_anthropic::WebSearchErrorCode;
    ///
    /// assert_eq!(WebSearchErrorCode::QueryTooLong.as_str(), "query_too_long");
    /// assert_eq!(WebSearchErrorCode::from("future_code").as_str(), "future_code");
    /// ```
    pub fn as_str(&self) -> &str {
        match self {
            WebSearchErrorCode::InvalidToolInput => "invalid_tool_input",
            WebSearchErrorCode::Unavailable => "unavailable",
            WebSearchErrorCode::MaxUsesExceeded => "max_uses_exceeded",
            WebSearchErrorCode::TooManyRequests => "too_many_requests",
            WebSearchErrorCode::QueryTooLong => "query_too_long",
            WebSearchErrorCode::RequestTooLarge => "request_too_large",
            WebSearchErrorCode::Unknown(code) => code,
        }
    }
}

impl From<&str> for WebSearchErrorCode {
    fn from(code: &str) -> Self {
        match code {
            "invalid_tool_input" => WebSearchErrorCode::InvalidToolInput,
            "unavailable" => WebSearchErrorCode::Unavailable,
            "max_uses_exceeded" => WebSearchErrorCode::MaxUsesExceeded,
            "too_many_requests" => WebSearchErrorCode::TooManyRequests,
            "query_too_long" => WebSearchErrorCode::QueryTooLong,
            "request_too_large" => WebSearchErrorCode::RequestTooLarge,
            other => WebSearchErrorCode::Unknown(other.to_string()),
        }
    }
}

impl Serialize for WebSearchErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WebSearchErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?.as_str()))
    }
}

impl fmt::Display for WebSearchErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error that occurred when using the web search tool.
///
/// This struct represents various failure conditions that can occur during
/// web search operations, from input validation errors to service availability issues.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename = "web_search_tool_result_error")]
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

    /// Returns true if the request exceeded the web search tool's size limit.
    pub fn is_request_too_large(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::RequestTooLarge)
    }

    /// Returns true if the error code is one this crate does not know.
    pub fn is_unknown(&self) -> bool {
        matches!(self.error_code, WebSearchErrorCode::Unknown(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_too_large_round_trips() {
        let wire = serde_json::json!({
            "type": "web_search_tool_result_error",
            "error_code": "request_too_large"
        });
        let error: WebSearchToolResultError = serde_json::from_value(wire.clone()).unwrap();

        assert_eq!(error.error_code, WebSearchErrorCode::RequestTooLarge);
        assert_eq!(error.error_code.to_string(), "request_too_large");
        assert_eq!(serde_json::to_value(error).unwrap(), wire);
    }

    #[test]
    fn documented_codes_round_trip() {
        for code in [
            WebSearchErrorCode::InvalidToolInput,
            WebSearchErrorCode::Unavailable,
            WebSearchErrorCode::MaxUsesExceeded,
            WebSearchErrorCode::TooManyRequests,
            WebSearchErrorCode::QueryTooLong,
            WebSearchErrorCode::RequestTooLarge,
        ] {
            let wire = serde_json::to_value(&code).unwrap();
            assert_eq!(wire, serde_json::json!(code.as_str()));
            assert_eq!(code.to_string(), code.as_str());
            assert_eq!(serde_json::from_value::<WebSearchErrorCode>(wire).unwrap(), code);
        }
    }

    #[test]
    fn unknown_code_round_trips_verbatim() {
        let wire = serde_json::json!({
            "type": "web_search_tool_result_error",
            "error_code": "future_search_code"
        });
        let error: WebSearchToolResultError = serde_json::from_value(wire.clone()).unwrap();

        assert_eq!(
            error,
            WebSearchToolResultError::new(WebSearchErrorCode::Unknown(
                "future_search_code".to_string()
            ))
        );
        assert!(error.is_unknown());
        assert_eq!(error.error_code.to_string(), "future_search_code");
        assert_eq!(serde_json::to_value(error).unwrap(), wire);
    }

    #[test]
    fn serialization() {
        let error = WebSearchToolResultError { error_code: WebSearchErrorCode::InvalidToolInput };

        let json = serde_json::to_string(&error).unwrap();
        let expected =
            r#"{"type":"web_search_tool_result_error","error_code":"invalid_tool_input"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}"#;
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

        let error = WebSearchToolResultError::new(WebSearchErrorCode::RequestTooLarge);
        assert!(error.is_request_too_large());
        assert!(!error.is_query_too_long());
        assert!(!error.is_unknown());
    }
}
