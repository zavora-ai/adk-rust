//! EnterpriseError — typed errors for the Enterprise API client.

use std::time::Duration;

/// Errors from the Enterprise API client.
///
/// Each variant is one HTTP status class of the service's error envelope
/// (`{"error": {"type", "message", "param"}}`), so a caller can match on the failure
/// rather than parse its message.
#[derive(Debug, thiserror::Error)]
pub enum EnterpriseError {
    /// 400 — invalid request parameters.
    #[error("invalid request: {message}")]
    InvalidRequest { message: String, param: Option<String> },

    /// 401 — missing or invalid API key.
    #[error("authentication failed: {message}")]
    Authentication { message: String },

    /// 403 — insufficient permissions.
    #[error("permission denied: {message}")]
    Permission { message: String },

    /// 404 — resource not found.
    #[error("not found: {message}")]
    NotFound { message: String },

    /// 409 — conflict (e.g., invalid state transition).
    #[error("conflict: {message}")]
    Conflict { message: String },

    /// 422 — validation error.
    #[error("validation error: {message}")]
    Validation { message: String },

    /// 429 — rate limited.
    #[error("rate limited: retry after {retry_after:?}")]
    RateLimit { message: String, retry_after: Option<Duration> },

    /// 500 — internal server error.
    #[error("internal error: {message}")]
    Internal { message: String },

    /// 503 — service unavailable.
    #[error("service unavailable: {message}")]
    Unavailable { message: String, retry_after: Option<Duration> },

    /// Network/connection error.
    #[error("connection error: {0}")]
    Connection(#[from] reqwest::Error),

    /// SSE stream error.
    #[error("stream error: {message}")]
    Stream { message: String },

    /// SSE stream timeout (no data within configured duration).
    #[error("stream timeout after {timeout_secs}s")]
    StreamTimeout { timeout_secs: u64 },

    /// JSON serialization/deserialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl EnterpriseError {
    /// Returns `true` if this error is retryable.
    ///
    /// Retryable errors include:
    /// - `RateLimit` (429)
    /// - `Internal` (500)
    /// - `Unavailable` (503)
    /// - `Connection` when it's a timeout or connection error (502/504 equivalent)
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimit { .. } | Self::Internal { .. } | Self::Unavailable { .. } => true,
            Self::Connection(e) => e.is_timeout() || e.is_connect(),
            _ => false,
        }
    }
}
