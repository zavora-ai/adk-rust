//! Typed errors for the computer-use orchestration layer.
//!
//! The runtime boundary ([`crate::ComputerUseRuntime`]) and the MCP adapter
//! ([`crate::ComputerUseMcpRuntime`]) return [`ComputerUseError`] instead of
//! stringly-typed failures. Each variant carries enough context to map cleanly
//! onto [`adk_core::AdkError`] at the host boundary via the provided
//! [`From`] implementation.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use thiserror::Error;

/// Convenience alias for fallible computer-use operations.
pub type Result<T, E = ComputerUseError> = std::result::Result<T, E>;

/// Structured error surfaced by the computer-use graph, runtime trait, and MCP adapter.
///
/// The variants distinguish caller mistakes ([`InvalidRequest`](Self::InvalidRequest)),
/// transport faults ([`Mcp`](Self::Mcp)), payload decoding failures
/// ([`Decode`](Self::Decode)), identity/authorization mismatches
/// ([`IdentityMismatch`](Self::IdentityMismatch)), unimplemented adapter
/// capabilities ([`Unsupported`](Self::Unsupported)), and residual invariant
/// violations ([`Runtime`](Self::Runtime)). Convert to [`adk_core::AdkError`]
/// with `?` or `.into()` when returning through an ADK trait boundary.
#[derive(Debug, Error)]
pub enum ComputerUseError {
    /// The underlying MCP transport or tool invocation failed.
    #[error("computer-use MCP call failed: {0}")]
    Mcp(String),

    /// A wire payload could not be decoded into the expected contract type.
    #[error("failed to decode computer-use payload: {0}")]
    Decode(String),

    /// A runtime response did not match the authenticated ADK principal or session.
    #[error("computer-use identity mismatch: {0}")]
    IdentityMismatch(String),

    /// A request argument violated a documented precondition.
    #[error("invalid computer-use request: {0}")]
    InvalidRequest(String),

    /// The runtime adapter does not implement the requested control operation.
    #[error("{operation} is not implemented by this runtime adapter")]
    Unsupported {
        /// The control operation the caller attempted (e.g. `pause_session`).
        operation: &'static str,
    },

    /// A runtime invariant expected by the graph was not satisfied.
    #[error("computer-use runtime error: {0}")]
    Runtime(String),
}

impl From<serde_json::Error> for ComputerUseError {
    fn from(error: serde_json::Error) -> Self {
        ComputerUseError::Decode(error.to_string())
    }
}

impl From<ComputerUseError> for AdkError {
    fn from(error: ComputerUseError) -> Self {
        let (component, category, code) = match &error {
            ComputerUseError::Mcp(_) => {
                (ErrorComponent::Tool, ErrorCategory::Unavailable, "tool.computer_use.mcp")
            }
            ComputerUseError::Decode(_) => {
                (ErrorComponent::Tool, ErrorCategory::InvalidInput, "tool.computer_use.decode")
            }
            ComputerUseError::IdentityMismatch(_) => {
                (ErrorComponent::Auth, ErrorCategory::Forbidden, "auth.computer_use.identity")
            }
            ComputerUseError::InvalidRequest(_) => (
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.computer_use.invalid_request",
            ),
            ComputerUseError::Unsupported { .. } => {
                (ErrorComponent::Tool, ErrorCategory::Unsupported, "tool.computer_use.unsupported")
            }
            ComputerUseError::Runtime(_) => {
                (ErrorComponent::Tool, ErrorCategory::Internal, "tool.computer_use.runtime")
            }
        };
        AdkError::new(component, category, code, error.to_string())
    }
}
