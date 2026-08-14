//! MCP Elicitation lifecycle support.
//!
//! This module provides the [`ElicitationHandler`] trait for handling MCP elicitation
//! requests from servers, an [`AutoDeclineElicitationHandler`] that declines all
//! requests, and the internal [`AdkClientHandler`] bridge to rmcp's `ClientHandler`.

// Sampling remains available only as a compatibility feature. rmcp marks the
// protocol surface deprecated under SEP-2577, so deprecation warnings are
// intentionally contained in this bridge while existing users migrate.
#![cfg_attr(feature = "mcp-sampling", allow(deprecated))]

use std::sync::Arc;

use futures::FutureExt;
use rmcp::model::{
    ClientInfo, ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationCapability,
    ElicitationSchema, FormElicitationCapability, InputRequest, InputRequests, InputResponses,
    UrlElicitationCapability,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient};
use serde_json::Value;

use super::resource_notifications::{
    ResourceNotificationHandler, dispatch_resource_list_changed, dispatch_resource_updated,
};

/// Trait for handling MCP elicitation requests from servers.
///
/// Implement this trait to provide custom elicitation behavior when
/// an MCP server requests additional information during tool execution.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::ElicitationHandler;
/// use rmcp::model::{ElicitResult, ElicitationAction, ElicitationSchema};
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
///     ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
///         println!("Server asks: {message}");
///         Ok(ElicitResult::new(ElicitationAction::Accept))
///     }
///
///     async fn handle_url_elicitation(
///         &self,
///         message: &str,
///         url: &str,
///         elicitation_id: &str,
///         metadata: Option<&serde_json::Value>,
///     ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
///         println!("Server asks to visit: {url}");
///         Ok(ElicitResult::new(ElicitationAction::Accept))
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
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>>;

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
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>>;
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
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ElicitResult::new(ElicitationAction::Decline))
    }

    async fn handle_url_elicitation(
        &self,
        _message: &str,
        _url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ElicitResult::new(ElicitationAction::Decline))
    }
}

/// Internal bridge between ADK's [`ElicitationHandler`] and rmcp's `ClientHandler`.
///
/// Wraps an `Arc<dyn ElicitationHandler>` and implements rmcp's `ClientHandler` trait,
/// advertising elicitation capabilities and delegating requests to the handler.
///
/// When the `mcp-sampling` feature is enabled, also accepts an optional
/// `Arc<dyn SamplingHandler>` to handle `sampling/createMessage` requests.
#[derive(Clone)]
pub struct AdkClientHandler {
    handler: Arc<dyn ElicitationHandler>,
    resource_notification_handler: Option<Arc<dyn ResourceNotificationHandler>>,
    tasks: bool,
    #[cfg(feature = "mcp-sampling")]
    sampling_handler: Option<Arc<dyn crate::sampling::SamplingHandler>>,
}

impl AdkClientHandler {
    /// Create a new `AdkClientHandler` with the given elicitation handler.
    pub fn new(handler: Arc<dyn ElicitationHandler>) -> Self {
        Self {
            handler,
            resource_notification_handler: None,
            tasks: false,
            #[cfg(feature = "mcp-sampling")]
            sampling_handler: None,
        }
    }

    /// Declare the SEP-2663 tasks extension during the handshake.
    ///
    /// A server decides per call whether to answer `tools/call` with a task, but
    /// it must not return one to a client that did not declare the extension.
    /// Without this, [`McpToolset::with_task_support`](crate::mcp::McpToolset::with_task_support)
    /// configures a path no server will ever take.
    pub fn with_tasks(mut self) -> Self {
        self.tasks = true;
        self
    }

    async fn handle_elicitation(&self, request: ElicitRequestParams) -> ElicitResult {
        let result = match &request {
            ElicitRequestParams::FormElicitationParams {
                message, requested_schema, meta, ..
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
            ElicitRequestParams::UrlElicitationParams {
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
            _ => return ElicitResult::new(ElicitationAction::Decline),
        };

        match result {
            Ok(Ok(elicitation_result)) => elicitation_result,
            Ok(Err(error)) => {
                tracing::warn!(%error, "elicitation handler returned error, declining");
                ElicitResult::new(ElicitationAction::Decline)
            }
            Err(_) => {
                tracing::warn!("elicitation handler panicked, declining");
                ElicitResult::new(ElicitationAction::Decline)
            }
        }
    }

    /// Fulfil stateless MRTR requests through the same policy used for legacy
    /// server-initiated elicitation. Sampling and roots are intentionally not
    /// accepted here: both are deprecated in the 2026-07-28 lifecycle.
    pub(crate) async fn fulfill_input_requests(
        &self,
        requests: InputRequests,
    ) -> Result<InputResponses, String> {
        let mut responses = InputResponses::new();
        for (key, request) in requests {
            let value = match request {
                InputRequest::Elicitation(request) => {
                    serde_json::to_value(self.handle_elicitation(request.params).await)
                        .map_err(|error| error.to_string())?
                }
                InputRequest::CreateMessage(_) => {
                    return Err("MRTR sampling is deprecated and not enabled by ADK".to_string());
                }
                InputRequest::ListRoots(_) => {
                    return Err("MRTR roots are deprecated and not enabled by ADK".to_string());
                }
                _ => return Err("unsupported MRTR input request".to_string()),
            };
            responses.insert(key, value);
        }
        Ok(responses)
    }

    /// Set the handler for resource update and resource-list notifications.
    pub fn with_resource_notification_handler(
        mut self,
        handler: Arc<dyn ResourceNotificationHandler>,
    ) -> Self {
        self.resource_notification_handler = Some(handler);
        self
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
        let elicitation = ElicitationCapability::new()
            .with_form(FormElicitationCapability::new())
            .with_url(UrlElicitationCapability::new());

        // The capability builder is typestate, so each `enable_*` returns a
        // different type and the optional ones cannot be chained conditionally.
        #[cfg(feature = "mcp-sampling")]
        {
            info.capabilities = if self.sampling_handler.is_some() {
                rmcp::model::ClientCapabilities::builder()
                    .enable_elicitation_with(elicitation)
                    .enable_sampling()
                    .build()
            } else {
                rmcp::model::ClientCapabilities::builder()
                    .enable_elicitation_with(elicitation)
                    .build()
            };
        }

        #[cfg(not(feature = "mcp-sampling"))]
        {
            info.capabilities = rmcp::model::ClientCapabilities::builder()
                .enable_elicitation_with(elicitation)
                .build();
        }

        if self.tasks {
            info.capabilities
                .extensions
                .get_or_insert_with(rmcp::model::ExtensionCapabilities::new)
                .insert(rmcp::model::TASKS_EXTENSION_ID.to_string(), Default::default());
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
        use rmcp::model::{CreateMessageResult, Role, SamplingMessageContentBlock};

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
                        SamplingMessageContentBlock::Text(t) => {
                            Some(SamplingContent::text(t.text.clone()))
                        }
                        SamplingMessageContentBlock::Image(img) => {
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
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        Ok(self.handle_elicitation(request).await)
    }

    async fn on_resource_updated(
        &self,
        params: rmcp::model::ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        dispatch_resource_updated(&self.resource_notification_handler, &params.uri).await;
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        dispatch_resource_list_changed(&self.resource_notification_handler).await;
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
