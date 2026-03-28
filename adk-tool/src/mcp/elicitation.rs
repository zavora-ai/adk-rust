//! MCP Elicitation lifecycle support.
//!
//! This module provides the [`ElicitationHandler`] trait for handling MCP elicitation
//! requests from servers, an [`AutoDeclineElicitationHandler`] that declines all
//! requests, and the internal [`AdkClientHandler`] bridge to rmcp's `ClientHandler`.

use std::sync::Arc;

use futures::FutureExt;
use rmcp::model::{
    ClientInfo, CreateElicitationRequestParams, CreateElicitationResult, ElicitationAction,
    ElicitationResponseNotificationParam, ElicitationSchema,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient};
use serde_json::Value;

/// Trait for handling MCP elicitation requests from servers.
///
/// Implement this trait to provide custom elicitation behavior when
/// an MCP server requests additional information during tool execution.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::ElicitationHandler;
/// use rmcp::model::{CreateElicitationResult, ElicitationAction, ElicitationSchema};
///
/// struct MyHandler;
///
/// #[async_trait::async_trait]
/// impl ElicitationHandler for MyHandler {
///     async fn handle_form_elicitation(
///         &self,
///         message: &str,
///         schema: &ElicitationSchema,
///         metadata: Option<&serde_json::Value>,
///     ) -> Result<CreateElicitationResult, Box<dyn std::error::Error + Send + Sync>> {
///         println!("Server asks: {message}");
///         Ok(CreateElicitationResult::new(ElicitationAction::Accept))
///     }
///
///     async fn handle_url_elicitation(
///         &self,
///         message: &str,
///         url: &str,
///         elicitation_id: &str,
///         metadata: Option<&serde_json::Value>,
///     ) -> Result<CreateElicitationResult, Box<dyn std::error::Error + Send + Sync>> {
///         println!("Server asks to visit: {url}");
///         Ok(CreateElicitationResult::new(ElicitationAction::Accept))
///     }
/// }
/// ```
#[async_trait::async_trait]
pub trait ElicitationHandler: Send + Sync {
    /// Handle a form-based elicitation request.
    ///
    /// The server sends a human-readable message and a typed schema describing
    /// the data it needs. Return `Accept` with content matching the schema,
    /// `Decline` to refuse, or `Cancel` to abort the operation.
    async fn handle_form_elicitation(
        &self,
        message: &str,
        schema: &ElicitationSchema,
        metadata: Option<&Value>,
    ) -> Result<CreateElicitationResult, Box<dyn std::error::Error + Send + Sync>>;

    /// Handle a URL-based elicitation request.
    ///
    /// The server sends a URL for the user to visit and interact with externally.
    /// The `elicitation_id` uniquely identifies this request for the completion
    /// notification flow.
    async fn handle_url_elicitation(
        &self,
        message: &str,
        url: &str,
        elicitation_id: &str,
        metadata: Option<&Value>,
    ) -> Result<CreateElicitationResult, Box<dyn std::error::Error + Send + Sync>>;
}

/// Default handler that declines all elicitation requests.
///
/// Used when no custom handler is configured, preserving backward-compatible
/// behavior identical to rmcp's `()` ClientHandler default.
#[derive(Debug, Clone, Copy)]
pub struct AutoDeclineElicitationHandler;

#[async_trait::async_trait]
impl ElicitationHandler for AutoDeclineElicitationHandler {
    async fn handle_form_elicitation(
        &self,
        _message: &str,
        _schema: &ElicitationSchema,
        _metadata: Option<&Value>,
    ) -> Result<CreateElicitationResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(CreateElicitationResult::new(ElicitationAction::Decline))
    }

    async fn handle_url_elicitation(
        &self,
        _message: &str,
        _url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> Result<CreateElicitationResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(CreateElicitationResult::new(ElicitationAction::Decline))
    }
}

/// Internal bridge between ADK's [`ElicitationHandler`] and rmcp's `ClientHandler`.
///
/// Wraps an `Arc<dyn ElicitationHandler>` and implements rmcp's `ClientHandler` trait,
/// advertising elicitation capabilities and delegating requests to the handler.
pub struct AdkClientHandler {
    handler: Arc<dyn ElicitationHandler>,
}

impl AdkClientHandler {
    pub fn new(handler: Arc<dyn ElicitationHandler>) -> Self {
        Self { handler }
    }
}

impl rmcp::handler::client::ClientHandler for AdkClientHandler {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.capabilities = rmcp::model::ClientCapabilities::builder().enable_elicitation().build();
        info
    }

    async fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateElicitationResult, rmcp::ErrorData> {
        {
            let result = match &request {
                CreateElicitationRequestParams::FormElicitationParams {
                    message,
                    requested_schema,
                    meta,
                    ..
                } => {
                    let metadata_value = meta.as_ref().and_then(|m| serde_json::to_value(m).ok());
                    std::panic::AssertUnwindSafe(self.handler.handle_form_elicitation(
                        message,
                        requested_schema,
                        metadata_value.as_ref(),
                    ))
                    .catch_unwind()
                    .await
                }
                CreateElicitationRequestParams::UrlElicitationParams {
                    message,
                    url,
                    elicitation_id,
                    meta,
                    ..
                } => {
                    let metadata_value = meta.as_ref().and_then(|m| serde_json::to_value(m).ok());
                    std::panic::AssertUnwindSafe(self.handler.handle_url_elicitation(
                        message,
                        url,
                        elicitation_id,
                        metadata_value.as_ref(),
                    ))
                    .catch_unwind()
                    .await
                }
            };

            match result {
                Ok(Ok(elicitation_result)) => Ok(elicitation_result),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "elicitation handler returned error, declining");
                    Ok(CreateElicitationResult::new(ElicitationAction::Decline))
                }
                Err(_panic) => {
                    tracing::warn!("elicitation handler panicked, declining");
                    Ok(CreateElicitationResult::new(ElicitationAction::Decline))
                }
            }
        }
    }

    async fn on_url_elicitation_notification_complete(
        &self,
        _params: ElicitationResponseNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        tracing::debug!("received URL elicitation completion notification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elicitation_handler_is_send_sync() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<AutoDeclineElicitationHandler>();
    }

    #[test]
    fn test_adk_client_handler_is_send_sync() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<AdkClientHandler>();
    }
}
