// MCP (Model Context Protocol) Toolset Integration
//
// Based on Go implementation: adk-go/tool/mcptoolset/
// Uses official Rust SDK: https://github.com/modelcontextprotocol/rust-sdk
//
// The McpToolset connects to an MCP server, discovers available tools,
// and exposes them as ADK-compatible tools for use with LlmAgent.

use super::task::{McpTaskConfig, TaskError, TaskStatus};
use super::{ConnectionFactory, RefreshConfig, should_refresh_connection};
use adk_core::{AdkError, ReadonlyContext, Result, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::{
    RoleClient,
    model::{
        CallToolRequest, CallToolRequestParams, CancelTaskParams, CancelTaskRequest, ClientRequest,
        CompletionContext, CompletionInfo, ContentBlock, ErrorCode, GetPromptRequestParams,
        GetPromptResult, GetTaskParams, GetTaskPayloadParams, GetTaskPayloadRequest,
        GetTaskRequest, Prompt, ReadResourceRequestParams, Resource, ResourceContents,
        ResourceTemplate, ServerResult, SubscribeRequestParams, TaskMetadata, TaskSupport,
        UnsubscribeRequestParams,
    },
    service::RunningService,
};
use serde_json::{Value, json};
use std::time::Instant;
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

/// Shared factory object used to recreate MCP connections for refresh/retry.
type DynConnectionFactory<S> = Arc<dyn ConnectionFactory<S>>;

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

fn should_retry_mcp_operation(
    error: &str,
    attempt: u32,
    refresh_config: &RefreshConfig,
    has_connection_factory: bool,
) -> bool {
    has_connection_factory
        && attempt < refresh_config.max_attempts
        && should_refresh_connection(error)
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
    client: Arc<Mutex<RunningService<RoleClient, S>>>,
    /// Optional filter to select which tools to expose
    tool_filter: Option<ToolFilter>,
    /// Name of this toolset
    name: String,
    /// Task configuration for long-running operations
    task_config: McpTaskConfig,
    /// Optional connection factory used for reconnection on transport failures.
    connection_factory: Option<DynConnectionFactory<S>>,
    /// Reconnection/retry configuration.
    refresh_config: RefreshConfig,
    /// Resource subscriptions restored after an automatic connection refresh.
    resource_subscriptions: Arc<RwLock<BTreeSet<String>>>,
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
            connection_factory: self.connection_factory.clone(),
            refresh_config: self.refresh_config.clone(),
            resource_subscriptions: Arc::clone(&self.resource_subscriptions),
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
        Self {
            client: Arc::new(Mutex::new(client)),
            tool_filter: None,
            name: "mcp_toolset".to_string(),
            task_config: McpTaskConfig::default(),
            connection_factory: None,
            refresh_config: RefreshConfig::default(),
            resource_subscriptions: Arc::new(RwLock::new(BTreeSet::new())),
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
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(arguments);
        let mut attempt = 0u32;
        let result = loop {
            let result = {
                let client = self.client.lock().await;
                client.call_tool(params.clone()).await.map_err(|error| error.to_string())
            };
            match result {
                Ok(result) => break result,
                Err(error)
                    if should_retry_mcp_operation(
                        &error,
                        attempt,
                        &self.refresh_config,
                        self.connection_factory.is_some(),
                    ) =>
                {
                    if self.refresh_config.retry_delay_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            self.refresh_config.retry_delay_ms,
                        ))
                        .await;
                    }
                    if !self.try_refresh_connection().await? {
                        return Err(AdkError::tool(format!(
                            "Failed to call MCP tool '{name}': {error}"
                        )));
                    }
                    attempt += 1;
                }
                Err(error) => {
                    return Err(AdkError::tool(format!(
                        "Failed to call MCP tool '{name}': {error}"
                    )));
                }
            }
        };
        if result.is_error == Some(true) {
            return Err(AdkError::tool(format!(
                "MCP tool '{name}' returned an error: {}",
                call_tool_result_to_adk_value(&result)
                    .unwrap_or_else(|_| json!({ "error": "unreadable MCP error" }))
            )));
        }
        call_tool_result_to_adk_value(&result)
            .map_err(|error| AdkError::tool(format!("Invalid MCP result from '{name}': {error}")))
    }

    async fn try_refresh_connection(&self) -> Result<bool> {
        let Some(factory) = self.connection_factory.clone() else {
            return Ok(false);
        };

        let new_client = factory
            .create_connection()
            .await
            .map_err(|e| AdkError::tool(format!("Failed to refresh MCP connection: {e}")))?;

        for uri in self.resource_subscriptions.read().await.iter() {
            new_client.subscribe(SubscribeRequestParams::new(uri.clone())).await.map_err(
                |error| {
                    AdkError::tool(format!(
                        "Failed to restore MCP resource subscription '{uri}': {error}"
                    ))
                },
            )?;
        }

        let mut client = self.client.lock().await;
        let old_token = client.cancellation_token();
        old_token.cancel();
        *client = new_client;
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
        client.subscribe(SubscribeRequestParams::new(uri)).await.map_err(|error| {
            AdkError::tool(format!("failed to subscribe to MCP resource '{uri}': {error}"))
        })?;
        self.resource_subscriptions.write().await.insert(uri.to_string());
        Ok(())
    }

    /// Remove a resource subscription created by [`subscribe_resource`](Self::subscribe_resource).
    pub async fn unsubscribe_resource(&self, uri: &str) -> Result<()> {
        let client = self.client.lock().await;
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
            client
                .peer_info()
                .and_then(|info| info.capabilities.tasks.as_ref().cloned())
                .is_some_and(|tasks| tasks.supports_tools_call())
        };

        for mcp_tool in mcp_tools {
            let tool_name = mcp_tool.name.to_string();

            // Apply filter if present
            if let Some(ref filter) = self.tool_filter
                && !filter(&tool_name)
            {
                continue;
            }

            let input_schema = Some(Value::Object(mcp_tool.input_schema.as_ref().clone()));

            debug!(
                tool_name = %tool_name,
                schema = ?input_schema,
                "registering MCP tool with raw schema"
            );
            let task_support = mcp_tool.task_support();

            let adk_tool = McpTool {
                name: tool_name,
                description: mcp_tool.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema,
                output_schema: mcp_tool.output_schema.map(|s| Value::Object(s.as_ref().clone())),
                client: self.client.clone(),
                connection_factory: self.connection_factory.clone(),
                refresh_config: self.refresh_config.clone(),
                task_support,
                server_supports_tasks,
                task_config: self.task_config.clone(),
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
    client: Arc<Mutex<RunningService<RoleClient, S>>>,
    connection_factory: Option<DynConnectionFactory<S>>,
    refresh_config: RefreshConfig,
    /// Per-tool task contract published by the MCP server.
    task_support: TaskSupport,
    /// Whether the negotiated server capabilities permit task-augmented tool calls.
    server_supports_tasks: bool,
    /// Task configuration
    task_config: McpTaskConfig,
}

impl<S> McpTool<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    async fn try_refresh_connection(&self) -> Result<bool> {
        let Some(factory) = self.connection_factory.clone() else {
            return Ok(false);
        };

        let new_client = factory
            .create_connection()
            .await
            .map_err(|e| AdkError::tool(format!("Failed to refresh MCP connection: {e}")))?;

        let mut client = self.client.lock().await;
        let old_token = client.cancellation_token();
        old_token.cancel();
        *client = new_client;
        Ok(true)
    }

    async fn call_tool_with_retry(
        &self,
        params: CallToolRequestParams,
    ) -> Result<rmcp::model::CallToolResult> {
        let has_connection_factory = self.connection_factory.is_some();
        let mut attempt = 0u32;

        loop {
            let call_result = {
                let client = self.client.lock().await;
                client.call_tool(params.clone()).await.map_err(|e| e.to_string())
            };

            match call_result {
                Ok(result) => return Ok(result),
                Err(error) => {
                    if !should_retry_mcp_operation(
                        &error,
                        attempt,
                        &self.refresh_config,
                        has_connection_factory,
                    ) {
                        return Err(AdkError::tool(format!(
                            "Failed to call MCP tool '{}': {error}",
                            self.name
                        )));
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
                        return Err(AdkError::tool(format!(
                            "Failed to call MCP tool '{}': {error}",
                            self.name
                        )));
                    }
                    attempt += 1;
                }
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

    async fn cancel_task(&self, task_id: &str) {
        let request = ClientRequest::CancelTaskRequest(CancelTaskRequest::new(
            CancelTaskParams::new(task_id),
        ));
        if let Err(error) = self.send_task_request(request).await {
            warn!(task_id, error = %error, "failed to cancel MCP task after local timeout");
        }
    }

    async fn fetch_task_result(&self, task_id: &str) -> std::result::Result<Value, TaskError> {
        let request = ClientRequest::GetTaskPayloadRequest(GetTaskPayloadRequest::new(
            GetTaskPayloadParams::new(task_id),
        ));
        match self.send_task_request(request).await? {
            ServerResult::CallToolResult(result) => {
                if result.is_error == Some(true) {
                    return Err(TaskError::TaskFailed {
                        task_id: task_id.to_string(),
                        error: call_tool_result_to_adk_value(&result)
                            .map(|value| value.to_string())
                            .unwrap_or_else(|error| error),
                    });
                }
                call_tool_result_to_adk_value(&result).map_err(TaskError::PollFailed)
            }
            ServerResult::CustomResult(result) => Ok(result.0),
            response => Err(TaskError::PollFailed(format!(
                "tasks/result returned an unexpected response: {response:?}"
            ))),
        }
    }

    /// Poll a protocol-level MCP task until completion or timeout.
    async fn poll_task(
        &self,
        initial_task: rmcp::model::Task,
    ) -> std::result::Result<Value, TaskError> {
        let task_id = initial_task.task_id;
        let mut poll_interval_ms =
            initial_task.poll_interval.unwrap_or(self.task_config.poll_interval_ms).max(1);
        let start = Instant::now();
        let mut attempts = 0u32;

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
            let task = match self.send_task_request(request).await? {
                ServerResult::GetTaskResult(result) => result.task,
                response => {
                    return Err(TaskError::PollFailed(format!(
                        "tasks/get returned an unexpected response: {response:?}"
                    )));
                }
            };
            poll_interval_ms = task.poll_interval.unwrap_or(poll_interval_ms).max(1);

            match task.status {
                TaskStatus::Completed => {
                    debug!(task_id, "MCP task completed successfully");
                    return self.fetch_task_result(&task_id).await;
                }
                TaskStatus::Failed => {
                    return Err(TaskError::TaskFailed {
                        task_id,
                        error: task
                            .status_message
                            .unwrap_or_else(|| "remote MCP task failed".to_string()),
                    });
                }
                TaskStatus::Cancelled => {
                    return Err(TaskError::Cancelled(task_id));
                }
                TaskStatus::InputRequired => {
                    return Err(TaskError::InputRequired {
                        task_id,
                        message: task.status_message.unwrap_or_else(|| {
                            "the remote server did not describe the required input".to_string()
                        }),
                    });
                }
                TaskStatus::Working => {
                    debug!(task_id, "MCP task is still working");
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

    fn is_long_running(&self) -> bool {
        self.task_support != TaskSupport::Forbidden
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.input_schema.clone()
    }

    fn response_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        // Determine if we should use task mode
        let use_task_mode = match self.task_support {
            TaskSupport::Required => {
                if !self.server_supports_tasks {
                    return Err(AdkError::tool(format!(
                        "MCP tool '{}' requires task execution, but the server did not negotiate tasks.requests.tools.call",
                        self.name
                    )));
                }
                true
            }
            TaskSupport::Optional => self.task_config.enable_tasks && self.server_supports_tasks,
            TaskSupport::Forbidden => false,
        };

        if use_task_mode {
            debug!(tool = self.name, "Executing tool in task mode (long-running)");

            let mut params = CallToolRequestParams::new(self.name.clone());
            if !(args.is_null() || args == json!({})) {
                match args {
                    Value::Object(map) => params = params.with_arguments(map),
                    _ => return Err(AdkError::tool("Tool arguments must be an object")),
                }
            }
            params = params.with_task(TaskMetadata::new());
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
            let task = match self.send_task_request(request).await {
                Ok(ServerResult::CreateTaskResult(result)) => result.task,
                Ok(response) => {
                    return Err(AdkError::tool(format!(
                        "MCP task call returned an unexpected response: {response:?}"
                    )));
                }
                Err(error) => return Err(AdkError::tool(error.to_string())),
            };

            debug!(tool = self.name, task_id = task.task_id, "MCP task created");

            let result = self
                .poll_task(task)
                .await
                .map_err(|e| AdkError::tool(format!("Task execution failed: {e}")))?;

            return Ok(result);
        }

        // Standard synchronous execution
        let result = self
            .call_tool_with_retry({
                let mut params = CallToolRequestParams::new(self.name.clone());
                if !(args.is_null() || args == json!({})) {
                    match args {
                        Value::Object(map) => {
                            params = params.with_arguments(map);
                        }
                        _ => {
                            return Err(AdkError::tool("Tool arguments must be an object"));
                        }
                    }
                }
                params
            })
            .await?;

        // Check for error response
        if result.is_error.unwrap_or(false) {
            let mut error_msg = format!("MCP tool '{}' execution failed", self.name);

            // Extract error details from content
            for content in &result.content {
                if let Some(text_content) = content.as_text() {
                    error_msg.push_str(": ");
                    error_msg.push_str(&text_content.text);
                    break;
                }
            }

            return Err(AdkError::tool(error_msg));
        }

        call_tool_result_to_adk_value(&result).map_err(|error| {
            AdkError::tool(format!("MCP tool '{}' result invalid: {error}", self.name))
        })
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
        assert!(should_retry_mcp_operation("EOF", 0, &config, true));
        assert!(should_retry_mcp_operation("connection reset by peer", 1, &config, true));
    }

    #[test]
    fn test_should_retry_mcp_operation_stops_at_max_attempts() {
        let config = RefreshConfig::default().with_max_attempts(2);
        assert!(!should_retry_mcp_operation("EOF", 2, &config, true));
    }

    #[test]
    fn test_should_retry_mcp_operation_requires_factory() {
        let config = RefreshConfig::default().with_max_attempts(3);
        assert!(!should_retry_mcp_operation("EOF", 0, &config, false));
    }

    #[test]
    fn test_should_retry_mcp_operation_non_reconnectable_error() {
        let config = RefreshConfig::default().with_max_attempts(3);
        assert!(!should_retry_mcp_operation("invalid arguments for tool", 0, &config, true));
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

    #[test]
    fn mcp_result_rejects_invalid_image_base64() {
        let result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::image(
            "not-base64!",
            "image/png",
        )]);
        assert!(call_tool_result_to_adk_value(&result).is_err());
    }
}
