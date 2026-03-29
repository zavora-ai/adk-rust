// Unified structured error envelope for all ADK-Rust operations.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// The subsystem that produced the error — the origin, not the boundary it surfaces through.
///
/// Choose the variant matching where the failure actually happened, not which trait
/// boundary returned it. For example:
/// - A code-execution timeout inside `python_code_tool.rs` → [`Code`](Self::Code)
/// - An auth denial inside middleware → [`Auth`](Self::Auth)
/// - A missing API key detected in model config → [`Model`](Self::Model) with `InvalidInput`
/// - A database write failure in session persistence → [`Session`](Self::Session)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorComponent {
    Agent,
    Model,
    Tool,
    Session,
    Artifact,
    Memory,
    Graph,
    Realtime,
    Code,
    Server,
    Auth,
    Guardrail,
    Eval,
    Deploy,
}

impl fmt::Display for ErrorComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Agent => "agent",
            Self::Model => "model",
            Self::Tool => "tool",
            Self::Session => "session",
            Self::Artifact => "artifact",
            Self::Memory => "memory",
            Self::Graph => "graph",
            Self::Realtime => "realtime",
            Self::Code => "code",
            Self::Server => "server",
            Self::Auth => "auth",
            Self::Guardrail => "guardrail",
            Self::Eval => "eval",
            Self::Deploy => "deploy",
        };
        f.write_str(s)
    }
}

/// The kind of failure independent of subsystem.
///
/// Choose the variant that best describes what went wrong:
/// - [`InvalidInput`](Self::InvalidInput) — caller provided bad data (config, request body, parameters)
/// - [`Unauthorized`](Self::Unauthorized) — missing or invalid credentials
/// - [`Forbidden`](Self::Forbidden) — valid credentials but insufficient permissions
/// - [`NotFound`](Self::NotFound) — requested resource does not exist
/// - [`RateLimited`](Self::RateLimited) — upstream rate limit hit (retryable by default)
/// - [`Timeout`](Self::Timeout) — operation exceeded time limit (retryable by default)
/// - [`Unavailable`](Self::Unavailable) — upstream service temporarily down (retryable by default)
/// - [`Cancelled`](Self::Cancelled) — operation was cancelled by caller or system
/// - [`Internal`](Self::Internal) — unexpected internal error (bugs, invariant violations)
/// - [`Unsupported`](Self::Unsupported) — requested feature or operation is not supported
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCategory {
    InvalidInput,
    Unauthorized,
    Forbidden,
    NotFound,
    RateLimited,
    Timeout,
    Unavailable,
    Cancelled,
    Internal,
    Unsupported,
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::InvalidInput => "invalid_input",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
            Self::Unsupported => "unsupported",
        };
        f.write_str(s)
    }
}

/// Structured retry guidance attached to every [`AdkError`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetryHint {
    pub should_retry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
}

impl RetryHint {
    /// Derive a default retry hint from the error category.
    pub fn for_category(category: ErrorCategory) -> Self {
        match category {
            ErrorCategory::RateLimited | ErrorCategory::Unavailable | ErrorCategory::Timeout => {
                Self { should_retry: true, ..Default::default() }
            }
            _ => Self::default(),
        }
    }

    /// Convert `retry_after_ms` to a [`Duration`].
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after_ms.map(Duration::from_millis)
    }

    /// Set the retry-after delay from a [`Duration`].
    pub fn with_retry_after(mut self, duration: Duration) -> Self {
        self.retry_after_ms = Some(duration.as_millis() as u64);
        self
    }
}

/// Optional structured metadata carried by an [`AdkError`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, Value>,
}

/// Unified structured error type for all ADK-Rust operations.
///
/// # Migration from enum syntax
///
/// Before (0.4.x enum):
/// ```rust,ignore
/// // Construction
/// Err(AdkError::Model("rate limited".into()))
/// // Matching
/// matches!(err, AdkError::Model(_))
/// ```
///
/// After (0.5.x struct):
/// ```rust
/// use adk_core::{AdkError, ErrorComponent, ErrorCategory};
///
/// // Structured construction
/// let err = AdkError::new(
///     ErrorComponent::Model,
///     ErrorCategory::RateLimited,
///     "model.openai.rate_limited",
///     "rate limited",
/// );
///
/// // Backward-compat construction (for migration)
/// let err = AdkError::model("rate limited");
///
/// // Checking
/// assert!(err.is_model());
/// assert!(err.is_retryable()); // reads retry.should_retry
/// ```
pub struct AdkError {
    pub component: ErrorComponent,
    pub category: ErrorCategory,
    pub code: &'static str,
    pub message: String,
    pub retry: RetryHint,
    pub details: Box<ErrorDetails>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Debug for AdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("AdkError");
        d.field("component", &self.component)
            .field("category", &self.category)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("retry", &self.retry)
            .field("details", &self.details);
        if let Some(src) = &self.source {
            d.field("source", &format_args!("{src}"));
        }
        d.finish()
    }
}

impl fmt::Display for AdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}: {}", self.component, self.category, self.message)
    }
}

impl std::error::Error for AdkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

const _: () = {
    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}
    fn _assertions() {
        _assert_send::<AdkError>();
        _assert_sync::<AdkError>();
    }
};

impl AdkError {
    pub fn new(
        component: ErrorComponent,
        category: ErrorCategory,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            component,
            category,
            code,
            message: message.into(),
            retry: RetryHint::for_category(category),
            details: Box::new(ErrorDetails::default()),
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn with_retry(mut self, retry: RetryHint) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_details(mut self, details: ErrorDetails) -> Self {
        self.details = Box::new(details);
        self
    }

    pub fn with_upstream_status(mut self, status_code: u16) -> Self {
        self.details.upstream_status_code = Some(status_code);
        self
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.details.request_id = Some(request_id.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.details.provider = Some(provider.into());
        self
    }
}

impl AdkError {
    pub fn not_found(
        component: ErrorComponent,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::new(component, ErrorCategory::NotFound, code, message)
    }

    pub fn rate_limited(
        component: ErrorComponent,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::new(component, ErrorCategory::RateLimited, code, message)
    }

    pub fn unauthorized(
        component: ErrorComponent,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::new(component, ErrorCategory::Unauthorized, code, message)
    }

    pub fn internal(
        component: ErrorComponent,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::new(component, ErrorCategory::Internal, code, message)
    }

    pub fn timeout(
        component: ErrorComponent,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::new(component, ErrorCategory::Timeout, code, message)
    }

    pub fn unavailable(
        component: ErrorComponent,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::new(component, ErrorCategory::Unavailable, code, message)
    }
}

impl AdkError {
    pub fn agent(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Agent, ErrorCategory::Internal, "agent.legacy", message)
    }

    pub fn model(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Model, ErrorCategory::Internal, "model.legacy", message)
    }

    pub fn tool(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Tool, ErrorCategory::Internal, "tool.legacy", message)
    }

    pub fn session(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Session, ErrorCategory::Internal, "session.legacy", message)
    }

    pub fn memory(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Memory, ErrorCategory::Internal, "memory.legacy", message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Server, ErrorCategory::InvalidInput, "config.legacy", message)
    }

    pub fn artifact(message: impl Into<String>) -> Self {
        Self::new(ErrorComponent::Artifact, ErrorCategory::Internal, "artifact.legacy", message)
    }
}

impl AdkError {
    pub fn is_agent(&self) -> bool {
        self.component == ErrorComponent::Agent
    }
    pub fn is_model(&self) -> bool {
        self.component == ErrorComponent::Model
    }
    pub fn is_tool(&self) -> bool {
        self.component == ErrorComponent::Tool
    }
    pub fn is_session(&self) -> bool {
        self.component == ErrorComponent::Session
    }
    pub fn is_artifact(&self) -> bool {
        self.component == ErrorComponent::Artifact
    }
    pub fn is_memory(&self) -> bool {
        self.component == ErrorComponent::Memory
    }
    pub fn is_config(&self) -> bool {
        self.code == "config.legacy"
    }
}

impl AdkError {
    pub fn is_retryable(&self) -> bool {
        self.retry.should_retry
    }
    pub fn is_not_found(&self) -> bool {
        self.category == ErrorCategory::NotFound
    }
    pub fn is_unauthorized(&self) -> bool {
        self.category == ErrorCategory::Unauthorized
    }
    pub fn is_rate_limited(&self) -> bool {
        self.category == ErrorCategory::RateLimited
    }
    pub fn is_timeout(&self) -> bool {
        self.category == ErrorCategory::Timeout
    }
}

impl AdkError {
    #[allow(unreachable_patterns)]
    pub fn http_status_code(&self) -> u16 {
        match self.category {
            ErrorCategory::InvalidInput => 400,
            ErrorCategory::Unauthorized => 401,
            ErrorCategory::Forbidden => 403,
            ErrorCategory::NotFound => 404,
            ErrorCategory::RateLimited => 429,
            ErrorCategory::Timeout => 408,
            ErrorCategory::Unavailable => 503,
            ErrorCategory::Cancelled => 499,
            ErrorCategory::Internal => 500,
            ErrorCategory::Unsupported => 501,
            _ => 500,
        }
    }
}

impl AdkError {
    pub fn to_problem_json(&self) -> Value {
        json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "component": self.component,
                "category": self.category,
                "requestId": self.details.request_id,
                "retryAfter": self.retry.retry_after_ms,
                "upstreamStatusCode": self.details.upstream_status_code,
            }
        })
    }
}

/// Convenience alias used throughout ADK crates.
pub type Result<T> = std::result::Result<T, AdkError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_sets_fields() {
        let err = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::RateLimited,
            "model.rate_limited",
            "too many requests",
        );
        assert_eq!(err.component, ErrorComponent::Model);
        assert_eq!(err.category, ErrorCategory::RateLimited);
        assert_eq!(err.code, "model.rate_limited");
        assert_eq!(err.message, "too many requests");
        assert!(err.retry.should_retry);
    }

    #[test]
    fn test_display_format() {
        let err = AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::NotFound,
            "session.not_found",
            "session xyz not found",
        );
        assert_eq!(err.to_string(), "session.not_found: session xyz not found");
    }

    #[test]
    fn test_convenience_not_found() {
        let err = AdkError::not_found(ErrorComponent::Session, "session.not_found", "gone");
        assert_eq!(err.category, ErrorCategory::NotFound);
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_convenience_rate_limited() {
        let err = AdkError::rate_limited(ErrorComponent::Model, "model.rate_limited", "slow down");
        assert!(err.is_retryable());
        assert!(err.is_rate_limited());
    }

    #[test]
    fn test_convenience_unauthorized() {
        let err = AdkError::unauthorized(ErrorComponent::Auth, "auth.unauthorized", "bad token");
        assert!(err.is_unauthorized());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_convenience_internal() {
        let err = AdkError::internal(ErrorComponent::Agent, "agent.internal", "oops");
        assert_eq!(err.category, ErrorCategory::Internal);
    }

    #[test]
    fn test_convenience_timeout() {
        let err = AdkError::timeout(ErrorComponent::Model, "model.timeout", "timed out");
        assert!(err.is_timeout());
        assert!(err.is_retryable());
    }

    #[test]
    fn test_convenience_unavailable() {
        let err = AdkError::unavailable(ErrorComponent::Model, "model.unavailable", "503");
        assert!(err.is_retryable());
    }

    #[test]
    fn test_backward_compat_agent() {
        let err = AdkError::agent("test error");
        assert!(err.is_agent());
        assert_eq!(err.code, "agent.legacy");
        assert_eq!(err.category, ErrorCategory::Internal);
        assert_eq!(err.to_string(), "agent.internal: test error");
    }

    #[test]
    fn test_backward_compat_model() {
        let err = AdkError::model("model fail");
        assert!(err.is_model());
        assert_eq!(err.code, "model.legacy");
    }

    #[test]
    fn test_backward_compat_tool() {
        let err = AdkError::tool("tool fail");
        assert!(err.is_tool());
        assert_eq!(err.code, "tool.legacy");
    }

    #[test]
    fn test_backward_compat_session() {
        let err = AdkError::session("session fail");
        assert!(err.is_session());
        assert_eq!(err.code, "session.legacy");
    }

    #[test]
    fn test_backward_compat_memory() {
        let err = AdkError::memory("memory fail");
        assert!(err.is_memory());
        assert_eq!(err.code, "memory.legacy");
    }

    #[test]
    fn test_backward_compat_artifact() {
        let err = AdkError::artifact("artifact fail");
        assert!(err.is_artifact());
        assert_eq!(err.code, "artifact.legacy");
    }

    #[test]
    fn test_backward_compat_config() {
        let err = AdkError::config("bad config");
        assert!(err.is_config());
        assert_eq!(err.code, "config.legacy");
        assert_eq!(err.component, ErrorComponent::Server);
        assert_eq!(err.category, ErrorCategory::InvalidInput);
    }

    #[test]
    fn test_backward_compat_codes_end_with_legacy() {
        let errors = [
            AdkError::agent("a"),
            AdkError::model("m"),
            AdkError::tool("t"),
            AdkError::session("s"),
            AdkError::memory("mem"),
            AdkError::config("c"),
            AdkError::artifact("art"),
        ];
        for err in &errors {
            assert!(err.code.ends_with(".legacy"), "code '{}' should end with .legacy", err.code);
        }
    }

    #[test]
    fn test_is_config_false_for_non_config() {
        assert!(!AdkError::agent("not config").is_config());
    }

    #[test]
    fn test_retryable_categories_default_true() {
        for cat in [ErrorCategory::RateLimited, ErrorCategory::Unavailable, ErrorCategory::Timeout]
        {
            let err = AdkError::new(ErrorComponent::Model, cat, "test", "msg");
            assert!(err.is_retryable(), "expected is_retryable() == true for {cat}");
        }
    }

    #[test]
    fn test_retryable_override_to_false() {
        let err =
            AdkError::new(ErrorComponent::Model, ErrorCategory::RateLimited, "m.rl", "overridden")
                .with_retry(RetryHint { should_retry: false, ..Default::default() });
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_non_retryable_categories_default_false() {
        for cat in [
            ErrorCategory::InvalidInput,
            ErrorCategory::Unauthorized,
            ErrorCategory::Forbidden,
            ErrorCategory::NotFound,
            ErrorCategory::Cancelled,
            ErrorCategory::Internal,
            ErrorCategory::Unsupported,
        ] {
            let err = AdkError::new(ErrorComponent::Model, cat, "test", "msg");
            assert!(!err.is_retryable(), "expected is_retryable() == false for {cat}");
        }
    }

    #[test]
    fn test_http_status_code_mapping() {
        let cases = [
            (ErrorCategory::InvalidInput, 400),
            (ErrorCategory::Unauthorized, 401),
            (ErrorCategory::Forbidden, 403),
            (ErrorCategory::NotFound, 404),
            (ErrorCategory::RateLimited, 429),
            (ErrorCategory::Timeout, 408),
            (ErrorCategory::Unavailable, 503),
            (ErrorCategory::Cancelled, 499),
            (ErrorCategory::Internal, 500),
            (ErrorCategory::Unsupported, 501),
        ];
        for (cat, expected) in &cases {
            let err = AdkError::new(ErrorComponent::Server, *cat, "test", "msg");
            assert_eq!(err.http_status_code(), *expected, "wrong status for {cat}");
        }
    }

    #[test]
    fn test_source_returns_some_when_set() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = AdkError::new(ErrorComponent::Session, ErrorCategory::NotFound, "s.f", "missing")
            .with_source(io_err);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_source_returns_none_when_not_set() {
        assert!(std::error::Error::source(&AdkError::agent("no source")).is_none());
    }

    #[test]
    fn test_retry_hint_for_category() {
        assert!(RetryHint::for_category(ErrorCategory::RateLimited).should_retry);
        assert!(RetryHint::for_category(ErrorCategory::Unavailable).should_retry);
        assert!(RetryHint::for_category(ErrorCategory::Timeout).should_retry);
        assert!(!RetryHint::for_category(ErrorCategory::Internal).should_retry);
        assert!(!RetryHint::for_category(ErrorCategory::NotFound).should_retry);
    }

    #[test]
    fn test_retry_hint_with_retry_after() {
        let hint = RetryHint::default().with_retry_after(Duration::from_secs(5));
        assert_eq!(hint.retry_after_ms, Some(5000));
        assert_eq!(hint.retry_after(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_to_problem_json() {
        let err = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::RateLimited,
            "model.rate_limited",
            "slow down",
        )
        .with_request_id("req-123")
        .with_upstream_status(429);
        let j = err.to_problem_json();
        let o = &j["error"];
        assert_eq!(o["code"], "model.rate_limited");
        assert_eq!(o["message"], "slow down");
        assert_eq!(o["component"], "model");
        assert_eq!(o["category"], "rate_limited");
        assert_eq!(o["requestId"], "req-123");
        assert_eq!(o["upstreamStatusCode"], 429);
    }

    #[test]
    fn test_to_problem_json_null_optionals() {
        let j = AdkError::agent("simple").to_problem_json();
        let o = &j["error"];
        assert!(o["requestId"].is_null());
        assert!(o["retryAfter"].is_null());
        assert!(o["upstreamStatusCode"].is_null());
    }

    #[test]
    fn test_builder_chaining() {
        let err = AdkError::new(ErrorComponent::Model, ErrorCategory::Unavailable, "m.u", "down")
            .with_provider("openai")
            .with_request_id("req-456")
            .with_upstream_status(503)
            .with_retry(RetryHint {
                should_retry: true,
                retry_after_ms: Some(1000),
                max_attempts: Some(3),
            });
        assert_eq!(err.details.provider.as_deref(), Some("openai"));
        assert_eq!(err.details.request_id.as_deref(), Some("req-456"));
        assert_eq!(err.details.upstream_status_code, Some(503));
        assert!(err.is_retryable());
        assert_eq!(err.retry.retry_after_ms, Some(1000));
        assert_eq!(err.retry.max_attempts, Some(3));
    }

    #[test]
    fn test_error_component_display() {
        assert_eq!(ErrorComponent::Agent.to_string(), "agent");
        assert_eq!(ErrorComponent::Model.to_string(), "model");
        assert_eq!(ErrorComponent::Graph.to_string(), "graph");
        assert_eq!(ErrorComponent::Realtime.to_string(), "realtime");
        assert_eq!(ErrorComponent::Deploy.to_string(), "deploy");
    }

    #[test]
    fn test_error_category_display() {
        assert_eq!(ErrorCategory::InvalidInput.to_string(), "invalid_input");
        assert_eq!(ErrorCategory::RateLimited.to_string(), "rate_limited");
        assert_eq!(ErrorCategory::NotFound.to_string(), "not_found");
        assert_eq!(ErrorCategory::Internal.to_string(), "internal");
    }

    #[test]
    #[allow(clippy::unnecessary_literal_unwrap)]
    fn test_result_type() {
        let ok: Result<i32> = Ok(42);
        assert_eq!(ok.unwrap(), 42);
        let err: Result<i32> = Err(AdkError::config("invalid"));
        assert!(err.is_err());
    }

    #[test]
    fn test_with_details() {
        let d = ErrorDetails {
            upstream_status_code: Some(502),
            request_id: Some("abc".into()),
            provider: Some("gemini".into()),
            metadata: HashMap::new(),
        };
        let err = AdkError::agent("test").with_details(d);
        assert_eq!(err.details.upstream_status_code, Some(502));
        assert_eq!(err.details.request_id.as_deref(), Some("abc"));
        assert_eq!(err.details.provider.as_deref(), Some("gemini"));
    }

    #[test]
    fn test_debug_impl() {
        let s = format!("{:?}", AdkError::agent("debug test"));
        assert!(s.contains("AdkError"));
        assert!(s.contains("agent.legacy"));
    }

    #[test]
    fn test_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AdkError>();
        assert_sync::<AdkError>();
    }
}
