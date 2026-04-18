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

    /// The sandbox enforcer failed to apply the profile.
    #[error("enforcer '{enforcer}' failed: {message}")]
    EnforcerFailed {
        /// The enforcer name (e.g., "seatbelt", "bubblewrap", "appcontainer").
        enforcer: String,
        /// A descriptive message explaining what failed.
        message: String,
    },

    /// The sandbox enforcer is not available on this system.
    #[error("enforcer '{enforcer}' unavailable: {message}")]
    EnforcerUnavailable {
        /// The enforcer name.
        enforcer: String,
        /// A message explaining why the enforcer is not functional.
        message: String,
    },

    /// A policy path or resource could not be resolved.
    #[error("policy violation: {0}")]
    PolicyViolation(String),
}

impl From<std::io::Error> for SandboxError {
    fn from(err: std::io::Error) -> Self {
        SandboxError::ExecutionFailed(format!("I/O error: {err}"))
    }
}

impl From<SandboxError> for adk_core::AdkError {
    fn from(err: SandboxError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            SandboxError::Timeout { .. } => (ErrorCategory::Timeout, "code.sandbox_timeout"),
            SandboxError::MemoryExceeded { .. } => (ErrorCategory::Internal, "code.sandbox_memory"),
            SandboxError::ExecutionFailed(_) => (ErrorCategory::Internal, "code.sandbox_execution"),
            SandboxError::InvalidRequest(_) => {
                (ErrorCategory::InvalidInput, "code.sandbox_invalid_request")
            }
            SandboxError::BackendUnavailable(_) => {
                (ErrorCategory::Unavailable, "code.sandbox_unavailable")
            }
            SandboxError::EnforcerFailed { .. } => {
                (ErrorCategory::Internal, "code.sandbox_enforcer_failed")
            }
            SandboxError::EnforcerUnavailable { .. } => {
                (ErrorCategory::Unavailable, "code.sandbox_enforcer_unavailable")
            }
            SandboxError::PolicyViolation(_) => {
                (ErrorCategory::InvalidInput, "code.sandbox_policy_violation")
            }
        };
        adk_core::AdkError::new(ErrorComponent::Code, category, code, err.to_string())
            .with_source(err)
    }
}
