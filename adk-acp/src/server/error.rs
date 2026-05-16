//! Server-specific error types for the ACP Server.
//!
//! These errors represent conditions that can occur during server operation,
//! from session management failures to transport issues.

use std::fmt;

/// Server-specific errors for the ACP Server.
///
/// Each variant maps to a specific error condition with a machine-readable
/// `error_code` and human-readable message. Internal details are logged
/// via `tracing` but never exposed to clients.
#[derive(Debug)]
pub enum AcpServerError {
    /// Session not found in the registry.
    SessionNotFound(String),

    /// Maximum concurrent sessions reached.
    MaxSessionsReached(usize),

    /// Server is shutting down, not accepting new requests.
    ShuttingDown,

    /// Protocol version not supported.
    UnsupportedVersion {
        /// The version requested by the client.
        requested: String,
        /// The versions supported by this server.
        supported: Vec<String>,
    },

    /// Malformed protocol message received.
    MalformedMessage(String),

    /// Internal server error (details logged, not exposed to client).
    Internal(String),

    /// Transport-level error.
    Transport(String),

    /// Agent execution error.
    Execution(String),
}

impl AcpServerError {
    /// Returns the machine-readable error code for this error.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let err = AcpServerError::SessionNotFound("sess-123".into());
    /// assert_eq!(err.error_code(), "session_not_found");
    /// ```
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::SessionNotFound(_) => "session_not_found",
            Self::MaxSessionsReached(_) => "max_sessions_reached",
            Self::ShuttingDown => "shutting_down",
            Self::UnsupportedVersion { .. } => "unsupported_version",
            Self::MalformedMessage(_) => "malformed_message",
            Self::Internal(_) => "internal_error",
            Self::Transport(_) => "transport_error",
            Self::Execution(_) => "execution_error",
        }
    }

    /// Returns a client-safe message that does not expose internal details.
    ///
    /// For `Internal` errors, a generic message is returned. For all other
    /// variants, the descriptive message is returned.
    pub fn client_message(&self) -> String {
        match self {
            Self::SessionNotFound(id) => format!("session not found: {id}"),
            Self::MaxSessionsReached(limit) => {
                format!("max sessions reached (limit: {limit})")
            }
            Self::ShuttingDown => "server is shutting down".to_string(),
            Self::UnsupportedVersion { requested, supported } => {
                format!("unsupported protocol version: {requested}, supported: {supported:?}")
            }
            Self::MalformedMessage(msg) => format!("malformed message: {msg}"),
            Self::Internal(_) => "internal server error".to_string(),
            Self::Transport(msg) => format!("transport error: {msg}"),
            Self::Execution(msg) => format!("execution error: {msg}"),
        }
    }

    /// Formats the error as a JSON-compatible error response.
    pub fn to_error_response(&self) -> ErrorResponse {
        ErrorResponse { error_code: self.error_code().to_string(), message: self.client_message() }
    }
}

impl fmt::Display for AcpServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound(id) => write!(f, "session not found: {id}"),
            Self::MaxSessionsReached(limit) => {
                write!(f, "max sessions reached (limit: {limit})")
            }
            Self::ShuttingDown => write!(f, "server is shutting down"),
            Self::UnsupportedVersion { requested, supported } => {
                write!(f, "unsupported protocol version: {requested}, supported: {supported:?}")
            }
            Self::MalformedMessage(msg) => write!(f, "malformed message: {msg}"),
            Self::Internal(msg) => write!(f, "internal server error: {msg}"),
            Self::Transport(msg) => write!(f, "transport error: {msg}"),
            Self::Execution(msg) => write!(f, "execution error: {msg}"),
        }
    }
}

impl std::error::Error for AcpServerError {}

impl From<AcpServerError> for crate::AcpError {
    fn from(err: AcpServerError) -> Self {
        crate::AcpError::Protocol(err.to_string())
    }
}

/// A structured error response sent to ACP clients.
///
/// Contains a machine-readable `error_code` and a human-readable `message`.
/// Internal details (stack traces, file paths) are never included.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorResponse {
    /// Machine-readable error code (e.g., "session_not_found").
    pub error_code: String,
    /// Human-readable error description.
    pub message: String,
}
