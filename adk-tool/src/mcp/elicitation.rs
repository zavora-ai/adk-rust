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
///
/// When the `mcp-sampling` feature is enabled, also accepts an optional
/// `Arc<dyn SamplingHandler>` to handle `sampling/createMessage` requests.
pub struct AdkClientHandler {
    handler: Arc<dyn ElicitationHandler>,
    #[cfg(feature = "mcp-sampling")]
    sampling_handler: Option<Arc<dyn crate::sampling::SamplingHandler>>,
}

impl AdkClientHandler {
    pub fn new(handler: Arc<dyn ElicitationHandler>) -> Self {
        Self {
            handler,
            #[cfg(feature = "mcp-sampling")]
            sampling_handler: None,
        }
    }

    /// Set a sampling handler for `sampling/createMessage` requests.
    ///
    /// When configured, the handler advertises sampling capability and
    /// delegates incoming sampling requests to the provided handler.
    #[cfg(feature = "mcp-sampling")]
    pub fn with_sampling_handler(
        mut self,
        handler: Arc<dyn crate::sampling::SamplingHandler>,
    ) -> Self {
        self.sampling_handler = Some(handler);
        self
    }
}

impl rmcp::handler::client::ClientHandler for AdkClientHandler {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();

        #[cfg(feature = "mcp-sampling")]
        {
            if self.sampling_handler.is_some() {
                info.capabilities = rmcp::model::ClientCapabilities::builder()
                    .enable_elicitation()
                    .enable_sampling()
                    .build();
            } else {
                info.capabilities =
                    rmcp::model::ClientCapabilities::builder().enable_elicitation().build();
            }
        }

        #[cfg(not(feature = "mcp-sampling"))]
        {
            info.capabilities =
                rmcp::model::ClientCapabilities::builder().enable_elicitation().build();
        }

        info
    }

    #[cfg(feature = "mcp-sampling")]
    async fn create_message(
        &self,
        params: rmcp::model::CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<rmcp::model::CreateMessageResult, rmcp::ErrorData> {
        use crate::sampling::{SamplingContent, SamplingMessage, SamplingRequest};
        use rmcp::model::{CreateMessageResult, Role, SamplingMessageContent};

        let Some(ref sampling_handler) = self.sampling_handler else {
            return Err(rmcp::ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                "sampling handler not configured",
                None,
            ));
        };

        // Convert rmcp SamplingMessages → our SamplingMessages
        let messages: Vec<SamplingMessage> = params
            .messages
            .iter()
            .map(|msg| {
                let role = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                // Extract text from the first content item
                let content = msg
                    .content
                    .first()
                    .and_then(|c| match c {
                        SamplingMessageContent::Text(t) => {
                            Some(SamplingContent::text(t.text.clone()))
                        }
                        SamplingMessageContent::Image(img) => {
                            Some(SamplingContent::image(img.data.clone(), img.mime_type.clone()))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| SamplingContent::text(""));
                SamplingMessage::new(role, content)
            })
            .collect();

        let request = SamplingRequest {
            messages,
            system_prompt: params.system_prompt.clone(),
            model_preferences: None,
            max_tokens: Some(params.max_tokens),
            temperature: params.temperature.map(|t| t as f64),
        };

        match std::panic::AssertUnwindSafe(sampling_handler.handle_create_message(request))
            .catch_unwind()
            .await
        {
            Ok(Ok(response)) => {
                // Convert our SamplingResponse → rmcp CreateMessageResult
                let text = match &response.content {
                    SamplingContent::Text { text } => text.clone(),
                    SamplingContent::Image { .. } => String::new(),
                };
                let message = rmcp::model::SamplingMessage::assistant_text(text);
                Ok(CreateMessageResult::new(message, response.model)
                    .with_stop_reason(response.stop_reason))
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "sampling handler returned error");
                Err(rmcp::ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    format!("sampling handler error: {e}"),
                    None,
                ))
            }
            Err(_panic) => {
                tracing::warn!("sampling handler panicked");
                Err(rmcp::ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    "sampling handler panicked",
                    None,
                ))
            }
        }
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
