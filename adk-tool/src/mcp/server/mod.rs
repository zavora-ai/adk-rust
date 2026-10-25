//! An MCP server that publishes a set of [`adk_core::Tool`]s over Streamable HTTP, with an MCP
//! Server Card (spec 3.5 §7.1, §7.4, §7.9; SEP-2127). **Experimental**: the Server Card
//! extension is itself experimental upstream.
//!
//! One declaration serves every binding: each tool's name, description, parameters schema and
//! [`ToolEffect`] become its MCP tool definition and annotations, the same tool the agent edge
//! exposes as an A2A skill and an AWP endpoint (spec 3.5 §7.4).
//!
//! | Part | Type | Role |
//! |------|------|------|
//! | Server | [`McpServer`] | Lists the tools and answers `tools/call`; an rmcp `ServerHandler` |
//! | Executor | [`McpCallExecutor`] | Runs or refuses every call; [`DirectExecutor`] by default |
//! | Caller | [`McpCallerResolver`] | Turns each HTTP request into the [`McpCaller`] the executor sees |
//! | Router | [`McpRouterBuilder`] | The `axum::Router`: Streamable HTTP, the Server Card at `{endpoint}/server-card` (SEP-2127) with its `/.well-known/mcp/server-card.json` alias (spec 3.5 §7.9), and `/.well-known/ai-catalog.json` |
//! | Card | [`ServerCard`], [`AiCatalog`] | Discovery documents to the SEP-2127 schema |
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::{AdkError, Url};
//! use adk_tool::FunctionTool;
//! use adk_tool::mcp::server::{McpRouterBuilder, McpServer};
//! use serde_json::json;
//!
//! # async fn serve() -> Result<(), Box<dyn std::error::Error>> {
//! let quote = Arc::new(
//!     FunctionTool::new("get_quote", "Quotes a solar installation", |_ctx, args| async move {
//!         Ok(json!({ "kw": args["kw"], "price": 4200 }))
//!     })
//!     .with_read_only(true),
//! );
//! let server = McpServer::builder("com.example/solar", "1.0.0")
//!     .description("Quotes for Example Solar installations")
//!     .tool(quote)
//!     .build()?;
//! let router = McpRouterBuilder::new(server)
//!     .public_url(Url::parse("http://127.0.0.1:8080/mcp")?)
//!     .build()?;
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
//! axum::serve(listener, router).await?;
//! # Ok(())
//! # }
//! ```

mod caller;
mod card;
mod executor;
mod http;

pub use caller::{AnonymousCallers, McpCaller, McpCallerResolver, McpHttpRequest};
pub use card::{
    AI_CATALOG_MEDIA_TYPE, AI_CATALOG_SPEC_VERSION, AI_CATALOG_WELL_KNOWN_PATH, AiCatalog,
    AiCatalogContent, AiCatalogEntry, AiCatalogHost, IconTheme, InputFormat, RemoteTransport,
    SERVER_CARD_ENDPOINT_SUFFIX, SERVER_CARD_MEDIA_TYPE, SERVER_CARD_SCHEMA_URL,
    SERVER_CARD_WELL_KNOWN_PATH, ServerCard, ServerCardHeader, ServerCardIcon, ServerCardInput,
    ServerCardRemote, ServerCardRepository,
};
pub use executor::{DirectExecutor, McpCallExecutor, McpToolCall, McpToolOutcome};
pub use http::{McpRouterBuilder, McpSessions};

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Result, RetryHint, TOOL_ERROR_MESSAGE_LIMIT, Tool,
    ToolEffect, Url, tool_error_response,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, ErrorData,
    Implementation, InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{Value, json};

use crate::ToolArgumentValidator;

/// The default bound on one call's run time.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest tool name a refusal quotes back to the caller.
const QUOTED_NAME_LIMIT: usize = 128;

/// An MCP server publishing a fixed set of [`Tool`]s (spec 3.5 §7.9).
///
/// `tools/list` describes each tool from its own declaration: name, description, parameters
/// schema as `inputSchema`, an object `response_schema` as `outputSchema`, and annotations
/// from [`Tool::effect`]:
///
/// | Effect | `readOnlyHint` | `idempotentHint` | `destructiveHint` |
/// |--------|----------------|------------------|-------------------|
/// | `ReadOnly` | `true` | `true` | `false` |
/// | `Idempotent` | `false` | `true` | the tool's own hint, else `true` |
/// | `NonIdempotent` | `false` | `false` | the tool's own hint, else `true` |
///
/// `openWorldHint` and `title` come from the tool's [`annotations`](Tool::annotations), else
/// the server's [`open_world`](McpServerBuilder::open_world) default. `tools/call` checks the
/// arguments against the parameters schema, then hands the call to the
/// [`McpCallExecutor`] under the [call timeout](McpServerBuilder::call_timeout).
///
/// The server is an rmcp `ServerHandler`, so it also serves stdio; there every caller is
/// [`McpCaller::anonymous`]. [`McpRouterBuilder`] serves it over Streamable HTTP.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_tool::FunctionTool;
/// use adk_tool::mcp::server::McpServer;
/// use serde_json::json;
///
/// let server = McpServer::builder("com.example/solar", "1.0.0")
///     .description("Quotes for Example Solar installations")
///     .tool(Arc::new(
///         FunctionTool::new("get_quote", "Quotes an installation", |_ctx, _args| async {
///             Ok(json!({ "price": 4200 }))
///         })
///         .with_read_only(true),
///     ))
///     .build()?;
/// let listed = server.tool_definitions();
/// assert_eq!(listed[0].name, "get_quote");
/// assert_eq!(listed[0].annotations.as_ref().unwrap().read_only_hint, Some(true));
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    version: String,
    description: String,
    title: Option<String>,
    website_url: Option<String>,
    repository: Option<ServerCardRepository>,
    icons: Vec<ServerCardIcon>,
    instructions: Option<String>,
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    executor: Arc<dyn McpCallExecutor>,
    call_timeout: Option<Duration>,
}

/// One published tool: the tool, its MCP definition and its argument check.
struct Entry {
    tool: Arc<dyn Tool>,
    definition: rmcp::model::Tool,
    validator: ToolArgumentValidator,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tools: Vec<&str> = self.inner.entries.iter().map(|entry| entry.tool.name()).collect();
        f.debug_struct("McpServer")
            .field("name", &self.inner.name)
            .field("version", &self.inner.version)
            .field("tools", &tools)
            .finish_non_exhaustive()
    }
}

impl McpServer {
    /// Starts a server named `name` (reverse DNS with one `/`, such as `com.example/shop`) at
    /// `version`.
    pub fn builder(name: impl Into<String>, version: impl Into<String>) -> McpServerBuilder {
        McpServerBuilder::new(name, version)
    }

    /// The server's name, as `serverInfo.name` and the card's `name`.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The server's version.
    pub fn version(&self) -> &str {
        &self.inner.version
    }

    /// The published tools, in the order they were added.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.inner.entries.iter().map(|entry| Arc::clone(&entry.tool)).collect()
    }

    /// The MCP definition of every tool, regardless of caller.
    pub fn tool_definitions(&self) -> Vec<rmcp::model::Tool> {
        self.inner.entries.iter().map(|entry| entry.definition.clone()).collect()
    }

    /// The MCP definitions `caller` sees in `tools/list`.
    pub fn tool_definitions_for(&self, caller: &McpCaller) -> Vec<rmcp::model::Tool> {
        self.inner
            .entries
            .iter()
            .filter(|entry| self.inner.executor.is_visible(entry.tool.as_ref(), caller))
            .map(|entry| entry.definition.clone())
            .collect()
    }

    /// The Server Card for the Streamable HTTP endpoint at `endpoint` (SEP-2127).
    ///
    /// The card's name, version, title and description are the server's `serverInfo`, and its
    /// one remote lists the protocol versions the server negotiates, so the card agrees with
    /// what a client sees once connected. `headers` describes headers a client must send, such
    /// as `Authorization`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Url;
    /// use adk_tool::mcp::server::{McpServer, SERVER_CARD_SCHEMA_URL};
    ///
    /// let server = McpServer::builder("com.example/solar", "1.0.0")
    ///     .description("Quotes for Example Solar installations")
    ///     .build()?;
    /// let card = server.server_card(&Url::parse("https://agent.example.com/mcp").unwrap(), &[]);
    /// assert_eq!(card.schema, SERVER_CARD_SCHEMA_URL);
    /// assert_eq!(card.remotes.unwrap()[0].url, "https://agent.example.com/mcp");
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn server_card(&self, endpoint: &Url, headers: &[ServerCardHeader]) -> ServerCard {
        let inner = &self.inner;
        let versions = ServerHandler::supported_protocol_versions(self);
        let mut remote = ServerCardRemote::new(RemoteTransport::StreamableHttp, endpoint.as_str())
            .with_protocol_versions(versions.iter().map(ToString::to_string));
        for header in headers {
            remote = remote.with_header(header.clone());
        }
        ServerCard {
            schema: SERVER_CARD_SCHEMA_URL.to_string(),
            name: inner.name.clone(),
            version: inner.version.clone(),
            description: inner.description.clone(),
            title: inner.title.clone(),
            website_url: inner.website_url.clone(),
            repository: inner.repository.clone(),
            icons: (!inner.icons.is_empty()).then(|| inner.icons.clone()),
            remotes: Some(vec![remote]),
            meta: None,
        }
    }

    /// The AI Catalog entry pointing at this server's card at `card_url`.
    ///
    /// Its identifier is `urn:air:{reversed namespace}:mcp:{name}`: `com.example/solar` becomes
    /// `urn:air:example.com:mcp:solar`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Url;
    /// use adk_tool::mcp::server::McpServer;
    ///
    /// let server = McpServer::builder("com.example/solar", "1.0.0")
    ///     .description("Quotes for Example Solar installations")
    ///     .build()?;
    /// let card_url = Url::parse("https://agent.example.com/mcp/server-card").unwrap();
    /// let entry = server.ai_catalog_entry(&card_url);
    /// assert_eq!(entry.identifier, "urn:air:example.com:mcp:solar");
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn ai_catalog_entry(&self, card_url: &Url) -> AiCatalogEntry {
        AiCatalogEntry::server_card_url(
            card::catalog_identifier(&self.inner.name),
            card_url.as_str(),
        )
    }

    /// The tool `name`, when `caller` may see it.
    fn visible(&self, name: &str, caller: &McpCaller) -> Option<&Entry> {
        let entry = &self.inner.entries[*self.inner.index.get(name)?];
        self.inner.executor.is_visible(entry.tool.as_ref(), caller).then_some(entry)
    }

    /// Runs one call: arguments checked, then the executor under the timeout and the request's
    /// cancellation.
    async fn call(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let parts = context.extensions.get::<axum::http::request::Parts>();
        let caller = parts
            .and_then(|parts| parts.extensions.get::<McpCaller>())
            .cloned()
            .unwrap_or_else(McpCaller::anonymous);
        let name = request.name.as_ref();
        let Some(entry) = self.visible(name, &caller) else {
            tracing::debug!(tool.name = %quoted(name), caller.id = caller.id(), "mcp call to an unknown tool");
            return Err(ErrorData::invalid_params(
                format!("unknown tool '{}'", quoted(name)),
                None,
            ));
        };
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        if let Err(error) = entry.validator.validate(name, &arguments) {
            return Ok(failed_result(name, &error));
        }
        let session_id = parts
            .and_then(|parts| parts.headers.get("mcp-session-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let call = McpToolCall {
            tool: Arc::clone(&entry.tool),
            arguments,
            caller,
            request_id: request_id_string(&context.id),
            session_id,
        };
        let caller_id = call.caller.id().to_string();
        let run = self.inner.executor.execute(call);
        let bounded = async {
            match self.inner.call_timeout {
                Some(limit) => tokio::time::timeout(limit, run)
                    .await
                    .unwrap_or_else(|_| Err(timeout_error(entry.tool.effect(), limit))),
                None => run.await,
            }
        };
        let outcome = tokio::select! {
            outcome = bounded => outcome,
            () = context.ct.cancelled() => Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Cancelled,
                "mcp.server.call_cancelled",
                "the client cancelled the call",
            )),
        };
        match outcome {
            Ok(McpToolOutcome::Completed(value)) => Ok(completed_result(value)),
            Ok(McpToolOutcome::Failed(error)) => {
                tracing::debug!(tool.name = name, caller.id = %caller_id, error = %error.diagnostics(), "mcp tool failed");
                Ok(failed_result(name, &error))
            }
            Err(error) => {
                tracing::info!(tool.name = name, caller.id = %caller_id, error = %error.diagnostics(), "mcp call refused");
                Err(refusal(&error))
            }
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        let inner = &self.inner;
        let mut implementation =
            Implementation::new(&inner.name, &inner.version).with_description(&inner.description);
        if let Some(title) = &inner.title {
            implementation = implementation.with_title(title);
        }
        if let Some(url) = &inner.website_url {
            implementation = implementation.with_website_url(url);
        }
        if !inner.icons.is_empty() {
            implementation = implementation.with_icons(inner.icons.iter().map(rmcp_icon).collect());
        }
        let config = InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation);
        match &inner.instructions {
            Some(instructions) => config.with_instructions(instructions),
            None => config,
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        let caller = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<McpCaller>())
            .cloned()
            .unwrap_or_else(McpCaller::anonymous);
        Ok(ListToolsResult::with_all_items(self.tool_definitions_for(&caller)))
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        let index = *self.inner.index.get(name)?;
        Some(self.inner.entries[index].definition.clone())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        self.call(request, context).await.map(CallToolResponse::from)
    }
}

/// Builds an [`McpServer`].
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
///
/// use adk_tool::mcp::server::{McpServer, ServerCardRepository};
///
/// let server = McpServer::builder("com.example/solar", "1.0.0")
///     .description("Quotes for Example Solar installations")
///     .title("Example Solar")
///     .website_url("https://example.com")
///     .repository(ServerCardRepository::new("https://github.com/example/solar", "github"))
///     .open_world(false)
///     .call_timeout(Some(Duration::from_secs(20)))
///     .build()?;
/// assert_eq!(server.name(), "com.example/solar");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct McpServerBuilder {
    name: String,
    version: String,
    description: Option<String>,
    title: Option<String>,
    website_url: Option<String>,
    repository: Option<ServerCardRepository>,
    icons: Vec<ServerCardIcon>,
    instructions: Option<String>,
    tools: Vec<Arc<dyn Tool>>,
    executor: Option<Arc<dyn McpCallExecutor>>,
    open_world: bool,
    call_timeout: Option<Duration>,
}

impl McpServerBuilder {
    fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: None,
            title: None,
            website_url: None,
            repository: None,
            icons: Vec::new(),
            instructions: None,
            tools: Vec::new(),
            executor: None,
            open_world: true,
            call_timeout: Some(DEFAULT_CALL_TIMEOUT),
        }
    }

    /// What the server does, 1 to 100 characters; required.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The display name, 1 to 100 characters.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The server's homepage or documentation.
    pub fn website_url(mut self, url: impl Into<String>) -> Self {
        self.website_url = Some(url.into());
        self
    }

    /// Where the server's source lives.
    pub fn repository(mut self, repository: ServerCardRepository) -> Self {
        self.repository = Some(repository);
        self
    }

    /// Adds an icon.
    pub fn icon(mut self, icon: ServerCardIcon) -> Self {
        self.icons.push(icon);
        self
    }

    /// Instructions for the client's model, sent in the `initialize` result.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Publishes `tool`.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Publishes every tool in `tools`.
    pub fn tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// Runs calls through `executor` instead of [`DirectExecutor`].
    pub fn executor(mut self, executor: Arc<dyn McpCallExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// The `openWorldHint` of tools that declare none; `true` by default, as in MCP.
    pub fn open_world(mut self, open_world: bool) -> Self {
        self.open_world = open_world;
        self
    }

    /// Bounds each call's run time; [`DEFAULT_CALL_TIMEOUT`] by default, `None` for no bound.
    ///
    /// A call over the bound is refused with a `Timeout` error, and the executor's future is
    /// dropped.
    pub fn call_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// Builds the server.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidInput` error when the name, version, description or title break the
    /// SEP-2127 card schema, when two tools share a name, or when a tool's parameters schema is
    /// not a JSON Schema object of type `object`.
    pub fn build(self) -> Result<McpServer> {
        check_name(&self.name)?;
        check_version(&self.version)?;
        let description = self.description.ok_or_else(|| {
            invalid("mcp.server.description", "set the server's description with `description(..)`")
        })?;
        check_text("description", &description)?;
        if let Some(title) = &self.title {
            check_text("title", title)?;
        }
        let mut entries = Vec::with_capacity(self.tools.len());
        let mut index = HashMap::with_capacity(self.tools.len());
        for tool in self.tools {
            let name = tool.name().to_string();
            if name.is_empty() {
                return Err(invalid("mcp.server.tool_name", "a published tool has an empty name"));
            }
            if index.insert(name.clone(), entries.len()).is_some() {
                return Err(invalid(
                    "mcp.server.duplicate_tool",
                    format!("two published tools are named '{name}'; MCP tool names are unique"),
                ));
            }
            let input_schema = input_schema(tool.as_ref())?;
            let validator = ToolArgumentValidator::new(&Value::Object(input_schema.clone()))?;
            let definition = definition(tool.as_ref(), input_schema, self.open_world);
            entries.push(Entry { tool, definition, validator });
        }
        let executor =
            self.executor.unwrap_or_else(|| Arc::new(DirectExecutor::new(self.name.clone())));
        Ok(McpServer {
            inner: Arc::new(Inner {
                name: self.name,
                version: self.version,
                description,
                title: self.title,
                website_url: self.website_url,
                repository: self.repository,
                icons: self.icons,
                instructions: self.instructions,
                entries,
                index,
                executor,
                call_timeout: self.call_timeout,
            }),
        })
    }
}

/// The tool's MCP definition from its own declaration (spec 3.5 §7.4).
fn definition(tool: &dyn Tool, input_schema: JsonObject, open_world: bool) -> rmcp::model::Tool {
    let declared = tool.annotations();
    let hint = |key: &str| declared.and_then(|hints| hints.get(key)).and_then(Value::as_bool);
    let title = declared.and_then(|hints| hints.get("title")).and_then(Value::as_str);
    let annotations = match title {
        Some(title) => ToolAnnotations::with_title(title),
        None => ToolAnnotations::new(),
    };
    let annotations = match tool.effect() {
        ToolEffect::ReadOnly => annotations.read_only(true).idempotent(true).destructive(false),
        ToolEffect::Idempotent => annotations
            .read_only(false)
            .idempotent(true)
            .destructive(hint("destructiveHint").unwrap_or(true)),
        // `NonIdempotent`, and any effect added later, is described as the least safe.
        _ => annotations
            .read_only(false)
            .idempotent(false)
            .destructive(hint("destructiveHint").unwrap_or(true)),
    }
    .open_world(hint("openWorldHint").unwrap_or(open_world));
    let mut definition = rmcp::model::Tool::new(
        tool.name().to_string(),
        tool.description().to_string(),
        Arc::new(input_schema),
    )
    .with_annotations(annotations);
    if let Some(title) = title {
        definition = definition.with_title(title);
    }
    if let Some(Value::Object(output)) = tool.response_schema()
        && output.get("type").and_then(Value::as_str) == Some("object")
    {
        definition = definition.with_raw_output_schema(Arc::new(output));
    }
    definition
}

/// The tool's parameters schema as an MCP `inputSchema`: a JSON object of type `object`.
fn input_schema(tool: &dyn Tool) -> Result<JsonObject> {
    match tool.parameters_schema() {
        None => Ok(JsonObject::from_iter([("type".to_string(), json!("object"))])),
        Some(Value::Object(mut schema)) => match schema.get("type") {
            None => {
                schema.insert("type".to_string(), json!("object"));
                Ok(schema)
            }
            Some(Value::String(kind)) if kind == "object" => Ok(schema),
            Some(other) => Err(invalid(
                "mcp.server.input_schema",
                format!(
                    "tool '{}' declares parameters of type {other}; an MCP tool's parameters are \
                     a JSON object, so wrap them in an object schema",
                    tool.name()
                ),
            )),
        },
        Some(_) => Err(invalid(
            "mcp.server.input_schema",
            format!(
                "tool '{}' declares a parameters schema that is not a JSON object",
                tool.name()
            ),
        )),
    }
}

/// The result of a call that returned `value`: the value as text, and as structured content
/// when it is an object.
fn completed_result(value: Value) -> CallToolResult {
    let text = match &value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    if value.is_object() {
        result.structured_content = Some(value);
    }
    result
}

/// The `isError` result of a call to `tool` that ran and failed with `error`.
fn failed_result(tool: &str, error: &AdkError) -> CallToolResult {
    let response = tool_error_response(tool, error, false);
    let text = response["error"].as_str().unwrap_or("the tool failed").to_string();
    CallToolResult::error(vec![ContentBlock::text(text)])
}

/// The JSON-RPC error a refused call is answered with.
///
/// Only the standard JSON-RPC codes are used: `-32602` when the call as made cannot be served
/// (`InvalidInput`, `NotFound`, `Unsupported`), `-32603` otherwise. The message names the
/// error's component, category and code, and adds the error's own message only for a rejection,
/// whose message is guidance for the caller; `data` carries the category, the code and whether
/// a retry may succeed.
fn refusal(error: &AdkError) -> ErrorData {
    let code = match error.category {
        ErrorCategory::InvalidInput | ErrorCategory::NotFound | ErrorCategory::Unsupported => {
            ErrorCode::INVALID_PARAMS
        }
        ErrorCategory::Unauthorized
        | ErrorCategory::Forbidden
        | ErrorCategory::RateLimited
        | ErrorCategory::Timeout
        | ErrorCategory::Unavailable
        | ErrorCategory::Cancelled
        | ErrorCategory::Internal
        | ErrorCategory::ResourceExhausted
        | ErrorCategory::Conflict => ErrorCode::INTERNAL_ERROR,
    };
    let message = if error.category.is_rejection() {
        let mut chars = error.message.chars();
        let head: String = chars.by_ref().take(TOOL_ERROR_MESSAGE_LIMIT).collect();
        let cut = if chars.next().is_some() { "… (truncated)" } else { "" };
        format!("{}: {head}{cut}", error.redacted())
    } else {
        error.redacted()
    };
    let mut data = json!({
        "category": error.category.to_string(),
        "code": error.code,
        "retryable": error.is_retryable(),
    });
    if let Some(after) = error.retry.retry_after_ms {
        data["retryAfterMs"] = json!(after);
    }
    ErrorData::new(code, message, Some(data))
}

/// The refusal of a call that ran past `limit`; one that may have had its effect is not marked
/// retryable.
fn timeout_error(effect: ToolEffect, limit: Duration) -> AdkError {
    let error = AdkError::timeout(
        ErrorComponent::Tool,
        "mcp.server.call_timeout",
        format!("the call ran past the server's {} ms limit", limit.as_millis()),
    );
    if effect.is_retry_safe() { error } else { error.with_retry(RetryHint::default()) }
}

/// The JSON-RPC request id as a string.
fn request_id_string(id: &rmcp::model::RequestId) -> String {
    match id {
        rmcp::model::NumberOrString::Number(number) => number.to_string(),
        rmcp::model::NumberOrString::String(text) => text.to_string(),
    }
}

/// `name` cut to [`QUOTED_NAME_LIMIT`] characters, for quoting a caller's input back.
fn quoted(name: &str) -> Cow<'_, str> {
    match name.char_indices().nth(QUOTED_NAME_LIMIT) {
        Some((end, _)) => Cow::Owned(format!("{}…", &name[..end])),
        None => Cow::Borrowed(name),
    }
}

/// The card icon as an rmcp `serverInfo` icon.
fn rmcp_icon(icon: &ServerCardIcon) -> rmcp::model::Icon {
    let mut converted = rmcp::model::Icon::new(&icon.src);
    if let Some(mime_type) = &icon.mime_type {
        converted = converted.with_mime_type(mime_type);
    }
    if let Some(sizes) = &icon.sizes {
        converted = converted.with_sizes(sizes.clone());
    }
    if let Some(theme) = icon.theme {
        converted = converted.with_theme(match theme {
            IconTheme::Light => rmcp::model::IconTheme::Light,
            IconTheme::Dark => rmcp::model::IconTheme::Dark,
        });
    }
    converted
}

fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::InvalidInput, code, message)
}

/// The SEP-2127 name rule: 3 to 200 characters, `namespace/name`, exactly one `/`.
fn check_name(name: &str) -> Result<()> {
    let valid = (3..=200).contains(&name.len())
        && name.split_once('/').is_some_and(|(namespace, server)| {
            !namespace.is_empty()
                && !server.is_empty()
                && namespace.bytes().all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
                && server.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        });
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "mcp.server.name",
            format!(
                "server name '{name}' is not a reverse-DNS name with one '/', such as \
                 'com.example/shop' (SEP-2127)"
            ),
        ))
    }
}

/// A version of at most 255 characters that is not a range (SEP-2127).
fn check_version(version: &str) -> Result<()> {
    let is_range = version.contains(['^', '~', '>', '<', '=', '*', ' '])
        || version.split('.').any(|part| part.eq_ignore_ascii_case("x"));
    if version.is_empty() || version.len() > 255 || is_range {
        return Err(invalid(
            "mcp.server.version",
            format!(
                "server version '{version}' must be one version of at most 255 characters, not a range, such as '1.0.2'"
            ),
        ));
    }
    Ok(())
}

/// A card text field of 1 to 100 characters.
fn check_text(field: &str, value: &str) -> Result<()> {
    let length = value.chars().count();
    if (1..=100).contains(&length) {
        Ok(())
    } else {
        Err(invalid(
            "mcp.server.card_text",
            format!("the server's {field} has {length} characters; SEP-2127 allows 1 to 100"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FunctionTool;

    fn tool(name: &str) -> Arc<dyn Tool> {
        Arc::new(FunctionTool::new(name, "a tool", |_ctx, _args| async { Ok(json!({})) }))
    }

    fn server() -> McpServerBuilder {
        McpServer::builder("com.example/shop", "1.0.0").description("Shop")
    }

    #[test]
    fn names_follow_the_card_schema() {
        for good in ["com.example/shop", "io.github.acme-co/shop_v2.1", "a/b"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in ["shop", "com.example/", "/shop", "com/example/shop", "com.example/sh op", "ab"]
        {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn versions_reject_ranges() {
        for good in ["1.0.0", "2.1.0-alpha", "2026.10"] {
            assert!(check_version(good).is_ok(), "{good}");
        }
        for bad in ["", "^1.2.3", "~1.2.3", ">=1.2.3", "1.x", "1.*"] {
            assert!(check_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_description_is_required_and_bounded() {
        let missing = McpServer::builder("com.example/shop", "1.0.0").build().unwrap_err();
        assert_eq!(missing.code, "mcp.server.description");
        let long = server().description("x".repeat(101)).build().unwrap_err();
        assert_eq!(long.code, "mcp.server.card_text");
    }

    #[test]
    fn duplicate_tool_names_are_refused() {
        let error = server().tool(tool("quote")).tool(tool("quote")).build().unwrap_err();
        assert_eq!(error.code, "mcp.server.duplicate_tool");
    }

    #[test]
    fn a_tool_without_parameters_takes_an_empty_object() {
        let built = server().tool(tool("quote")).build().unwrap();
        assert_eq!(
            Value::Object((*built.tool_definitions()[0].input_schema).clone()),
            json!({ "type": "object" })
        );
    }

    #[test]
    fn refusals_keep_internal_messages_back() {
        let internal = AdkError::unavailable(
            ErrorComponent::Tool,
            "orders.db",
            "connect postgres://admin:secret@db.internal failed",
        );
        let data = refusal(&internal);
        assert_eq!(data.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(data.message, "tool.unavailable (orders.db)");
        assert_eq!(
            data.data,
            Some(json!({ "category": "unavailable", "code": "orders.db", "retryable": true }))
        );

        let denied = AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Forbidden,
            "awp.capability.tier",
            "this capability needs the known tier",
        );
        assert_eq!(
            refusal(&denied).message,
            "tool.forbidden (awp.capability.tier): this capability needs the known tier"
        );
        let invalid_input = invalid("x.y", "bad");
        assert_eq!(refusal(&invalid_input).code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn a_timed_out_non_idempotent_call_is_not_retryable() {
        let limit = Duration::from_secs(1);
        assert!(!timeout_error(ToolEffect::NonIdempotent, limit).is_retryable());
        assert!(timeout_error(ToolEffect::ReadOnly, limit).is_retryable());
    }

    #[test]
    fn quoted_names_are_bounded() {
        let long = "a".repeat(500);
        assert_eq!(quoted(&long).chars().count(), QUOTED_NAME_LIMIT + 1);
        assert_eq!(quoted("quote"), "quote");
    }
}
