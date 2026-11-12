//! The client's draft ACP protocol v2 path (`unstable-protocol-v2`).
//!
//! [`AcpConnection::connect`] goes through the SDK's client protocol connector with a v2 and
//! a v1 implementation: it asks for v2 and falls back to v1 when the agent selects it. The
//! v1 fallback advertises capabilities the v2 request cannot carry (file system, terminal,
//! notices and the rest), so the connector opens a fresh agent connection for it: a v1-only
//! agent process is started a second time.
//!
//! Over v2 the connection keeps adk-acp's v1-typed API. The agent's v2 capabilities are
//! reported in their v1 form in [`AcpAgentInfo`], its v2 session updates are delivered to
//! sessions as their v1 equivalents, and a turn ends with the agent's `idle` state update.
//! Sessions can be created, resumed, listed, deleted, prompted, cancelled, configured and
//! closed; the other operations fail as unsupported. The v2 client answers permission
//! requests through the [`PermissionPolicy`]. It offers no file system or terminal (v2 has
//! no client-side terminals), no elicitation and no authentication: a connection configured
//! with [`AcpAgentConfig::auth_method`] speaks v1.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::v1::schema as v1;
use agent_client_protocol::v2::{self as acp_v2, InitializeOutcome, schema as v2};
use agent_client_protocol::{
    AcpAgent, Agent, Channel, Client, ConnectTo, ConnectionTo, Dispatch, DynConnectTo, Error,
    HandleDispatchFrom, Handled, JsonRpcRequest,
};
use tokio::sync::{oneshot, watch};
use tracing::{debug, warn};

use crate::client::{
    AcpAgentInfo, AcpConnection, ClientHandler, Observer, Routes, SdkConnection, StopSignal,
    approved, check_protocol_version, initialize, initialize_request, requested_protocol_version,
    validate_tool_servers,
};
use crate::connection::{AcpAgentConfig, build_agent};
use crate::error::{AcpError, Result};
use crate::host::AcpHostHandler;
use crate::mcp::{ToolServerHost, ToolServers};
use crate::permissions::{PermissionOption, PermissionPolicy, PermissionRequest, outcome_for};
use crate::protocol_v2 as convert;
use crate::session::AcpSession;
use crate::status::AgentStatus;
use crate::streaming::OutputChunk;

impl AcpConnection {
    /// Connect to an agent through a transport factory, asking for draft protocol v2 and
    /// falling back to v1 when the agent selects it.
    ///
    /// The fallback may need a fresh connection, so the transport is a factory: each call
    /// returns a new connection to the agent, such as a new in-process
    /// [`Channel`](agent_client_protocol::Channel) served by a new agent.
    /// [`connect_to`](Self::connect_to) takes a single transport and therefore speaks v1.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent selects an unsupported protocol version or
    /// authentication fails.
    pub async fn connect_with_factory<C: ConnectTo<Client>>(
        transport: impl FnMut() -> C + Send + 'static,
        config: AcpAgentConfig,
        policy: Arc<PermissionPolicy>,
    ) -> Result<Self> {
        open_negotiated(transport, config, policy, None).await
    }
}

/// Open a connection to an agent process, preferring protocol v2. The v1 fallback after a v2
/// probe starts the process again from `config`.
pub(crate) async fn open_agent(
    agent: AcpAgent,
    config: AcpAgentConfig,
    policy: Arc<PermissionPolicy>,
    observer: Option<Observer>,
) -> Result<AcpConnection> {
    let rebuild = config.clone();
    let mut first = Some(agent);
    let transport = move || match first.take() {
        Some(agent) => DynConnectTo::new(agent),
        None => match build_agent(&rebuild) {
            Ok(agent) => DynConnectTo::new(agent),
            Err(error) => {
                warn!(%error, "could not start the ACP agent again for the protocol v1 fallback");
                closed_transport()
            }
        },
    };
    open_negotiated(transport, config, policy, observer).await
}

/// A transport whose peer is gone, so a connection over it fails at once.
fn closed_transport() -> DynConnectTo<Client> {
    let (channel, _peer) = Channel::duplex();
    DynConnectTo::new(channel)
}

/// The initialized connection, in the version the agent selected.
enum Ready {
    V1(acp_v1::ConnectionTo<Agent>, v1::InitializeResponse),
    V2(acp_v2::ConnectionTo<Agent>, v2::InitializeResponse),
}

use agent_client_protocol::v1 as acp_v1;

type ReadySlot = Arc<Mutex<Option<oneshot::Sender<std::result::Result<Ready, Error>>>>>;

fn deliver_ready(slot: &ReadySlot, ready: std::result::Result<Ready, Error>) {
    let sender = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    if let Some(sender) = sender {
        let _ = sender.send(ready);
    }
}

/// Connect through the SDK's client protocol connector with a v2 and a v1 implementation,
/// each initializing from a task spawned on its connection and handing the connection out.
pub(crate) async fn open_negotiated<C: ConnectTo<Client>>(
    mut transport: impl FnMut() -> C + Send + 'static,
    config: AcpAgentConfig,
    policy: Arc<PermissionPolicy>,
    observer: Option<Observer>,
) -> Result<AcpConnection> {
    validate_tool_servers(&config)?;
    let tool_servers = ToolServers::new(&config.tool_servers);
    let routes = Routes::default();
    let (ready_tx, ready_rx) = oneshot::channel();
    let slot: ReadySlot = Arc::new(Mutex::new(Some(ready_tx)));
    let (stop_tx, stop_rx) = watch::channel(false);

    let v1_client = {
        let (routes, slot, config, policy, observer, tool_servers) = (
            routes.clone(),
            slot.clone(),
            config.clone(),
            policy.clone(),
            observer.clone(),
            tool_servers.clone(),
        );
        move || {
            let handler = ClientHandler::new(
                policy.clone(),
                routes.clone(),
                config.elicitation.clone(),
                observer.clone(),
            );
            let host = AcpHostHandler::new(config.filesystem.clone(), config.terminal.clone());
            let request = initialize_request(&config);
            let auth_method = config.auth_method.clone();
            let slot = slot.clone();
            let tool_host = ToolServerHost::new(tool_servers.clone());
            Client
                .v1()
                .name("adk-acp")
                .with_handler(handler)
                .with_handler(host)
                .with_handler(tool_host)
                .with_spawned(move |connection: acp_v1::ConnectionTo<Agent>| async move {
                    let initialized =
                        initialize(&connection, request, auth_method.as_deref()).await;
                    deliver_ready(
                        &slot,
                        initialized.map(|response| Ready::V1(connection, response)),
                    );
                    Ok(())
                })
        }
    };
    let v2_client = {
        let (routes, slot, config, policy, observer) =
            (routes.clone(), slot.clone(), config.clone(), policy.clone(), observer.clone());
        move || {
            let handler = V2ClientHandler {
                policy: policy.clone(),
                routes: routes.clone(),
                observer: observer.clone(),
                projection: Arc::default(),
            };
            let request = v2::InitializeRequest::new(
                requested_protocol_version(),
                v2::Implementation::new(
                    config.client_info.name.clone(),
                    config.client_info.version.clone(),
                )
                .title(config.client_info.title.clone()),
            );
            let slot = slot.clone();
            Client.v2().name("adk-acp").with_handler(handler).with_spawned(
                move |connection: acp_v2::ConnectionTo<Agent>| async move {
                    let outcome = connection.send_initialize(request).block_task().await;
                    let ready = match outcome {
                        Ok(InitializeOutcome::Initialized(response)) => {
                            check_protocol_version(response.protocol_version)
                                .map(|()| Ready::V2(connection, response))
                        }
                        // The connector hands a v1 selection to the v1 implementation.
                        Ok(_) => Err(Error::invalid_params()
                            .data("the ACP agent selected a protocol version below v2")),
                        Err(error) => Err(error),
                    };
                    deliver_ready(&slot, ready);
                    Ok(())
                },
            )
        }
    };

    let mut connector = Client.protocol_connector().with_v1(v1_client);
    // v2 authentication (`auth/login`) is not implemented, so a connection that must
    // authenticate speaks v1. MCP over ACP is served on v1 only, so a connection with tool
    // servers speaks v1 too.
    if config.auth_method.is_none() && config.tool_servers.is_empty() {
        connector = connector.with_v2(v2_client);
    }
    let agent =
        move || StoppableAgent { inner: DynConnectTo::new(transport()), stop: stop_rx.clone() };
    let task = tokio::spawn(async move { connector.connect_to(agent).await });
    let stop: StopSignal = Box::new(move || {
        let _ = stop_tx.send(true);
    });

    let ready = match ready_rx.await {
        Ok(Ok(ready)) => ready,
        Ok(Err(error)) => {
            stop();
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
    Ok(match ready {
        Ready::V1(connection, response) => AcpConnection::from_parts(
            SdkConnection::V1(connection),
            routes,
            AcpAgentInfo::from_response(response),
            config,
            tool_servers,
            stop,
            task,
        ),
        Ready::V2(connection, response) => {
            debug!(
                agent = %response.info.name,
                "ACP agent initialized with draft protocol v2"
            );
            let info = agent_info(&response);
            let mut connection = AcpConnection::from_parts(
                SdkConnection::V2(connection),
                routes,
                info,
                config,
                tool_servers,
                stop,
                task,
            );
            if let Some(inner) = Arc::get_mut(&mut connection.inner) {
                inner.v2_capabilities = Some(response.capabilities);
            }
            connection
        }
    })
}

/// What a v2 agent reported in `initialize`, in adk-acp's v1 form.
fn agent_info(response: &v2::InitializeResponse) -> AcpAgentInfo {
    AcpAgentInfo {
        protocol_version: response.protocol_version,
        agent: Some(
            v1::Implementation::new(response.info.name.clone(), response.info.version.clone())
                .title(response.info.title.clone()),
        ),
        capabilities: capabilities_to_v1(&response.capabilities),
        // v2 authentication is not implemented, so no method is offered.
        auth_methods: Vec::new(),
    }
}

/// A v2 agent's capabilities in their v1 form, so the operations adk-acp checks before
/// sending find them. `session/list`, `resume` and `close` are part of v2's baseline session
/// surface; v2 has no `session/load`.
pub(crate) fn capabilities_to_v1(capabilities: &v2::AgentCapabilities) -> v1::AgentCapabilities {
    let Some(session) = &capabilities.session else {
        return v1::AgentCapabilities::new();
    };
    let prompt = session.prompt.as_ref();
    let prompt_capabilities = v1::PromptCapabilities::new()
        .image(prompt.is_some_and(|prompt| prompt.image.is_some()))
        .audio(prompt.is_some_and(|prompt| prompt.audio.is_some()))
        .embedded_context(prompt.is_some_and(|prompt| prompt.embedded_context.is_some()));
    let mut session_capabilities = v1::SessionCapabilities::new()
        .list(v1::SessionListCapabilities::new())
        .resume(v1::SessionResumeCapabilities::new())
        .close(v1::SessionCloseCapabilities::new());
    if session.delete.is_some() {
        session_capabilities = session_capabilities.delete(v1::SessionDeleteCapabilities::new());
    }
    if session.additional_directories.is_some() {
        session_capabilities = session_capabilities
            .additional_directories(v1::SessionAdditionalDirectoriesCapabilities::new());
    }
    if let Some(system_prompt) = &session.system_prompt
        && let Some(system_prompt) =
            convert::convert_json::<_, v1::SessionSystemPromptCapabilities>(system_prompt)
    {
        session_capabilities = session_capabilities.system_prompt(system_prompt);
    }
    if session.fork.is_some() {
        session_capabilities = session_capabilities.fork(v1::SessionForkCapabilities::new());
    }
    if session.archive.is_some() {
        session_capabilities = session_capabilities.archive(v1::SessionArchiveCapabilities::new());
    }
    if session.status.is_some() {
        session_capabilities = session_capabilities.status(v1::SessionStatusCapabilities::new());
    }
    if session.options.is_some() {
        session_capabilities = session_capabilities.options(v1::SessionOptionsCapabilities::new());
    }
    if session.title.is_some() {
        session_capabilities = session_capabilities.title(v1::SessionTitleCapabilities::new());
    }
    let http = session.mcp.as_ref().is_some_and(|mcp| mcp.http.is_some());
    v1::AgentCapabilities::new()
        .prompt_capabilities(prompt_capabilities)
        .mcp_capabilities(v1::McpCapabilities::new().http(http))
        .session_capabilities(session_capabilities)
}

/// The agent transport behind the connector, through a relay that closes the agent's input
/// when the connection is closed, so the agent exits as it does over v1.
struct StoppableAgent {
    inner: DynConnectTo<Client>,
    stop: watch::Receiver<bool>,
}

impl ConnectTo<Client> for StoppableAgent {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> std::result::Result<(), Error> {
        let Self { inner, mut stop } = self;
        crate::relay::relay_until(
            client.into_channel_and_future(),
            inner.into_channel_and_future(),
            async move {
                let _ = stop.wait_for(|stopped| *stopped).await;
            },
        )
        .await
    }
}

/// The ids a connection has seen, for mapping v2 upserts and snapshots to v1 updates.
#[derive(Default)]
struct Projection {
    tool_calls: HashSet<String>,
    messages: HashSet<String>,
}

/// Answers a v2 agent's permission requests and routes its session updates.
struct V2ClientHandler {
    policy: Arc<PermissionPolicy>,
    routes: Routes,
    observer: Option<Observer>,
    projection: Arc<Mutex<Projection>>,
}

impl HandleDispatchFrom<Agent> for V2ClientHandler {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        connection: ConnectionTo<Agent>,
    ) -> std::result::Result<Handled<Dispatch>, Error> {
        let routes = self.routes.clone();
        let projection = self.projection.clone();
        let update_observer = self.observer.clone();
        let policy = self.policy.clone();
        let observer = self.observer.clone();

        MatchDispatch::new(message)
            .if_notification(async move |notification: v2::UpdateSessionNotification| {
                if let (Some(observer), v2::SessionUpdate::StateUpdate(state)) =
                    (&update_observer, &notification.update)
                {
                    observer.status.set(match state {
                        v2::StateUpdate::Idle(_) => AgentStatus::Idle,
                        _ => AgentStatus::Running,
                    });
                }
                deliver(&routes, &projection, notification);
                Ok(())
            })
            .await
            .if_request(async move |request: v2::RequestPermissionRequest, responder| {
                if let Some(observer) = &observer {
                    observer.status.set(AgentStatus::WaitingPermission);
                }
                connection.spawn(async move {
                    let cancellation = responder.cancellation();
                    let permission = permission_request(&request);
                    let outcome = tokio::select! {
                        decision = policy.decide(&permission) => {
                            debug!(title = %permission.title, %decision, "ACP permission policy evaluated");
                            outcome_for(&permission, &decision)
                        }
                        () = cancellation.cancelled() => v1::RequestPermissionOutcome::Cancelled,
                    };
                    if let Some(observer) = &observer {
                        let approved = approved(&permission, &outcome);
                        let _ = observer
                            .events
                            .send(OutputChunk::PermissionRequested {
                                title: permission.title.clone(),
                                approved,
                            })
                            .await;
                        observer.status.set(AgentStatus::Running);
                    }
                    responder.respond(v2::RequestPermissionResponse::new(outcome_to_v2(&outcome)))
                })?;
                Ok(())
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ADK-Rust ACP protocol v2 client"
    }
}

/// A v2 permission request as the policy sees it.
fn permission_request(request: &v2::RequestPermissionRequest) -> PermissionRequest {
    let tool_call = match &request.subject {
        Some(v2::RequestPermissionSubject::ToolCall(subject)) => Some(&subject.tool_call),
        _ => None,
    };
    PermissionRequest {
        session_id: request.session_id.0.to_string(),
        tool_call_id: tool_call.map(|call| call.tool_call_id.0.to_string()).unwrap_or_default(),
        title: request.title.clone(),
        kind: tool_call
            .and_then(|call| call.kind.value())
            .map(convert::tool_kind_to_v1)
            .unwrap_or_default(),
        raw_input: tool_call.and_then(|call| call.raw_input.value().cloned()),
        // An option kind v1 does not know is not offered to the policy.
        options: request
            .options
            .iter()
            .filter_map(|option| {
                convert::permission_kind_to_v1(&option.kind).map(|kind| PermissionOption {
                    id: option.option_id.0.to_string(),
                    name: option.name.clone(),
                    kind,
                })
            })
            .collect(),
    }
}

fn outcome_to_v2(outcome: &v1::RequestPermissionOutcome) -> v2::RequestPermissionOutcome {
    match outcome {
        v1::RequestPermissionOutcome::Selected(selected) => v2::RequestPermissionOutcome::Selected(
            v2::SelectedPermissionOutcome::new(selected.option_id.0.to_string()),
        ),
        _ => v2::RequestPermissionOutcome::Cancelled(v2::CancelledPermissionOutcome::new()),
    }
}

/// Route a v2 session update to its session as v1 updates. Foreground-state updates go to
/// the session's state route, which ends its turns.
fn deliver(
    routes: &Routes,
    projection: &Mutex<Projection>,
    notification: v2::UpdateSessionNotification,
) {
    let session_id = v1::SessionId::new(notification.session_id.0.clone());
    let mut projection = projection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let updates = match notification.update {
        v2::SessionUpdate::StateUpdate(state) => {
            routes.deliver_state(&session_id, state);
            return;
        }
        v2::SessionUpdate::ToolCallUpdate(upsert) => {
            let first_seen = projection.tool_calls.insert(upsert.tool_call_id.0.to_string());
            vec![convert::tool_call_upsert_to_v1(&upsert, first_seen)]
        }
        v2::SessionUpdate::AgentMessageChunk(chunk) => {
            projection.messages.insert(chunk.message_id.0.to_string());
            chunk_to_v1(&chunk.content, v1::SessionUpdate::AgentMessageChunk)
        }
        v2::SessionUpdate::AgentThoughtChunk(chunk) => {
            projection.messages.insert(chunk.message_id.0.to_string());
            chunk_to_v1(&chunk.content, v1::SessionUpdate::AgentThoughtChunk)
        }
        v2::SessionUpdate::UserMessageChunk(chunk) => {
            projection.messages.insert(chunk.message_id.0.to_string());
            chunk_to_v1(&chunk.content, v1::SessionUpdate::UserMessageChunk)
        }
        // A message snapshot repeats what its chunks carried; it is surfaced only for a
        // message no chunk announced.
        v2::SessionUpdate::UserMessage(message) => snapshot_to_v1(
            &mut projection,
            &message.message_id,
            message.content.value(),
            v1::SessionUpdate::UserMessageChunk,
        ),
        v2::SessionUpdate::AgentMessage(message) => snapshot_to_v1(
            &mut projection,
            &message.message_id,
            message.content.value(),
            v1::SessionUpdate::AgentMessageChunk,
        ),
        v2::SessionUpdate::AgentThought(message) => snapshot_to_v1(
            &mut projection,
            &message.message_id,
            message.content.value(),
            v1::SessionUpdate::AgentThoughtChunk,
        ),
        v2::SessionUpdate::UsageUpdate(update) => {
            vec![v1::SessionUpdate::UsageUpdate(convert::usage_update_to_v1(&update))]
        }
        v2::SessionUpdate::AvailableCommandsUpdate(update) => {
            vec![v1::SessionUpdate::AvailableCommandsUpdate(v1::AvailableCommandsUpdate::new(
                update.available_commands.iter().map(convert::command_to_v1).collect(),
            ))]
        }
        v2::SessionUpdate::ConfigOptionUpdate(update) => {
            vec![v1::SessionUpdate::ConfigOptionUpdate(v1::ConfigOptionUpdate::new(
                convert::config_options_to_v1(&update.config_options),
            ))]
        }
        // Notices, compactions, plans, session info and subagent updates share their wire
        // shape across the versions.
        other => match convert::convert_json::<_, v1::SessionUpdate>(&other) {
            Some(update) => vec![update],
            None => {
                debug!(update = ?other, "an ACP v2 session update has no v1 form; not surfaced");
                Vec::new()
            }
        },
    };
    drop(projection);
    for update in updates {
        routes.deliver(v1::SessionNotification::new(session_id.clone(), update));
    }
}

fn chunk_to_v1(
    content: &v2::ContentBlock,
    kind: fn(v1::ContentChunk) -> v1::SessionUpdate,
) -> Vec<v1::SessionUpdate> {
    convert::block_to_v1(content)
        .map(|block| kind(v1::ContentChunk::new(block)))
        .into_iter()
        .collect()
}

fn snapshot_to_v1(
    projection: &mut Projection,
    message_id: &v2::MessageId,
    content: Option<&Vec<v2::ContentBlock>>,
    kind: fn(v1::ContentChunk) -> v1::SessionUpdate,
) -> Vec<v1::SessionUpdate> {
    if !projection.messages.insert(message_id.0.to_string()) {
        return Vec::new();
    }
    content
        .into_iter()
        .flatten()
        .filter_map(convert::block_to_v1)
        .map(|block| kind(v1::ContentChunk::new(block)))
        .collect()
}

/// Send a v2 request and wait for its response.
async fn send<Req: JsonRpcRequest>(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
    request: Req,
) -> Result<Req::Response> {
    sdk.send_request(request).block_task().await.map_err(|error| connection.error(error))
}

/// The configured MCP servers in their v2 form, checked against the agent's v2 capabilities:
/// over v2 even stdio is an advertised transport.
fn mcp_servers(connection: &AcpConnection) -> Result<Vec<v2::McpServer>> {
    let mcp = connection
        .inner
        .v2_capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.session.as_ref())
        .and_then(|session| session.mcp.as_ref());
    connection
        .config()
        .mcp_servers
        .iter()
        .map(|server| {
            let advertised = match server {
                v1::McpServer::Stdio(_) => mcp.is_some_and(|mcp| mcp.stdio.is_some()),
                v1::McpServer::Http(_) => mcp.is_some_and(|mcp| mcp.http.is_some()),
                _ => false,
            };
            match convert::mcp_server_to_v2(server) {
                Some(server) if advertised => Ok(server),
                _ => Err(AcpError::Unsupported(
                    "the ACP agent did not advertise this MCP transport over protocol v2".into(),
                )),
            }
        })
        .collect()
}

fn system_prompt(connection: &AcpConnection) -> Result<Option<v2::SystemPrompt>> {
    connection
        .system_prompt()?
        .map(|prompt| {
            convert::system_prompt_to_v2(&prompt).ok_or_else(|| {
                AcpError::Unsupported("the system prompt has no ACP protocol v2 form".into())
            })
        })
        .transpose()
}

fn initial_config(connection: &AcpConnection) -> Result<Vec<v2::InitialSessionConfigOption>> {
    let initial = &connection.config().initial_config_options;
    if initial.is_empty() {
        return Ok(Vec::new());
    }
    connection.require(
        connection.capabilities().session_capabilities.options.is_some(),
        "session/options",
    )?;
    convert::initial_config_to_v2(initial).ok_or_else(|| {
        AcpError::Unsupported("an initial configuration value has no ACP protocol v2 form".into())
    })
}

/// `session/new` over v2.
pub(crate) async fn new_session(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
) -> Result<AcpSession> {
    let config = connection.config();
    let request = v2::NewSessionRequest::new(config.working_dir.clone())
        .additional_directories(convert::paths_to_v2(&connection.additional_directories()?))
        .mcp_servers(mcp_servers(connection)?)
        .system_prompt(system_prompt(connection)?)
        .config_options(initial_config(connection)?);
    let response = send(connection, sdk, request).await?;
    let session_id = v1::SessionId::new(response.session_id.0.clone());
    let routes = &connection.inner.routes;
    let states = routes.register_states(&session_id);
    let events = routes.register(&session_id);
    Ok(session(connection, session_id, events, &response.config_options).with_states(states))
}

/// `session/resume` over v2, without replaying history.
pub(crate) async fn resume_session(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
    session_id: v1::SessionId,
) -> Result<AcpSession> {
    let config = connection.config();
    let request =
        v2::ResumeSessionRequest::new(session_id.0.to_string(), config.working_dir.clone())
            .additional_directories(convert::paths_to_v2(&connection.additional_directories()?))
            .mcp_servers(mcp_servers(connection)?)
            .system_prompt(system_prompt(connection)?)
            .config_options(initial_config(connection)?);
    // Routed before the request: the agent synchronizes the session's state before answering.
    let routes = &connection.inner.routes;
    let states = routes.register_states(&session_id);
    let events = routes.register(&session_id);
    let response =
        send(connection, sdk, request).await.inspect_err(|_| routes.remove(&session_id))?;
    Ok(session(connection, session_id, events, &response.config_options).with_states(states))
}

fn session(
    connection: &AcpConnection,
    session_id: v1::SessionId,
    events: tokio::sync::mpsc::UnboundedReceiver<crate::client::RoutedUpdate>,
    config_options: &[v2::SessionConfigOption],
) -> AcpSession {
    let config_options =
        (!config_options.is_empty()).then(|| convert::config_options_to_v1(config_options));
    // v2 has no session modes.
    AcpSession::from_parts(connection.clone(), session_id, events, None, config_options)
}

/// One page of `session/list` over v2.
pub(crate) async fn list_sessions_page(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
    request: v1::ListSessionsRequest,
) -> Result<v1::ListSessionsResponse> {
    let archived = match &request.archived {
        None | Some(v1::SessionArchiveFilter::Exclude) => None,
        Some(filter) => {
            connection.require(
                connection.capabilities().session_capabilities.archive.is_some(),
                "session/archive",
            )?;
            Some(match filter {
                v1::SessionArchiveFilter::Only => v2::SessionArchiveFilter::Only,
                _ => v2::SessionArchiveFilter::Include,
            })
        }
    };
    let response = send(
        connection,
        sdk,
        v2::ListSessionsRequest::new()
            .cwd(request.cwd.clone().map(v2::AbsolutePath::new))
            .cursor(request.cursor.clone().map(v2::SessionListCursor::from))
            .archived(archived),
    )
    .await?;
    Ok(v1::ListSessionsResponse::new(
        response.sessions.iter().map(convert::session_info_to_v1).collect(),
    )
    .next_cursor(response.next_cursor.map(|cursor| cursor.0.to_string())))
}

/// `session/delete` over v2.
pub(crate) async fn delete_session(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
    session_id: v1::SessionId,
) -> Result<()> {
    send(connection, sdk, v2::DeleteSessionRequest::new(session_id.0.to_string())).await.map(drop)
}

/// `session/close` over v2, which every v2 agent answers and which is idempotent.
pub(crate) async fn close_session(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
    session_id: &v1::SessionId,
) -> Result<()> {
    send(connection, sdk, v2::CloseSessionRequest::new(session_id.0.to_string())).await.map(drop)
}

/// `session/set_config_option` over v2, returning the complete option set.
pub(crate) async fn set_config_option(
    connection: &AcpConnection,
    sdk: &acp_v2::ConnectionTo<Agent>,
    session_id: &v1::SessionId,
    config_id: v1::SessionConfigId,
    value: v1::SessionConfigOptionValue,
) -> Result<Vec<v1::SessionConfigOption>> {
    let Some(value) = convert::config_value_to_v2(&value) else {
        return Err(AcpError::Unsupported(
            "the configuration value has no ACP protocol v2 form".into(),
        ));
    };
    let response = send(
        connection,
        sdk,
        v2::SetSessionConfigOptionRequest::new(
            session_id.0.to_string(),
            config_id.0.to_string(),
            value,
        ),
    )
    .await?;
    Ok(convert::config_options_to_v1(&response.config_options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_capabilities_map_to_their_v1_checks() {
        let capabilities = capabilities_to_v1(
            &v2::AgentCapabilities::new().session(
                v2::SessionCapabilities::new()
                    .prompt(v2::PromptCapabilities::new().image(v2::PromptImageCapabilities::new()))
                    .delete(v2::SessionDeleteCapabilities::new()),
            ),
        );
        let session = &capabilities.session_capabilities;
        assert!(session.list.is_some() && session.resume.is_some() && session.close.is_some());
        assert!(session.delete.is_some());
        assert!(session.fork.is_none());
        assert!(!capabilities.load_session, "v2 has no session/load");
        assert!(capabilities.prompt_capabilities.image);
        assert!(!capabilities.prompt_capabilities.audio);
    }

    #[test]
    fn a_permission_request_reaches_the_policy_with_its_tool_call() {
        let request = v2::RequestPermissionRequest::new(
            "s1",
            "Run tool 'search'",
            vec![
                v2::PermissionOption::new("allow", "Allow", v2::PermissionOptionKind::AllowOnce),
                v2::PermissionOption::new(
                    "later",
                    "Later",
                    v2::PermissionOptionKind::Other("defer".into()),
                ),
            ],
        )
        .subject(v2::RequestPermissionSubject::ToolCall(Box::new(
            v2::ToolCallPermissionSubject::new(
                v2::ToolCallUpdate::new("call-1")
                    .kind(v2::ToolKind::Search)
                    .raw_input(serde_json::json!({ "q": "acp" })),
            ),
        )));
        let permission = permission_request(&request);
        assert_eq!(permission.tool_call_id, "call-1");
        assert_eq!(permission.kind, v1::ToolKind::Search);
        assert_eq!(permission.options.len(), 1, "an unknown option kind is not offered");
        assert!(matches!(
            outcome_to_v2(&v1::RequestPermissionOutcome::Cancelled),
            v2::RequestPermissionOutcome::Cancelled(_)
        ));
    }

    #[test]
    fn snapshots_repeat_nothing_their_chunks_carried() {
        let mut projection = Projection::default();
        let id = v2::MessageId::new("m1");
        projection.messages.insert("m1".into());
        let content = vec![v2::ContentBlock::from("hello")];
        assert!(
            snapshot_to_v1(
                &mut projection,
                &id,
                Some(&content),
                v1::SessionUpdate::AgentMessageChunk
            )
            .is_empty()
        );
        let fresh = v2::MessageId::new("m2");
        assert_eq!(
            snapshot_to_v1(
                &mut projection,
                &fresh,
                Some(&content),
                v1::SessionUpdate::UserMessageChunk
            )
            .len(),
            1
        );
    }
}
