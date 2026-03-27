//! Error types for adk-auth.

use thiserror::Error;

/// Error returned when access is denied.
#[derive(Debug, Clone, Error)]
#[error("Access denied: user '{user}' cannot access {permission}")]
pub struct AccessDenied {
    /// The user who was denied.
    pub user: String,
    /// The permission that was denied.
    pub permission: String,
}

impl AccessDenied {
    /// Create a new access denied error.
    pub fn new(user: impl Into<String>, permission: impl Into<String>) -> Self {
        Self { user: user.into(), permission: permission.into() }
    }
}

/// General auth error.
#[derive(Debug, Error)]
pub enum AuthError {
    /// Access was denied.
    #[error(transparent)]
    AccessDenied(#[from] AccessDenied),

    /// Role not found.
    #[error("Role not found: {0}")]
    RoleNotFound(String),

    /// User not found.
    #[error("User not found: {0}")]
    UserNotFound(String),

    /// Audit error.
    #[error("Audit error: {0}")]
    AuditError(String),

    /// IO error (for file-based audit).
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

impl From<AuthError> for adk_core::AdkError {
    fn from(err: AuthError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            AuthError::AccessDenied(_) => (ErrorCategory::Forbidden, "auth.access_denied"),
            AuthError::RoleNotFound(_) => (ErrorCategory::NotFound, "auth.role_not_found"),
            AuthError::UserNotFound(_) => (ErrorCategory::NotFound, "auth.user_not_found"),
            AuthError::AuditError(_) => (ErrorCategory::Internal, "auth.audit"),
            AuthError::IoError(_) => (ErrorCategory::Internal, "auth.io"),
        };
        adk_core::AdkError::new(ErrorComponent::Auth, category, code, err.to_string())
            .with_source(err)
    }
}
