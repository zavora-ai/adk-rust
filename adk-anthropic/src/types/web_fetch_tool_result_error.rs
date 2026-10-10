use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Error codes that can be returned when a web fetch tool operation fails.
///
/// Codes this crate does not know are kept verbatim in [`WebFetchErrorCode::Unknown`],
/// so a result replayed to the API carries the code the API sent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WebFetchErrorCode {
    /// The input provided to the web fetch tool is invalid, such as a malformed URL.
    InvalidToolInput,

    /// The URL exceeds the maximum length (250 characters).
    UrlTooLong,

    /// The web fetch service is currently unavailable.
    Unavailable,

    /// The maximum number of uses for the web fetch tool has been exceeded.
    MaxUsesExceeded,

    /// Too many requests have been made to the web fetch service.
    TooManyRequests,

    /// The requested URL is blocked by domain filtering or Anthropic-side restrictions.
    UrlNotAllowed,

    /// Fetching the URL failed with an HTTP error.
    UrlNotAccessible,

    /// The content type is not supported (only text, HTML, and PDF are).
    UnsupportedContentType,

    /// The fetch operation failed.
    FetchFailed,

    /// The requested URL did not appear earlier in the conversation.
    UrlNotInPriorContext,

    /// An error code from a newer API version, kept verbatim.
    Unknown(String),
}

impl WebFetchErrorCode {
    /// Returns the wire value of the code, for example `"url_not_accessible"`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_anthropic::WebFetchErrorCode;
    ///
    /// assert_eq!(WebFetchErrorCode::UrlNotAccessible.as_str(), "url_not_accessible");
    /// assert_eq!(WebFetchErrorCode::from("future_code").as_str(), "future_code");
    /// ```
    pub fn as_str(&self) -> &str {
        match self {
            WebFetchErrorCode::InvalidToolInput => "invalid_tool_input",
            WebFetchErrorCode::UrlTooLong => "url_too_long",
            WebFetchErrorCode::Unavailable => "unavailable",
            WebFetchErrorCode::MaxUsesExceeded => "max_uses_exceeded",
            WebFetchErrorCode::TooManyRequests => "too_many_requests",
            WebFetchErrorCode::UrlNotAllowed => "url_not_allowed",
            WebFetchErrorCode::UrlNotAccessible => "url_not_accessible",
            WebFetchErrorCode::UnsupportedContentType => "unsupported_content_type",
            WebFetchErrorCode::FetchFailed => "fetch_failed",
            WebFetchErrorCode::UrlNotInPriorContext => "url_not_in_prior_context",
            WebFetchErrorCode::Unknown(code) => code,
        }
    }
}

impl From<&str> for WebFetchErrorCode {
    fn from(code: &str) -> Self {
        match code {
            "invalid_tool_input" => WebFetchErrorCode::InvalidToolInput,
            "url_too_long" => WebFetchErrorCode::UrlTooLong,
            "unavailable" => WebFetchErrorCode::Unavailable,
            "max_uses_exceeded" => WebFetchErrorCode::MaxUsesExceeded,
            "too_many_requests" => WebFetchErrorCode::TooManyRequests,
            "url_not_allowed" => WebFetchErrorCode::UrlNotAllowed,
            "url_not_accessible" => WebFetchErrorCode::UrlNotAccessible,
            "unsupported_content_type" => WebFetchErrorCode::UnsupportedContentType,
            "fetch_failed" => WebFetchErrorCode::FetchFailed,
            "url_not_in_prior_context" => WebFetchErrorCode::UrlNotInPriorContext,
            other => WebFetchErrorCode::Unknown(other.to_string()),
        }
    }
}

impl Serialize for WebFetchErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WebFetchErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?.as_str()))
    }
}

impl fmt::Display for WebFetchErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error that occurred when using the web fetch tool.
///
/// Serializes with `"type": "web_fetch_tool_result_error"`, which the API requires
/// when the block is sent back in conversation history.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WebFetchToolResultError {
    /// The specific error code indicating the type of failure.
    pub error_code: WebFetchErrorCode,
}

impl Serialize for WebFetchToolResultError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("WebFetchToolResultError", 2)?;
        state.serialize_field("type", "web_fetch_tool_result_error")?;
        state.serialize_field("error_code", &self.error_code)?;
        state.end()
    }
}

impl WebFetchToolResultError {
    /// Creates a new WebFetchToolResultError with the specified error code.
    pub fn new(error_code: WebFetchErrorCode) -> Self {
        Self { error_code }
    }

    /// Returns true if the error is due to an invalid tool input.
    pub fn is_invalid_input(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::InvalidToolInput)
    }

    /// Returns true if the error is due to the service being unavailable.
    pub fn is_unavailable(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::Unavailable)
    }

    /// Returns true if the error is due to exceeding the maximum number of uses.
    pub fn is_max_uses_exceeded(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::MaxUsesExceeded)
    }

    /// Returns true if the error is due to too many requests.
    pub fn is_too_many_requests(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::TooManyRequests)
    }

    /// Returns true if the URL was not in the allowed domains.
    pub fn is_url_not_allowed(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::UrlNotAllowed)
    }

    /// Returns true if the fetch operation itself failed.
    pub fn is_fetch_failed(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::FetchFailed)
    }

    /// Returns true if the URL was not referenced in the prior context (Anthropic restriction:
    /// only URLs from prior web search results may be fetched).
    pub fn is_url_not_in_prior_context(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::UrlNotInPriorContext)
    }

    /// Returns true if the URL could not be fetched because of an HTTP error.
    pub fn is_url_not_accessible(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::UrlNotAccessible)
    }

    /// Returns true if the error code was not recognised (forward-compatibility catch-all).
    pub fn is_unknown(&self) -> bool {
        matches!(self.error_code, WebFetchErrorCode::Unknown(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let error = WebFetchToolResultError { error_code: WebFetchErrorCode::InvalidToolInput };
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(
            json,
            r#"{"type":"web_fetch_tool_result_error","error_code":"invalid_tool_input"}"#
        );
    }

    #[test]
    fn documented_error_codes_round_trip() {
        // Every code listed in the web fetch tool documentation.
        for code in [
            "invalid_tool_input",
            "url_too_long",
            "url_not_allowed",
            "url_not_in_prior_context",
            "url_not_accessible",
            "too_many_requests",
            "unsupported_content_type",
            "max_uses_exceeded",
            "unavailable",
        ] {
            let wire =
                serde_json::json!({"type": "web_fetch_tool_result_error", "error_code": code});
            let error: WebFetchToolResultError = serde_json::from_value(wire.clone()).unwrap();
            assert!(!error.is_unknown(), "{code} should be a known code");
            assert_eq!(serde_json::to_value(&error).unwrap(), wire);
        }
    }

    #[test]
    fn deserialization() {
        let json = r#"{"error_code":"max_uses_exceeded"}"#;
        let error: WebFetchToolResultError = serde_json::from_str(json).unwrap();
        assert_eq!(error.error_code, WebFetchErrorCode::MaxUsesExceeded);
    }

    #[test]
    fn error_code_helpers() {
        let error = WebFetchToolResultError::new(WebFetchErrorCode::InvalidToolInput);
        assert!(error.is_invalid_input());
        assert!(!error.is_unavailable());
        assert!(!error.is_max_uses_exceeded());
        assert!(!error.is_too_many_requests());
        assert!(!error.is_url_not_allowed());
        assert!(!error.is_fetch_failed());
        assert!(!error.is_url_not_in_prior_context());
        assert!(!error.is_unknown());
    }

    #[test]
    fn url_not_in_prior_context_roundtrips() {
        let error = WebFetchToolResultError::new(WebFetchErrorCode::UrlNotInPriorContext);
        assert!(error.is_url_not_in_prior_context());
        // This is the error code the live API returns when a model tries to fetch a URL that
        // wasn't mentioned in prior web_search results.
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(
            json,
            r#"{"type":"web_fetch_tool_result_error","error_code":"url_not_in_prior_context"}"#
        );
        let deserialized: WebFetchToolResultError = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.error_code, WebFetchErrorCode::UrlNotInPriorContext);
    }

    #[test]
    fn unknown_error_code_round_trips_verbatim() {
        let json = r#"{"error_code":"some_future_code"}"#;
        let error: WebFetchToolResultError = serde_json::from_str(json).unwrap();
        assert!(error.is_unknown());
        assert_eq!(error.error_code.to_string(), "some_future_code");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({"type": "web_fetch_tool_result_error", "error_code": "some_future_code"})
        );
    }

    #[test]
    fn url_not_in_prior_context_with_type_field_ignored() {
        // The live API includes a "type" field in the error content that our struct ignores.
        let json =
            r#"{"type":"web_fetch_tool_result_error","error_code":"url_not_in_prior_context"}"#;
        let error: WebFetchToolResultError = serde_json::from_str(json).unwrap();
        assert!(error.is_url_not_in_prior_context());
    }
}
