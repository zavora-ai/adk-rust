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
use std::sync::atomic::{AtomicU64, Ordering};

use futures::FutureExt;
use rmcp::model::{
    ElicitRequest, ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationCapability,
    ElicitationSchema, FormElicitationCapability, InitializeRequestParams, InputRequest,
    InputRequests, InputResponses, UrlElicitationCapability,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient};
use serde_json::Value;

use super::parked::{ParkedCallKind, ParkedMcpCall};
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

/// The run and tool call in flight when a server asked (spec 3.1 §10 T1).
///
/// A toolset registers one per in-flight tool call, and the connection's
/// [`AdkClientHandler`] answers an elicitation inside it, so a handler reads the scope
/// with [`ElicitationScope::current`] and can show the request as a prompt of that
/// session or park it as an approval of that run.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::ElicitationScope;
///
/// let scope = ElicitationScope::new("app", "alice", "s1");
/// assert_eq!(scope.session_id, "s1");
/// assert!(ElicitationScope::current().is_none());
/// ```
#[derive(Clone, Debug, Default)]
pub struct ElicitationScope {
    /// The application of the session.
    pub app_name: String,
    /// The user of the session.
    pub user_id: String,
    /// The session the tool call belongs to.
    pub session_id: String,
    /// The invocation of the tool call.
    pub invocation_id: String,
    /// The agent that made the tool call.
    pub agent_name: String,
    /// The function call id, when the call has one.
    pub function_call_id: Option<String>,
    /// The durable run id, when the run is durable.
    pub run_id: Option<String>,
    /// The acting agent and the subject it acts for.
    pub acting: Option<adk_core::ActingContext>,
    /// The run's audit trail, when it records one.
    pub audit: Option<adk_core::audit_log::AuditTrail>,
}

tokio::task_local! {
    static CURRENT_ELICITATION_SCOPE: ElicitationScope;
}

impl ElicitationScope {
    /// A scope of `session_id` of `user_id` in `app_name`, with no call details.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
            ..Self::default()
        }
    }

    /// The scope of the tool call `ctx` executes.
    pub fn for_call(ctx: &dyn adk_core::ToolContext) -> Self {
        let call_id = ctx.function_call_id();
        Self {
            app_name: ctx.app_name().to_string(),
            user_id: ctx.user_id().to_string(),
            session_id: ctx.session_id().to_string(),
            invocation_id: ctx.invocation_id().to_string(),
            agent_name: ctx.agent_name().to_string(),
            function_call_id: (!call_id.is_empty()).then(|| call_id.to_string()),
            run_id: ctx.run_config().and_then(|config| config.durable_run_id.clone()),
            acting: Some(ctx.acting_context()),
            audit: ctx.run_config().and_then(|config| config.audit.clone()),
        }
    }

    /// Whether both scopes name the same session and call.
    pub fn same_as(&self, other: &Self) -> bool {
        self.app_name == other.app_name
            && self.user_id == other.user_id
            && self.session_id == other.session_id
            && self.invocation_id == other.invocation_id
            && self.function_call_id == other.function_call_id
    }

    /// The scope the elicitation being answered belongs to, if any.
    pub fn current() -> Option<Self> {
        CURRENT_ELICITATION_SCOPE.try_with(Clone::clone).ok()
    }

    /// Runs `future` with this scope as [`current`](Self::current).
    pub async fn within<F: Future>(self, future: F) -> F::Output {
        CURRENT_ELICITATION_SCOPE.scope(self, future).await
    }
}

/// The scopes a connection's elicitations resolve against, mirroring
/// [`SamplingScopes`](crate::sampling::SamplingScopes) (spec 3.1 §10 T1).
///
/// A toolset [`enter`](Self::enter)s the scope of each tool call it makes; a request that
/// arrives while none is in flight resolves to the [fallback](Self::set_fallback).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_tool::mcp::{ElicitationScope, ElicitationScopes};
///
/// let scopes = Arc::new(ElicitationScopes::default());
/// assert!(scopes.resolve().is_none());
/// let guard = scopes.enter(ElicitationScope::new("app", "alice", "s1"));
/// assert_eq!(scopes.resolve().map(|scope| scope.session_id), Some("s1".to_string()));
/// drop(guard);
/// assert!(scopes.resolve().is_none());
/// ```
#[derive(Debug, Default)]
pub struct ElicitationScopes {
    active: std::sync::Mutex<Vec<(u64, ElicitationScope)>>,
    fallback: std::sync::Mutex<Option<ElicitationScope>>,
    next_id: AtomicU64,
    /// Calls awaiting their `tools/call` response, which a legacy `elicitation/create`
    /// belongs to (spec 3.2 §10 T2).
    calls: std::sync::Mutex<Vec<(u64, ParkedMcpCall)>>,
}

impl ElicitationScopes {
    /// Sets the scope used when no registered call is in flight.
    pub fn set_fallback(&self, scope: Option<ElicitationScope>) {
        *self.fallback.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = scope;
    }

    /// The scope used when no registered call is in flight.
    pub fn fallback(&self) -> Option<ElicitationScope> {
        self.fallback.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Registers `scope` until the returned guard is dropped.
    pub fn enter(self: &Arc<Self>, scope: ElicitationScope) -> ElicitationScopeGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((id, scope));
        ElicitationScopeGuard { scopes: Arc::clone(self), id }
    }

    /// The scope an elicitation arriving now belongs to.
    pub fn resolve(&self) -> Option<ElicitationScope> {
        let active = self.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, latest)) = active.last() {
            if active.iter().any(|(_, scope)| !scope.same_as(latest)) {
                tracing::debug!(
                    in_flight = active.len(),
                    "tool calls from several runs are in flight; the elicitation is attributed to the latest"
                );
            }
            return Some(latest.clone());
        }
        drop(active);
        self.fallback()
    }
}

impl ElicitationScopes {
    /// Registers `call` as awaiting its response until the returned guard is dropped.
    pub(crate) fn enter_call(self: &Arc<Self>, call: ParkedMcpCall) -> InFlightCall {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((id, call));
        InFlightCall { scopes: Arc::clone(self), id }
    }

    /// The call a legacy elicitation arriving now belongs to: the latest one registered.
    fn resolve_call(&self) -> Option<ParkedMcpCall> {
        let calls = self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        calls.last().map(|(_, call)| call.clone())
    }
}

/// Unregisters a call registered with [`ElicitationScopes::enter_call`] when dropped.
pub(crate) struct InFlightCall {
    scopes: Arc<ElicitationScopes>,
    id: u64,
}

impl Drop for InFlightCall {
    fn drop(&mut self) {
        self.scopes
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|(id, _)| *id != self.id);
    }
}

/// Unregisters an [`ElicitationScope`] when dropped.
#[derive(Debug)]
pub struct ElicitationScopeGuard {
    scopes: Arc<ElicitationScopes>,
    id: u64,
}

impl Drop for ElicitationScopeGuard {
    fn drop(&mut self) {
        self.scopes
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|(id, _)| *id != self.id);
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
    /// `notifications/tools/list_changed` received on this connection; a
    /// toolset compares it against its cached `tools/list` result.
    tool_list_changes: Arc<AtomicU64>,
    /// Scopes elicitations resolve against; a toolset swaps in its own on connect.
    elicitation_scopes: Arc<std::sync::RwLock<Arc<ElicitationScopes>>>,
    #[cfg(feature = "mcp-sampling")]
    sampling_handler: Option<Arc<dyn crate::sampling::SamplingHandler>>,
    /// Scopes sampling requests count against; a toolset swaps in its own on connect.
    #[cfg(feature = "mcp-sampling")]
    sampling_scopes: Arc<std::sync::RwLock<Arc<crate::sampling::SamplingScopes>>>,
}

impl AdkClientHandler {
    /// Create a new `AdkClientHandler` with the given elicitation handler.
    pub fn new(handler: Arc<dyn ElicitationHandler>) -> Self {
        Self {
            handler,
            resource_notification_handler: None,
            tasks: false,
            tool_list_changes: Arc::new(AtomicU64::new(0)),
            elicitation_scopes: Arc::default(),
            #[cfg(feature = "mcp-sampling")]
            sampling_handler: None,
            #[cfg(feature = "mcp-sampling")]
            sampling_scopes: Arc::default(),
        }
    }

    /// Attributes elicitations that arrive outside any run's tool call to `scope`.
    ///
    /// Requests that arrive while a run's [`McpToolset`](crate::McpToolset) tool call is
    /// in flight belong to that call instead (spec 3.1 §10 T1).
    pub fn with_elicitation_scope(self, scope: ElicitationScope) -> Self {
        self.elicitation_scopes().set_fallback(Some(scope));
        self
    }

    pub(crate) fn elicitation_scopes(&self) -> Arc<ElicitationScopes> {
        Arc::clone(&self.elicitation_scopes.read().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Makes this connection's elicitations resolve against `scopes`, keeping this
    /// handler's fallback scope when `scopes` has none.
    pub(crate) fn share_elicitation_scopes(&self, scopes: Arc<ElicitationScopes>) {
        let mut current =
            self.elicitation_scopes.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if Arc::ptr_eq(&current, &scopes) {
            return;
        }
        if scopes.fallback().is_none() {
            scopes.set_fallback(current.fallback());
        }
        *current = scopes;
    }

    pub(crate) fn tool_list_changes(&self) -> u64 {
        self.tool_list_changes.load(Ordering::Acquire)
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
        // The handler answers inside the scope of the tool call in flight, so it knows
        // which run and session asked (spec 3.1 §10 T1).
        let scope = self.elicitation_scopes().resolve();
        let handled = async {
            match &request {
                ElicitRequestParams::FormElicitationParams {
                    message,
                    requested_schema,
                    meta,
                    ..
                } => {
                    let metadata_value = meta.as_ref().and_then(|m| serde_json::to_value(m).ok());
                    Some(
                        self.handler
                            .handle_form_elicitation(
                                message,
                                requested_schema,
                                metadata_value.as_ref(),
                            )
                            .await,
                    )
                }
                ElicitRequestParams::UrlElicitationParams {
                    message,
                    url,
                    elicitation_id,
                    meta,
                    ..
                } => {
                    let metadata_value = meta.as_ref().and_then(|m| serde_json::to_value(m).ok());
                    Some(
                        self.handler
                            .handle_url_elicitation(
                                message,
                                url,
                                elicitation_id,
                                metadata_value.as_ref(),
                            )
                            .await,
                    )
                }
                _ => None,
            }
        };
        let handled = match scope {
            Some(scope) => futures::future::Either::Left(scope.within(handled)),
            None => futures::future::Either::Right(handled),
        };
        let result = match std::panic::AssertUnwindSafe(handled).catch_unwind().await {
            Ok(Some(result)) => Ok(result),
            Ok(None) => return ElicitResult::new(ElicitationAction::Decline),
            Err(panic) => Err(panic),
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

    /// Meters sampling requests that arrive outside any run's tool call against `scope`.
    ///
    /// Requests that arrive while a run's [`McpToolset`](crate::McpToolset) tool call is
    /// in flight count against that run instead. See
    /// [sampling metering](crate::sampling#metering).
    #[cfg(feature = "mcp-sampling")]
    pub fn with_sampling_scope(self, scope: adk_core::ModelCallScope) -> Self {
        self.sampling_scopes().set_fallback(Some(scope));
        self
    }

    #[cfg(feature = "mcp-sampling")]
    pub(crate) fn sampling_scopes(&self) -> Arc<crate::sampling::SamplingScopes> {
        Arc::clone(&self.sampling_scopes.read().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Makes this connection's sampling requests resolve against `scopes`, keeping this
    /// handler's fallback scope when `scopes` has none.
    #[cfg(feature = "mcp-sampling")]
    pub(crate) fn share_sampling_scopes(&self, scopes: Arc<crate::sampling::SamplingScopes>) {
        let mut current =
            self.sampling_scopes.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if Arc::ptr_eq(&current, &scopes) {
            return;
        }
        if scopes.fallback().is_none() {
            scopes.set_fallback(current.fallback());
        }
        *current = scopes;
    }
}

impl rmcp::handler::client::ClientHandler for AdkClientHandler {
    fn get_info(&self) -> InitializeRequestParams {
        let mut info = InitializeRequestParams::default();
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

        // The request counts against the run whose tool call is in flight, if any, and the
        // handler learns that call's scope as an elicitation handler does (spec 3.1 §10 T1).
        let handled = sampling_handler.handle_create_message(request);
        let handled = match self.sampling_scopes().resolve() {
            Some(scope) => futures::future::Either::Left(scope.within(handled)),
            None => futures::future::Either::Right(handled),
        };
        let handled = match self.elicitation_scopes().resolve() {
            Some(scope) => futures::future::Either::Left(scope.within(handled)),
            None => futures::future::Either::Right(handled),
        };
        match std::panic::AssertUnwindSafe(handled).catch_unwind().await {
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
                // A model error can echo the sampled prompt, so only its content-free part
                // is logged.
                tracing::warn!(error = %e.diagnostics(), "sampling handler returned error");
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
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        // A server before 2026-07-28 asks while one of the toolset's calls awaits its
        // response; the handler sees that call's parked form (spec 3.2 §10 T2).
        let legacy = ParkedMcpCall::current()
            .is_none()
            .then(|| self.elicitation_scopes().resolve_call())
            .flatten()
            .map(|call| {
                let rounds = call.input_rounds;
                let question = InputRequest::Elicitation(ElicitRequest::new(request.clone()));
                call.waiting(
                    ParkedCallKind::Legacy,
                    None,
                    [(context.id.to_string(), question)].into(),
                    rounds,
                )
            });
        Ok(match legacy {
            Some(call) => call.within(self.handle_elicitation(request)).await,
            None => self.handle_elicitation(request).await,
        })
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

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.tool_list_changes.fetch_add(1, Ordering::AcqRel);
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
