//! Errors for model streams that end before their terminal event or report a failure
//! mid-stream.
//!
//! No message quotes a raw chunk. A provider's own error message is cut to
//! [`adk_core::UPSTREAM_EXCERPT_BYTES`], and a chunk that cannot be decoded is described only by the
//! decoder's error class and position, because chunks carry generated text and tool
//! arguments.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use serde_json::Value;

/// The stream ended before the provider's terminal event, so the answer or a tool call may
/// be truncated. Retryable: the request can be sent again.
pub(crate) fn incomplete(provider: &str, code: &'static str, unfinished: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        code,
        format!("{provider} stream ended before {unfinished}"),
    )
    .with_provider(provider)
}

/// The connection failed, or sent nothing for the idle timeout, while the stream was being
/// read. Retryable.
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "ollama",
    feature = "deepseek",
    feature = "azure-ai"
))]
pub(crate) fn interrupted(provider: &str, code: &'static str, error: &reqwest::Error) -> AdkError {
    if error.is_timeout() {
        return AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Timeout,
            code,
            format!("{provider} stream sent no data within the idle timeout: {error}"),
        )
        .with_provider(provider);
    }
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        code,
        format!("{provider} stream was interrupted: {error}"),
    )
    .with_provider(provider)
}

/// A stream chunk that is not valid JSON or does not match the provider's chunk schema.
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "ollama",
    feature = "deepseek",
    feature = "azure-ai"
))]
pub(crate) fn undecodable(
    provider: &str,
    code: &'static str,
    error: &serde_json::Error,
) -> AdkError {
    // Data errors can quote the offending value, so only the class and position are kept.
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Internal,
        code,
        format!(
            "{provider} sent a stream chunk that could not be decoded ({})",
            adk_core::json_error_summary(error)
        ),
    )
    .with_provider(provider)
}

/// An error the provider reported inside the stream, as an OpenAI-shaped object
/// (`{"message", "type", "code"}`), a bare string (Ollama), or `null` for a chunk that
/// only finished with `finish_reason: "error"`.
///
/// A numeric `code` or `status` in the HTTP error range sets the category and the
/// upstream status; otherwise the string code or type does, and an unrecognized failure
/// is `Unavailable`, since a generation that fails after it started is a server-side
/// failure.
pub(crate) fn reported(provider: &str, code: &'static str, error: &Value) -> AdkError {
    let field = |key: &str| error.get(key);
    let status =
        ["code", "status"].into_iter().filter_map(|key| field(key)?.as_u64()).find_map(|status| {
            u16::try_from(status).ok().filter(|status| (400..=599).contains(status))
        });
    let provider_code = field("code").and_then(Value::as_str);
    let kind = field("type").and_then(Value::as_str);
    let category = status.map(crate::retry::category_for_status_code).unwrap_or_else(|| {
        match provider_code.or(kind) {
            Some("rate_limit_exceeded" | "rate_limit_error" | "too_many_requests") => {
                ErrorCategory::RateLimited
            }
            Some("timeout" | "request_timeout") => ErrorCategory::Timeout,
            Some("invalid_request_error" | "context_length_exceeded") => {
                ErrorCategory::InvalidInput
            }
            _ => ErrorCategory::Unavailable,
        }
    });
    let message = match error {
        Value::String(message) => Some(message.as_str()),
        _ => field("message").and_then(Value::as_str),
    }
    .map(adk_core::upstream_excerpt)
    .filter(|message| !message.is_empty());
    let message = match message {
        Some(message) => format!("{provider} reported an error during generation: {message}"),
        None => format!("{provider} reported an error during generation"),
    };
    let mut mapped =
        AdkError::new(ErrorComponent::Model, category, code, message).with_provider(provider);
    if let Some(status) = status {
        mapped = mapped.with_upstream_status(status);
    }
    crate::error_metadata::with_code(mapped, provider_code.or(kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CODE: &str = "model.test.stream_error";

    #[test]
    fn reported_classifies_openai_shaped_errors() {
        let error = reported(
            "openai",
            CODE,
            &json!({"message": "The server had an error.", "type": "server_error", "code": null}),
        );
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert!(error.is_retryable());
        assert_eq!(error.code, CODE);
        assert_eq!(
            error.message,
            "openai reported an error during generation: The server had an error."
        );
        assert_eq!(error.details.metadata["provider_error_code"], "server_error");

        let limited = reported("openai", CODE, &json!({"code": "rate_limit_exceeded"}));
        assert_eq!(limited.category, ErrorCategory::RateLimited);

        let invalid = reported("openai", CODE, &json!({"type": "invalid_request_error"}));
        assert_eq!(invalid.category, ErrorCategory::InvalidInput);
        assert!(!invalid.is_retryable());
    }

    #[test]
    fn reported_uses_numeric_status_codes() {
        let error = reported("openrouter", CODE, &json!({"code": 429, "message": "slow down"}));
        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert_eq!(error.details.upstream_status_code, Some(429));
        let out_of_range = reported("provider", CODE, &json!({"code": 12}));
        assert_eq!(out_of_range.category, ErrorCategory::Unavailable);
        assert_eq!(out_of_range.details.upstream_status_code, None);
    }

    #[test]
    fn reported_accepts_bare_strings_and_null() {
        let error = reported("ollama", CODE, &json!("model runner stopped"));
        assert_eq!(
            error.message,
            "ollama reported an error during generation: model runner stopped"
        );
        assert_eq!(error.category, ErrorCategory::Unavailable);
        let bare = reported("groq", CODE, &Value::Null);
        assert_eq!(bare.message, "groq reported an error during generation");
    }

    #[test]
    fn reported_bounds_the_provider_message() {
        let long = "y".repeat(adk_core::UPSTREAM_EXCERPT_BYTES * 4);
        let error = reported("openai", CODE, &json!({"message": long}));
        assert_eq!(
            error.message.len(),
            "openai reported an error during generation: ".len() + adk_core::UPSTREAM_EXCERPT_BYTES
        );
    }

    #[test]
    fn incomplete_is_retryable() {
        let error = incomplete("groq", "model.groq.incomplete_stream", "the response finished");
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert!(error.is_retryable());
        assert_eq!(error.message, "groq stream ended before the response finished");
    }

    #[cfg(any(feature = "openai", feature = "groq", feature = "ollama"))]
    #[test]
    fn undecodable_never_quotes_the_chunk() {
        let parse = serde_json::from_str::<Value>(r#"{"secret": "canary"#).unwrap_err();
        let error = undecodable("groq", "model.groq.invalid_chunk", &parse);
        assert!(!error.message.contains("canary"), "{}", error.message);
        assert_eq!(error.category, ErrorCategory::Internal);

        #[derive(Debug, serde::Deserialize)]
        struct Typed {
            #[serde(rename = "count")]
            _count: u32,
        }
        let schema = serde_json::from_str::<Typed>(r#"{"count": "canary"}"#).unwrap_err();
        let error = undecodable("groq", "model.groq.invalid_chunk", &schema);
        assert!(!error.message.contains("canary"), "{}", error.message);
        assert!(error.message.contains("schema error"), "{}", error.message);
    }
}
