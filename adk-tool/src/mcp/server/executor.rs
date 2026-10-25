//! The executor hook every `tools/call` goes through (spec 3.5 §7.4, §7.9).

use std::sync::Arc;

use adk_core::{AdkError, Result, Tool};
use async_trait::async_trait;
use serde_json::Value;

use super::McpCaller;
use crate::SimpleToolContext;

/// One `tools/call` the server is about to run: the tool, its arguments, the caller and the
/// request that carried it.
///
/// The arguments are a JSON object that already matched the tool's parameters schema.
#[derive(Clone)]
#[non_exhaustive]
pub struct McpToolCall {
    /// The tool the call names.
    pub tool: Arc<dyn Tool>,
    /// The call's arguments, a JSON object.
    pub arguments: Value,
    /// The caller the server's [`McpCallerResolver`](super::McpCallerResolver) resolved.
    pub caller: McpCaller,
    /// The JSON-RPC request id, as a string; unique within its session, not across them.
    pub request_id: String,
    /// The MCP session the request belongs to, for stateful sessions.
    pub session_id: Option<String>,
}

impl McpToolCall {
    /// Creates a call, outside a session, for exercising an executor directly.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_tool::FunctionTool;
    /// use adk_tool::mcp::server::{McpCaller, McpToolCall};
    /// use serde_json::json;
    ///
    /// let tool = Arc::new(FunctionTool::new("ping", "Answers pong", |_ctx, _args| async {
    ///     Ok(json!("pong"))
    /// }));
    /// let call = McpToolCall::new(tool, json!({}), McpCaller::anonymous(), "1")
    ///     .with_session_id("session-1");
    /// assert_eq!(call.session_id.as_deref(), Some("session-1"));
    /// ```
    pub fn new(
        tool: Arc<dyn Tool>,
        arguments: Value,
        caller: McpCaller,
        request_id: impl Into<String>,
    ) -> Self {
        Self { tool, arguments, caller, request_id: request_id.into(), session_id: None }
    }

    /// Sets the MCP session the call belongs to.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

impl std::fmt::Debug for McpToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCall")
            .field("tool", &self.tool.name())
            .field("caller", &self.caller.id())
            .field("request_id", &self.request_id)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// How a call that ran ended.
#[derive(Debug)]
#[non_exhaustive]
pub enum McpToolOutcome {
    /// The tool returned this value. An object becomes the result's `structuredContent`; every
    /// value is also sent as text.
    Completed(Value),
    /// The tool ran and failed. The result carries `isError: true` and the error as
    /// `adk_core::tool_error_response` describes it: a rejection's message, otherwise only its
    /// category and code.
    Failed(AdkError),
}

/// Runs, or refuses, each call the server receives (spec 3.5 §7.9).
///
/// The server hands every `tools/call` to a listed, visible tool whose arguments match its
/// schema to [`execute`](Self::execute), so an executor is the one place to route calls
/// through `adk_core::authorize_tool_call` and the action ledger. An `Err` refuses the call:
/// the client receives a JSON-RPC error whose message keeps the error's details back unless
/// the error is a rejection ([`ErrorCategory::is_rejection`](adk_core::ErrorCategory::is_rejection)).
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolEffect};
/// use adk_tool::mcp::server::{
///     DirectExecutor, McpCallExecutor, McpCaller, McpToolCall, McpToolOutcome,
/// };
///
/// /// Anonymous callers may only read.
/// struct ReadOnlyForAnonymous(DirectExecutor);
///
/// #[async_trait::async_trait]
/// impl McpCallExecutor for ReadOnlyForAnonymous {
///     async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
///         if !call.caller.is_authenticated() && call.tool.effect() != ToolEffect::ReadOnly {
///             return Err(AdkError::new(
///                 ErrorComponent::Tool,
///                 ErrorCategory::Forbidden,
///                 "mcp.caller.read_only",
///                 "sign in to make changes",
///             ));
///         }
///         self.0.execute(call).await
///     }
///
///     fn is_visible(&self, tool: &dyn Tool, caller: &McpCaller) -> bool {
///         caller.is_authenticated() || tool.effect() == ToolEffect::ReadOnly
///     }
/// }
/// ```
#[async_trait]
pub trait McpCallExecutor: Send + Sync {
    /// Runs `call`, or refuses it with an error.
    ///
    /// # Errors
    ///
    /// Returns the error the call is refused with; the client receives it as a JSON-RPC error.
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome>;

    /// Whether `caller` may see and call `tool`.
    ///
    /// `tools/list` lists only the tools this allows, and a call to any other tool is answered
    /// as a call to an unknown tool. The default shows every tool.
    fn is_visible(&self, tool: &dyn Tool, caller: &McpCaller) -> bool {
        let _ = (tool, caller);
        true
    }
}

/// The default executor: calls [`Tool::execute`] with a [`SimpleToolContext`].
///
/// The context's app and agent name are the executor's `app_name`, its session is the MCP
/// session, and its function call id is `mcp-{request id}`. A tool error is reported as
/// [`McpToolOutcome::Failed`]; nothing is refused.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::DirectExecutor;
///
/// let executor = DirectExecutor::new("com.example/shop");
/// assert_eq!(executor.app_name(), "com.example/shop");
/// ```
#[derive(Debug, Clone)]
pub struct DirectExecutor {
    app_name: String,
}

impl DirectExecutor {
    /// Creates an executor whose tool contexts carry `app_name`.
    pub fn new(app_name: impl Into<String>) -> Self {
        Self { app_name: app_name.into() }
    }

    /// The app name the tool contexts carry.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

#[async_trait]
impl McpCallExecutor for DirectExecutor {
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
        let mut context = SimpleToolContext::new(self.app_name.clone())
            .with_function_call_id(format!("mcp-{}", call.request_id));
        if let Some(session) = call.session_id {
            context = context.with_session_id(session);
        }
        Ok(match call.tool.execute(Arc::new(context), call.arguments).await {
            Ok(value) => McpToolOutcome::Completed(value),
            Err(error) => McpToolOutcome::Failed(error),
        })
    }
}
