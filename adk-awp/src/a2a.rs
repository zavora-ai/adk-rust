//! Application-provided A2A message dispatch for AWP.

use async_trait::async_trait;
use awp_types::AwpError;
use axum::http::HeaderMap;
use serde_json::Value;

/// Handles an A2A message received through the AWP public endpoint.
///
/// `adk-awp` owns discovery, protocol middleware, and the HTTP boundary, but it
/// cannot choose which agent or application operation should receive a message.
/// Applications install an implementation through
/// [`crate::AwpStateBuilder::a2a_handler`].
///
/// Implementations must authenticate and authorize any application-specific
/// operation represented by the message. The request headers are supplied so a
/// handler can use the same credentials as the surrounding Axum application.
#[async_trait]
pub trait AwpA2aHandler: Send + Sync {
    /// Dispatches one validated JSON object and returns the protocol response.
    ///
    /// # Errors
    ///
    /// Returns an [`AwpError`] when authentication, validation, dispatch, or
    /// downstream execution fails.
    async fn handle(&self, headers: HeaderMap, message: Value) -> Result<Value, AwpError>;
}

/// Fail-closed handler used until an application installs real A2A dispatch.
pub(crate) struct UnconfiguredA2aHandler;

#[async_trait]
impl AwpA2aHandler for UnconfiguredA2aHandler {
    async fn handle(&self, _headers: HeaderMap, _message: Value) -> Result<Value, AwpError> {
        Err(AwpError::ServiceUnavailable(
            "AWP A2A dispatch is not configured; install an AwpA2aHandler before serving \
             /awp/a2a"
                .to_string(),
        ))
    }
}
