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
// Feature-gated variants (`workspace`) mean downstream crates cannot match exhaustively.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
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

    // ── Workspace lifecycle variants (behind `workspace` feature) ──────────
    /// Workspace provisioning failed.
    #[cfg(feature = "workspace")]
    #[error("provisioning failed for '{resource}': {reason}. {suggestion}")]
    ProvisionFailed {
        /// The resource that failed to provision (e.g., manifest entry path).
        resource: String,
        /// A description of why provisioning failed.
        reason: String,
        /// An actionable suggestion for resolution.
        suggestion: String,
    },

    /// Referenced session does not exist or has been stopped.
    #[cfg(feature = "workspace")]
    #[error("session '{handle}' not found. It may have been stopped or expired.")]
    SessionNotFound {
        /// The session handle that was not found.
        handle: String,
    },

    /// Referenced snapshot does not exist.
    #[cfg(feature = "workspace")]
    #[error("snapshot '{id}' not found. It may have been deleted or expired.")]
    SnapshotNotFound {
        /// The snapshot ID that was not found.
        id: String,
    },

    /// Path traversal attempt detected.
    #[cfg(feature = "workspace")]
    #[error("path traversal rejected: '{path}' escapes workspace root. Use relative paths only.")]
    PathTraversal {
        /// The offending path that attempted to escape the workspace root.
        path: String,
    },

    /// Docker is not available on this host.
    #[cfg(feature = "workspace")]
    #[error("Docker unavailable: {reason}. Ensure Docker daemon is running and accessible.")]
    DockerUnavailable {
        /// A description of why Docker is unavailable.
        reason: String,
    },

    /// Session exceeded its configured timeout.
    #[cfg(feature = "workspace")]
    #[error(
        "session timed out after {timeout:?}. Consider increasing session_timeout in SandboxConfig."
    )]
    SessionTimeout {
        /// The timeout duration that was exceeded.
        timeout: Duration,
    },
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
            #[cfg(feature = "workspace")]
            SandboxError::ProvisionFailed { .. } => {
                (ErrorCategory::Internal, "code.sandbox_provision_failed")
            }
            #[cfg(feature = "workspace")]
            SandboxError::SessionNotFound { .. } => {
                (ErrorCategory::NotFound, "code.sandbox_session_not_found")
            }
            #[cfg(feature = "workspace")]
            SandboxError::SnapshotNotFound { .. } => {
                (ErrorCategory::NotFound, "code.sandbox_snapshot_not_found")
            }
            #[cfg(feature = "workspace")]
            SandboxError::PathTraversal { .. } => {
                (ErrorCategory::InvalidInput, "code.sandbox_path_traversal")
            }
            #[cfg(feature = "workspace")]
            SandboxError::DockerUnavailable { .. } => {
                (ErrorCategory::Unavailable, "code.sandbox_docker_unavailable")
            }
            #[cfg(feature = "workspace")]
            SandboxError::SessionTimeout { .. } => {
                (ErrorCategory::Timeout, "code.sandbox_session_timeout")
            }
        };
        adk_core::AdkError::new(ErrorComponent::Code, category, code, err.to_string())
            .with_source(err)
    }
}
