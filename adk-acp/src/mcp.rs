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
//! Every `tools/call` runs through `adk_core::authorize_tool_call` and the
//! action ledger under the server's [run configuration](AcpToolServer::run_config)
//! before the tool executes, as an agent run's tool calls do.
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

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{
    ActingContext, ApprovalScope, Artifacts, CallbackContext, Content, EventActions,
    GovernanceControl, GovernedCall, MemoryEntry, ReadonlyContext, RunConfig, SubjectKind,
    SubjectRef, Tool, ToolAuthorization, ToolContext, ToolGate,
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
use tracing::{debug, info, warn};

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

/// The user every call an ACP agent makes is scoped and recorded for: the agent
/// process, not a person.
const ACP_AGENT_USER: &str = "acp-agent";

/// A named set of ADK-Rust tools served to an ACP agent as one MCP server.
///
/// The agent sees each tool under its ADK name, with its description and
/// parameter schema. A call runs in the client application, in this order:
///
/// 1. `adk_core::authorize_tool_call` under the server's
///    [run configuration](Self::run_config): its kill switches, including the
///    server's own [`governance`](Self::governance) control, its consent gate,
///    egress and tool policies, and confirmation through its handler or approval
///    store. Its audit trail records each decision.
/// 2. The action ledger, for a [`ToolEffect::NonIdempotent`](adk_core::ToolEffect)
///    tool: a call the ledger already holds is answered from its record without
///    running, and a ledger that cannot be read or written refuses the call.
/// 3. The kill switch again, then [`Tool::execute`].
///
/// A refused or failed call is reported to the agent as an MCP tool result with
/// `isError: true`, so the agent's model can read why. A call held for approval
/// is not run in the request that asked for it: with an approval store or service
/// it waits there, and the same call made again on the connection after an
/// approval runs; with neither, nothing can approve it, so it is refused.
///
/// Without a tool policy every served tool is allowed, as in an agent run
/// without one; the kill switch, ledger and audit still apply.
///
/// Tools run with a minimal [`ToolContext`]: no session state, artifacts or
/// memory. Its function call id is the MCP request id, its session is the
/// connection's server id, and it exposes the server's run configuration, so an
/// agent-as-tool delegate runs under the same governance. Tools that need more
/// should capture what they need when they are constructed.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_acp::AcpToolServer;
/// use adk_core::{DeclarativePolicy, GovernanceControl, RunConfig};
///
/// let operator = GovernanceControl::new();
/// let server = AcpToolServer::new("app-tools").run_config(
///     RunConfig::builder()
///         .governance(operator.clone())
///         .tool_policy(Arc::new(DeclarativePolicy::builder().allow("lookup_order").build()))
///         .build(),
/// );
///
/// // Either control refuses every call while it is frozen.
/// operator.freeze("incident 4012");
/// server.governance().freeze("incident 4012");
/// assert!(server.governance().is_frozen());
/// ```
#[derive(Clone)]
pub struct AcpToolServer {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
    run_config: Arc<RunConfig>,
    governance: GovernanceControl,
}

impl std::fmt::Debug for AcpToolServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpToolServer")
            .field("name", &self.name)
            .field("tools", &self.tools.iter().map(|tool| tool.name()).collect::<Vec<_>>())
            .field("frozen", &self.governance.is_frozen())
            .finish_non_exhaustive()
    }
}

impl AcpToolServer {
    /// Create an empty tool server. `name` is the MCP server name the agent
    /// sees; it must be unique among a session's MCP servers.
    ///
    /// The server governs its calls under the default run configuration with its
    /// own [`governance`](Self::governance) control attached.
    pub fn new(name: impl Into<String>) -> Self {
        let governance = GovernanceControl::new();
        let run_config = Arc::new(RunConfig::builder().governance(governance.clone()).build());
        Self { name: name.into(), tools: Vec::new(), run_config, governance }
    }

    /// Govern every call under `config`.
    ///
    /// Its kill switches, consent gate, egress and tool policies, confirmation
    /// handler, approval store and service, action ledger, and audit trail apply
    /// to the agent's calls as they do to an agent run's tool calls. The server's
    /// own [`governance`](Self::governance) control stays attached.
    ///
    /// Approval comes only from what `config` attaches: its
    /// [`tool_confirmation_decisions`](RunConfig::tool_confirmation_decisions) and
    /// their fingerprints are dropped, because the agent chooses each call's id.
    /// The calls act as the agent named after the server, whose principal
    /// [`RunConfig::acting`] may set; nothing the agent sends names it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_acp::AcpToolServer;
    /// use adk_core::{DeclarativePolicy, InMemoryActionLedger, RunConfig};
    ///
    /// let server = AcpToolServer::new("app-tools").run_config(
    ///     RunConfig::builder()
    ///         .tool_policy(Arc::new(
    ///             DeclarativePolicy::builder()
    ///                 .allow("lookup_order")
    ///                 .require_approval("refund_order", "refunds move money")
    ///                 .build(),
    ///         ))
    ///         .action_ledger(Arc::new(InMemoryActionLedger::new()))
    ///         .build(),
    /// );
    /// assert_eq!(server.name(), "app-tools");
    /// ```
    pub fn run_config(mut self, mut config: RunConfig) -> Self {
        // A call's id is the agent's MCP request id, so a decision keyed by it could be
        // matched by the caller it is meant to govern.
        config.tool_confirmation_decisions.clear();
        config.tool_confirmation_fingerprints.clear();
        if !config.governance.iter().any(|control| control.same_as(&self.governance)) {
            config.governance.push(self.governance.clone());
        }
        self.run_config = Arc::new(config);
        self
    }

    /// The server's own kill switch. Clones of the server share it; while it is
    /// frozen every call is refused without running.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_acp::AcpToolServer;
    ///
    /// let server = AcpToolServer::new("app-tools");
    /// server.governance().freeze("incident 4012");
    /// assert!(server.clone().governance().is_frozen());
    /// ```
    pub fn governance(&self) -> &GovernanceControl {
        &self.governance
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
    ///
    /// A tool call runs through the governed path described on [`AcpToolServer`].
    /// A request answered here names no connection, so its approval scope and
    /// idempotency key carry the server name and request id only.
    pub async fn handle(
        &self,
        request_id: &str,
        method: &str,
        params: Option<&Map<String, Value>>,
    ) -> McpOutcome {
        self.serve("", request_id, method, params).await
    }

    /// [`handle`](Self::handle) for a request sent to the server declared as
    /// `server_id`, which scopes a call's approvals and idempotency key to the
    /// connection that declared it.
    async fn serve(
        &self,
        server_id: &str,
        request_id: &str,
        method: &str,
        params: Option<&Map<String, Value>>,
    ) -> McpOutcome {
        if let Some(error) = request_error(method, params) {
            return McpOutcome::Error(error);
        }
        match method {
            "tools/list" => McpOutcome::Result(self.list_tools()),
            "tools/call" => self.call_tool(server_id, request_id, params).await,
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

    async fn call_tool(
        &self,
        server_id: &str,
        request_id: &str,
        params: Option<&Map<String, Value>>,
    ) -> McpOutcome {
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
        let context = Arc::new(McpCallContext::new(self, server_id, request_id));
        debug!(mcp.server = %self.name, tool.name = name, "ACP agent called an ADK tool over MCP");
        McpOutcome::Result(match self.run_governed(tool, context, arguments).await {
            Ok(value) => tool_result(value),
            Err(text) => json!({
                "resultType": "complete",
                "content": [{ "type": "text", "text": text }],
                "isError": true,
            }),
        })
    }

    /// Runs one call through the governed path, the action ledger and the tool.
    /// `Err` is the text the agent is answered with: why the call was not run, or
    /// the tool's error.
    async fn run_governed(
        &self,
        tool: &Arc<dyn Tool>,
        context: Arc<McpCallContext>,
        arguments: Value,
    ) -> std::result::Result<Value, String> {
        let config = self.run_config.as_ref();
        let name = tool.name();
        let gate = ToolGate::new(
            config,
            &self.name,
            context.invocation_id(),
            ApprovalScope::new(&self.name, ACP_AGENT_USER, context.session_id()),
        )
        .with_acting_context(context.acting_context());
        let call = GovernedCall::for_tool(tool.as_ref(), context.function_call_id(), arguments);
        let arguments = match adk_core::authorize_tool_call(&gate, call).await {
            Ok(ToolAuthorization::Execute { args, .. }) => args,
            Ok(ToolAuthorization::Refuse { reason, .. }) => {
                info!(mcp.server = %self.name, tool.name = name, "ACP agent's tool call refused");
                return Err(reason);
            }
            // Nothing waits on a held call here: the request that asked for it is answered
            // now, and an approval reaches the call only when the agent makes it again.
            Ok(ToolAuthorization::Pending { reason, .. }) => {
                if config.approval_store.is_some() || config.approval_service.is_some() {
                    info!(
                        mcp.server = %self.name,
                        tool.name = name,
                        "ACP agent's tool call held for approval"
                    );
                    return Err(format!(
                        "{reason}. It was not run and waits for an approver's decision; call it \
                         again with the same arguments once it is approved."
                    ));
                }
                warn!(
                    mcp.server = %self.name,
                    tool.name = name,
                    "ACP agent's tool call needs approval and the tool server has no approver"
                );
                return Err(format!(
                    "{reason}. It was not run: tool server '{}' has no approver. Attach a \
                     ToolConfirmationHandler or an ApprovalStore with AcpToolServer::run_config, \
                     or allow the tool in its ToolPolicy.",
                    self.name
                ));
            }
            Err(error)
                if error.code == adk_core::GOVERNANCE_FROZEN_CODE
                    || error.code == adk_core::GOVERNANCE_STALE_CODE =>
            {
                info!(
                    mcp.server = %self.name,
                    tool.name = name,
                    error = %error.diagnostics(),
                    "ACP agent's tool call not run: the governance control refuses work"
                );
                return Err(adk_core::not_run_text(&error));
            }
            // A handler, store or audit failure can name its backend, so the agent is given
            // the error's code only.
            Err(error) => {
                warn!(
                    mcp.server = %self.name,
                    tool.name = name,
                    error = %error.diagnostics(),
                    "ACP agent's tool call not run: its authorization failed"
                );
                return Err(format!(
                    "Tool '{name}' was not run: its authorization failed ({})",
                    error.redacted()
                ));
            }
        };

        // A non-idempotent call runs at most once per idempotency key; a ledger that
        // cannot be read or written refuses it.
        let ledgered = match adk_core::begin_ledgered_call(
            config,
            context.as_ref(),
            tool.effect(),
            context.idempotency_key(),
            name,
            &arguments,
        )
        .await
        {
            Ok(Ok(ledgered)) => ledgered,
            Ok(Err(answer)) => return Err(answer.response.to_string()),
            Err(error) => {
                return Err(format!("Tool '{name}' was not run: {}", error.redacted()));
            }
        };

        // The ledger write can outlast a freeze, so a call the switch refuses now never
        // starts, and its begun record is completed with the refusal.
        if let Err(refusal) = config.check_governance() {
            let text = adk_core::not_run_text(&refusal);
            info!(
                mcp.server = %self.name,
                tool.name = name,
                error = %refusal.diagnostics(),
                "ACP agent's tool call not run: the governance control refused it before it started"
            );
            if let Some(trail) = &config.audit {
                let draft = AuditRecordDraft::new(AuditRecordKind::ToolCall, name)
                    .actor(AuditActor::for_context(context.as_ref()))
                    .args_digest(trail.digest(&arguments))
                    .outcome(AuditRecordOutcome::NotRun)
                    .ext("reason", Value::String(text.clone()))
                    .ext("sessionId", Value::String(context.session_id().to_string()));
                if let Err(error) = trail.record(draft).await {
                    warn!(tool.name = name, error = %error.diagnostics(), "not-run record not written");
                }
            }
            if let Some(ledgered) = ledgered {
                ledgered.complete(name, None, &text).await;
            }
            return Err(text);
        }

        let result = tool.execute(context, arguments).await.map_err(|error| error.to_string());
        if let Some(ledgered) = ledgered {
            match &result {
                Ok(value) => ledgered.complete(name, Some(value), "").await,
                Err(error) => ledgered.complete(name, None, error).await,
            }
        }
        result
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
            let work = server.serve(
                &context.server_id().0,
                &request_id,
                &request.method,
                request.params.as_ref(),
            );
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
                    let work = server.serve(
                        &request.server_id.0,
                        &request_id,
                        &request.method,
                        request.params.as_ref(),
                    );
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
    /// The connection's declared server id, so two connections' request ids never
    /// share an approval scope or idempotency key.
    session_id: String,
    function_call_id: String,
    invocation_id: String,
    user_content: Content,
    actions: Mutex<EventActions>,
    run_config: Arc<RunConfig>,
}

impl McpCallContext {
    fn new(server: &AcpToolServer, server_id: &str, function_call_id: &str) -> Self {
        Self {
            server: server.name.clone(),
            session_id: server_id.to_string(),
            invocation_id: format!("mcp-over-acp:{function_call_id}"),
            function_call_id: function_call_id.to_string(),
            user_content: Content::new("user"),
            actions: Mutex::new(EventActions::default()),
            run_config: server.run_config.clone(),
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
        ACP_AGENT_USER
    }

    fn app_name(&self) -> &str {
        &self.server
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    // The principal comes from local configuration only, never from the agent's own
    // `initialize` details or requests.
    fn acting_context(&self) -> ActingContext {
        ActingContext::new(self.run_config.acting.principal_for(&self.server, &self.server))
            .on_behalf_of(SubjectRef::new(SubjectKind::Agent, ACP_AGENT_USER))
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

    // An agent-as-tool delegate reads it, so its own calls run under the same governance.
    fn run_config(&self) -> Option<&RunConfig> {
        Some(&self.run_config)
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

    // ─── The governed path ──────────────────────────────────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering};

    use adk_core::audit_log::{AuditDecision, AuditTrail, InMemoryAuditRecorder};
    use adk_core::{
        ActionLedger, ActionOutcome, ApprovalStore, DeclarativePolicy, InMemoryActionLedger,
        InMemoryApprovalStore, ToolApproval, ToolConfirmationDecision,
    };

    /// A payment: non-idempotent, and it counts its runs.
    struct Pay(Arc<AtomicUsize>);

    #[async_trait]
    impl Tool for Pay {
        fn name(&self) -> &str {
            "pay"
        }

        fn description(&self) -> &str {
            "Pay an invoice."
        }

        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            args: Value,
        ) -> adk_core::Result<Value> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "paid": args["cents"] }))
        }
    }

    fn pay_server(runs: &Arc<AtomicUsize>, config: RunConfig) -> AcpToolServer {
        AcpToolServer::new("app").tool(Arc::new(Pay(runs.clone()))).run_config(config)
    }

    fn policy(policy: DeclarativePolicy) -> RunConfig {
        RunConfig::builder().tool_policy(Arc::new(policy)).build()
    }

    /// Calls `pay` through the server declared as `server_id` on a connection.
    async fn pay(server: &AcpToolServer, server_id: &str, request_id: &str) -> Value {
        let mut params = meta();
        params.insert("name".into(), json!("pay"));
        params.insert("arguments".into(), json!({ "cents": 500 }));
        result(server.serve(server_id, request_id, "tools/call", Some(&params)).await)
    }

    fn refusal(answer: &Value) -> &str {
        assert_eq!(answer["isError"], true, "expected a refusal, got {answer}");
        answer["content"][0]["text"].as_str().expect("refusal text")
    }

    #[tokio::test]
    async fn a_denied_call_is_not_executed() {
        let runs = Arc::new(AtomicUsize::new(0));
        let recorder = InMemoryAuditRecorder::new();
        let mut config =
            policy(DeclarativePolicy::builder().deny("pay", "payments are off").build());
        config.audit = Some(AuditTrail::new(Arc::new(recorder.clone())));
        let server = pay_server(&runs, config);

        let answer = pay(&server, "srv-1", "1").await;
        assert!(refusal(&answer).contains("payments are off"), "got {answer}");
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        let decisions: Vec<_> =
            recorder.records().into_iter().map(|record| record.decision).collect();
        assert_eq!(decisions, vec![Some(AuditDecision::Denied)]);
    }

    #[tokio::test]
    async fn a_frozen_kill_switch_refuses_calls_until_unfrozen() {
        let runs = Arc::new(AtomicUsize::new(0));
        let operator = GovernanceControl::new();
        let server = pay_server(&runs, RunConfig::builder().governance(operator.clone()).build());

        operator.freeze("incident 4012");
        let answer = pay(&server, "srv-1", "1").await;
        let text = refusal(&answer);
        assert!(adk_core::is_not_run(text) && text.contains("incident 4012"), "got {answer}");
        operator.unfreeze();

        // The server's own switch, shared by its clones, refuses as well.
        server.clone().governance().freeze("drill");
        assert!(adk_core::is_not_run(refusal(&pay(&server, "srv-1", "2").await)));
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        server.governance().unfreeze();
        assert_eq!(pay(&server, "srv-1", "3").await["structuredContent"]["paid"], 500);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_call_needing_approval_with_no_approver_is_refused() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut config = policy(
            DeclarativePolicy::builder().require_approval("pay", "a person decides").build(),
        );
        // A decision keyed by the call's id is dropped: the agent chooses the id, so it
        // would approve its own call.
        config.tool_confirmation_decisions =
            std::collections::HashMap::from([("1".to_string(), ToolConfirmationDecision::Approve)]);
        let server = pay_server(&runs, config);

        let answer = pay(&server, "srv-1", "1").await;
        let text = refusal(&answer);
        assert!(text.contains("requires confirmation: a person decides"), "got {answer}");
        assert!(text.contains("has no approver"), "got {answer}");
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_held_call_waits_in_the_approval_store_and_runs_once_approved() {
        let runs = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(InMemoryApprovalStore::new());
        let mut config = policy(
            DeclarativePolicy::builder().require_approval("pay", "a person decides").build(),
        );
        config.approval_store = Some(store.clone());
        let server = pay_server(&runs, config);

        let answer = pay(&server, "srv-1", "1").await;
        assert!(refusal(&answer).contains("waits for an approver's decision"), "got {answer}");
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        let scope = ApprovalScope::new("app", ACP_AGENT_USER, "srv-1");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1, "the call is held on its connection");
        store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();

        // The same call made again on the connection runs on the approval.
        assert_eq!(pay(&server, "srv-1", "2").await["structuredContent"]["paid"], 500);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // Another connection's call is held again: the approval was for that connection.
        assert!(refusal(&pay(&server, "srv-2", "1").await).contains("waits for an approver"));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_allowed_call_runs_and_is_recorded() {
        let runs = Arc::new(AtomicUsize::new(0));
        let recorder = InMemoryAuditRecorder::new();
        let ledger = Arc::new(InMemoryActionLedger::new());
        let mut config = policy(DeclarativePolicy::builder().allow("pay").build());
        config.audit = Some(AuditTrail::new(Arc::new(recorder.clone())));
        config.action_ledger = Some(ledger.clone());
        let server = pay_server(&runs, config);

        assert_eq!(pay(&server, "srv-1", "7").await["structuredContent"]["paid"], 500);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let decisions: Vec<_> =
            recorder.records().into_iter().map(|record| record.decision).collect();
        assert_eq!(decisions, vec![Some(AuditDecision::Allowed)]);
        let record = ledger
            .get("app/acp-agent/srv-1/mcp-over-acp:7/7")
            .await
            .unwrap()
            .expect("the call is ledgered under its connection and request id");
        assert!(matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })), "got {record:?}");

        // A replay on the connection is answered from the ledger without running.
        assert!(refusal(&pay(&server, "srv-1", "7").await).contains("already_succeeded"));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // The same request id on another connection is another call.
        assert_eq!(pay(&server, "srv-2", "7").await["structuredContent"]["paid"], 500);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// Reports the run configuration its context exposes, as an agent-as-tool delegate
    /// reads it.
    struct Inspect;

    #[async_trait]
    impl Tool for Inspect {
        fn name(&self) -> &str {
            "inspect"
        }

        fn description(&self) -> &str {
            "Report the run configuration."
        }

        async fn execute(
            &self,
            ctx: Arc<dyn ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            let config = ctx.run_config().expect("the context exposes the run configuration");
            Ok(json!({
                "policy": config.tool_policy.is_some(),
                "switches": config.governance.len(),
                "session": ctx.session_id(),
            }))
        }
    }

    #[tokio::test]
    async fn tools_run_under_the_servers_run_configuration() {
        let operator = GovernanceControl::new();
        let mut config = policy(DeclarativePolicy::builder().allow("inspect").build());
        config.governance.push(operator);
        let server = AcpToolServer::new("app").tool(Arc::new(Inspect)).run_config(config);

        let mut params = meta();
        params.insert("name".into(), json!("inspect"));
        let answer = result(server.serve("srv-1", "1", "tools/call", Some(&params)).await);
        assert_eq!(
            answer["structuredContent"],
            json!({ "policy": true, "switches": 2, "session": "srv-1" }),
            "the operator's switch and the server's own"
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
