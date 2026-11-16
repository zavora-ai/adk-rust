//! Bounded provider error codes separate from provider-controlled display text.
//!
//! Every provider records its code under [`PROVIDER_ERROR_CODE`] in
//! `AdkError::details.metadata`, chosen by one rule: a Google `ErrorInfo.reason`, then the
//! error object's string `code`, `type`, and `status`, in that order.

use adk_core::AdkError;
#[cfg(any(
    feature = "azure-ai",
    feature = "deepseek",
    feature = "gemini",
    feature = "openai"
))]
use serde_json::Value;

/// `ErrorDetails::metadata` key holding the provider's own error code.
pub(crate) const PROVIDER_ERROR_CODE: &str = "provider_error_code";

/// Bodies above this size are not parsed for a code.
#[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
pub(crate) const MAX_BODY_LEN: usize = 64 * 1024;

fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 128
        && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

pub(crate) fn with_code(mut error: AdkError, code: Option<&str>) -> AdkError {
    if let Some(code) = code.filter(|code| valid_code(code)) {
        error.details.metadata.insert(PROVIDER_ERROR_CODE.into(), serde_json::json!(code));
    }
    error
}

/// Records the first candidate that is a bounded code, for providers whose error object
/// is already deserialized.
#[cfg(any(feature = "openai", feature = "openrouter"))]
pub(crate) fn with_first_code<'a>(
    error: AdkError,
    candidates: impl IntoIterator<Item = Option<&'a str>>,
) -> AdkError {
    let code = candidates.into_iter().flatten().find(|code| valid_code(code));
    with_code(error, code)
}

/// Records the first bounded code from a JSON error body, see [`code_from_body`].
#[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "openai"))]
pub(crate) fn from_body(error: AdkError, body: &str) -> AdkError {
    let code = code_from_body(body);
    with_code(error, code.as_deref())
}

/// Extracts the provider code from a JSON body shaped `{"error": {...}}` or, as Gemini's
/// OpenAI-compatible endpoint sends it, `[{"error": {...}}]`.
#[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
pub(crate) fn code_from_body(body: &str) -> Option<String> {
    if body.len() > MAX_BODY_LEN {
        return None;
    }
    let body: Value = serde_json::from_str(body).ok()?;
    let error = body.get("error").or_else(|| body.get(0).and_then(|item| item.get("error")))?;
    let reasons = error
        .get("details")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|detail| detail.get("@type").and_then(Value::as_str).is_some_and(is_error_info))
        .filter_map(|detail| detail.get("reason").and_then(Value::as_str));
    let fields = ["code", "type", "status"].into_iter().filter_map(|key| error.get(key)?.as_str());
    reasons.chain(fields).find(|code| valid_code(code)).map(str::to_owned)
}

/// Provider error codes and types for an account whose quota, credit balance or spend
/// limit is used up: OpenAI's `insufficient_quota` type with its `credit_balance_exhausted`,
/// `billing_hard_limit_reached` and spend-limit codes, and Anthropic's `billing_error`.
#[cfg(any(
    feature = "anthropic",
    feature = "azure-ai",
    feature = "deepseek",
    feature = "groq",
    feature = "muse",
    feature = "ollama",
    feature = "openai",
    feature = "openrouter"
))]
const QUOTA_EXHAUSTED_CODES: &[&str] = &[
    "billing_error",
    "billing_hard_limit_reached",
    "credit_balance_exhausted",
    "insufficient_quota",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
];

/// Whether a provider error code or type says the account's quota, credit balance or
/// spend limit is used up.
#[cfg(any(
    feature = "anthropic",
    feature = "azure-ai",
    feature = "deepseek",
    feature = "groq",
    feature = "muse",
    feature = "ollama",
    feature = "openai",
    feature = "openrouter"
))]
pub(crate) fn is_quota_exhausted(code: &str) -> bool {
    QUOTA_EXHAUSTED_CODES.contains(&code)
}

/// Whether a JSON error body's `code` or `type` says the account is out of quota or credit.
///
/// OpenAI sends both, for example `"type": "insufficient_quota"` with
/// `"code": "credit_balance_exhausted"`, so neither is preferred.
#[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "openai"))]
pub(crate) fn body_reports_exhausted_quota(body: &str) -> bool {
    if body.len() > MAX_BODY_LEN {
        return false;
    }
    let Ok(body) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let Some(error) = body.get("error").or_else(|| body.get(0).and_then(|item| item.get("error")))
    else {
        return false;
    };
    ["code", "type"].into_iter().filter_map(|key| error.get(key)?.as_str()).any(is_quota_exhausted)
}

/// Reports `error` as an account out of quota or credit when `reported` says so or the
/// provider answered HTTP 402 Payment Required.
///
/// The error becomes [`ErrorCategory::ResourceExhausted`](adk_core::ErrorCategory) and is
/// not retried: a retry fails the same way until the account has credit again.
#[cfg(any(
    feature = "anthropic",
    feature = "azure-ai",
    feature = "deepseek",
    feature = "groq",
    feature = "muse",
    feature = "ollama",
    feature = "openai",
    feature = "openrouter"
))]
pub(crate) fn classify_quota(mut error: AdkError, reported: bool) -> AdkError {
    if !reported && error.details.upstream_status_code != Some(402) {
        return error;
    }
    error.category = adk_core::ErrorCategory::ResourceExhausted;
    error.retry = adk_core::RetryHint::for_category(error.category);
    error.message = format!(
        "{}. The account's quota, credit balance or spend limit is used up: add credit or \
         raise the limit with the provider; retrying before then fails the same way",
        error.message.trim_end_matches('.')
    );
    error
}

/// Matches the `google.rpc.ErrorInfo` detail under any type-URL prefix.
#[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
pub(crate) fn is_error_info(type_url: &str) -> bool {
    type_url.ends_with("google.rpc.ErrorInfo")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn error() -> AdkError {
        AdkError::model("provider failure")
    }

    fn code(error: &AdkError) -> Option<&str> {
        error.details.metadata.get(PROVIDER_ERROR_CODE).and_then(Value::as_str)
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
    fn body_code(body: Value) -> Option<String> {
        code_from_body(&body.to_string())
    }

    #[test]
    fn bounds_typed_codes() {
        let oversized = "x".repeat(129);
        for value in ["", "bad code", "line\nbreak", "é", oversized.as_str()] {
            assert_eq!(code(&with_code(error(), Some(value))), None, "{value:?}");
        }
        let boundary = "x".repeat(128);
        assert_eq!(code(&with_code(error(), Some(&boundary))), Some(boundary.as_str()));
        assert_eq!(
            code(&with_code(error(), Some("RESOURCE_EXHAUSTED"))),
            Some("RESOURCE_EXHAUSTED")
        );
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
    #[test]
    fn parses_only_bounded_error_objects() {
        assert_eq!(
            code_from_body(r#"{"error":{"code":"quota.exhausted"}}"#).as_deref(),
            Some("quota.exhausted")
        );
        assert_eq!(
            code_from_body(r#"{"error":{"status":"RESOURCE_EXHAUSTED"}}"#).as_deref(),
            Some("RESOURCE_EXHAUSTED")
        );
        assert_eq!(
            code_from_body(r#"{"error":{"code":12,"status":"RESOURCE_EXHAUSTED"}}"#).as_deref(),
            Some("RESOURCE_EXHAUSTED")
        );
        for body in [
            r#"{"error":{"code":"bad code"}}"#,
            r#"{"error":{"code":12}}"#,
            r#"{"message":"error.code=secret"}"#,
            r#"prefix {"error":{"code":"secret"}}"#,
            r#"{"error":{"code":"secret"}} trailing"#,
            r#"[{"message":"no error object"}]"#,
        ] {
            assert_eq!(code_from_body(body), None, "{body}");
        }
        let oversized_code = format!(r#"{{"error":{{"code":"{}"}}}}"#, "x".repeat(129));
        assert_eq!(code_from_body(&oversized_code), None);
        let oversized_body =
            format!(r#"{{"error":{{"code":"valid","message":"{}"}}}}"#, "x".repeat(64 * 1024));
        assert_eq!(code_from_body(&oversized_body), None);
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
    #[test]
    fn null_code_falls_back_to_type() {
        assert_eq!(
            body_code(serde_json::json!({"error": {
                "message": "The server had an error",
                "type": "server_error",
                "param": null,
                "code": null
            }}))
            .as_deref(),
            Some("server_error")
        );
        assert_eq!(
            body_code(serde_json::json!({"error": {
                "code": "rate_limit_exceeded",
                "type": "requests"
            }}))
            .as_deref(),
            Some("rate_limit_exceeded")
        );
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
    #[test]
    fn array_wrapped_body_reads_first_error() {
        assert_eq!(
            body_code(serde_json::json!([{"error": {
                "code": 400,
                "message": "Invalid JSON payload",
                "status": "INVALID_ARGUMENT"
            }}]))
            .as_deref(),
            Some("INVALID_ARGUMENT")
        );
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "gemini", feature = "openai"))]
    #[test]
    fn error_info_reason_takes_precedence_over_status() {
        let body = serde_json::json!({"error": {
            "code": 429,
            "status": "RESOURCE_EXHAUSTED",
            "details": [
                {"@type": "type.googleapis.com/google.rpc.Help", "reason": "IGNORED"},
                {"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "bad reason"},
                {"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "RATE_LIMIT_EXCEEDED"}
            ]
        }});
        assert_eq!(body_code(body).as_deref(), Some("RATE_LIMIT_EXCEEDED"));
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "openai"))]
    #[test]
    fn reads_an_exhausted_quota_from_the_code_or_the_type() {
        for body in [
            // OpenAI, live on 2026-11-16 for an account with no credit.
            r#"{"error":{"message":"You have no credits remaining.","type":"insufficient_quota","param":null,"code":"credit_balance_exhausted"}}"#,
            r#"{"error":{"type":"insufficient_quota","code":null}}"#,
            r#"{"error":{"type":"requests","code":"project_spend_limit_exceeded"}}"#,
            r#"[{"error":{"code":"insufficient_quota"}}]"#,
        ] {
            assert!(body_reports_exhausted_quota(body), "{body}");
        }
        for body in [
            r#"{"error":{"type":"requests","code":"rate_limit_exceeded"}}"#,
            r#"{"error":{"message":"insufficient_quota"}}"#,
            r#"{"code":"insufficient_quota"}"#,
            "insufficient_quota",
        ] {
            assert!(!body_reports_exhausted_quota(body), "{body}");
        }
    }

    #[cfg(any(
        feature = "anthropic",
        feature = "azure-ai",
        feature = "deepseek",
        feature = "groq",
        feature = "muse",
        feature = "ollama",
        feature = "openai",
        feature = "openrouter"
    ))]
    #[test]
    fn classifies_an_exhausted_quota_as_not_retryable() {
        use adk_core::{ErrorCategory, ErrorComponent};

        let rate_limited = || {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::RateLimited,
                "model.test.api_error",
                "test API error (HTTP 429): You have no credits remaining.",
            )
            .with_upstream_status(429)
        };
        let unchanged = classify_quota(rate_limited(), false);
        assert_eq!(unchanged.category, ErrorCategory::RateLimited);
        assert!(unchanged.is_retryable());

        let exhausted = classify_quota(rate_limited(), true);
        assert_eq!(exhausted.category, ErrorCategory::ResourceExhausted);
        assert!(!exhausted.is_retryable());
        assert_eq!(
            exhausted.message,
            "test API error (HTTP 429): You have no credits remaining. The account's quota, \
             credit balance or spend limit is used up: add credit or raise the limit with the \
             provider; retrying before then fails the same way"
        );

        let payment_required = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.test.api_error",
            "Insufficient Balance",
        )
        .with_upstream_status(402);
        assert_eq!(
            classify_quota(payment_required, false).category,
            ErrorCategory::ResourceExhausted
        );
    }

    #[cfg(any(feature = "azure-ai", feature = "deepseek", feature = "openai"))]
    #[test]
    fn from_body_records_the_code() {
        assert_eq!(
            code(&from_body(error(), r#"{"error":{"code":"quota.exhausted"}}"#)),
            Some("quota.exhausted")
        );
        assert_eq!(code(&from_body(error(), "not json")), None);
    }
}
