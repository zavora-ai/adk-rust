//! Structured error types for the Anthropic provider.
//!
//! Provides [`AnthropicApiError`] for API-level errors with full diagnostic
//! context (error type, message, status code, request ID) and
//! [`ConversionError`] for content mapping failures.

use adk_core::AdkError;

/// Structured Anthropic API error preserving all diagnostic context.
///
/// Captures the error type, message, HTTP status code, and optional request ID
/// returned by the Anthropic API, enabling precise debugging and support
/// escalation.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::AnthropicApiError;
///
/// let err = AnthropicApiError {
///     error_type: "rate_limit_error".to_string(),
///     message: "Too many requests".to_string(),
///     status_code: 429,
///     request_id: Some("req_abc123".to_string()),
/// };
///
/// assert!(err.to_string().contains("429"));
/// assert!(err.to_string().contains("rate_limit_error"));
/// assert!(err.to_string().contains("req_abc123"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicApiError {
    /// Error type from Anthropic (e.g., "invalid_request_error", "rate_limit_error").
    pub error_type: String,
    /// Human-readable error message.
    pub message: String,
    /// HTTP status code.
    pub status_code: u16,
    /// Request ID from the `request-id` response header.
    pub request_id: Option<String>,
}

impl std::fmt::Display for AnthropicApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Anthropic API error ({}): {} [type={}]",
            self.status_code, self.message, self.error_type
        )?;
        if let Some(ref rid) = self.request_id {
            write!(f, " [request_id={rid}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for AnthropicApiError {}

impl From<AnthropicApiError> for AdkError {
    fn from(e: AnthropicApiError) -> Self {
        AdkError::Model(e.to_string())
    }
}

/// Error type for content conversion failures.
///
/// Used when mapping ADK content types to Anthropic API types encounters
/// unsupported or invalid content.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::ConversionError;
///
/// let err = ConversionError::UnsupportedMimeType("audio/wav".to_string());
/// assert!(err.to_string().contains("audio/wav"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionError {
    /// The MIME type is not supported by the Anthropic API.
    UnsupportedMimeType(String),
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionError::UnsupportedMimeType(mime) => {
                write!(f, "unsupported MIME type for Anthropic API: {mime}")
            }
        }
    }
}

impl std::error::Error for ConversionError {}

impl From<ConversionError> for AdkError {
    fn from(e: ConversionError) -> Self {
        AdkError::Model(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_error_display_with_request_id() {
        let err = AnthropicApiError {
            error_type: "rate_limit_error".to_string(),
            message: "Too many requests".to_string(),
            status_code: 429,
            request_id: Some("req_abc123".to_string()),
        };
        let display = err.to_string();
        assert!(display.contains("429"));
        assert!(display.contains("rate_limit_error"));
        assert!(display.contains("Too many requests"));
        assert!(display.contains("req_abc123"));
    }

    #[test]
    fn test_api_error_display_without_request_id() {
        let err = AnthropicApiError {
            error_type: "invalid_request_error".to_string(),
            message: "Invalid model".to_string(),
            status_code: 400,
            request_id: None,
        };
        let display = err.to_string();
        assert!(display.contains("400"));
        assert!(display.contains("invalid_request_error"));
        assert!(display.contains("Invalid model"));
        assert!(!display.contains("request_id"));
    }

    #[test]
    fn test_api_error_into_adk_error() {
        let err = AnthropicApiError {
            error_type: "overloaded_error".to_string(),
            message: "Server overloaded".to_string(),
            status_code: 529,
            request_id: Some("req_xyz".to_string()),
        };
        let adk_err: AdkError = err.into();
        assert!(matches!(adk_err, AdkError::Model(_)));
        assert!(adk_err.to_string().contains("529"));
    }

    #[test]
    fn test_conversion_error_display() {
        let err = ConversionError::UnsupportedMimeType("audio/wav".to_string());
        assert!(err.to_string().contains("audio/wav"));
        assert!(err.to_string().contains("unsupported MIME type"));
    }

    #[test]
    fn test_conversion_error_into_adk_error() {
        let err = ConversionError::UnsupportedMimeType("video/mp4".to_string());
        let adk_err: AdkError = err.into();
        assert!(matches!(adk_err, AdkError::Model(_)));
        assert!(adk_err.to_string().contains("video/mp4"));
    }
}
