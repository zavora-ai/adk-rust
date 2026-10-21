//! # Realtime Tool Context
//!
//! Provides a `ToolContext` implementation for tools executed within the realtime
//! integration layer. Holds session identity (`app_name`, `user_id`, `session_id`),
//! an optional `MemoryService` for `search_memory`, and `EventActions` state.

use std::sync::{Arc, Mutex};

use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext,
    RequestContext, ToolContext,
};
use adk_memory::MemoryService;
use async_trait::async_trait;

/// A [`ToolContext`] implementation for tools executed in the realtime integration layer.
///
/// Holds session identity (`app_name`, `user_id`, `session_id`), the current
/// `function_call_id`, an optional [`MemoryService`] for memory search, and
/// mutable [`EventActions`] state.
///
/// # Example
///
/// ```rust,ignore
/// let ctx = RealtimeToolContext::new(
///     "my-app".to_string(),
///     "user-1".to_string(),
///     "session-1".to_string(),
///     "call-42".to_string(),
///     None,
/// );
/// assert_eq!(ctx.app_name(), "my-app");
/// ```
pub struct RealtimeToolContext {
    app_name: String,
    user_id: String,
    session_id: String,
    function_call_id: String,
    memory_service: Option<Arc<dyn MemoryService>>,
    actions: Mutex<EventActions>,
    user_content: Content,
    /// The identity the server authenticated for the session, if any.
    request_context: Option<RequestContext>,
}

impl RealtimeToolContext {
    /// Creates a new `RealtimeToolContext`.
    ///
    /// # Arguments
    ///
    /// * `app_name` - The application name for this session.
    /// * `user_id` - The user identifier.
    /// * `session_id` - The session identifier.
    /// * `function_call_id` - The unique ID for this tool invocation.
    /// * `memory_service` - Optional memory service for `search_memory`.
    pub fn new(
        app_name: String,
        user_id: String,
        session_id: String,
        function_call_id: String,
        memory_service: Option<Arc<dyn MemoryService>>,
    ) -> Self {
        Self {
            app_name,
            user_id,
            session_id,
            function_call_id,
            memory_service,
            actions: Mutex::new(EventActions::default()),
            user_content: Content::new("user"),
            request_context: None,
        }
    }

    /// Marks the context as acting for the identity the server authenticated (spec 3.1
    /// §7.17): [`is_authenticated`](ReadonlyContext::is_authenticated) answers `true` and
    /// [`user_scopes`](ToolContext::user_scopes) answers the context's scopes. The caller
    /// keeps the context's `user_id` equal to this context's user.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ReadonlyContext, RequestContext, ToolContext};
    /// use adk_realtime::integration::RealtimeToolContext;
    ///
    /// let identity = RequestContext {
    ///     user_id: "user-1".to_string(),
    ///     scopes: vec!["orders:read".to_string()],
    ///     metadata: Default::default(),
    /// };
    /// let ctx = RealtimeToolContext::new(
    ///     "voice".to_string(),
    ///     "user-1".to_string(),
    ///     "session-1".to_string(),
    ///     "call-1".to_string(),
    ///     None,
    /// )
    /// .with_request_context(identity);
    /// assert!(ctx.is_authenticated());
    /// assert_eq!(ctx.user_scopes(), ["orders:read"]);
    /// ```
    #[must_use]
    pub fn with_request_context(mut self, request_context: RequestContext) -> Self {
        self.request_context = Some(request_context);
        self
    }
}

#[async_trait]
impl ReadonlyContext for RealtimeToolContext {
    fn invocation_id(&self) -> &str {
        &self.function_call_id
    }

    fn agent_name(&self) -> &str {
        "realtime"
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn branch(&self) -> &str {
        "main"
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn is_authenticated(&self) -> bool {
        self.request_context.is_some()
    }
}

#[async_trait]
impl CallbackContext for RealtimeToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for RealtimeToolContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }

    fn user_scopes(&self) -> Vec<String> {
        self.request_context.as_ref().map(|context| context.scopes.clone()).unwrap_or_default()
    }

    async fn search_memory(&self, query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        match &self.memory_service {
            Some(service) => {
                let resp = service
                    .search(adk_memory::SearchRequest {
                        query: query.to_string(),
                        user_id: self.user_id.clone(),
                        app_name: self.app_name.clone(),
                        limit: Some(10),
                        min_score: None,
                        project_id: None,
                    })
                    .await?;
                Ok(resp
                    .memories
                    .into_iter()
                    .map(|m| MemoryEntry { content: m.content, author: m.author })
                    .collect())
            }
            None => Ok(vec![]),
        }
    }
}

// ─── ToolContextFactory ──────────────────────────────────────────────────────

use super::SessionIdentity;

/// Factory for creating [`ToolContext`] instances per tool invocation.
///
/// The `IntegratedRealtimeRunner` provides this factory, scoped to the current
/// session, so that each tool call receives a fresh context with the correct
/// session identity and optional memory service.
///
/// # Implementors
///
/// The default implementation is [`DefaultToolContextFactory`], which creates
/// [`RealtimeToolContext`] instances. Custom implementations can be provided
/// for testing or advanced use cases.
pub trait ToolContextFactory: Send + Sync {
    /// Creates a new [`ToolContext`] for the given function call invocation.
    ///
    /// # Arguments
    ///
    /// * `function_call_id` - The unique identifier for this tool invocation,
    ///   as provided by the realtime provider.
    fn create_context(&self, function_call_id: &str) -> Arc<dyn ToolContext>;
}

/// Default [`ToolContextFactory`] implementation that creates [`RealtimeToolContext`]
/// instances with the configured session identity and optional memory service.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::integration::context::{DefaultToolContextFactory, ToolContextFactory};
/// use adk_realtime::integration::SessionIdentity;
///
/// let factory = DefaultToolContextFactory {
///     identity: SessionIdentity {
///         app_name: "my-app".to_string(),
///         user_id: "user-1".to_string(),
///         session_id: "session-1".to_string(),
///     },
///     memory_service: None,
/// };
///
/// let ctx = factory.create_context("call-42");
/// assert_eq!(ctx.app_name(), "my-app");
/// ```
pub struct DefaultToolContextFactory {
    /// The session identity triple used for all created contexts.
    pub identity: SessionIdentity,
    /// Optional memory service for `search_memory` support in created contexts.
    pub memory_service: Option<Arc<dyn MemoryService>>,
}

impl ToolContextFactory for DefaultToolContextFactory {
    fn create_context(&self, function_call_id: &str) -> Arc<dyn ToolContext> {
        Arc::new(RealtimeToolContext::new(
            self.identity.app_name.clone(),
            self.identity.user_id.clone(),
            self.identity.session_id.clone(),
            function_call_id.to_string(),
            self.memory_service.clone(),
        ))
    }
}

/// The factory the integrated runner's builder bridges ADK tools with: the session identity,
/// the memory service, and the authenticated request context when one was given.
pub(crate) struct IdentityToolContextFactory {
    pub(crate) identity: SessionIdentity,
    pub(crate) memory_service: Option<Arc<dyn MemoryService>>,
    pub(crate) request_context: Option<RequestContext>,
}

impl ToolContextFactory for IdentityToolContextFactory {
    fn create_context(&self, function_call_id: &str) -> Arc<dyn ToolContext> {
        let ctx = RealtimeToolContext::new(
            self.identity.app_name.clone(),
            self.identity.user_id.clone(),
            self.identity.session_id.clone(),
            function_call_id.to_string(),
            self.memory_service.clone(),
        );
        Arc::new(match &self.request_context {
            Some(request_context) => ctx.with_request_context(request_context.clone()),
            None => ctx,
        })
    }
}
