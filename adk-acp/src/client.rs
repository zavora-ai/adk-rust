//! A connection to an external ACP agent, shared by its sessions.
//!
//! [`AcpConnection`] owns one agent process (or any other SDK transport), runs
//! the ACP `initialize` handshake, authenticates when configured, and answers
//! the agent's requests: permissions through the [`PermissionPolicy`], file
//! and terminal access through the configured hosts, and elicitations through
//! an [`AcpElicitation`]. Its methods cover the connection-level protocol
//! surface — listing, loading, resuming, forking, archiving and deleting
//! sessions, session options and status, and authentication — and each
//! session it opens is an [`AcpSession`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_acp::{AcpAgentConfig, AcpConnection, PermissionPolicy};
//! use std::sync::Arc;
//!
//! let connection = AcpConnection::connect(
//!     AcpAgentConfig::new("my-coding-agent --acp").working_dir("/path/to/project"),
//!     Arc::new(PermissionPolicy::DenyAll),
//! )
//! .await?;
//! println!("negotiated ACP v{}", connection.agent_info().protocol_version);
//!
//! for info in connection.list_sessions(None).await? {
//!     println!("{} {:?}", info.session_id, info.title);
//! }
//! let mut session = connection.new_session().await?;
//! let reply = session.prompt("Summarize the README").await?;
//! session.close().await?;
//! connection.close().await?;
//! ```

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, ArchiveSessionRequest, AuthMethod, AuthStatusRequest, AuthStatusResponse,
    AuthStatusUpdateNotification, AuthenticateRequest, ClientCapabilities,
    ClientSessionCapabilities, CompactionCapabilities, CompleteElicitationNotification,
    CreateElicitationRequest, CreateElicitationResponse, DeleteSessionRequest,
    DisableProviderRequest, ElicitationAction, ElicitationCapabilities,
    ElicitationFormCapabilities, ElicitationUrlCapabilities, ForkSessionRequest, Implementation,
    InitializeRequest, InitializeResponse, ListProvidersRequest, ListSessionsRequest,
    ListSessionsResponse, LoadSessionRequest, LogoutRequest, McpServer, NewSessionRequest,
    NoticeCapabilities, PermissionOptionKind, PlanCapabilities, ProviderId, ProviderInfo,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SessionArchiveFilter, SessionConfigOptionsCapabilities, SessionId,
    SessionInfo, SessionNotification, SessionOptionsRequest, SessionOptionsResponse, SessionStatus,
    SessionStatusRequest, SessionUpdate, SetProviderRequest, SubagentCapabilities, SystemPrompt,
    SystemPromptMode, UnarchiveSessionRequest,
};
use agent_client_protocol::{
    Agent, Client, ConnectTo, ConnectionTo, Dispatch, Error, HandleDispatchFrom, Handled, v1,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::connection::AcpAgentConfig;
use crate::elicitation::AcpElicitation;
use crate::error::{AcpError, Result};
use crate::host::{AcpHostHandler, capabilities as host_capabilities};
use crate::mcp::{ToolServerHost, ToolServers};
use crate::permissions::{PermissionPolicy, PermissionRequest, outcome_for};
use crate::session::AcpSession;
use crate::status::{AgentStatus, StatusTracker};
use crate::streaming::OutputChunk;

/// The ACP protocol versions this client speaks.
///
/// The SDK's v1 connection guard sends `initialize` for protocol v1 and
/// rejects an answer that selects another version; adk-acp checks the
/// negotiated version against this list as well.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V1];

/// What the agent reported about itself in `initialize`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AcpAgentInfo {
    /// The negotiated ACP protocol version.
    pub protocol_version: ProtocolVersion,
    /// The agent's name and version, when it sent them.
    pub agent: Option<Implementation>,
    /// The capabilities the agent advertised.
    pub capabilities: AgentCapabilities,
    /// The authentication methods the agent offers.
    pub auth_methods: Vec<AuthMethod>,
}

impl AcpAgentInfo {
    fn from_response(response: InitializeResponse) -> Self {
        Self {
            protocol_version: response.protocol_version,
            agent: response.agent_info,
            capabilities: response.agent_capabilities,
            auth_methods: response.auth_methods,
        }
    }
}

/// A connection to one ACP agent, shared by the sessions it opens.
///
/// Cloning is cheap; every clone uses the same agent connection. The agent
/// process stops when [`close`](Self::close) is called or the last clone and
/// the last session opened from it are dropped.
#[derive(Clone)]
pub struct AcpConnection {
    pub(crate) inner: Arc<ConnectionInner>,
}

pub(crate) struct ConnectionInner {
    pub(crate) connection: v1::ConnectionTo<Agent>,
    pub(crate) routes: Routes,
    pub(crate) info: AcpAgentInfo,
    pub(crate) config: AcpAgentConfig,
    /// The tool servers this connection serves, with their declared ids.
    tool_servers: ToolServers,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<std::result::Result<(), Error>>>>,
}

/// Watches a connection on behalf of [`stream_prompt`](crate::stream_prompt).
#[derive(Clone)]
pub(crate) struct Observer {
    pub(crate) status: StatusTracker,
    pub(crate) events: mpsc::Sender<OutputChunk>,
}

impl AcpConnection {
    /// Start the configured agent process and complete the ACP handshake.
    ///
    /// The handshake negotiates the protocol version, advertises the client's
    /// capabilities and identity, and authenticates with
    /// [`AcpAgentConfig::auth_method`] when one is configured.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be parsed or started, the agent
    /// selects an unsupported protocol version, or authentication fails.
    pub async fn connect(config: AcpAgentConfig, policy: Arc<PermissionPolicy>) -> Result<Self> {
        let agent = crate::connection::build_agent(&config)?;
        Self::open(agent, config, policy, None).await
    }

    /// Connect to an agent over any SDK transport, such as an in-process
    /// [`Channel`](agent_client_protocol::Channel) or a custom stream.
    ///
    /// `config.command` is not used; every other setting applies.
    pub async fn connect_to(
        transport: impl ConnectTo<Client> + 'static,
        config: AcpAgentConfig,
        policy: Arc<PermissionPolicy>,
    ) -> Result<Self> {
        Self::open(transport, config, policy, None).await
    }

    pub(crate) async fn open(
        transport: impl ConnectTo<Client> + 'static,
        config: AcpAgentConfig,
        policy: Arc<PermissionPolicy>,
        observer: Option<Observer>,
    ) -> Result<Self> {
        validate_tool_servers(&config)?;
        let tool_servers = ToolServers::new(&config.tool_servers);
        let tool_host = ToolServerHost::new(tool_servers.clone());
        let routes = Routes::default();
        let handler = ClientHandler {
            policy,
            routes: routes.clone(),
            elicitation: config.elicitation.clone(),
            observer,
        };
        let host = AcpHostHandler::new(config.filesystem.clone(), config.terminal.clone());
        let request = initialize_request(&config);
        let auth_method = config.auth_method.clone();
        let (ready_tx, ready_rx) = oneshot::channel::<
            std::result::Result<(v1::ConnectionTo<Agent>, InitializeResponse), Error>,
        >();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();

        let task = tokio::spawn(async move {
            Client
                .v1()
                .name("adk-acp")
                .with_handler(handler)
                .with_handler(host)
                .with_handler(tool_host)
                .connect_with(transport, async move |connection: v1::ConnectionTo<Agent>| {
                    match initialize(&connection, request, auth_method.as_deref()).await {
                        Ok(response) => {
                            if ready_tx.send(Ok((connection.clone(), response))).is_err() {
                                return Ok(());
                            }
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                            return Ok(());
                        }
                    }
                    tokio::select! {
                        _ = stop_rx => {}
                        () = connection.incoming_closed() => {}
                    }
                    Ok(())
                })
                .await
        });

        let (connection, response) = match ready_rx.await {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                let _ = task.await;
                return Err(AcpError::Protocol(error.to_string()));
            }
            Err(_) => {
                return Err(match task.await {
                    Ok(Err(error)) => AcpError::ConnectionLost(error.to_string()),
                    Ok(Ok(())) => AcpError::ConnectionLost(
                        "the agent closed the connection during initialize".into(),
                    ),
                    Err(error) => AcpError::ConnectionLost(error.to_string()),
                });
            }
        };
        let info = AcpAgentInfo::from_response(response);
        debug!(
            protocol_version = %info.protocol_version,
            agent = ?info.agent.as_ref().map(|agent| agent.name.as_str()),
            "ACP agent initialized"
        );
        Ok(Self {
            inner: Arc::new(ConnectionInner {
                connection,
                routes,
                info,
                config,
                tool_servers,
                stop: Mutex::new(Some(stop_tx)),
                task: Mutex::new(Some(task)),
            }),
        })
    }

    /// What the agent reported in `initialize`: the negotiated protocol
    /// version, its identity, capabilities and authentication methods.
    pub fn agent_info(&self) -> &AcpAgentInfo {
        &self.inner.info
    }

    /// The configuration this connection was opened with.
    pub fn config(&self) -> &AcpAgentConfig {
        &self.inner.config
    }

    /// Whether the agent has closed the connection.
    pub fn is_closed(&self) -> bool {
        self.inner.connection.is_incoming_closed()
    }

    /// The underlying SDK connection, for requests adk-acp does not wrap, such
    /// as extension methods.
    pub fn sdk_connection(&self) -> &v1::ConnectionTo<Agent> {
        &self.inner.connection
    }

    /// Create a session in the configured working directory (`session/new`).
    ///
    /// The request carries the configured MCP servers and tool servers,
    /// additional directories, system prompt and initial configuration values;
    /// each is checked against the agent's capabilities first.
    pub async fn new_session(&self) -> Result<AcpSession> {
        let config = &self.inner.config;
        let mut request = NewSessionRequest::new(&config.working_dir)
            .mcp_servers(self.session_mcp_servers()?)
            .additional_directories(self.additional_directories()?)
            .system_prompt(self.system_prompt()?);
        if !config.initial_config_options.is_empty() {
            self.require(
                self.capabilities().session_capabilities.options.is_some(),
                "session/options",
            )?;
            request = request.config_options(config.initial_config_options.clone());
        }
        // Updates the agent sends before answering are parked until the route
        // for the new session id exists.
        let response = self.request(request).await?;
        let events = self.inner.routes.register(&response.session_id);
        Ok(AcpSession::from_parts(
            self.clone(),
            response.session_id,
            events,
            response.modes,
            response.config_options,
        ))
    }

    /// Load a stored session (`session/load`), replaying its history.
    ///
    /// The replayed updates are waiting on the session; read them with
    /// [`AcpSession::pending_updates`].
    pub async fn load_session(&self, session_id: impl Into<SessionId>) -> Result<AcpSession> {
        self.require(self.capabilities().load_session, "session/load")?;
        let config = &self.inner.config;
        let session_id = session_id.into();
        let mut request = LoadSessionRequest::new(session_id.clone(), &config.working_dir)
            .mcp_servers(self.session_mcp_servers()?)
            .additional_directories(self.additional_directories()?)
            .system_prompt(self.system_prompt()?);
        if !config.initial_config_options.is_empty() {
            self.require(
                self.capabilities().session_capabilities.options.is_some(),
                "session/options",
            )?;
            request = request.config_options(config.initial_config_options.clone());
        }
        // Routed before the request, so the replay that precedes the response
        // reaches the session.
        let events = self.inner.routes.register(&session_id);
        let response =
            self.request(request).await.inspect_err(|_| self.inner.routes.remove(&session_id))?;
        Ok(AcpSession::from_parts(
            self.clone(),
            session_id,
            events,
            response.modes,
            response.config_options,
        ))
    }

    /// Resume a stored session without replaying its history (`session/resume`).
    pub async fn resume_session(&self, session_id: impl Into<SessionId>) -> Result<AcpSession> {
        self.require(self.capabilities().session_capabilities.resume.is_some(), "session/resume")?;
        let config = &self.inner.config;
        let session_id = session_id.into();
        let mut request = ResumeSessionRequest::new(session_id.clone(), &config.working_dir)
            .mcp_servers(self.session_mcp_servers()?)
            .additional_directories(self.additional_directories()?)
            .system_prompt(self.system_prompt()?);
        if !config.initial_config_options.is_empty() {
            self.require(
                self.capabilities().session_capabilities.options.is_some(),
                "session/options",
            )?;
            request = request.config_options(config.initial_config_options.clone());
        }
        let events = self.inner.routes.register(&session_id);
        let response =
            self.request(request).await.inspect_err(|_| self.inner.routes.remove(&session_id))?;
        Ok(AcpSession::from_parts(
            self.clone(),
            session_id,
            events,
            response.modes,
            response.config_options,
        ))
    }

    /// Fork a stored session into a new one (`session/fork`).
    pub async fn fork_session(&self, session_id: impl Into<SessionId>) -> Result<AcpSession> {
        self.require(self.capabilities().session_capabilities.fork.is_some(), "session/fork")?;
        let config = &self.inner.config;
        let request = ForkSessionRequest::new(session_id.into(), &config.working_dir)
            .mcp_servers(self.session_mcp_servers()?)
            .additional_directories(self.additional_directories()?)
            .system_prompt(self.system_prompt()?);
        let response = self.request(request).await?;
        let events = self.inner.routes.register(&response.session_id);
        Ok(AcpSession::from_parts(
            self.clone(),
            response.session_id,
            events,
            response.modes,
            response.config_options,
        ))
    }

    /// List the agent's stored sessions (`session/list`), following every
    /// page. `cwd` narrows the list to one working directory.
    ///
    /// Archived sessions are left out; use [`list_sessions_page`](Self::list_sessions_page)
    /// with an `archived` filter to include them.
    pub async fn list_sessions(&self, cwd: Option<PathBuf>) -> Result<Vec<SessionInfo>> {
        const MAX_PAGES: usize = 1000;
        let mut sessions = Vec::new();
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let page = self
                .list_sessions_page(ListSessionsRequest::new().cwd(cwd.clone()).cursor(cursor))
                .await?;
            sessions.extend(page.sessions);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(sessions),
            }
        }
        Err(AcpError::Protocol(format!("session/list returned more than {MAX_PAGES} pages")))
    }

    /// One page of `session/list`, for full control over the filters.
    pub async fn list_sessions_page(
        &self,
        request: ListSessionsRequest,
    ) -> Result<ListSessionsResponse> {
        self.require(self.capabilities().session_capabilities.list.is_some(), "session/list")?;
        if request.archived.as_ref().is_some_and(|filter| *filter != SessionArchiveFilter::Exclude)
        {
            self.require(
                self.capabilities().session_capabilities.archive.is_some(),
                "session/archive",
            )?;
        }
        self.request(request).await
    }

    /// Delete a stored session (`session/delete`).
    pub async fn delete_session(&self, session_id: impl Into<SessionId>) -> Result<()> {
        self.require(self.capabilities().session_capabilities.delete.is_some(), "session/delete")?;
        self.request(DeleteSessionRequest::new(session_id.into())).await.map(drop)
    }

    /// Archive a stored session (`session/archive`).
    pub async fn archive_session(&self, session_id: impl Into<SessionId>) -> Result<()> {
        self.require(
            self.capabilities().session_capabilities.archive.is_some(),
            "session/archive",
        )?;
        self.request(ArchiveSessionRequest::new(session_id.into())).await.map(drop)
    }

    /// Restore an archived session (`session/unarchive`).
    pub async fn unarchive_session(&self, session_id: impl Into<SessionId>) -> Result<()> {
        self.require(
            self.capabilities().session_capabilities.archive.is_some(),
            "session/archive",
        )?;
        self.request(UnarchiveSessionRequest::new(session_id.into())).await.map(drop)
    }

    /// Whether a session is open on the agent or only saved (`session/status`).
    pub async fn session_status(&self, session_id: impl Into<SessionId>) -> Result<SessionStatus> {
        self.require(self.capabilities().session_capabilities.status.is_some(), "session/status")?;
        Ok(self.request(SessionStatusRequest::new(session_id.into())).await?.status)
    }

    /// The modes and configuration options a session in the configured working
    /// directory would start with (`session/options`). With a session id, the
    /// options of that stored session.
    pub async fn session_options(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<SessionOptionsResponse> {
        self.require(
            self.capabilities().session_capabilities.options.is_some(),
            "session/options",
        )?;
        let config = &self.inner.config;
        self.request(
            SessionOptionsRequest::new(&config.working_dir)
                .additional_directories(config.additional_directories.clone())
                .session_id(session_id),
        )
        .await
    }

    /// Authenticate with one of the agent's [`auth_methods`](AcpAgentInfo::auth_methods)
    /// (`authenticate`).
    pub async fn authenticate(&self, method_id: impl Into<String>) -> Result<()> {
        let method_id = method_id.into();
        if !self.inner.info.auth_methods.iter().any(|method| *method.id().0 == *method_id) {
            return Err(AcpError::Unsupported(format!(
                "the ACP agent does not offer the authentication method '{method_id}'"
            )));
        }
        self.request(AuthenticateRequest::new(method_id)).await.map(drop)
    }

    /// End the agent's authenticated state (`logout`).
    pub async fn logout(&self) -> Result<()> {
        self.require(self.capabilities().auth.logout.is_some(), "logout")?;
        self.request(LogoutRequest::new()).await.map(drop)
    }

    /// The agent's authentication state and accounts (`auth/status`).
    pub async fn auth_status(&self) -> Result<AuthStatusResponse> {
        self.require(self.capabilities().auth.status.is_some(), "auth/status")?;
        self.request(AuthStatusRequest::new()).await
    }

    /// The model providers the agent lets the client configure, with each
    /// one's current non-secret routing target (`providers/list`).
    ///
    /// Requires the agent's `providers` capability. Configure providers before
    /// creating or loading sessions; the agent applies them to the sessions
    /// that follow, for the life of its process.
    pub async fn list_providers(&self) -> Result<Vec<ProviderInfo>> {
        self.require(self.capabilities().providers.is_some(), "providers")?;
        Ok(self.request(ListProvidersRequest::new()).await?.providers)
    }

    /// Replace one provider's API protocol, base URL and complete header map
    /// (`providers/set`).
    ///
    /// Headers may carry credentials. The agent never echoes them back, but
    /// SDK trace logging of JSON-RPC bodies would record them.
    pub async fn set_provider(&self, request: SetProviderRequest) -> Result<()> {
        self.require(self.capabilities().providers.is_some(), "providers")?;
        self.request(request).await.map(drop)
    }

    /// Disable a provider that is not required (`providers/disable`).
    /// Disabling an unknown provider succeeds.
    pub async fn disable_provider(&self, provider_id: impl Into<ProviderId>) -> Result<()> {
        self.require(self.capabilities().providers.is_some(), "providers")?;
        self.request(DisableProviderRequest::new(provider_id)).await.map(drop)
    }

    /// The most recent `auth/status_update` the agent sent, if any.
    pub fn last_auth_status(&self) -> Option<AuthStatusUpdateNotification> {
        self.inner.routes.last_auth_status()
    }

    /// Close the connection and stop the agent process.
    pub async fn close(&self) -> Result<()> {
        if let Some(stop) = self.inner.stop.lock().ok().and_then(|mut stop| stop.take()) {
            let _ = stop.send(());
        }
        let task = self.inner.task.lock().ok().and_then(|mut task| task.take());
        if let Some(task) = task {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(AcpError::ConnectionLost(error.to_string())),
                Err(error) => return Err(AcpError::ConnectionLost(error.to_string())),
            }
        }
        Ok(())
    }

    pub(crate) fn capabilities(&self) -> &AgentCapabilities {
        &self.inner.info.capabilities
    }

    /// Send a request and wait for its response.
    pub(crate) async fn request<Req: agent_client_protocol::JsonRpcRequest>(
        &self,
        request: Req,
    ) -> Result<Req::Response> {
        self.inner
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(|error| self.error(error))
    }

    pub(crate) fn error(&self, error: Error) -> AcpError {
        if self.is_closed() {
            AcpError::ConnectionLost(error.to_string())
        } else {
            AcpError::Protocol(error.to_string())
        }
    }

    pub(crate) fn require(&self, advertised: bool, surface: &str) -> Result<()> {
        if advertised {
            Ok(())
        } else {
            Err(AcpError::Unsupported(format!("the ACP agent does not advertise {surface}")))
        }
    }

    /// The MCP servers to declare on a session setup request: the configured
    /// ones, then the connection's tool servers, each checked against the
    /// agent's MCP capabilities.
    fn session_mcp_servers(&self) -> Result<Vec<McpServer>> {
        let mut servers = self.inner.config.mcp_servers.clone();
        servers.extend(self.inner.tool_servers.declarations());
        check_mcp_servers(self.capabilities(), &servers).map_err(|error| {
            AcpError::Unsupported(
                error.data.and_then(|data| data.as_str().map(str::to_string)).unwrap_or_default(),
            )
        })?;
        Ok(servers)
    }

    fn additional_directories(&self) -> Result<Vec<PathBuf>> {
        let directories = &self.inner.config.additional_directories;
        if !directories.is_empty() {
            self.require(
                self.capabilities().session_capabilities.additional_directories.is_some(),
                "session additionalDirectories",
            )?;
        }
        Ok(directories.clone())
    }

    /// The configured system prompt, checked against the agent's
    /// `sessionCapabilities.systemPrompt`: clients must not send one the agent
    /// did not advertise.
    fn system_prompt(&self) -> Result<Option<SystemPrompt>> {
        let Some(prompt) = &self.inner.config.system_prompt else {
            return Ok(None);
        };
        let Some(capability) = &self.capabilities().session_capabilities.system_prompt else {
            return Err(AcpError::Unsupported(
                "the ACP agent does not accept a system prompt".into(),
            ));
        };
        let override_supported =
            capability.mode.as_ref().is_some_and(|mode| mode.override_.is_some());
        if prompt.mode == SystemPromptMode::Override && !override_supported {
            return Err(AcpError::Unsupported(
                "the ACP agent accepts system prompts in append mode only".into(),
            ));
        }
        Ok(Some(prompt.clone()))
    }
}

/// The highest protocol version this client speaks, which it requests in
/// `initialize`.
pub(crate) fn requested_protocol_version() -> ProtocolVersion {
    SUPPORTED_PROTOCOL_VERSIONS.iter().copied().max().unwrap_or(ProtocolVersion::V1)
}

/// The `initialize` request for `config`: the highest supported protocol
/// version, the client's identity, and every client capability adk-acp
/// implements.
fn initialize_request(config: &AcpAgentConfig) -> InitializeRequest {
    InitializeRequest::new(requested_protocol_version())
        .client_capabilities(client_capabilities(config))
        .client_info(config.client_info.clone())
}

/// The client capabilities adk-acp implements for `config`.
///
/// - file system and terminal, when hosts are configured;
/// - the session updates adk-acp surfaces: boolean config options, notices,
///   compactions, plan operations and subagents;
/// - elicitation, in the modes the configured handler supports.
pub(crate) fn client_capabilities(config: &AcpAgentConfig) -> ClientCapabilities {
    let mut capabilities = host_capabilities(config.filesystem.as_ref(), config.terminal.as_ref())
        .session(
            ClientSessionCapabilities::new()
                .config_options(SessionConfigOptionsCapabilities::new().boolean(
                    agent_client_protocol::v1::schema::BooleanConfigOptionCapabilities::new(),
                ))
                .notices(NoticeCapabilities::new())
                .compaction(CompactionCapabilities::new()),
        )
        .plan(PlanCapabilities::new())
        .subagents(SubagentCapabilities::new());
    if let Some(elicitation) = &config.elicitation {
        let mut modes = ElicitationCapabilities::new();
        if elicitation.supports_form() {
            modes = modes.form(ElicitationFormCapabilities::new());
        }
        if elicitation.supports_url() {
            modes = modes.url(ElicitationUrlCapabilities::new());
        }
        capabilities = capabilities.elicitation(modes);
    }
    capabilities
}

/// Run `initialize`, check the negotiated version, and authenticate with
/// `auth_method` when one is configured.
async fn initialize(
    connection: &v1::ConnectionTo<Agent>,
    request: InitializeRequest,
    auth_method: Option<&str>,
) -> std::result::Result<InitializeResponse, Error> {
    let response = connection.send_request(request).block_task().await?;
    check_protocol_version(response.protocol_version)?;
    if let Some(method) = auth_method {
        if !response.auth_methods.iter().any(|offered| *offered.id().0 == *method) {
            return Err(Error::invalid_params().data(format!(
                "the ACP agent does not offer the authentication method '{method}'"
            )));
        }
        connection.send_request(AuthenticateRequest::new(method.to_string())).block_task().await?;
    }
    Ok(response)
}

/// Fail unless the agent selected a version this client supports.
pub(crate) fn check_protocol_version(version: ProtocolVersion) -> std::result::Result<(), Error> {
    if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
        Ok(())
    } else {
        Err(Error::invalid_params().data(format!(
            "the ACP agent selected protocol version {version}; this client supports {SUPPORTED_PROTOCOL_VERSIONS:?}"
        )))
    }
}

/// Check the configured tool servers before connecting: each is valid, and
/// every MCP server name in a session setup request is unique.
fn validate_tool_servers(config: &AcpAgentConfig) -> Result<()> {
    let mut names = std::collections::HashSet::new();
    for server in &config.mcp_servers {
        let name = match server {
            McpServer::Stdio(server) => server.name.as_str(),
            McpServer::Http(server) => server.name.as_str(),
            McpServer::Sse(server) => server.name.as_str(),
            McpServer::Acp(server) => server.name.as_str(),
            _ => continue,
        };
        names.insert(name);
    }
    for server in &config.tool_servers {
        server.validate().map_err(AcpError::InvalidConfig)?;
        if !names.insert(server.name()) {
            return Err(AcpError::InvalidConfig(format!(
                "MCP server name '{}' is used twice; names must be unique within a session's MCP servers",
                server.name()
            )));
        }
    }
    Ok(())
}

/// Fail when an MCP server's transport is one the agent did not advertise.
/// ACP agents must support stdio; HTTP, SSE and ACP are opt-in.
pub(crate) fn check_mcp_servers(
    capabilities: &AgentCapabilities,
    servers: &[McpServer],
) -> std::result::Result<(), Error> {
    for server in servers {
        match server {
            McpServer::Stdio(_) => {}
            McpServer::Http(_) if capabilities.mcp_capabilities.http => {}
            McpServer::Sse(_) if capabilities.mcp_capabilities.sse => {}
            McpServer::Acp(_) if capabilities.mcp_capabilities.acp => {}
            McpServer::Http(_) => {
                return Err(Error::invalid_params()
                    .data("the ACP agent did not advertise HTTP MCP support"));
            }
            McpServer::Sse(_) => {
                return Err(
                    Error::invalid_params().data("the ACP agent did not advertise SSE MCP support")
                );
            }
            McpServer::Acp(_) => {
                return Err(Error::invalid_params()
                    .data("the ACP agent did not advertise MCP over ACP (mcpCapabilities.acp)"));
            }
            _ => {
                return Err(Error::invalid_params()
                    .data("the ACP agent did not advertise this MCP transport"));
            }
        }
    }
    Ok(())
}

/// A session update addressed to a routed session, or to a subagent session
/// announced on it.
#[derive(Debug, Clone)]
pub(crate) struct RoutedUpdate {
    /// The session the agent addressed the update to.
    pub(crate) session_id: SessionId,
    pub(crate) update: SessionUpdate,
}

/// The most notifications held for sessions that have no route yet, such as
/// the activation updates an agent sends before answering `session/new`.
const MAX_PARKED: usize = 1024;

/// Routes session updates to the [`AcpSession`] they belong to.
#[derive(Clone, Default)]
pub(crate) struct Routes(Arc<Mutex<RouteTable>>);

#[derive(Default)]
struct RouteTable {
    sessions: HashMap<SessionId, mpsc::UnboundedSender<RoutedUpdate>>,
    /// Updates for sessions without a route yet, oldest first.
    parked: VecDeque<RoutedUpdate>,
    last_auth_status: Option<AuthStatusUpdateNotification>,
}

impl Routes {
    fn table(&self) -> std::sync::MutexGuard<'_, RouteTable> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Route `session_id`'s updates to a new receiver, delivering the ones
    /// parked for it first.
    pub(crate) fn register(&self, session_id: &SessionId) -> mpsc::UnboundedReceiver<RoutedUpdate> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut table = self.table();
        let parked = std::mem::take(&mut table.parked);
        for update in parked {
            if update.session_id == *session_id {
                let _ = sender.send(update);
            } else {
                table.parked.push_back(update);
            }
        }
        table.sessions.insert(session_id.clone(), sender);
        receiver
    }

    /// Stop routing `session_id` and every subagent session announced on it.
    pub(crate) fn remove(&self, session_id: &SessionId) {
        let mut table = self.table();
        if let Some(sender) = table.sessions.remove(session_id) {
            table.sessions.retain(|_, other| !other.same_channel(&sender));
        }
    }

    fn deliver(&self, notification: SessionNotification) {
        let mut table = self.table();
        let SessionNotification { session_id, update, .. } = notification;
        let Some(sender) = table.sessions.get(&session_id).cloned() else {
            table.parked.push_back(RoutedUpdate { session_id, update });
            while table.parked.len() > MAX_PARKED {
                table.parked.pop_front();
            }
            return;
        };
        // A subagent's own updates arrive under its session id; route them to
        // the session that announced it.
        if let SessionUpdate::SubagentUpdate(subagent) = &update
            && !table.sessions.contains_key(&subagent.session_id)
        {
            let child = subagent.session_id.clone();
            table.sessions.insert(child.clone(), sender.clone());
            let parked = std::mem::take(&mut table.parked);
            for parked_update in parked {
                if parked_update.session_id == child {
                    let _ = sender.send(parked_update);
                } else {
                    table.parked.push_back(parked_update);
                }
            }
        }
        if sender.send(RoutedUpdate { session_id: session_id.clone(), update }).is_err() {
            table.sessions.remove(&session_id);
        }
    }

    fn record_auth_status(&self, notification: AuthStatusUpdateNotification) {
        self.table().last_auth_status = Some(notification);
    }

    fn last_auth_status(&self) -> Option<AuthStatusUpdateNotification> {
        self.table().last_auth_status.clone()
    }
}

/// Answers the agent's requests to the client and routes its notifications.
pub(crate) struct ClientHandler {
    policy: Arc<PermissionPolicy>,
    routes: Routes,
    elicitation: Option<Arc<dyn AcpElicitation>>,
    observer: Option<Observer>,
}

impl HandleDispatchFrom<Agent> for ClientHandler {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        connection: ConnectionTo<Agent>,
    ) -> std::result::Result<Handled<Dispatch>, Error> {
        let routes = self.routes.clone();
        let status_routes = self.routes.clone();
        let policy = self.policy.clone();
        let observer = self.observer.clone();
        let elicitation = self.elicitation.clone();
        let completion = self.elicitation.clone();
        let permission_connection = connection.clone();

        MatchDispatch::new(message)
            .if_notification(async move |notification: SessionNotification| {
                routes.deliver(notification);
                Ok(())
            })
            .await
            .if_request(async move |request: RequestPermissionRequest, responder| {
                if let Some(observer) = &observer {
                    observer.status.set(AgentStatus::WaitingPermission);
                }
                permission_connection.spawn(async move {
                    let cancellation = responder.cancellation();
                    let permission = PermissionRequest::from_acp(&request);
                    let outcome = tokio::select! {
                        decision = policy.decide(&permission) => {
                            debug!(title = %permission.title, %decision, "ACP permission policy evaluated");
                            outcome_for(&permission, &decision)
                        }
                        () = cancellation.cancelled() => RequestPermissionOutcome::Cancelled,
                    };
                    if let Some(observer) = &observer {
                        let approved = approved(&permission, &outcome);
                        let _ = observer
                            .events
                            .send(OutputChunk::PermissionRequested { title: permission.title.clone(), approved })
                            .await;
                        observer.status.set(AgentStatus::Running);
                    }
                    responder.respond(RequestPermissionResponse::new(outcome))
                })?;
                Ok(())
            })
            .await
            .if_request(async move |request: CreateElicitationRequest, responder| {
                let Some(elicitation) = elicitation else {
                    // Elicitation was not advertised, so the agent should not ask.
                    return responder.respond_with_error(Error::method_not_found());
                };
                connection.spawn(async move {
                    let cancellation = responder.cancellation();
                    let response = tokio::select! {
                        response = elicitation.elicit(request) => response,
                        () = cancellation.cancelled() => {
                            Ok(CreateElicitationResponse::new(ElicitationAction::Cancel))
                        }
                    };
                    responder.respond_with_result(response)
                })?;
                Ok(())
            })
            .await
            .if_notification(async move |notification: CompleteElicitationNotification| {
                match completion {
                    Some(elicitation) => elicitation.completed(&notification.elicitation_id).await,
                    None => warn!("elicitation/complete arrived without an elicitation handler"),
                }
                Ok(())
            })
            .await
            .if_notification(async move |notification: AuthStatusUpdateNotification| {
                status_routes.record_auth_status(notification);
                Ok(())
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ADK-Rust ACP client"
    }
}

/// Whether `outcome` selected an allow option of `request`.
fn approved(request: &PermissionRequest, outcome: &RequestPermissionOutcome) -> bool {
    match outcome {
        RequestPermissionOutcome::Selected(selected) => request
            .options
            .iter()
            .find(|option| option.id == selected.option_id.to_string())
            .is_some_and(|option| {
                matches!(
                    option.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                )
            }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{ContentBlock, ContentChunk, SubagentUpdate};

    use super::*;

    fn notification(session: &str, update: SessionUpdate) -> SessionNotification {
        SessionNotification::new(session.to_string(), update)
    }

    fn text(value: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(value)))
    }

    #[test]
    fn updates_before_registration_are_delivered_on_register() {
        let routes = Routes::default();
        routes.deliver(notification("s1", text("early")));
        routes.deliver(notification("other", text("not mine")));
        let mut receiver = routes.register(&SessionId::new("s1"));
        let first = receiver.try_recv().expect("parked update");
        assert_eq!(first.session_id, SessionId::new("s1"));
        assert!(receiver.try_recv().is_err(), "only s1's update is delivered");
        assert_eq!(routes.table().parked.len(), 1, "the other session's update stays parked");
    }

    #[test]
    fn parked_updates_are_bounded() {
        let routes = Routes::default();
        for index in 0..(MAX_PARKED + 10) {
            routes.deliver(notification(&format!("s{index}"), text("x")));
        }
        assert_eq!(routes.table().parked.len(), MAX_PARKED);
    }

    #[test]
    fn subagent_updates_route_to_the_announcing_session() {
        let routes = Routes::default();
        let mut parent = routes.register(&SessionId::new("parent"));
        routes.deliver(notification("child", text("before announce")));
        routes.deliver(notification(
            "parent",
            SessionUpdate::SubagentUpdate(SubagentUpdate::new("child")),
        ));
        routes.deliver(notification("child", text("after announce")));
        let received: Vec<String> = std::iter::from_fn(|| parent.try_recv().ok())
            .map(|update| update.session_id.to_string())
            .collect();
        assert_eq!(received, vec!["child", "parent", "child"]);

        routes.remove(&SessionId::new("parent"));
        assert!(routes.table().sessions.is_empty(), "removing the parent drops its subagents");
    }

    #[test]
    fn only_supported_protocol_versions_are_accepted() {
        assert!(check_protocol_version(ProtocolVersion::V1).is_ok());
        assert!(check_protocol_version(ProtocolVersion::V0).is_err());
        assert!(check_protocol_version(ProtocolVersion::from(2u16)).is_err());
        assert_eq!(requested_protocol_version(), ProtocolVersion::V1);
    }

    #[test]
    fn client_capabilities_advertise_the_surfaced_updates_and_elicitation_only_when_handled() {
        let config = AcpAgentConfig::new("agent");
        let capabilities = client_capabilities(&config);
        let session = capabilities.session.expect("session capabilities");
        assert!(session.notices.is_some());
        assert!(session.compaction.is_some());
        assert!(session.config_options.is_some());
        assert!(capabilities.plan.is_some());
        assert!(capabilities.subagents.is_some());
        assert!(capabilities.elicitation.is_none());
        assert!(!capabilities.terminal);

        let config = config.elicitation(Arc::new(crate::DeclineElicitation));
        let elicitation = client_capabilities(&config).elicitation.expect("elicitation");
        assert!(elicitation.form.is_some());
        assert!(elicitation.url.is_none());
    }
}
