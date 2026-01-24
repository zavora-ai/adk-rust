// MCP (Model Context Protocol) Toolset Integration
//
// Based on Go implementation: adk-go/tool/mcptoolset/
// Uses official Rust SDK: https://github.com/modelcontextprotocol/rust-sdk
//
// The McpToolset connects to an MCP server, discovers available tools,
// and exposes them as ADK-compatible tools for use with LlmAgent.

use adk_core::{AdkError, ReadonlyContext, Result, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use rmcp::{
    RoleClient,
    model::{CallToolRequestParam, RawContent, ResourceContents},
    service::RunningService,
};
use serde_json::{Value, json};
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Type alias for tool filter predicate
pub type ToolFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Sanitize JSON schema for LLM compatibility.
/// Removes fields like `$schema`, `additionalProperties`, `definitions`, `$ref`
/// that some LLM APIs (like Gemini) don't accept.
fn sanitize_schema(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("$schema");
        map.remove("definitions");
        map.remove("$ref");
        map.remove("additionalProperties");

        for (_, v) in map.iter_mut() {
            sanitize_schema(v);
        }
    } else if let Value::Array(arr) = value {
        for v in arr.iter_mut() {
            sanitize_schema(v);
        }
    }
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
/// use adk_tool::McpToolset;
/// use rmcp::{ServiceExt, transport::TokioChildProcess};
/// use tokio::process::Command;
///
/// // Create MCP client connection to a local server
/// let client = ().serve(TokioChildProcess::new(
///     Command::new("npx")
///         .arg("-y")
///         .arg("@modelcontextprotocol/server-everything")
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
}

impl<S> McpToolset<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// Create a new MCP toolset from a running MCP client service.
    ///
    /// The client should already be connected and initialized.
    /// Use `rmcp::ServiceExt::serve()` to create the client.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use rmcp::{ServiceExt, transport::TokioChildProcess};
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
        }
    }

    /// Set a custom name for this toolset.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
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
        let client = self.client.lock().await;

        // List all tools from the MCP server (handles pagination internally)
        let mcp_tools = client
            .list_all_tools()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to list MCP tools: {}", e)))?;

        // Convert MCP tools to ADK tools
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        for mcp_tool in mcp_tools {
            let tool_name = mcp_tool.name.to_string();

            // Apply filter if present
            if let Some(ref filter) = self.tool_filter {
                if !filter(&tool_name) {
                    continue;
                }
            }

            let adk_tool = McpTool {
                name: tool_name,
                description: mcp_tool.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: {
                    let mut schema = Value::Object(mcp_tool.input_schema.as_ref().clone());
                    sanitize_schema(&mut schema);
                    Some(schema)
                },
                output_schema: mcp_tool.output_schema.map(|s| {
                    let mut schema = Value::Object(s.as_ref().clone());
                    sanitize_schema(&mut schema);
                    schema
                }),
                client: self.client.clone(),
            };

            tools.push(Arc::new(adk_tool) as Arc<dyn Tool>);
        }

        Ok(tools)
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
        false
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.input_schema.clone()
    }

    fn response_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let client = self.client.lock().await;

        // Call the MCP tool
        let result = client
            .call_tool(CallToolRequestParam {
                name: self.name.clone().into(),
                arguments: if args.is_null() || args == json!({}) {
                    None
                } else {
                    // Convert Value to the expected type
                    match args {
                        Value::Object(map) => Some(map),
                        _ => {
                            return Err(AdkError::Tool(
                                "Tool arguments must be an object".to_string(),
                            ));
                        }
                    }
                },
                task: None, // Task support added in MCP spec 2025-11
            })
            .await
            .map_err(|e| {
                AdkError::Tool(format!("Failed to call MCP tool '{}': {}", self.name, e))
            })?;

        // Check for error response
        if result.is_error.unwrap_or(false) {
            let mut error_msg = format!("MCP tool '{}' execution failed", self.name);

            // Extract error details from content
            for content in &result.content {
                // Use Deref to access the inner RawContent
                if let Some(text_content) = content.deref().as_text() {
                    error_msg.push_str(": ");
                    error_msg.push_str(&text_content.text);
                    break;
                }
            }

            return Err(AdkError::Tool(error_msg));
        }

        // Return structured content if available
        if let Some(structured) = result.structured_content {
            return Ok(json!({ "output": structured }));
        }

        // Otherwise, collect text content
        let mut text_parts: Vec<String> = Vec::new();

        for content in &result.content {
            // Access the inner RawContent via Deref
            let raw: &RawContent = content.deref();
            match raw {
                RawContent::Text(text_content) => {
                    text_parts.push(text_content.text.clone());
                }
                RawContent::Image(image_content) => {
                    // Return image data as base64
                    text_parts.push(format!(
                        "[Image: {} bytes, mime: {}]",
                        image_content.data.len(),
                        image_content.mime_type
                    ));
                }
                RawContent::Resource(resource_content) => {
                    let uri = match &resource_content.resource {
                        ResourceContents::TextResourceContents { uri, .. } => uri,
                        ResourceContents::BlobResourceContents { uri, .. } => uri,
                    };
                    text_parts.push(format!("[Resource: {}]", uri));
                }
                RawContent::Audio(_) => {
                    text_parts.push("[Audio content]".to_string());
                }
                RawContent::ResourceLink(link) => {
                    text_parts.push(format!("[ResourceLink: {}]", link.uri));
                }
            }
        }

        if text_parts.is_empty() {
            return Err(AdkError::Tool(format!("MCP tool '{}' returned no content", self.name)));
        }

        Ok(json!({ "output": text_parts.join("\n") }))
    }
}

// Ensure McpTool is Send + Sync
unsafe impl<S> Send for McpTool<S> where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static
{
}
unsafe impl<S> Sync for McpTool<S> where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static
{
}
