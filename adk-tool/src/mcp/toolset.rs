// MCP (Model Context Protocol) Toolset Integration
//
// Based on Go implementation: adk-go/tool/mcptoolset/
// Uses official Rust SDK: https://github.com/modelcontextprotocol/rust-sdk
//
// The McpToolset connects to an MCP server, discovers available tools,
// and exposes them as ADK-compatible tools for use with LlmAgent.

use super::reconnect::{DEFAULT_RETRY_TOOL_CALLS, should_retry_mcp_operation};
use super::schema_limits::McpSchemaLimits;
use super::task::{McpTaskConfig, TaskError};
use super::{ConnectionFactory, RefreshConfig, should_refresh_connection};
use adk_core::{AdkError, ReadonlyContext, Result, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::StreamExt;
use rmcp::{
    RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientRequest,
        CompletionContext, CompletionInfo, ContentBlock, ErrorCode, GetPromptRequestParams,
        GetPromptResult, GetTaskParams, GetTaskRequest, InputRequest, InputRequests,
        InputResponses, Prompt, ReadResourceRequestParams, Resource, ResourceContents,
        ResourceTemplate, ServerResult, SubscribeRequestParams, TaskPayload, ToolAnnotations,
        UnsubscribeRequestParams, UpdateTaskParams, UpdateTaskRequest,
    },
    service::RunningService,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

/// Shared factory object used to recreate MCP connections for refresh/retry.
type DynConnectionFactory<S> = Arc<dyn ConnectionFactory<S>>;

/// The live connection, swapped wholesale on refresh. The inner `Arc` lets a
/// caller keep using one connection after releasing the lock.
type SharedClient<S> = Arc<Mutex<Arc<RunningService<RoleClient, S>>>>;

/// Longest server-issued task id retained in the pending-task set.
const MAX_TASK_ID_BYTES: usize = 16 * 1024;

/// Most input requests answered in one MRTR or in-task input round.
const MAX_INPUT_REQUESTS_PER_ROUND: usize = 64;

/// Longest prefix of a server-chosen tool name written to a discovery log line.
const MAX_LOGGED_TOOL_NAME_BYTES: usize = 128;

/// Most tasks `cancel_pending_tasks` checks or cancels at once.
const MAX_CONCURRENT_TASK_CLEANUPS: usize = 16;

/// Longest wait for a `tasks/cancel` acknowledgement, so an unresponsive
/// server cannot hold a call far past its own deadline.
const TASK_CANCEL_TIMEOUT: Duration = Duration::from_secs(2);

/// Returns `true` when a task request failed because the server no longer
/// tracks the task, or (for `tasks/cancel`) because it already finished.
/// The specification reports both as `-32602`; `-32002` is the older
/// not-found code.
fn is_unknown_or_finished_task(error: &rmcp::ServiceError) -> bool {
    matches!(
        error,
        rmcp::ServiceError::McpError(error)
            if error.code == ErrorCode::INVALID_PARAMS || error.code == ErrorCode::RESOURCE_NOT_FOUND
    )
}

async fn restore_subscriptions<S: rmcp::service::Service<RoleClient>>(
    client: &RunningService<RoleClient, S>,
    subscriptions: &RwLock<BTreeSet<String>>,
) -> Result<()> {
    for uri in subscriptions.read().await.iter() {
        // Existing resource callbacks use the negotiated legacy subscription API.
        #[allow(deprecated)]
        client.subscribe(SubscribeRequestParams::new(uri.clone())).await.map_err(|error| {
            AdkError::tool(format!("Failed to restore MCP resource subscription '{uri}': {error}"))
        })?;
    }
    Ok(())
}

fn mcp_tool_safety(annotations: Option<&ToolAnnotations>) -> (bool, bool) {
    let read_only = annotations.and_then(|value| value.read_only_hint).unwrap_or(false);
    let idempotent = annotations.and_then(|value| value.idempotent_hint).unwrap_or(false);
    (read_only, read_only || idempotent)
}

/// Preserve every MCP content block in ADK's multimodal tool-result envelope.
/// `FunctionResponseData::from_tool_result` consumes this shape in the agent loop.
fn call_tool_result_to_adk_value(
    result: &rmcp::model::CallToolResult,
) -> std::result::Result<Value, String> {
    let mut text_parts = Vec::new();
    let mut inline_data = Vec::new();
    let mut file_data = Vec::new();

    for content in &result.content {
        match content {
            ContentBlock::Text(text) => text_parts.push(text.text.clone()),
            ContentBlock::Image(image) => {
                let data = STANDARD
                    .decode(&image.data)
                    .map_err(|error| format!("invalid MCP image base64: {error}"))?;
                inline_data.push(json!({ "mime_type": image.mime_type, "data": data }));
            }
            ContentBlock::Audio(audio) => {
                let data = STANDARD
                    .decode(&audio.data)
                    .map_err(|error| format!("invalid MCP audio base64: {error}"))?;
                inline_data.push(json!({ "mime_type": audio.mime_type, "data": data }));
            }
            ContentBlock::Resource(resource) => match &resource.resource {
                ResourceContents::TextResourceContents { uri, mime_type, text, .. } => {
                    text_parts.push(text.clone());
                    file_data.push(json!({
                        "mime_type": mime_type.as_deref().unwrap_or("text/plain"),
                        "file_uri": uri,
                    }));
                }
                ResourceContents::BlobResourceContents { uri, mime_type, blob, .. } => {
                    let data = STANDARD
                        .decode(blob)
                        .map_err(|error| format!("invalid MCP resource base64: {error}"))?;
                    inline_data.push(json!({
                        "mime_type": mime_type.as_deref().unwrap_or("application/octet-stream"),
                        "data": data,
                    }));
                    file_data.push(json!({
                        "mime_type": mime_type.as_deref().unwrap_or("application/octet-stream"),
                        "file_uri": uri,
                    }));
                }
                _ => return Err("unsupported MCP embedded resource content".to_string()),
            },
            ContentBlock::ResourceLink(link) => file_data.push(json!({
                "mime_type": link.mime_type.as_deref().unwrap_or("application/octet-stream"),
                "file_uri": link.uri,
            })),
            _ => {}
        }
    }

    let output = match (&result.structured_content, text_parts.is_empty()) {
        (Some(structured), true) => json!({ "output": structured }),
        (Some(structured), false) => json!({ "output": structured, "text": text_parts }),
        (None, false) => json!({ "output": text_parts.join("\n") }),
        (None, true) if !inline_data.is_empty() || !file_data.is_empty() => Value::Null,
        (None, true) => return Err("MCP tool returned no content".to_string()),
    };

    if inline_data.is_empty() && file_data.is_empty() {
        Ok(output)
    } else {
        Ok(json!({
            "response": output,
            "inline_data": inline_data,
            "file_data": file_data,
        }))
    }
}

/// Type alias for tool filter predicate
pub type ToolFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

fn mcp_tool_call_error(
    tool_name: &str,
    error: &str,
    has_connection_factory: bool,
    replay_allowed: bool,
) -> AdkError {
    if has_connection_factory && !replay_allowed && should_refresh_connection(error) {
        AdkError::tool(format!(
            "MCP tool '{tool_name}' result is uncertain and was not replayed: {error}. \
             Enable tool-call retries only for replay-safe operations"
        ))
    } else {
        AdkError::tool(format!("Failed to call MCP tool '{tool_name}': {error}"))
    }
}

/// Returns `true` when the `ServiceError` wraps an MCP `MethodNotFound` (-32601)
/// JSON-RPC error, indicating the server does not implement the requested method.
fn is_method_not_found(err: &rmcp::ServiceError) -> bool {
    matches!(
        err,
        rmcp::ServiceError::McpError(e) if e.code == ErrorCode::METHOD_NOT_FOUND
    )
}

/// MCP Toolset - connects to an MCP server and exposes its tools as ADK tools.
///
/// This toolset implements the ADK `Toolset` trait and bridges the gap between
/// MCP servers and ADK agents. It:
/// 1. Connects to an MCP server via the provided transport
/// 2. Discovers available tools from the server
/// 3. Converts MCP tools to ADK-compatible `Tool` implementations
/// 4. Proxies tool execution calls to the MCP server
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::{
///     McpToolset,
///     mcp::rmcp::{ServiceExt, transport::TokioChildProcess},
/// };
/// use tokio::process::Command;
///
/// // Create MCP client connection to a local server
/// let client = ().serve(TokioChildProcess::new(
///     Command::new("/opt/company/bin/workspace-mcp")
///         .arg("--stdio")
///         .arg("--root")
///         .arg("/srv/workspace")
/// )?).await?;
///
/// // Create toolset from the client
/// let toolset = McpToolset::new(client);
///
/// // Add to agent
/// let agent = LlmAgentBuilder::new("assistant")
///     .toolset(Arc::new(toolset))
///     .build()?;
/// ```
pub struct McpToolset<S = ()>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// The running MCP client service
    client: SharedClient<S>,
    /// Optional filter to select which tools to expose
    tool_filter: Option<ToolFilter>,
    /// Name of this toolset
    name: String,
    /// Task configuration for long-running operations
    task_config: McpTaskConfig,
    /// Remote tasks whose terminal state has not been observed.
    active_tasks: Arc<Mutex<BTreeSet<String>>>,
    /// Optional connection factory used for reconnection on transport failures.
    connection_factory: Option<DynConnectionFactory<S>>,
    /// Reconnection/retry configuration.
    refresh_config: RefreshConfig,
    /// Whether ambiguous tool-call outcomes may be replayed after reconnection.
    retry_tool_calls: bool,
    /// Resource subscriptions restored after an automatic connection refresh.
    resource_subscriptions: Arc<RwLock<BTreeSet<String>>>,
    /// Policy bridge used to fulfil stateless MRTR and in-task input requests.
    mrtr_handler: Option<super::elicitation::AdkClientHandler>,
    /// Size limits checked against each discovered tool schema.
    schema_limits: McpSchemaLimits,
}

impl<S> Clone for McpToolset<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            tool_filter: self.tool_filter.clone(),
            name: self.name.clone(),
            task_config: self.task_config.clone(),
            active_tasks: self.active_tasks.clone(),
            connection_factory: self.connection_factory.clone(),
            refresh_config: self.refresh_config.clone(),
            retry_tool_calls: self.retry_tool_calls,
            resource_subscriptions: Arc::clone(&self.resource_subscriptions),
            mrtr_handler: self.mrtr_handler.clone(),
            schema_limits: self.schema_limits,
        }
    }
}

impl<S> McpToolset<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// Create a new MCP toolset from a running MCP client service.
    ///
    /// The client should already be connected and initialized.
    /// Use `adk_tool::mcp::rmcp::ServiceExt::serve()` to create the client.
    ///
    /// MRTR and in-task input requests follow ADK's input policy when the client
    /// runs on [`AdkClientHandler`](super::AdkClientHandler), which rejects
    /// sampling and roots. Any other handler receives elicitation requests, and
    /// sampling or roots requests only when it declared those capabilities.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_tool::mcp::rmcp::{ServiceExt, transport::TokioChildProcess};
    /// use tokio::process::Command;
    ///
    /// let client = ().serve(TokioChildProcess::new(
    ///     Command::new("my-mcp-server")
    /// )?).await?;
    ///
    /// let toolset = McpToolset::new(client);
    /// ```
    pub fn new(client: RunningService<RoleClient, S>) -> Self {
        // A client served on ADK's own handler keeps ADK's MRTR input policy, however
        // it was built, so a configured sampling handler stays unreachable through MRTR.
        let mrtr_handler = (client.service() as &dyn std::any::Any)
            .downcast_ref::<super::elicitation::AdkClientHandler>()
            .cloned();
        Self {
            client: Arc::new(Mutex::new(Arc::new(client))),
            tool_filter: None,
            name: "mcp_toolset".to_string(),
            task_config: McpTaskConfig::default(),
            active_tasks: Arc::new(Mutex::new(BTreeSet::new())),
            connection_factory: None,
            refresh_config: RefreshConfig::default(),
            retry_tool_calls: DEFAULT_RETRY_TOOL_CALLS,
            resource_subscriptions: Arc::new(RwLock::new(BTreeSet::new())),
            mrtr_handler,
            schema_limits: McpSchemaLimits::default(),
        }
    }

    /// Create a McpToolset from a RunningService with a custom ClientHandler.
    ///
    /// This is functionally identical to `new()` but makes the intent explicit
    /// when using a custom `ClientHandler` type.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_tool::{McpToolset, mcp::rmcp::ServiceExt};
    ///
    /// let client = my_custom_handler.serve(transport).await?;
    /// let toolset = McpToolset::with_client_handler(client);
    /// ```
    pub fn with_client_handler(client: RunningService<RoleClient, S>) -> Self {
        Self::new(client)
    }

    /// Set a custom name for this toolset.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Enable negotiated MCP task support for long-running operations.
    ///
    /// A tool declared with required task support always uses the task flow.
    /// A tool declaring optional task support uses it when this configuration
    /// is enabled and the server negotiated `tasks.requests.tools.call`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let toolset = McpToolset::new(client)
    ///     .with_task_support(McpTaskConfig::enabled()
    ///         .poll_interval(Duration::from_secs(2))
    ///         .timeout(Duration::from_secs(300)));
    /// ```
    pub fn with_task_support(mut self, config: McpTaskConfig) -> Self {
        self.task_config = config;
        self
    }

    /// Set the size limits checked against each discovered tool schema.
    ///
    /// Discovery measures every input and output schema before copying or logging
    /// it. A tool whose schema exceeds a limit is skipped with a warning naming
    /// this toolset and the tool, and the remaining tools are still registered.
    /// See [`McpSchemaLimits`] for how size is measured and why the defaults are
    /// what they are.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::mcp::{McpSchemaLimits, McpToolset};
    ///
    /// fn admit_larger_schemas(toolset: McpToolset) -> McpToolset {
    ///     toolset.with_schema_limits(
    ///         McpSchemaLimits::default().with_max_bytes(1024 * 1024).with_max_nodes(40_000),
    ///     )
    /// }
    /// ```
    pub fn with_schema_limits(mut self, limits: McpSchemaLimits) -> Self {
        self.schema_limits = limits;
        self
    }

    /// Reuse the connection's client policy for stateless MRTR and task input.
    pub(crate) fn with_mrtr_handler(
        mut self,
        handler: super::elicitation::AdkClientHandler,
    ) -> Self {
        self.mrtr_handler = Some(handler);
        self
    }

    /// Provide a connection factory to enable automatic MCP reconnection.
    pub fn with_connection_factory<F>(mut self, factory: Arc<F>) -> Self
    where
        F: ConnectionFactory<S> + 'static,
    {
        self.connection_factory = Some(factory);
        self
    }

    /// Configure MCP reconnect/retry behavior.
    pub fn with_refresh_config(mut self, config: RefreshConfig) -> Self {
        self.refresh_config = config;
        self
    }

    /// Allow MCP tool calls to be replayed after reconnecting.
    ///
    /// A transport failure after request transmission is an ambiguous outcome:
    /// a mutating tool may have completed its external effect before the
    /// response was lost. Enable this only for read-only tools or operations
    /// protected by a stable provider idempotency guarantee. Discovery and
    /// resource operations keep their normal reconnect behavior without this.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let toolset = McpToolset::new(client)
    ///     .with_connection_factory(Arc::new(factory))
    ///     .with_tool_call_retries();
    /// ```
    pub fn with_tool_call_retries(mut self) -> Self {
        self.retry_tool_calls = true;
        self
    }

    /// Add a filter to select which tools to expose.
    ///
    /// The filter function receives a tool name and returns true if the tool
    /// should be included.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let toolset = McpToolset::new(client)
    ///     .with_filter(|name| {
    ///         matches!(name, "read_file" | "list_directory" | "search_files")
    ///     });
    /// ```
    pub fn with_filter<F>(mut self, filter: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.tool_filter = Some(Arc::new(filter));
        self
    }

    /// Add a filter that only includes tools with the specified names.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let toolset = McpToolset::new(client)
    ///     .with_tools(&["read_file", "write_file"]);
    /// ```
    pub fn with_tools(self, tool_names: &[&str]) -> Self {
        let names: Vec<String> = tool_names.iter().map(|s| s.to_string()).collect();
        self.with_filter(move |name| names.iter().any(|n| n == name))
    }

    /// Get a cancellation token that can be used to shutdown the MCP server.
    ///
    /// Call `cancel()` on the returned token to cleanly shutdown the MCP server.
    /// This should be called before exiting to avoid EPIPE errors.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let toolset = McpToolset::new(client);
    /// let cancel_token = toolset.cancellation_token().await;
    ///
    /// // ... use the toolset ...
    ///
    /// // Before exiting:
    /// cancel_token.cancel();
    /// ```
    pub async fn cancellation_token(&self) -> rmcp::service::RunningServiceCancellationToken {
        let client = self.client.lock().await;
        client.cancellation_token()
    }

    /// Check whether the underlying MCP service connection has been closed or cancelled.
    ///
    /// Returns `true` if the service loop has terminated (transport closed,
    /// cancellation token fired, or the background task completed). This is
    /// useful for health monitoring — a closed connection indicates the server
    /// process has crashed or the transport has been lost.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if toolset.is_closed().await {
    ///     tracing::warn!("MCP server connection lost");
    /// }
    /// ```
    pub async fn is_closed(&self) -> bool {
        let client = self.client.lock().await;
        client.is_closed()
    }

    /// Call one MCP tool and preserve structured, text, image, audio, and resource content
    /// in the same ADK multimodal value shape used by model-facing tool execution.
    pub async fn call_tool_value(
        &self,
        name: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<Value> {
        let server_supports_tasks = self
            .client
            .lock()
            .await
            .peer_info()
            .is_some_and(|info| info.capabilities.supports_tasks());
        McpTool {
            name: name.into(),
            description: String::new(),
            input_schema: None,
            output_schema: None,
            client: self.client.clone(),
            connection_factory: self.connection_factory.clone(),
            refresh_config: self.refresh_config.clone(),
            retry_tool_calls: self.retry_tool_calls,
            annotations: None,
            server_supports_tasks,
            task_config: self.task_config.clone(),
            active_tasks: self.active_tasks.clone(),
            mrtr_handler: self.mrtr_handler.clone(),
            resource_subscriptions: self.resource_subscriptions.clone(),
        }
        .execute_value(Value::Object(arguments))
        .await
    }

    /// Cancels every remote task this toolset started and has not seen finish.
    ///
    /// The set covers tasks whose tool-call future was dropped and tasks an
    /// in-flight call is still polling, so this also cancels in-flight calls:
    /// each one returns a task error once the server reports the cancellation.
    ///
    /// Each task is checked with `tasks/get` first. A task that is already
    /// terminal, or that the server no longer knows (its TTL expired, or the
    /// connection was refreshed into a new session), is released without a
    /// cancel request. Any other task receives `tasks/cancel` and a second
    /// `tasks/get`. A task stays tracked until a terminal status or an
    /// unknown-task response confirms it, so calling this again re-checks it.
    ///
    /// Nothing else prunes the set: every task whose call ended before it saw a
    /// terminal status stays tracked until this runs, so a long-lived toolset
    /// that drops calls should call it periodically as well as at shutdown.
    ///
    /// Cancellation is cooperative on the server side, so bound this call
    /// with the shutdown deadline. [`McpServerManager`](super::McpServerManager)
    /// runs it within its grace period before closing a managed session.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` naming a task whose outcome is not confirmed:
    /// `tasks/get` or `tasks/cancel` failed for a reason other than an unknown
    /// or finished task, or the task is still running after the cancel request.
    /// Tasks confirmed by the same call are released regardless.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_tool::mcp::McpToolset;
    /// use std::time::Duration;
    ///
    /// async fn shutdown(toolset: &McpToolset) {
    ///     let cleanup = toolset.cancel_pending_tasks();
    ///     match tokio::time::timeout(Duration::from_secs(5), cleanup).await {
    ///         Ok(Ok(())) => {}
    ///         Ok(Err(error)) => tracing::warn!(%error, "remote MCP tasks may still run"),
    ///         Err(_) => tracing::warn!("MCP task cleanup exceeded the shutdown deadline"),
    ///     }
    ///     toolset.cancellation_token().await.cancel();
    /// }
    /// ```
    pub async fn cancel_pending_tasks(&self) -> Result<()> {
        let ids: Vec<_> = self.active_tasks.lock().await.iter().cloned().collect();
        if ids.is_empty() {
            return Ok(());
        }
        let client = self.client.lock().await.peer().clone();
        let settled = |id: String| {
            let client = client.clone();
            async move {
                match client.get_task(GetTaskParams::new(&id)).await {
                    Ok(status) if status.task.task.task_id != id => Err(AdkError::tool(format!(
                        "MCP server answered tasks/get for task '{id}' with task '{}'",
                        status.task.task.task_id
                    ))),
                    Ok(status) => Ok(status.task.status().is_terminal()),
                    Err(error) if is_unknown_or_finished_task(&error) => Ok(true),
                    Err(error) => Err(AdkError::tool(format!(
                        "MCP task '{id}' status is unavailable: {error}"
                    ))),
                }
            }
        };
        let results: Vec<Result<()>> = futures::stream::iter(ids)
            .map(|id| {
                let client = &client;
                let settled = &settled;
                async move {
                    if !settled(id.clone()).await? {
                        if let Err(error) = client.cancel_task(CancelTaskParams::new(&id)).await
                            && !is_unknown_or_finished_task(&error)
                        {
                            return Err(AdkError::tool(format!(
                                "MCP task '{id}' cancellation outcome is unknown: {error}"
                            )));
                        }
                        if !settled(id.clone()).await? {
                            return Err(AdkError::tool(format!(
                                "MCP task '{id}' has not confirmed termination after tasks/cancel; \
                             call cancel_pending_tasks again to re-check it"
                            )));
                        }
                    }
                    self.active_tasks.lock().await.remove(&id);
                    Ok(())
                }
            })
            .buffer_unordered(MAX_CONCURRENT_TASK_CLEANUPS)
            .collect()
            .await;
        for result in results {
            result?;
        }
        Ok(())
    }

    async fn try_refresh_connection(&self) -> Result<bool> {
        let Some(factory) = self.connection_factory.clone() else {
            return Ok(false);
        };

        let new_client = factory
            .create_connection()
            .await
            .map_err(|e| AdkError::tool(format!("Failed to refresh MCP connection: {e}")))?;

        restore_subscriptions(&new_client, &self.resource_subscriptions).await?;

        let mut client = self.client.lock().await;
        let old_token = client.cancellation_token();
        old_token.cancel();
        *client = Arc::new(new_client);
        Ok(true)
    }

    /// List static resources from the connected MCP server.
    ///
    /// Returns the list of resources advertised by the server via the
    /// `resources/list` protocol method. Returns an empty `Vec` when the
    /// server does not support resources (i.e. responds with
    /// `MethodNotFound`).
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` on transport or unexpected server errors.
    pub async fn list_resources(&self) -> Result<Vec<Resource>> {
        let client = self.client.lock().await;
        match client.list_all_resources().await {
            Ok(resources) => Ok(resources),
            Err(e) => {
                if is_method_not_found(&e) {
                    Ok(vec![])
                } else {
                    Err(AdkError::tool(format!("Failed to list MCP resources: {e}")))
                }
            }
        }
    }

    /// List URI template resources from the connected MCP server.
    ///
    /// Returns the list of resource templates advertised by the server via
    /// the `resourceTemplates/list` protocol method. Returns an empty `Vec`
    /// when the server does not support resource templates (i.e. responds
    /// with `MethodNotFound`).
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool` on transport or unexpected server errors.
    pub async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>> {
        let client = self.client.lock().await;
        match client.list_all_resource_templates().await {
            Ok(templates) => Ok(templates),
            Err(e) => {
                if is_method_not_found(&e) {
                    Ok(vec![])
                } else {
                    Err(AdkError::tool(format!("Failed to list MCP resource templates: {e}")))
                }
            }
        }
    }

    /// Read a resource by URI from the connected MCP server.
    ///
    /// Delegates to the `resources/read` protocol method. Returns the
    /// resource contents on success.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Tool("resource not found: {uri}")` when the URI
    /// does not match any resource on the server. Returns a generic
    /// `AdkError::Tool` on transport or other server errors.
    pub async fn read_resource(&self, uri: &str) -> Result<Vec<ResourceContents>> {
        let client = self.client.lock().await;
        let params = ReadResourceRequestParams::new(uri.to_string());
        match client.read_resource(params).await {
            Ok(result) => Ok(result.contents),
            Err(e) => {
                if is_method_not_found(&e) {
                    Err(AdkError::tool(format!("resource not found: {uri}")))
                } else {
                    Err(AdkError::tool(format!("Failed to read MCP resource '{uri}': {e}")))
                }
            }
        }
    }

    /// Return the prompt templates published by the connected MCP server.
    pub async fn list_prompts(&self) -> Result<Vec<Prompt>> {
        let client = self.client.lock().await;
        match client.list_all_prompts().await {
            Ok(prompts) => Ok(prompts),
            Err(error) if is_method_not_found(&error) => Ok(Vec::new()),
            Err(error) => Err(AdkError::tool(format!("failed to list MCP prompts: {error}"))),
        }
    }

    /// Resolve one published MCP prompt with optional typed arguments.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, Value>>,
    ) -> Result<GetPromptResult> {
        let mut params = GetPromptRequestParams::new(name);
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let client = self.client.lock().await;
        client
            .get_prompt(params)
            .await
            .map_err(|error| AdkError::tool(format!("failed to get MCP prompt '{name}': {error}")))
    }

    /// Request completion suggestions for one prompt argument.
    pub async fn complete_prompt_argument(
        &self,
        prompt_name: &str,
        argument_name: &str,
        current_value: &str,
        context: Option<CompletionContext>,
    ) -> Result<CompletionInfo> {
        let client = self.client.lock().await;
        client
            .complete_prompt_argument(prompt_name, argument_name, current_value, context)
            .await
            .map_err(|error| {
                AdkError::tool(format!(
                    "failed to complete MCP prompt argument '{argument_name}': {error}"
                ))
            })
    }

    /// Request completion suggestions for one resource-template argument.
    pub async fn complete_resource_argument(
        &self,
        uri_template: &str,
        argument_name: &str,
        current_value: &str,
        context: Option<CompletionContext>,
    ) -> Result<CompletionInfo> {
        let client = self.client.lock().await;
        client
            .complete_resource_argument(uri_template, argument_name, current_value, context)
            .await
            .map_err(|error| {
                AdkError::tool(format!(
                    "failed to complete MCP resource argument '{argument_name}': {error}"
                ))
            })
    }

    /// Subscribe to change notifications for a resource URI.
    pub async fn subscribe_resource(&self, uri: &str) -> Result<()> {
        let client = self.client.lock().await;
        // `subscriptions/listen` replaces this in 2026-07-28, but we negotiate
        // 2025-11-25, and `listen` also stops routing notifications through
        // `ClientHandler`, which this crate's resource callbacks rely on.
        #[allow(deprecated)]
        client.subscribe(SubscribeRequestParams::new(uri)).await.map_err(|error| {
            AdkError::tool(format!("failed to subscribe to MCP resource '{uri}': {error}"))
        })?;
        self.resource_subscriptions.write().await.insert(uri.to_string());
        Ok(())
    }

    /// Remove a resource subscription created by [`subscribe_resource`](Self::subscribe_resource).
    pub async fn unsubscribe_resource(&self, uri: &str) -> Result<()> {
        let client = self.client.lock().await;
        // Paired with `subscribe_resource`; see the note there.
        #[allow(deprecated)]
        client.unsubscribe(UnsubscribeRequestParams::new(uri)).await.map_err(|error| {
            AdkError::tool(format!("failed to unsubscribe MCP resource '{uri}': {error}"))
        })?;
        self.resource_subscriptions.write().await.remove(uri);
        Ok(())
    }
}

#[async_trait]
impl<S> Toolset for McpToolset<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let mut attempt = 0u32;
        let has_connection_factory = self.connection_factory.is_some();
        let mcp_tools = loop {
            let list_result = {
                let client = self.client.lock().await;
                client.list_all_tools().await.map_err(|e| e.to_string())
            };

            match list_result {
                Ok(tools) => break tools,
                Err(error) => {
                    if !should_retry_mcp_operation(
                        &error,
                        attempt,
                        &self.refresh_config,
                        has_connection_factory,
                        true,
                    ) {
                        return Err(AdkError::tool(format!("Failed to list MCP tools: {error}")));
                    }

                    let retry_attempt = attempt + 1;
                    if self.refresh_config.log_reconnections {
                        warn!(
                            attempt = retry_attempt,
                            max_attempts = self.refresh_config.max_attempts,
                            error = %error,
                            "MCP list_all_tools failed; reconnecting and retrying"
                        );
                    }

                    if self.refresh_config.retry_delay_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            self.refresh_config.retry_delay_ms,
                        ))
                        .await;
                    }

                    if !self.try_refresh_connection().await? {
                        return Err(AdkError::tool(format!("Failed to list MCP tools: {error}")));
                    }
                    attempt += 1;
                }
            }
        };

        // Convert MCP tools to ADK tools
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let server_supports_tasks = {
            let client = self.client.lock().await;
            client.peer_info().is_some_and(|info| info.capabilities.supports_tasks())
        };

        for mcp_tool in mcp_tools {
            let tool_name = mcp_tool.name.to_string();

            // Apply filter if present
            if let Some(ref filter) = self.tool_filter
                && !filter(&tool_name)
            {
                continue;
            }

            // The server chooses the name as well as the schema, so the log keeps a prefix.
            let logged_name =
                &tool_name[..tool_name.floor_char_boundary(MAX_LOGGED_TOOL_NAME_BYTES)];
            // Measured from the server's document before anything copies or logs it.
            let limits = self.schema_limits;
            let measured = limits
                .measure(&mcp_tool.input_schema)
                .map_err(|size| ("input", size))
                .and_then(|input| {
                    mcp_tool
                        .output_schema
                        .as_deref()
                        .map(|schema| limits.measure(schema))
                        .transpose()
                        .map(|output| (input, output))
                        .map_err(|size| ("output", size))
                });
            let (input_size, output_size) = match measured {
                Ok(sizes) => sizes,
                Err((kind, size)) => {
                    warn!(
                        toolset.name = %self.name,
                        tool.name = logged_name,
                        schema.kind = kind,
                        schema.bytes = size.bytes,
                        schema.nodes = size.nodes,
                        schema.max_bytes = limits.max_bytes,
                        schema.max_nodes = limits.max_nodes,
                        "skipping MCP tool whose schema exceeds the size limits; \
                         raise them with McpToolset::with_schema_limits if the server is trusted"
                    );
                    continue;
                }
            };
            debug!(
                toolset.name = %self.name,
                tool.name = logged_name,
                schema.bytes = input_size.bytes,
                schema.nodes = input_size.nodes,
                output_schema.bytes = output_size.map(|size| size.bytes),
                output_schema.nodes = output_size.map(|size| size.nodes),
                "registering MCP tool"
            );
            let adk_tool = McpTool {
                name: tool_name,
                description: mcp_tool.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: Some(Value::Object(Arc::unwrap_or_clone(mcp_tool.input_schema))),
                output_schema: mcp_tool
                    .output_schema
                    .map(|schema| Value::Object(Arc::unwrap_or_clone(schema))),
                client: self.client.clone(),
                connection_factory: self.connection_factory.clone(),
                refresh_config: self.refresh_config.clone(),
                retry_tool_calls: self.retry_tool_calls,
                annotations: mcp_tool.annotations,
                server_supports_tasks,
                task_config: self.task_config.clone(),
                active_tasks: self.active_tasks.clone(),
                mrtr_handler: self.mrtr_handler.clone(),
                resource_subscriptions: self.resource_subscriptions.clone(),
            };

            tools.push(Arc::new(adk_tool) as Arc<dyn Tool>);
        }

        Ok(tools)
    }
}

impl McpToolset<super::elicitation::AdkClientHandler> {
    /// Create a McpToolset with elicitation support from a transport.
    ///
    /// This creates the MCP client using `AdkClientHandler`, which advertises
    /// elicitation capabilities and delegates requests to the provided handler.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_tool::{McpToolset, ElicitationHandler, AutoDeclineElicitationHandler};
    /// use adk_tool::mcp::rmcp::transport::TokioChildProcess;
    /// use tokio::process::Command;
    /// use std::sync::Arc;
    ///
    /// let transport = TokioChildProcess::new(Command::new("my-mcp-server"))?;
    /// let handler = Arc::new(AutoDeclineElicitationHandler);
    /// let toolset = McpToolset::with_elicitation_handler(transport, handler).await?;
    /// ```
    ///
    /// # ConnectionFactory with Elicitation
    ///
    /// To preserve elicitation across reconnections, clone the `Arc<dyn ElicitationHandler>`
    /// into your `ConnectionFactory` implementation:
    ///
    /// ```rust,ignore
    /// use adk_tool::{McpToolset, ElicitationHandler};
    /// use adk_tool::mcp::ConnectionFactory;
    /// use adk_tool::mcp::rmcp::{
    ///     ServiceExt,
    ///     service::{RoleClient, RunningService},
    ///     transport::TokioChildProcess,
    /// };
    /// use tokio::process::Command;
    /// use std::sync::Arc;
    ///
    /// struct MyReconnectFactory {
    ///     handler: Arc<dyn ElicitationHandler>,
    ///     server_command: String,
    /// }
    ///
    /// // The factory creates a fresh AdkClientHandler on each reconnection,
    /// // so the new connection advertises elicitation capabilities.
    /// // The ConnectionFactory trait itself is unchanged.
    /// ```
    pub async fn with_elicitation_handler<T, E, A>(
        transport: T,
        handler: std::sync::Arc<dyn super::elicitation::ElicitationHandler>,
    ) -> Result<Self>
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        use rmcp::ServiceExt;
        let adk_handler = super::elicitation::AdkClientHandler::new(handler);
        let client = adk_handler
            .serve(transport)
            .await
            .map_err(|e| AdkError::tool(format!("failed to connect MCP server: {e}")))?;
        Ok(Self::new(client))
    }

    /// Create an MCP toolset with elicitation and resource notification handlers.
    ///
    /// Both handlers are installed before the protocol handshake, so resource
    /// update notifications can be received immediately after subscribing.
    pub async fn with_handlers<T, E, A>(
        transport: T,
        elicitation_handler: std::sync::Arc<dyn super::elicitation::ElicitationHandler>,
        resource_notification_handler: std::sync::Arc<
            dyn super::resource_notifications::ResourceNotificationHandler,
        >,
    ) -> Result<Self>
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        use rmcp::ServiceExt;
        let adk_handler = super::elicitation::AdkClientHandler::new(elicitation_handler)
            .with_resource_notification_handler(resource_notification_handler);
        let client = adk_handler
            .serve(transport)
            .await
            .map_err(|error| AdkError::tool(format!("failed to connect MCP server: {error}")))?;
        Ok(Self::new(client))
    }

    /// Create a McpToolset with MCP sampling support from a transport.
    ///
    /// This creates the MCP client using `AdkClientHandler`, which advertises
    /// both elicitation and sampling capabilities. When the connected MCP server
    /// sends a `sampling/createMessage` request, it is delegated to the provided
    /// [`SamplingHandler`](crate::sampling::SamplingHandler).
    ///
    /// An elicitation handler is also required because `AdkClientHandler` always
    /// advertises elicitation. Use [`AutoDeclineElicitationHandler`](super::elicitation::AutoDeclineElicitationHandler) if you don't
    /// need custom elicitation behavior.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_tool::{McpToolset, AutoDeclineElicitationHandler};
    /// use adk_tool::sampling::LlmSamplingHandler;
    /// use adk_tool::mcp::rmcp::transport::TokioChildProcess;
    /// use tokio::process::Command;
    /// use std::sync::Arc;
    ///
    /// let transport = TokioChildProcess::new(Command::new("my-mcp-server"))?;
    /// let elicitation = Arc::new(AutoDeclineElicitationHandler);
    /// let sampling = Arc::new(LlmSamplingHandler::new(my_llm.clone()));
    /// let toolset = McpToolset::with_sampling_handler(transport, elicitation, sampling).await?;
    /// ```
    ///
    /// # ConnectionFactory with Sampling
    ///
    /// To preserve sampling across reconnections, clone both handler `Arc`s
    /// into your `ConnectionFactory` implementation and rebuild the
    /// `AdkClientHandler` on each reconnection.
    #[cfg(feature = "mcp-sampling")]
    pub async fn with_sampling_handler<T, E, A>(
        transport: T,
        elicitation_handler: std::sync::Arc<dyn super::elicitation::ElicitationHandler>,
        sampling_handler: std::sync::Arc<dyn crate::sampling::SamplingHandler>,
    ) -> Result<Self>
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        use rmcp::ServiceExt;
        let adk_handler = super::elicitation::AdkClientHandler::new(elicitation_handler)
            .with_sampling_handler(sampling_handler);
        let client = adk_handler
            .serve(transport)
            .await
            .map_err(|e| AdkError::tool(format!("failed to connect MCP server: {e}")))?;
        Ok(Self::new(client))
    }

    /// Create a toolset with elicitation, sampling, and resource notifications.
    #[cfg(feature = "mcp-sampling")]
    pub async fn with_sampling_and_resource_handlers<T, E, A>(
        transport: T,
        elicitation_handler: std::sync::Arc<dyn super::elicitation::ElicitationHandler>,
        sampling_handler: std::sync::Arc<dyn crate::sampling::SamplingHandler>,
        resource_notification_handler: std::sync::Arc<
            dyn super::resource_notifications::ResourceNotificationHandler,
        >,
    ) -> Result<Self>
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        use rmcp::ServiceExt;
        let adk_handler = super::elicitation::AdkClientHandler::new(elicitation_handler)
            .with_sampling_handler(sampling_handler)
            .with_resource_notification_handler(resource_notification_handler);
        let client = adk_handler
            .serve(transport)
            .await
            .map_err(|error| AdkError::tool(format!("failed to connect MCP server: {error}")))?;
        Ok(Self::new(client))
    }
}

/// Individual MCP tool wrapper that implements the ADK `Tool` trait.
///
/// This struct wraps an MCP tool and proxies execution calls to the MCP server.
struct McpTool<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    name: String,
    description: String,
    input_schema: Option<Value>,
    output_schema: Option<Value>,
    client: SharedClient<S>,
    connection_factory: Option<DynConnectionFactory<S>>,
    refresh_config: RefreshConfig,
    retry_tool_calls: bool,
    /// Safety hints published by the MCP server for this tool.
    annotations: Option<ToolAnnotations>,
    /// Whether the negotiated server capabilities permit task-augmented tool calls.
    server_supports_tasks: bool,
    /// Task configuration
    task_config: McpTaskConfig,
    active_tasks: Arc<Mutex<BTreeSet<String>>>,
    resource_subscriptions: Arc<RwLock<BTreeSet<String>>>,
    /// Policy bridge used to fulfil MRTR input without keeping server state.
    mrtr_handler: Option<super::elicitation::AdkClientHandler>,
}

impl<S> McpTool<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    async fn execute_value(&self, args: Value) -> Result<Value> {
        let mut params = CallToolRequestParams::new(self.name.clone());
        if !(args.is_null() || args == json!({})) {
            match args {
                Value::Object(map) => params = params.with_arguments(map),
                _ => return Err(AdkError::tool("Tool arguments must be an object")),
            }
        }

        // SEP-2663 moved the task decision to the server, so one request shape
        // covers both modes and the response says which one happened.
        let result = match self.call_tool_with_retry(params).await? {
            CallToolResponse::Complete(result) => result,
            CallToolResponse::Task(created) => {
                let task_id = created.task.task_id.clone();
                if task_id.is_empty() || task_id.len() > MAX_TASK_ID_BYTES {
                    return Err(AdkError::tool(format!(
                        "MCP tool '{}' returned a task id of {} bytes; expected 1 to \
                         {MAX_TASK_ID_BYTES}",
                        self.name,
                        task_id.len()
                    )));
                }
                self.active_tasks.lock().await.insert(task_id.clone());
                if !self.task_config.enable_tasks || !self.server_supports_tasks {
                    // Nothing polls this task, so stop it rather than leave it running remotely.
                    self.cancel_task(&task_id).await;
                    let remedy = if self.task_config.enable_tasks {
                        "the server did not negotiate the tasks capability"
                    } else {
                        "task support is disabled on this toolset; enable it with \
                         `McpToolset::with_task_support(McpTaskConfig::enabled())`"
                    };
                    return Err(AdkError::tool(format!(
                        "MCP tool '{}' returned task '{task_id}', which was cancelled because \
                         {remedy}",
                        self.name
                    )));
                }
                debug!(tool = self.name, task_id, "MCP server materialized a task");
                return self
                    .poll_task(created.task)
                    .await
                    .map_err(|error| AdkError::tool(format!("Task execution failed: {error}")));
            }
            CallToolResponse::InputRequired(_) => {
                return Err(AdkError::tool(format!(
                    "MCP tool '{}' returned an unresolved MRTR input request",
                    self.name
                )));
            }
            response => {
                return Err(AdkError::tool(format!(
                    "MCP tool '{}' returned an unsupported response: {response:?}",
                    self.name
                )));
            }
        };

        if result.is_error.unwrap_or(false) {
            let detail = match result.content.iter().find_map(|content| content.as_text()) {
                Some(text) => text.text.clone(),
                // Structured-only errors carry their detail outside the text blocks.
                None => call_tool_result_to_adk_value(&result)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|error| error),
            };
            return Err(AdkError::tool(format!(
                "MCP tool '{}' execution failed: {detail}",
                self.name
            )));
        }

        call_tool_result_to_adk_value(&result).map_err(|error| {
            AdkError::tool(format!("MCP tool '{}' result invalid: {error}", self.name))
        })
    }

    async fn try_refresh_connection(&self) -> Result<bool> {
        let Some(factory) = self.connection_factory.clone() else {
            return Ok(false);
        };

        let new_client = factory
            .create_connection()
            .await
            .map_err(|e| AdkError::tool(format!("Failed to refresh MCP connection: {e}")))?;

        restore_subscriptions(&new_client, &self.resource_subscriptions).await?;
        let mut client = self.client.lock().await;
        let old_token = client.cancellation_token();
        old_token.cancel();
        *client = Arc::new(new_client);
        Ok(true)
    }

    /// Sends `tools/call` and returns the response envelope unchanged.
    ///
    /// Uses `call_tool_once` rather than `call_tool`: the latter fulfils SEP-2322
    /// `input_required` rounds on its own and rejects a task response outright,
    /// which would break every server that materializes a task.
    async fn call_tool_with_retry(
        &self,
        mut params: CallToolRequestParams,
    ) -> Result<CallToolResponse> {
        let has_connection_factory = self.connection_factory.is_some();
        let (_, metadata_allows_replay) = mcp_tool_safety(self.annotations.as_ref());
        let replay_allowed = self.retry_tool_calls || metadata_allows_replay;
        let mut attempt = 0u32;

        let mut input_rounds = 0usize;
        let mut state_only_rounds = 0u32;
        loop {
            let call_result = {
                let client = self.client.lock().await;
                client.call_tool_once(params.clone()).await.map_err(|e| e.to_string())
            };

            match call_result {
                Ok(CallToolResponse::InputRequired(required)) => {
                    input_rounds += 1;
                    if input_rounds > self.task_config.max_input_rounds {
                        return Err(AdkError::tool(format!(
                            "MCP tool '{}' exceeded {} MRTR input rounds",
                            self.name, self.task_config.max_input_rounds
                        )));
                    }
                    // The same checks rmcp's `call_tool` applies to each round.
                    let requests = required.input_requests.filter(|requests| !requests.is_empty());
                    if requests.is_none() && required.request_state.is_none() {
                        return Err(AdkError::tool(format!(
                            "MCP tool '{}' returned input_required with neither input requests \
                             nor request state",
                            self.name
                        )));
                    }
                    let responses = match requests {
                        Some(requests) => {
                            state_only_rounds = 0;
                            self.fulfill_input(requests).await.map_err(|error| {
                                AdkError::tool(format!(
                                    "MCP tool '{}' input request failed: {error}",
                                    self.name
                                ))
                            })?
                        }
                        None => {
                            // A state-only round asks nothing, so back off before resending it.
                            let delay_ms = (50u64 << state_only_rounds.min(3)).min(250);
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            state_only_rounds += 1;
                            InputResponses::new()
                        }
                    };
                    params.input_responses = (!responses.is_empty()).then_some(responses);
                    params.request_state = required.request_state;
                    attempt = 0;
                }
                Ok(result) => return Ok(result),
                Err(error) => {
                    if !should_retry_mcp_operation(
                        &error,
                        attempt,
                        &self.refresh_config,
                        has_connection_factory,
                        replay_allowed,
                    ) {
                        return Err(mcp_tool_call_error(
                            &self.name,
                            &error,
                            has_connection_factory,
                            replay_allowed,
                        ));
                    }

                    let retry_attempt = attempt + 1;
                    if self.refresh_config.log_reconnections {
                        warn!(
                            tool = %self.name,
                            attempt = retry_attempt,
                            max_attempts = self.refresh_config.max_attempts,
                            error = %error,
                            "MCP call_tool failed; reconnecting and retrying"
                        );
                    }

                    if self.refresh_config.retry_delay_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            self.refresh_config.retry_delay_ms,
                        ))
                        .await;
                    }

                    if !self.try_refresh_connection().await? {
                        return Err(mcp_tool_call_error(
                            &self.name,
                            &error,
                            has_connection_factory,
                            replay_allowed,
                        ));
                    }
                    attempt += 1;
                }
            }
        }
    }

    /// Answers one MRTR or in-task input batch through the connection's policy.
    async fn fulfill_input(
        &self,
        requests: InputRequests,
    ) -> std::result::Result<InputResponses, String> {
        if requests.is_empty() {
            return Err("the server sent an empty input request batch".to_string());
        }
        if requests.len() > MAX_INPUT_REQUESTS_PER_ROUND {
            return Err(format!(
                "the server sent {} input requests in one round; at most \
                 {MAX_INPUT_REQUESTS_PER_ROUND} are accepted",
                requests.len()
            ));
        }
        match &self.mrtr_handler {
            Some(handler) => handler.fulfill_input_requests(requests).await,
            None => {
                // A person may take minutes to answer, and the handler may call back
                // into this toolset, so the connection lock is released first.
                let client = Arc::clone(&*self.client.lock().await);
                super::input::fulfill(&client, requests).await
            }
        }
    }

    async fn send_task_request(
        &self,
        request: ClientRequest,
    ) -> std::result::Result<ServerResult, TaskError> {
        let client = self.client.lock().await;
        client.send_request(request).await.map_err(|error| TaskError::PollFailed(error.to_string()))
    }

    /// Requests cancellation once; the pending-task set keeps the task until a
    /// status confirms it.
    async fn cancel_task(&self, task_id: &str) {
        let cancel = async {
            let peer = self.client.lock().await.peer().clone();
            peer.cancel_task(CancelTaskParams::new(task_id)).await
        };
        match tokio::time::timeout(TASK_CANCEL_TIMEOUT, cancel).await {
            Ok(Ok(())) => {}
            // The task already finished or the server dropped it; nothing to cancel.
            Ok(Err(error)) if is_unknown_or_finished_task(&error) => {}
            Ok(Err(error)) => warn!(task_id, error = %error, "failed to cancel MCP task"),
            Err(_) => warn!(
                task_id,
                timeout_ms = TASK_CANCEL_TIMEOUT.as_millis(),
                "MCP server did not acknowledge tasks/cancel"
            ),
        }
    }

    /// Poll a protocol-level MCP task until completion or timeout.
    async fn poll_task(
        &self,
        initial_task: rmcp::model::Task,
    ) -> std::result::Result<Value, TaskError> {
        let task_id = initial_task.task_id;
        let mut poll_interval_ms =
            initial_task.poll_interval_ms.unwrap_or(self.task_config.poll_interval_ms).max(1);
        let start = Instant::now();
        let mut attempts = 0u32;
        let mut input_rounds = 0;
        // Answers already sent, re-sent while the server still lists their requests.
        let mut answered: BTreeMap<String, (InputRequest, Value)> = BTreeMap::new();

        loop {
            if let Some(timeout_ms) = self.task_config.timeout_ms {
                let elapsed = start.elapsed().as_millis() as u64;
                if elapsed >= timeout_ms {
                    self.cancel_task(&task_id).await;
                    return Err(TaskError::Timeout { task_id, elapsed_ms: elapsed });
                }
            }

            if let Some(max_attempts) = self.task_config.max_poll_attempts
                && attempts >= max_attempts
            {
                self.cancel_task(&task_id).await;
                return Err(TaskError::MaxAttemptsExceeded { task_id, attempts });
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(poll_interval_ms)).await;
            attempts += 1;

            debug!(task_id, attempt = attempts, "polling MCP task status");
            let request =
                ClientRequest::GetTaskRequest(GetTaskRequest::new(GetTaskParams::new(&task_id)));
            let detailed = match self.send_task_request(request).await? {
                ServerResult::GetTaskResult(result) => result.task,
                response => {
                    return Err(TaskError::PollFailed(format!(
                        "tasks/get returned an unexpected response: {response:?}"
                    )));
                }
            };
            let (task, payload) = (detailed.task, detailed.payload);
            if task.task_id != task_id {
                return Err(TaskError::PollFailed("MCP task identity changed".into()));
            }
            if task.status.is_terminal() {
                self.active_tasks.lock().await.remove(&task_id);
            }
            poll_interval_ms = task.poll_interval_ms.unwrap_or(poll_interval_ms).max(1);

            match payload {
                // SEP-2663 inlines the result in the status response, so a
                // completed task needs no second round trip.
                TaskPayload::Completed { result } => {
                    debug!(task_id, "MCP task completed successfully");
                    let call_result: rmcp::model::CallToolResult =
                        serde_json::from_value(Value::Object(result)).map_err(|error| {
                            TaskError::PollFailed(format!(
                                "tasks/get returned a result that is not a CallToolResult: {error}"
                            ))
                        })?;
                    if call_result.is_error == Some(true) {
                        return Err(TaskError::TaskFailed {
                            task_id,
                            error: call_tool_result_to_adk_value(&call_result)
                                .map(|value| value.to_string())
                                .unwrap_or_else(|error| error),
                        });
                    }
                    return call_tool_result_to_adk_value(&call_result)
                        .map_err(TaskError::PollFailed);
                }
                TaskPayload::Failed { error } => {
                    return Err(TaskError::TaskFailed {
                        task_id,
                        error: task
                            .status_message
                            .unwrap_or_else(|| Value::Object(error).to_string()),
                    });
                }
                TaskPayload::Cancelled => {
                    return Err(TaskError::Cancelled(task_id));
                }
                TaskPayload::InputRequired { input_requests } => {
                    if input_requests.is_empty() {
                        return Err(TaskError::PollFailed(format!(
                            "task '{task_id}' reported input_required without any input requests"
                        )));
                    }
                    let mut pending = InputRequests::new();
                    let mut responses = InputResponses::new();
                    for (key, request) in &input_requests {
                        if let Some((previous, response)) = answered.get(key) {
                            if previous != request {
                                return Err(TaskError::PollFailed(format!(
                                    "task '{task_id}' changed input request '{key}' after it \
                                     was answered"
                                )));
                            }
                            responses.insert(key.clone(), response.clone());
                        } else {
                            pending.insert(key.clone(), request.clone());
                        }
                    }
                    // Re-sending cached answers is not a new round of questions.
                    if !pending.is_empty() {
                        input_rounds += 1;
                        if input_rounds > self.task_config.max_input_rounds {
                            return Err(TaskError::PollFailed(format!(
                                "task '{task_id}' exceeded {} input rounds",
                                self.task_config.max_input_rounds
                            )));
                        }
                        let received =
                            self.fulfill_input(pending).await.map_err(TaskError::PollFailed)?;
                        for (key, response) in received {
                            answered.insert(
                                key.clone(),
                                (input_requests[&key].clone(), response.clone()),
                            );
                            responses.insert(key, response);
                        }
                    }
                    let request = ClientRequest::UpdateTaskRequest(UpdateTaskRequest::new(
                        UpdateTaskParams::new(&task_id, responses),
                    ));
                    match self.send_task_request(request).await? {
                        ServerResult::TaskAckResult(_) => {}
                        response => {
                            return Err(TaskError::PollFailed(format!(
                                "tasks/update returned an unexpected response: {response:?}"
                            )));
                        }
                    }
                }
                TaskPayload::Working => {
                    debug!(task_id, "MCP task is still working");
                    // The server consumed every answer, so a later key reuse is a new question.
                    answered.clear();
                }
                _ => {
                    return Err(TaskError::PollFailed(
                        "server returned an unsupported MCP task status".to_string(),
                    ));
                }
            }
        }
    }
}

#[async_trait]
impl<S> Tool for McpTool<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// SEP-2663 removed the per-tool task contract, so the server decides per
    /// call whether to materialize a task. The remaining signal is therefore
    /// per-connection: any call may return a task when both sides allow it.
    fn is_long_running(&self) -> bool {
        self.task_config.enable_tasks && self.server_supports_tasks
    }

    fn is_read_only(&self) -> bool {
        mcp_tool_safety(self.annotations.as_ref()).0
    }

    fn is_concurrency_safe(&self) -> bool {
        mcp_tool_safety(self.annotations.as_ref()).1
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.input_schema.clone()
    }

    fn response_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.execute_value(args).await
    }
}

// McpTool<S> is Send + Sync when S: Send + Sync because all fields are
// composed of Send + Sync primitives (String, Arc<Mutex<_>>, Arc<dyn Send + Sync>, etc.).
// The compiler enforces this through the Tool trait bound (Tool: Send + Sync).
// No unsafe impl needed — the previous unsafe impl was removed as unnecessary.

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves that `McpTool<S>` is `Send + Sync` for any service `S: Send + Sync`
    /// without requiring `unsafe impl`. The compiler rejects this test at build
    /// time if any field breaks the auto-trait derivation.
    ///
    /// This replaced a previous `unsafe impl Send/Sync for McpTool<S>` that was
    /// unnecessary — all fields (String, Arc<Mutex<_>>, Arc<dyn Send+Sync>, bool)
    /// are naturally Send + Sync.
    #[test]
    fn mcp_tool_is_send_and_sync() {
        fn require_send_sync<T: Send + Sync>() {}

        // The compiler proves Send + Sync for McpTool<S> and McpToolset<S> by
        // type-checking these function bodies. If any field were !Send or !Sync,
        // this would be a compile error — no unsafe needed.
        //
        // () satisfies Service<RoleClient> via the ClientHandler blanket impl
        // in rmcp, so this is a valid concrete instantiation.
        require_send_sync::<McpTool<()>>();
        require_send_sync::<McpToolset<()>>();
    }

    #[test]
    fn test_should_retry_mcp_operation_reconnectable_errors() {
        let config = RefreshConfig::default().with_max_attempts(3);
        assert!(should_retry_mcp_operation("EOF", 0, &config, true, true));
        assert!(should_retry_mcp_operation("connection reset by peer", 1, &config, true, true));
    }

    #[test]
    fn mcp_tool_safety_defaults_to_conservative_values() {
        assert_eq!(mcp_tool_safety(None), (false, false));
        assert_eq!(mcp_tool_safety(Some(&ToolAnnotations::default())), (false, false));
    }

    #[test]
    fn mcp_tool_safety_maps_read_only_and_idempotent_hints() {
        let mut read_only = ToolAnnotations::default();
        read_only.read_only_hint = Some(true);
        assert_eq!(mcp_tool_safety(Some(&read_only)), (true, true));

        let mut idempotent = ToolAnnotations::default();
        idempotent.idempotent_hint = Some(true);
        assert_eq!(mcp_tool_safety(Some(&idempotent)), (false, true));
    }

    #[test]
    fn test_should_retry_mcp_operation_stops_at_max_attempts() {
        let config = RefreshConfig::default().with_max_attempts(2);
        assert!(!should_retry_mcp_operation("EOF", 2, &config, true, true));
    }

    #[test]
    fn test_should_retry_mcp_operation_requires_factory() {
        let config = RefreshConfig::default().with_max_attempts(3);
        assert!(!should_retry_mcp_operation("EOF", 0, &config, false, true));
    }

    #[test]
    fn test_should_retry_mcp_operation_non_reconnectable_error() {
        let config = RefreshConfig::default().with_max_attempts(3);
        assert!(!should_retry_mcp_operation("invalid arguments for tool", 0, &config, true, true));
    }

    /// The default must stay off.
    ///
    /// The opt-in *is* the security boundary of #504. The other tests pass `false` explicitly,
    /// so they verify the gate honours the flag but not that the flag starts closed — flipping
    /// every constructor to `true` left the whole suite green. This is the guard for that.
    #[test]
    fn tool_call_replay_is_disabled_by_default() {
        let config = RefreshConfig::default();
        assert!(
            !should_retry_mcp_operation("EOF", 0, &config, true, DEFAULT_RETRY_TOOL_CALLS),
            "a connection-level error must not replay tools/call under the default; see #504"
        );
    }

    #[test]
    fn test_should_retry_mcp_operation_requires_explicit_replay() {
        let config = RefreshConfig::default().with_max_attempts(3);
        assert!(!should_retry_mcp_operation("EOF", 0, &config, true, false));
    }

    #[test]
    fn test_ambiguous_tool_call_error_explains_no_replay() {
        let error = mcp_tool_call_error("create_record", "EOF", true, false);
        let message = error.to_string();
        assert!(message.contains("result is uncertain and was not replayed"));
        assert!(message.contains("replay-safe operations"));
    }

    #[test]
    fn mcp_result_preserves_structured_text_and_image_content() {
        let mut result = rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text("observation"),
            rmcp::model::ContentBlock::image(STANDARD.encode([1_u8, 2, 3]), "image/png"),
        ]);
        result.structured_content = Some(json!({ "observation_id": "obs-1" }));

        let value = call_tool_result_to_adk_value(&result).unwrap();
        let response = adk_core::FunctionResponseData::from_tool_result("screenshot", value);

        assert_eq!(
            response.response,
            json!({ "output": { "observation_id": "obs-1" }, "text": ["observation"] })
        );
        assert_eq!(response.inline_data.len(), 1);
        assert_eq!(response.inline_data[0].mime_type, "image/png");
        assert_eq!(response.inline_data[0].data, vec![1, 2, 3]);
    }

    /// Publishes a fixed tool catalog over `tools/list`.
    #[derive(Clone)]
    struct CatalogServer(Vec<rmcp::model::Tool>);

    impl rmcp::ServerHandler for CatalogServer {
        fn get_info(&self) -> rmcp::model::ServerInfo {
            rmcp::model::ServerInfo::new(
                rmcp::model::ServerCapabilities::builder().enable_tools().build(),
            )
        }

        async fn list_tools(
            &self,
            _params: Option<rmcp::model::PaginatedRequestParams>,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> std::result::Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
            Ok(rmcp::model::ListToolsResult::with_all_items(self.0.clone()))
        }
    }

    /// Runs discovery against `catalog` over an in-process pipe.
    async fn discover(
        catalog: Vec<rmcp::model::Tool>,
        limits: McpSchemaLimits,
    ) -> Vec<Arc<dyn Tool>> {
        use rmcp::ServiceExt;

        let (server_io, client_io) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            if let Ok(running) = CatalogServer(catalog).serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let client = ().serve(client_io).await.unwrap();
        let toolset = McpToolset::new(client).with_name("catalog").with_schema_limits(limits);
        let ctx = Arc::new(crate::SimpleToolContext::new("schema-limits-test"));
        toolset.tools(ctx).await.unwrap()
    }

    fn schema_object(value: Value) -> serde_json::Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected a JSON object, got {other}"),
        }
    }

    fn small_schema() -> serde_json::Map<String, Value> {
        schema_object(json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        }))
    }

    #[tokio::test]
    async fn discovery_skips_oversized_schemas_and_registers_the_other_tools() {
        let long_text = "x".repeat(300 * 1024);
        let wide: serde_json::Map<String, Value> =
            (0..6_000).map(|index| (format!("p{index}"), json!({ "type": "string" }))).collect();
        let output = schema_object(json!({ "type": "object" }));
        let catalog = vec![
            rmcp::model::Tool::new("before", "Valid.", small_schema()),
            rmcp::model::Tool::new(
                "long_string",
                "Input schema over the byte limit.",
                schema_object(json!({ "type": "object", "description": long_text })),
            ),
            rmcp::model::Tool::new(
                "wide",
                "Input schema over the node limit.",
                schema_object(json!({ "type": "object", "properties": wide })),
            ),
            rmcp::model::Tool::new(
                "long_output",
                "Output schema over the byte limit.",
                small_schema(),
            )
            .with_raw_output_schema(Arc::new(schema_object(json!({ "description": long_text })))),
            rmcp::model::Tool::new("after", "Valid.", small_schema())
                .with_raw_output_schema(Arc::new(output.clone())),
        ];

        let tools = discover(catalog, McpSchemaLimits::default()).await;

        let registered: Vec<_> = tools
            .iter()
            .map(|tool| (tool.name(), tool.parameters_schema(), tool.response_schema()))
            .collect();
        assert_eq!(
            registered,
            vec![
                ("before", Some(Value::Object(small_schema())), None),
                ("after", Some(Value::Object(small_schema())), Some(Value::Object(output))),
            ]
        );
    }

    #[tokio::test]
    async fn discovery_accepts_a_schema_exactly_at_the_configured_limits() {
        let schema = small_schema();
        let bytes = serde_json::to_string(&schema).unwrap().len();
        // The root, "type", "properties", the "id" object and its "type", "required", "id".
        let nodes = 7;
        let catalog = vec![rmcp::model::Tool::new("lookup", "Valid.", schema)];
        let at = McpSchemaLimits::default().with_max_bytes(bytes).with_max_nodes(nodes);

        let names = |tools: Vec<Arc<dyn Tool>>| {
            tools.iter().map(|tool| tool.name().to_string()).collect::<Vec<_>>()
        };
        assert_eq!(names(discover(catalog.clone(), at).await), vec!["lookup"]);
        assert!(discover(catalog.clone(), at.with_max_bytes(bytes - 1)).await.is_empty());
        assert!(discover(catalog, at.with_max_nodes(nodes - 1)).await.is_empty());
    }

    #[test]
    fn mcp_result_rejects_invalid_image_base64() {
        let result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::image(
            "not-base64!",
            "image/png",
        )]);
        assert!(call_tool_result_to_adk_value(&result).is_err());
    }
}
