//! ADK-Rust tools served to an external ACP agent over the ACP connection.
//!
//! This is ACP's unstable MCP-over-ACP surface (`unstable_mcp_over_acp`). The
//! client declares an [`AcpToolServer`] in each session setup request as an
//! `acp` MCP server (`McpServer::Acp`, a name and an opaque `serverId`), and
//! the agent calls its tools by sending `mcp/message` requests back over the
//! same connection. No process is started and no port is opened: the tools run
//! in the client application.
//!
//! The inner protocol is MCP `2026-07-28`, which has no `initialize`
//! handshake. Every request carries its protocol version and the caller's
//! capabilities in `params._meta`. An [`AcpToolServer`] answers `tools/list`,
//! `tools/call` and `server/discover`; other methods get MCP's method-not-found
//! error.
//!
//! [`AcpConnection`](crate::AcpConnection) serves the tool servers configured
//! with [`AcpAgentConfig::tool_server`](crate::AcpAgentConfig::tool_server) and
//! declares them on `session/new`, `session/load`, `session/resume` and
//! `session/fork`, after checking that the agent advertises
//! `mcpCapabilities.acp`. An [`AcpToolServer`] is also an SDK
//! [`McpService`], so applications that drive the SDK directly can attach one
//! with `McpServer::new_service` and `SessionBuilder::with_mcp_server`.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_acp::{AcpAgentConfig, AcpToolServer};
//!
//! let config = AcpAgentConfig::new("my-coding-agent --acp")
//!     .tool_server(AcpToolServer::new("app-tools").tool(lookup_order_tool));
//! ```

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use adk_core::{
    ActingContext, Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext,
    SubjectKind, Tool, ToolContext,
};
use agent_client_protocol::mcp_server::{McpOutcome, McpRequest, McpRequestContext, McpService};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::v1::schema::{
    McpError, McpServer, McpServerAcp, McpServerAcpId, MessageMcpRequest, MessageMcpResponse,
};
use agent_client_protocol::{
    Agent, ConnectionTo, Dispatch, Error, HandleDispatchFrom, Handled, RequestCancellation, Role,
};
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Map, Value, json};
use tracing::debug;

/// The MCP protocol version MCP-over-ACP carries.
pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";

/// The `params._meta` key that names the request's MCP protocol version.
pub const MCP_PROTOCOL_VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";

/// The `params._meta` key that carries the caller's MCP client capabilities.
pub const MCP_CLIENT_CAPABILITIES_META: &str = "io.modelcontextprotocol/clientCapabilities";

const SERVER_INFO_META: &str = "io.modelcontextprotocol/serverInfo";

/// MCP's JSON-RPC error codes.
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

/// A named set of ADK-Rust tools served to an ACP agent as one MCP server.
///
/// The agent sees each tool under its ADK name, with its description and
/// parameter schema. A call runs [`Tool::execute`] in the client application;
/// a failed call is reported to the agent as an MCP tool result with
/// `isError: true`, so the agent's model can read the error and correct the
/// call.
///
/// Tools run with a minimal [`ToolContext`]: no session state, artifacts or
/// memory, and the function call id is the MCP request id. Tools that need
/// more should capture what they need when they are constructed.
#[derive(Clone)]
pub struct AcpToolServer {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for AcpToolServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpToolServer")
            .field("name", &self.name)
            .field("tools", &self.tools.iter().map(|tool| tool.name()).collect::<Vec<_>>())
            .finish()
    }
}

impl AcpToolServer {
    /// Create an empty tool server. `name` is the MCP server name the agent
    /// sees; it must be unique among a session's MCP servers.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), tools: Vec::new() }
    }

    /// Serve `tool`.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Serve every tool in `tools`.
    pub fn tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// The MCP server name the agent sees.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The names of the served tools.
    pub fn tool_names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(|tool| tool.name())
    }

    /// Check the server before it is declared: a non-empty name, and tool
    /// names that are present and unique.
    pub(crate) fn validate(&self) -> std::result::Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("ACP tool server names cannot be empty".into());
        }
        let mut names = HashSet::new();
        for tool in &self.tools {
            if tool.name().is_empty() {
                return Err(format!("ACP tool server '{}' has a tool without a name", self.name));
            }
            if !names.insert(tool.name()) {
                return Err(format!(
                    "ACP tool server '{}' serves two tools named '{}'",
                    self.name,
                    tool.name()
                ));
            }
        }
        Ok(())
    }

    /// Answer one MCP request.
    ///
    /// `request_id` is the logical MCP request id, used as the function call
    /// id of a tool call. The request's `params._meta` must name protocol
    /// version [`MCP_PROTOCOL_VERSION`] and carry client capabilities; a
    /// request that does not is answered with an MCP error, as is an
    /// `initialize` request, which MCP `2026-07-28` does not use.
    pub async fn handle(
        &self,
        request_id: &str,
        method: &str,
        params: Option<&Map<String, Value>>,
    ) -> McpOutcome {
        if let Some(error) = request_error(method, params) {
            return McpOutcome::Error(error);
        }
        match method {
            "tools/list" => McpOutcome::Result(self.list_tools()),
            "tools/call" => self.call_tool(request_id, params).await,
            "server/discover" => McpOutcome::Result(self.discover()),
            other => McpOutcome::Error(
                McpError::new(METHOD_NOT_FOUND, "Method not found")
                    .data(json!({ "method": other })),
            ),
        }
    }

    fn list_tools(&self) -> Value {
        let tools: Vec<Value> =
            self.tools.iter().map(|tool| tool_declaration(tool.as_ref())).collect();
        json!({
            "resultType": "complete",
            "tools": tools,
            "ttlMs": 0,
            "cacheScope": "private",
        })
    }

    fn discover(&self) -> Value {
        json!({
            "resultType": "complete",
            "supportedVersions": [MCP_PROTOCOL_VERSION],
            "capabilities": { "tools": {} },
            "ttlMs": 0,
            "cacheScope": "private",
            "_meta": {
                SERVER_INFO_META: { "name": self.name, "version": env!("CARGO_PKG_VERSION") },
            },
        })
    }

    async fn call_tool(&self, request_id: &str, params: Option<&Map<String, Value>>) -> McpOutcome {
        let Some(name) = params.and_then(|params| params.get("name")).and_then(Value::as_str)
        else {
            return McpOutcome::Error(McpError::new(
                INVALID_PARAMS,
                "tools/call requires a tool name in params.name",
            ));
        };
        let arguments = match params.and_then(|params| params.get("arguments")) {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(arguments @ Value::Object(_)) => arguments.clone(),
            Some(_) => {
                return McpOutcome::Error(McpError::new(
                    INVALID_PARAMS,
                    "tools/call arguments must be an object",
                ));
            }
        };
        let Some(tool) = self.tools.iter().find(|tool| tool.name() == name) else {
            return McpOutcome::Error(
                McpError::new(INVALID_PARAMS, format!("Unknown tool: {name}"))
                    .data(json!({ "name": name })),
            );
        };
        let context: Arc<dyn ToolContext> =
            Arc::new(McpCallContext::new(&self.name, request_id.to_string()));
        debug!(server = %self.name, tool = name, "ACP agent called an ADK tool over MCP");
        McpOutcome::Result(match tool.execute(context, arguments).await {
            Ok(value) => tool_result(value),
            Err(error) => json!({
                "resultType": "complete",
                "content": [{ "type": "text", "text": error.to_string() }],
                "isError": true,
            }),
        })
    }
}

/// An SDK MCP service, so an [`AcpToolServer`] can also be attached through
/// the SDK's own `McpServer::new_service` and `with_mcp_server` builders. A
/// call stops when the SDK cancels the operation.
impl<Counterpart: Role> McpService<Counterpart> for AcpToolServer {
    fn execute(
        &self,
        request: McpRequest,
        context: McpRequestContext<Counterpart>,
    ) -> BoxFuture<'static, std::result::Result<McpOutcome, Error>> {
        let server = self.clone();
        Box::pin(async move {
            let request_id = context.request_id().to_string();
            let cancellation = context.operation_cancellation().clone();
            let work = server.handle(&request_id, &request.method, request.params.as_ref());
            tokio::select! {
                outcome = work => Ok(outcome),
                () = cancellation.cancelled() => Err(Error::request_cancelled()),
            }
        })
    }
}

/// The MCP tool declaration of an ADK tool: its name, description and input
/// schema. A tool without a parameter schema takes an empty object.
fn tool_declaration(tool: &dyn Tool) -> Value {
    let input_schema = match tool.parameters_schema() {
        Some(schema @ Value::Object(_)) => schema,
        _ => json!({ "type": "object" }),
    };
    json!({
        "name": tool.name(),
        "description": tool.enhanced_description(),
        "inputSchema": input_schema,
    })
}

/// The MCP `tools/call` result for an ADK tool's return value: an object is
/// structured content with its JSON as text; anything else is text.
fn tool_result(value: Value) -> Value {
    match value {
        Value::Object(_) => json!({
            "resultType": "complete",
            "content": [{ "type": "text", "text": value.to_string() }],
            "structuredContent": value,
        }),
        Value::String(text) => json!({
            "resultType": "complete",
            "content": [{ "type": "text", "text": text }],
        }),
        other => json!({
            "resultType": "complete",
            "content": [{ "type": "text", "text": other.to_string() }],
        }),
    }
}

/// The MCP error for an MCP-over-ACP request the binding refuses, if any: an
/// `initialize`, or `params._meta` that does not name
/// [`MCP_PROTOCOL_VERSION`] with a client capabilities object. These are the
/// checks the SDK's own binding applies, answered the same way, as MCP errors.
fn request_error(method: &str, params: Option<&Map<String, Value>>) -> Option<McpError> {
    let invalid = |detail: String| {
        Some(McpError::new(INVALID_PARAMS, "Invalid params").data(Value::String(detail)))
    };
    if method == "initialize" {
        return Some(
            McpError::new(METHOD_NOT_FOUND, "Method not found")
                .data(json!("native MCP requests do not use initialize")),
        );
    }
    let Some(meta) = params.and_then(|params| params.get("_meta")).and_then(Value::as_object)
    else {
        return invalid("inner params._meta must be an object".into());
    };
    let Some(version) = meta.get(MCP_PROTOCOL_VERSION_META).and_then(Value::as_str) else {
        return invalid(format!("inner params._meta requires {MCP_PROTOCOL_VERSION_META}"));
    };
    if version != MCP_PROTOCOL_VERSION {
        return Some(
            McpError::new(UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version")
                .data(json!({ "requested": version, "supported": [MCP_PROTOCOL_VERSION] })),
        );
    }
    if !meta.get(MCP_CLIENT_CAPABILITIES_META).is_some_and(Value::is_object) {
        return invalid(format!(
            "inner params._meta requires {MCP_CLIENT_CAPABILITIES_META} object"
        ));
    }
    None
}

/// A server id for one connection's tool server. Ids are opaque to the
/// agent; this one is unique within the process and hard to guess.
fn server_id(name: &str) -> McpServerAcpId {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write(name.as_bytes());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(elapsed.as_nanos());
    }
    McpServerAcpId::new(format!("adk-tools:{:016x}", hasher.finish()))
}

/// The tool servers one connection serves, with the ids it declared them
/// under.
#[derive(Clone, Default)]
pub(crate) struct ToolServers {
    servers: Arc<Vec<(McpServerAcpId, AcpToolServer)>>,
}

impl ToolServers {
    /// Give each server a fresh id.
    pub(crate) fn new(servers: &[AcpToolServer]) -> Self {
        Self {
            servers: Arc::new(
                servers.iter().map(|server| (server_id(&server.name), server.clone())).collect(),
            ),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// The `McpServer::Acp` declarations to send with a session setup request.
    pub(crate) fn declarations(&self) -> Vec<McpServer> {
        self.servers
            .iter()
            .map(|(id, server)| McpServer::Acp(McpServerAcp::new(server.name.clone(), id.clone())))
            .collect()
    }

    fn find(&self, id: &McpServerAcpId) -> Option<&AcpToolServer> {
        self.servers.iter().find(|(declared, _)| declared == id).map(|(_, server)| server)
    }
}

/// Answers the agent's `mcp/message` requests for one connection's tool
/// servers.
///
/// Requests for other server ids are passed on, so a server the application
/// attached through the SDK is still reached. A request id that is already
/// running on the same server is refused, and a request whose ACP request is
/// cancelled stops its tool call.
pub(crate) struct ToolServerHost {
    servers: ToolServers,
    active: Arc<Mutex<HashSet<(McpServerAcpId, String)>>>,
}

impl ToolServerHost {
    pub(crate) fn new(servers: ToolServers) -> Self {
        Self { servers, active: Arc::default() }
    }
}

/// Removes a request id from the active set however the call ends.
struct ActiveRequest {
    active: Arc<Mutex<HashSet<(McpServerAcpId, String)>>>,
    key: (McpServerAcpId, String),
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.key);
    }
}

impl HandleDispatchFrom<Agent> for ToolServerHost {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        connection: ConnectionTo<Agent>,
    ) -> std::result::Result<Handled<Dispatch>, Error> {
        if self.servers.is_empty() {
            return Ok(Handled::No { message, retry: false });
        }
        let servers = self.servers.clone();
        let active = self.active.clone();
        MatchDispatch::new(message)
            .if_request(async move |request: MessageMcpRequest, responder| {
                let Some(server) = servers.find(&request.server_id).cloned() else {
                    return Ok(Handled::No { message: (request, responder), retry: false });
                };
                let key = (request.server_id.clone(), request.request_id.to_string());
                let admitted = active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key.clone());
                if !admitted {
                    responder.respond_with_error(
                        Error::invalid_params().data("duplicate active MCP requestId"),
                    )?;
                    return Ok(Handled::Yes);
                }
                let guard = ActiveRequest { active, key };
                let cancellation: RequestCancellation = responder.cancellation();
                connection.spawn(async move {
                    let _guard = guard;
                    let request_id = request.request_id.to_string();
                    let work = server.handle(&request_id, &request.method, request.params.as_ref());
                    let outcome = tokio::select! {
                        outcome = work => outcome,
                        () = cancellation.cancelled() => {
                            return responder.respond_with_error(Error::request_cancelled());
                        }
                    };
                    responder.respond(match outcome {
                        McpOutcome::Result(result) => MessageMcpResponse::success(result),
                        McpOutcome::Error(error) => MessageMcpResponse::error(error),
                    })
                })?;
                Ok(Handled::Yes)
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ADK-Rust ACP tool servers"
    }
}

/// The [`ToolContext`] an ADK tool runs with when an ACP agent calls it.
struct McpCallContext {
    server: String,
    function_call_id: String,
    invocation_id: String,
    user_content: Content,
    actions: Mutex<EventActions>,
}

impl McpCallContext {
    fn new(server: &str, function_call_id: String) -> Self {
        Self {
            server: server.to_string(),
            invocation_id: format!("mcp-over-acp:{function_call_id}"),
            function_call_id,
            user_content: Content::new("user"),
            actions: Mutex::new(EventActions::default()),
        }
    }
}

#[async_trait]
impl ReadonlyContext for McpCallContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    fn agent_name(&self) -> &str {
        &self.server
    }

    fn user_id(&self) -> &str {
        "acp-agent"
    }

    fn app_name(&self) -> &str {
        &self.server
    }

    fn session_id(&self) -> &str {
        ""
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn acting_context(&self) -> ActingContext {
        ActingContext::derived(&self.server, &self.server, "acp-agent", SubjectKind::Agent)
    }
}

#[async_trait]
impl CallbackContext for McpCallContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for McpCallContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = actions;
    }

    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echo the arguments."
        }

        fn parameters_schema(&self) -> Option<Value> {
            Some(json!({ "type": "object", "properties": { "text": { "type": "string" } } }))
        }

        async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
            if args.get("fail").is_some() {
                return Err(adk_core::AdkError::tool("asked to fail"));
            }
            Ok(json!({ "echo": args, "call": ctx.function_call_id() }))
        }
    }

    fn meta() -> Map<String, Value> {
        let mut params = Map::new();
        params.insert(
            "_meta".into(),
            json!({ MCP_PROTOCOL_VERSION_META: MCP_PROTOCOL_VERSION, MCP_CLIENT_CAPABILITIES_META: {} }),
        );
        params
    }

    fn server() -> AcpToolServer {
        AcpToolServer::new("app").tool(Arc::new(Echo))
    }

    fn result(outcome: McpOutcome) -> Value {
        match outcome {
            McpOutcome::Result(value) => value,
            McpOutcome::Error(error) => panic!("expected a result, got {error:?}"),
        }
    }

    fn error(outcome: McpOutcome) -> McpError {
        match outcome {
            McpOutcome::Error(error) => error,
            McpOutcome::Result(value) => panic!("expected an error, got {value}"),
        }
    }

    #[tokio::test]
    async fn lists_tools_with_their_schemas() {
        let listed = result(server().handle("1", "tools/list", Some(&meta())).await);
        assert_eq!(listed["resultType"], "complete");
        assert_eq!(listed["tools"][0]["name"], "echo");
        assert_eq!(listed["tools"][0]["inputSchema"]["properties"]["text"]["type"], "string");
    }

    #[tokio::test]
    async fn calls_a_tool_and_reports_failures_as_tool_results() {
        let mut params = meta();
        params.insert("name".into(), json!("echo"));
        params.insert("arguments".into(), json!({ "text": "hi" }));
        let called = result(server().handle("call-7", "tools/call", Some(&params)).await);
        assert_eq!(called["structuredContent"]["echo"]["text"], "hi");
        assert_eq!(
            called["structuredContent"]["call"], "call-7",
            "the MCP request id is the call id"
        );
        assert!(called.get("isError").is_none());

        params.insert("arguments".into(), json!({ "fail": true }));
        let failed = result(server().handle("call-8", "tools/call", Some(&params)).await);
        assert_eq!(failed["isError"], true);
        assert!(failed["content"][0]["text"].as_str().unwrap().contains("asked to fail"));
    }

    #[tokio::test]
    async fn rejects_unknown_tools_methods_and_bad_metadata() {
        let mut params = meta();
        params.insert("name".into(), json!("missing"));
        assert_eq!(
            error(server().handle("1", "tools/call", Some(&params)).await).code,
            INVALID_PARAMS
        );
        assert_eq!(
            error(server().handle("1", "resources/list", Some(&meta())).await).code,
            METHOD_NOT_FOUND
        );
        assert_eq!(
            error(server().handle("1", "initialize", Some(&meta())).await).code,
            METHOD_NOT_FOUND
        );
        assert_eq!(error(server().handle("1", "tools/list", None).await).code, INVALID_PARAMS);

        let mut old = Map::new();
        old.insert(
            "_meta".into(),
            json!({ MCP_PROTOCOL_VERSION_META: "2025-06-18", MCP_CLIENT_CAPABILITIES_META: {} }),
        );
        assert_eq!(
            error(server().handle("1", "tools/list", Some(&old)).await).code,
            UNSUPPORTED_PROTOCOL_VERSION
        );
    }

    #[test]
    fn validates_names_and_gives_each_connection_fresh_ids() {
        assert!(server().validate().is_ok());
        assert!(AcpToolServer::new(" ").validate().is_err());
        assert!(server().tool(Arc::new(Echo)).validate().is_err(), "duplicate tool names");

        let first = ToolServers::new(&[server()]).declarations();
        let second = ToolServers::new(&[server()]).declarations();
        let id = |servers: &[McpServer]| match &servers[0] {
            McpServer::Acp(server) => server.server_id.clone(),
            other => panic!("expected an ACP declaration, got {other:?}"),
        };
        assert_ne!(id(&first), id(&second));
    }
}
