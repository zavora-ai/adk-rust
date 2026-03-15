//! Error types for sandbox execution.

use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during sandbox execution.
///
/// All variants include actionable context to help callers diagnose issues.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::SandboxError;
/// use std::time::Duration;
///
/// let err = SandboxError::Timeout { timeout: Duration::from_secs(30) };
/// assert!(err.to_string().contains("30s"));
/// ```
#[derive(Debug, Clone, Error)]
pub enum SandboxError {
    /// Execution exceeded the configured timeout.
    #[error("execution timed out after {timeout:?}")]
    Timeout {
        /// The timeout duration that was exceeded.
        timeout: Duration,
    },

    /// Execution exceeded the configured memory limit (Wasm only).
    #[error("memory limit exceeded: {limit_mb} MB")]
    MemoryExceeded {
        /// The memory limit in megabytes that was exceeded.
        limit_mb: u32,
    },

    /// Execution failed due to an internal error (e.g., subprocess I/O failure).
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// The request is invalid (e.g., unsupported language for this backend).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The backend is not available (e.g., missing runtime or feature not enabled).
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
}

impl From<std::io::Error> for SandboxError {
    fn from(err: std::io::Error) -> Self {
        SandboxError::ExecutionFailed(format!("I/O error: {err}"))
    }
}
