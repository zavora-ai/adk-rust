//! The message an error response carries, under [`SecurityConfig::expose_error_details`].
//!
//! REST Problem JSON, the run streams' error events, and the governance endpoints' `503`
//! status all take their message from here, so one rule decides what a client sees.
//!
//! [`SecurityConfig::expose_error_details`]: crate::SecurityConfig::expose_error_details

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use serde_json::Value;

/// Whether `error`'s message is one the server wrote about the request, which a client sees
/// while error details are hidden.
///
/// All three must hold:
///
/// - the category rejects the request: `invalid_input`, `unauthorized`, `forbidden`,
///   `not_found`, `unsupported`, `conflict`, or `resource_exhausted`;
/// - the component handles the request inside the server: `server`, `auth`, `session`,
///   `agent`, or `guardrail`;
/// - the error carries no upstream status code and no provider.
///
/// Every other message can quote a model provider, a tool's service, or a store driver.
fn describes_the_request(error: &AdkError) -> bool {
    let rejects_the_request = match error.category {
        ErrorCategory::InvalidInput
        | ErrorCategory::Unauthorized
        | ErrorCategory::Forbidden
        | ErrorCategory::NotFound
        | ErrorCategory::Unsupported
        | ErrorCategory::Conflict
        | ErrorCategory::ResourceExhausted => true,
        ErrorCategory::RateLimited
        | ErrorCategory::Timeout
        | ErrorCategory::Unavailable
        | ErrorCategory::Cancelled
        | ErrorCategory::Internal => false,
    };
    let handles_the_request = match error.component {
        ErrorComponent::Server
        | ErrorComponent::Auth
        | ErrorComponent::Session
        | ErrorComponent::Agent
        | ErrorComponent::Guardrail => true,
        ErrorComponent::Model
        | ErrorComponent::Tool
        | ErrorComponent::Artifact
        | ErrorComponent::Memory
        | ErrorComponent::Graph
        | ErrorComponent::Realtime
        | ErrorComponent::Code
        | ErrorComponent::Eval
        | ErrorComponent::Deploy => false,
    };
    let quotes_no_upstream =
        error.details.upstream_status_code.is_none() && error.details.provider.is_none();
    rejects_the_request && handles_the_request && quotes_no_upstream
}

/// The message a response carries in place of a withheld one of `category`.
fn generic_message(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::InvalidInput => "The request is invalid",
        ErrorCategory::Unauthorized => "Authentication failed",
        ErrorCategory::Forbidden => "The request is not permitted",
        ErrorCategory::NotFound => "The requested resource was not found",
        ErrorCategory::RateLimited => "Too many requests; retry later",
        ErrorCategory::Timeout => "The operation timed out",
        ErrorCategory::Unavailable => "A service the server depends on is unavailable; retry later",
        ErrorCategory::Cancelled => "The operation was cancelled",
        ErrorCategory::Internal => "Internal server error",
        ErrorCategory::Unsupported => "The operation is not supported",
        ErrorCategory::ResourceExhausted => "A resource budget is used up",
        ErrorCategory::Conflict => "The request conflicts with the current state",
    }
}

/// The message sent in place of `error`'s own, or `None` when its own is sent.
///
/// A withheld error is logged in full. The log line is written inside the request's
/// `http.request` span, whose `request.id` is the `x-request-id` the response carries.
fn withheld(error: &AdkError, expose_details: bool) -> Option<&'static str> {
    if expose_details || describes_the_request(error) {
        return None;
    }
    if error.http_status_code() >= 500 {
        tracing::error!(
            error = %error.diagnostics(),
            error.message = %error.message,
            "error details withheld from the response"
        );
    } else {
        tracing::warn!(
            error = %error.diagnostics(),
            error.message = %error.message,
            "error details withheld from the response"
        );
    }
    Some(generic_message(error.category))
}

/// `error` as Problem JSON ([`AdkError::to_problem_json`]), its message replaced by the
/// generic message for its category unless `expose_details` is set or the server wrote it.
///
/// `code`, `component`, `category`, `requestId`, `retryAfter`, and `upstreamStatusCode` are
/// always kept.
pub(crate) fn problem_json(error: &AdkError, expose_details: bool) -> Value {
    let mut body = error.to_problem_json();
    if let Some(message) = withheld(error, expose_details) {
        body["error"]["message"] = Value::from(message);
    }
    body
}

/// `error` as `{component}.{category}: {message}`, the message withheld as for
/// [`problem_json`].
pub(crate) fn display(error: &AdkError, expose_details: bool) -> String {
    match withheld(error, expose_details) {
        Some(message) => format!("{}.{}: {message}", error.component, error.category),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DRIVER_TEXT: &str =
        "error communicating with database: connect to 10.0.4.12:5432 refused (role adk_ops)";

    fn error(component: ErrorComponent, category: ErrorCategory, message: &str) -> AdkError {
        AdkError::new(component, category, "test.code", message)
    }

    #[test]
    fn messages_the_server_writes_about_the_request_are_kept() {
        for kept in [
            error(ErrorComponent::Server, ErrorCategory::InvalidInput, "unknown app 'x'"),
            error(ErrorComponent::Auth, ErrorCategory::Unauthorized, "missing authorization"),
            error(ErrorComponent::Auth, ErrorCategory::Forbidden, "acting for another user"),
            error(ErrorComponent::Session, ErrorCategory::NotFound, "session s-1 not found"),
            error(ErrorComponent::Session, ErrorCategory::Conflict, "session s-1 exists"),
            error(ErrorComponent::Agent, ErrorCategory::Forbidden, "execution is frozen"),
            error(ErrorComponent::Agent, ErrorCategory::ResourceExhausted, "tool calls used up"),
            error(ErrorComponent::Agent, ErrorCategory::Unsupported, "no resume here"),
            error(ErrorComponent::Guardrail, ErrorCategory::InvalidInput, "input blocked"),
        ] {
            assert_eq!(display(&kept, false), kept.to_string());
            assert_eq!(problem_json(&kept, false), kept.to_problem_json());
        }
    }

    #[test]
    fn failures_and_upstream_messages_are_withheld() {
        let cases = [
            (
                error(ErrorComponent::Session, ErrorCategory::Unavailable, DRIVER_TEXT),
                "session.unavailable: A service the server depends on is unavailable; retry later",
            ),
            (
                error(ErrorComponent::Agent, ErrorCategory::Internal, DRIVER_TEXT),
                "agent.internal: Internal server error",
            ),
            (
                error(ErrorComponent::Server, ErrorCategory::Timeout, DRIVER_TEXT),
                "server.timeout: The operation timed out",
            ),
            (
                error(ErrorComponent::Model, ErrorCategory::NotFound, DRIVER_TEXT),
                "model.not_found: The requested resource was not found",
            ),
            (
                error(ErrorComponent::Tool, ErrorCategory::InvalidInput, DRIVER_TEXT),
                "tool.invalid_input: The request is invalid",
            ),
            (
                error(ErrorComponent::Server, ErrorCategory::Unauthorized, DRIVER_TEXT)
                    .with_provider("anthropic"),
                "server.unauthorized: Authentication failed",
            ),
            (
                error(ErrorComponent::Agent, ErrorCategory::InvalidInput, DRIVER_TEXT)
                    .with_upstream_status(400),
                "agent.invalid_input: The request is invalid",
            ),
        ];
        for (withheld, expected) in cases {
            assert_eq!(display(&withheld, false), expected);
            assert_eq!(display(&withheld, true), withheld.to_string());
        }
    }

    #[test]
    fn a_withheld_problem_keeps_everything_but_the_message() {
        let mut upstream = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::RateLimited,
            "model.anthropic.api_error",
            "Anthropic API error (429): quota for org-7731 exceeded",
        )
        .with_upstream_status(429)
        .with_provider("anthropic")
        .with_request_id("req_011");
        upstream.retry.retry_after_ms = Some(5000);

        assert_eq!(
            problem_json(&upstream, false),
            json!({
                "error": {
                    "code": "model.anthropic.api_error",
                    "message": "Too many requests; retry later",
                    "component": "model",
                    "category": "rate_limited",
                    "requestId": "req_011",
                    "retryAfter": 5000,
                    "upstreamStatusCode": 429,
                }
            })
        );
        assert_eq!(problem_json(&upstream, true), upstream.to_problem_json());
    }
}
