//! Error types for ACP integration.

use thiserror::Error;

/// Errors that can occur during ACP operations.
#[derive(Debug, Error)]
pub enum AcpError {
    /// Failed to spawn the ACP agent process.
    #[error("failed to spawn ACP agent: {0}")]
    Spawn(String),

    /// ACP protocol error (initialization, session, or prompt failure).
    #[error("ACP protocol error: {0}")]
    Protocol(String),

    /// The ACP agent returned an error response.
    #[error("ACP agent error: {0}")]
    AgentError(String),

    /// Connection to the ACP agent was lost.
    #[error("ACP connection lost: {0}")]
    ConnectionLost(String),

    /// Timeout waiting for ACP agent response.
    #[error("ACP agent timed out after {0}ms")]
    Timeout(u64),

    /// Invalid configuration for the ACP agent.
    #[error("invalid ACP agent config: {0}")]
    InvalidConfig(String),

    /// The ACP agent does not advertise the capability a request needs, so
    /// the client did not send it.
    #[error("unsupported by the ACP agent: {0}")]
    Unsupported(String),
}

impl From<AcpError> for adk_core::AdkError {
    fn from(err: AcpError) -> Self {
        adk_core::AdkError::tool(err.to_string())
    }
}

/// Result type for ACP operations.
pub type Result<T> = std::result::Result<T, AcpError>;
