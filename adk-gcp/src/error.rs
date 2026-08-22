//! Consumer-branded error construction for Google Cloud REST clients.
//!
//! Every Vertex backend in the workspace stamps its own error identity —
//! component, machine-readable code, and human-readable subject — onto the
//! same transport failures. [`GcpErrorContext`] carries that identity so the
//! shared client and poller produce errors indistinguishable from the ones
//! each backend built for itself.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use reqwest::StatusCode;

/// The machine-readable error codes a consumer brands its errors with.
///
/// [`AdkError`] codes are `&'static str`, so each consuming crate declares
/// its table as a `const` — one literal per failure class, following the
/// workspace `<component>.<backend>.<failure>` convention.
///
/// # Example
///
/// ```rust
/// use adk_gcp::GcpErrorCodes;
///
/// const CODES: GcpErrorCodes = GcpErrorCodes {
///     invalid_input: "memory.vertex.invalid_input",
///     unauthorized: "memory.vertex.unauthorized",
///     forbidden: "memory.vertex.forbidden",
///     not_found: "memory.vertex.not_found",
///     rate_limited: "memory.vertex.rate_limited",
///     timeout: "memory.vertex.timeout",
///     unavailable: "memory.vertex.unavailable",
///     credentials_unavailable: "memory.vertex.credentials_unavailable",
///     invalid_response: "memory.vertex.invalid_response",
///     invalid_request: "memory.vertex.invalid_request",
///     upstream_error: "memory.vertex.upstream_error",
///     operation_failed: "memory.vertex.operation_failed",
/// };
/// ```
#[derive(Debug, Clone, Copy)]
pub struct GcpErrorCodes {
    /// Caller-side input rejected before any request is sent.
    pub invalid_input: &'static str,
    /// Missing or invalid credentials (401 and non-transient credential failures).
    pub unauthorized: &'static str,
    /// Valid credentials but insufficient permissions (403).
    pub forbidden: &'static str,
    /// Requested resource does not exist (404).
    pub not_found: &'static str,
    /// Upstream rate limit hit (429).
    pub rate_limited: &'static str,
    /// Request, credential acquisition, or operation deadline exceeded.
    pub timeout: &'static str,
    /// Transport failures and upstream 5xx availability errors.
    pub unavailable: &'static str,
    /// Transient credential acquisition failures.
    pub credentials_unavailable: &'static str,
    /// Malformed, oversized, or unparsable upstream responses.
    pub invalid_response: &'static str,
    /// Upstream rejected the request as invalid (400, 409, 422).
    pub invalid_request: &'static str,
    /// Any other non-success upstream status.
    pub upstream_error: &'static str,
    /// A long-running operation completed with an error result.
    pub operation_failed: &'static str,
}

/// Error identity for one consuming backend.
///
/// Bundles the [`ErrorComponent`], the code table, the human-readable
/// subject used in messages (e.g. `"vertex memory"`), and the provider tag.
///
/// # Example
///
/// ```rust
/// use adk_core::ErrorComponent;
/// use adk_gcp::{GcpErrorCodes, GcpErrorContext};
///
/// const CODES: GcpErrorCodes = GcpErrorCodes {
///     invalid_input: "memory.vertex.invalid_input",
///     unauthorized: "memory.vertex.unauthorized",
///     forbidden: "memory.vertex.forbidden",
///     not_found: "memory.vertex.not_found",
///     rate_limited: "memory.vertex.rate_limited",
///     timeout: "memory.vertex.timeout",
///     unavailable: "memory.vertex.unavailable",
///     credentials_unavailable: "memory.vertex.credentials_unavailable",
///     invalid_response: "memory.vertex.invalid_response",
///     invalid_request: "memory.vertex.invalid_request",
///     upstream_error: "memory.vertex.upstream_error",
///     operation_failed: "memory.vertex.operation_failed",
/// };
///
/// let errors = GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory");
/// let error = errors.invalid_input("reasoning engine ID must be numeric");
/// assert!(error.is_not_found() == false);
/// ```
#[derive(Debug, Clone)]
pub struct GcpErrorContext {
    component: ErrorComponent,
    codes: GcpErrorCodes,
    subject: String,
    provider: String,
    response_too_large_code: Option<&'static str>,
}

impl GcpErrorContext {
    /// Creates an error context with the default `vertex_ai` provider tag.
    pub fn new(
        component: ErrorComponent,
        codes: GcpErrorCodes,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            component,
            codes,
            subject: subject.into(),
            provider: "vertex_ai".to_string(),
            response_too_large_code: None,
        }
    }

    /// Overrides the provider tag stamped on every error.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Sets a dedicated code for oversized responses.
    ///
    /// Without the override, [`response_too_large`](Self::response_too_large)
    /// stamps the consumer's `invalid_response` code. Consumers with a
    /// dedicated size-limit code (e.g. `session.vertex.response_too_large`)
    /// set it here.
    #[must_use]
    pub fn with_response_too_large_code(mut self, code: &'static str) -> Self {
        self.response_too_large_code = Some(code);
        self
    }

    /// The human-readable subject used in error messages.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The component every error is attributed to.
    pub fn component(&self) -> ErrorComponent {
        self.component
    }

    /// The consumer's error code table.
    pub fn codes(&self) -> &GcpErrorCodes {
        &self.codes
    }

    /// Builds an error with an explicit category and code.
    pub fn error(
        &self,
        category: ErrorCategory,
        code: &'static str,
        message: impl Into<String>,
    ) -> AdkError {
        AdkError::new(self.component, category, code, message).with_provider(self.provider.clone())
    }

    /// Caller-side input rejected before any request is sent.
    pub fn invalid_input(&self, message: impl Into<String>) -> AdkError {
        self.error(ErrorCategory::InvalidInput, self.codes.invalid_input, message)
    }

    /// Missing or invalid credentials.
    pub fn unauthorized(&self, message: impl Into<String>) -> AdkError {
        self.error(ErrorCategory::Unauthorized, self.codes.unauthorized, message)
    }

    /// A deadline was exceeded.
    pub fn timeout(&self, message: impl Into<String>) -> AdkError {
        self.error(ErrorCategory::Timeout, self.codes.timeout, message)
    }

    /// The upstream service is temporarily unavailable.
    pub fn unavailable(&self, message: impl Into<String>) -> AdkError {
        self.error(ErrorCategory::Unavailable, self.codes.unavailable, message)
    }

    /// A malformed, oversized, or unparsable upstream response.
    pub fn invalid_response(&self, message: impl Into<String>) -> AdkError {
        self.error(ErrorCategory::Internal, self.codes.invalid_response, message)
    }

    /// Classifies a credential acquisition failure as transient or terminal.
    pub fn credentials_error(
        &self,
        error: &google_cloud_auth::errors::CredentialsError,
    ) -> AdkError {
        let message = format!(
            "failed to obtain google cloud auth headers: {}",
            truncate_for_error(&error.to_string()),
        );
        if error.is_transient() {
            self.error(ErrorCategory::Unavailable, self.codes.credentials_unavailable, message)
        } else {
            self.unauthorized(message)
        }
    }

    /// Classifies a request transport failure, distinguishing timeouts.
    pub fn transport_error(&self, error: reqwest::Error) -> AdkError {
        let timeout = error.is_timeout();
        let detail = truncate_for_error(&error.without_url().to_string());
        if timeout {
            return self.timeout(format!("{} HTTP request timed out: {detail}", self.subject));
        }
        self.unavailable(format!("failed to send {} request: {detail}", self.subject))
    }

    /// Maps a non-success HTTP status and response body to a categorized error.
    pub fn status_error(&self, status: StatusCode, body: &str) -> AdkError {
        let message = format!(
            "{} request failed with status {}: {}",
            self.subject,
            status.as_u16(),
            truncate_for_error(body),
        );
        let (category, code) = match status.as_u16() {
            400 | 409 | 422 => (ErrorCategory::InvalidInput, self.codes.invalid_request),
            401 => (ErrorCategory::Unauthorized, self.codes.unauthorized),
            403 => (ErrorCategory::Forbidden, self.codes.forbidden),
            404 => (ErrorCategory::NotFound, self.codes.not_found),
            408 | 504 => (ErrorCategory::Timeout, self.codes.timeout),
            429 => (ErrorCategory::RateLimited, self.codes.rate_limited),
            500 | 502 | 503 => (ErrorCategory::Unavailable, self.codes.unavailable),
            _ => (ErrorCategory::Internal, self.codes.upstream_error),
        };
        self.error(category, code, message).with_upstream_status(status.as_u16())
    }

    /// Maps a completed operation's gRPC status code to a categorized error.
    pub fn operation_error(
        &self,
        operation_kind: &str,
        operation_name: &str,
        code: i64,
        message: &str,
    ) -> AdkError {
        let operation_name = truncate_for_error(operation_name);
        let operation_message =
            if message.trim().is_empty() { "<no error message>" } else { message };
        let message = format!(
            "{} {operation_kind} operation '{operation_name}' failed with code {code}: {}",
            self.subject,
            truncate_for_error(operation_message),
        );
        let category = match code {
            1 => ErrorCategory::Cancelled,
            3 | 6 | 9 | 11 => ErrorCategory::InvalidInput,
            4 => ErrorCategory::Timeout,
            5 => ErrorCategory::NotFound,
            7 => ErrorCategory::Forbidden,
            8 => ErrorCategory::RateLimited,
            10 | 14 => ErrorCategory::Unavailable,
            12 => ErrorCategory::Unsupported,
            16 => ErrorCategory::Unauthorized,
            _ => ErrorCategory::Internal,
        };
        self.error(category, self.codes.operation_failed, message)
    }

    /// An oversized response, reporting the observed size against the limit.
    pub fn response_too_large(&self, context: &str, limit: usize, observed: u64) -> AdkError {
        self.error(
            ErrorCategory::Internal,
            self.response_too_large_code.unwrap_or(self.codes.invalid_response),
            format!(
                "{} {context} of at least {observed} bytes exceeds the {limit}-byte limit",
                self.subject,
            ),
        )
    }
}

/// Sanitizes and truncates untrusted text for inclusion in error messages.
///
/// Control characters are replaced with `U+FFFD` and the result is capped at
/// 512 bytes (with a `...` suffix when truncated), so upstream response
/// bodies can never flood logs or smuggle terminal escapes.
pub fn truncate_for_error(value: &str) -> String {
    const MAX_LEN: usize = 512;
    let mut sanitized = String::with_capacity(value.len().min(MAX_LEN));
    let mut truncated = false;
    for character in value.chars() {
        let character =
            if character.is_control() { char::REPLACEMENT_CHARACTER } else { character };
        if sanitized.len() + character.len_utf8() > MAX_LEN {
            truncated = true;
            break;
        }
        sanitized.push(character);
    }
    if truncated {
        sanitized.push_str("...");
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: GcpErrorCodes = GcpErrorCodes {
        invalid_input: "memory.vertex.invalid_input",
        unauthorized: "memory.vertex.unauthorized",
        forbidden: "memory.vertex.forbidden",
        not_found: "memory.vertex.not_found",
        rate_limited: "memory.vertex.rate_limited",
        timeout: "memory.vertex.timeout",
        unavailable: "memory.vertex.unavailable",
        credentials_unavailable: "memory.vertex.credentials_unavailable",
        invalid_response: "memory.vertex.invalid_response",
        invalid_request: "memory.vertex.invalid_request",
        upstream_error: "memory.vertex.upstream_error",
        operation_failed: "memory.vertex.operation_failed",
    };

    fn context() -> GcpErrorContext {
        GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory")
    }

    #[test]
    fn status_errors_map_to_the_backend_categories_and_codes() {
        let cases: &[(u16, ErrorCategory, &str)] = &[
            (400, ErrorCategory::InvalidInput, "memory.vertex.invalid_request"),
            (401, ErrorCategory::Unauthorized, "memory.vertex.unauthorized"),
            (403, ErrorCategory::Forbidden, "memory.vertex.forbidden"),
            (404, ErrorCategory::NotFound, "memory.vertex.not_found"),
            (408, ErrorCategory::Timeout, "memory.vertex.timeout"),
            (409, ErrorCategory::InvalidInput, "memory.vertex.invalid_request"),
            (422, ErrorCategory::InvalidInput, "memory.vertex.invalid_request"),
            (429, ErrorCategory::RateLimited, "memory.vertex.rate_limited"),
            (500, ErrorCategory::Unavailable, "memory.vertex.unavailable"),
            (502, ErrorCategory::Unavailable, "memory.vertex.unavailable"),
            (503, ErrorCategory::Unavailable, "memory.vertex.unavailable"),
            (504, ErrorCategory::Timeout, "memory.vertex.timeout"),
            (418, ErrorCategory::Internal, "memory.vertex.upstream_error"),
        ];
        for (status, category, code) in cases {
            let error =
                context().status_error(StatusCode::from_u16(*status).unwrap(), "upstream detail");
            assert_eq!(error.category, *category, "status {status}");
            assert_eq!(error.code, *code, "status {status}");
            assert_eq!(error.details.upstream_status_code, Some(*status));
            assert_eq!(error.details.provider.as_deref(), Some("vertex_ai"));
        }
    }

    #[test]
    fn operation_errors_map_grpc_codes_to_categories() {
        let cases: &[(i64, ErrorCategory)] = &[
            (1, ErrorCategory::Cancelled),
            (3, ErrorCategory::InvalidInput),
            (4, ErrorCategory::Timeout),
            (5, ErrorCategory::NotFound),
            (7, ErrorCategory::Forbidden),
            (8, ErrorCategory::RateLimited),
            (10, ErrorCategory::Unavailable),
            (12, ErrorCategory::Unsupported),
            (13, ErrorCategory::Internal),
            (14, ErrorCategory::Unavailable),
            (16, ErrorCategory::Unauthorized),
        ];
        for (code, category) in cases {
            let error = context().operation_error("generate", "projects/p/op", *code, "boom");
            assert_eq!(error.category, *category, "grpc code {code}");
            assert_eq!(error.code, "memory.vertex.operation_failed");
        }
    }

    #[test]
    fn operation_errors_substitute_a_placeholder_for_blank_messages() {
        let error = context().operation_error("generate", "projects/p/op", 13, "  ");
        assert!(error.message.contains("<no error message>"), "{}", error.message);
    }

    #[test]
    fn truncate_replaces_control_characters_and_caps_length() {
        assert_eq!(truncate_for_error("plain"), "plain");
        assert_eq!(truncate_for_error("a\x1b[31mb\n"), "a\u{fffd}[31mb\u{fffd}");
        let long = "x".repeat(1000);
        let truncated = truncate_for_error(&long);
        assert_eq!(truncated.len(), 515);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn provider_override_is_stamped_on_errors() {
        let error = context().with_provider("gcs").invalid_input("bad");
        assert_eq!(error.details.provider.as_deref(), Some("gcs"));
    }

    #[test]
    fn response_too_large_uses_invalid_response_unless_overridden() {
        let default = context().response_too_large("response body", 256, 512);
        assert_eq!(default.code, "memory.vertex.invalid_response");

        let dedicated = context()
            .with_response_too_large_code("memory.vertex.response_too_large")
            .response_too_large("response body", 256, 512);
        assert_eq!(dedicated.code, "memory.vertex.response_too_large");
        assert_eq!(dedicated.category, ErrorCategory::Internal);
        assert!(dedicated.message.contains("exceeds the 256-byte limit"));
    }
}
