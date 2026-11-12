//! Draft ACP protocol v2 served over the same [`AcpSessionHandler`] as v1.
//!
//! Enabled by the `unstable-protocol-v2` feature. The SDK's agent protocol router hands a
//! client that initializes with v2 to the agent built here, and every other client to the v1
//! agent, so a v1 client sees the v1 server unchanged.
//!
//! The baseline v2 session surface is served: `session/new`, `session/list`,
//! `session/resume` (replaying from the start on request), `session/close`, `session/prompt`,
//! `session/cancel` and `session/update`, plus `session/delete` and
//! `session/set_config_option`. The adapter owns what v2 changes about a turn:
//!
//! - `session/prompt` answers when the user message is inserted, with its message id, and the
//!   turn's end arrives as an `idle` state update carrying the stop reason and token usage. A
//!   failure after insertion ends the turn with the `error` stop reason instead of an error
//!   response.
//! - A prompt sent while the session is busy is held and inserted in received order, rather
//!   than rejected. `session/cancel` and `session/close` cancel held prompts with `-32800`.
//! - Every message chunk carries a message id, and tool calls are upserts by id.
//! - Closing is idempotent and waits for cancelled work to report `idle` first.
//!
//! The server owns no cancellable tool calls or terminals, so `tool_call/cancel` and
//! `terminal/cancel` are left to the SDK, which answers them with `-32602`.
//!
//! An agent configured with an [`AcpAuthenticator`](crate::server::AcpAuthenticator) is not
//! served over v2: v2 authentication (`auth/login`) is not implemented yet, and a session
//! must not be reachable without it. Such a server routes every client to v1.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::ToolConfirmationRequest;
use agent_client_protocol::v1::schema as v1;
use agent_client_protocol::v2::{self, schema};
use agent_client_protocol::{Agent, Client, Error, HandleDispatchFrom, RequestCancellation};
use tokio::sync::Notify;
use tracing::{debug, warn};

use super::{ConnectionSettings, to_protocol_error};
use crate::protocol_v2 as convert;
use crate::server::error::AcpServerError;
use crate::server::handler::{AcpSessionHandler, TurnSink};
use crate::server::permission::{PermissionAnswer, PermissionBridge, StandingOffer};
use crate::server::streamer::{ResponseStreamer, infer_tool_kind};

/// Whether a server with these settings serves protocol v2.
pub(super) fn serves_v2(settings: &ConnectionSettings) -> bool {
    settings.authenticator.is_none()
}

/// The protocol v2 agent for one connection.
pub(super) fn v2_agent(
    handler: Arc<AcpSessionHandler>,
    settings: Arc<ConnectionSettings>,
) -> v2::Builder<Agent, impl HandleDispatchFrom<Client> + 'static> {
    let lanes = Arc::new(Lanes::default());
    Agent
        .v2()
        .name(settings.name.clone())
        .on_receive_request(
            {
                let settings = settings.clone();
                move |request: schema::InitializeRequest,
                      responder: v2::Responder<schema::InitializeResponse>,
                      _connection: v2::ConnectionTo<Client>| {
                    let response = initialize_response(&settings, &request);
                    async move { responder.respond(response) }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let (handler, lanes) = (handler.clone(), lanes.clone());
                move |request: schema::NewSessionRequest,
                      responder: v2::Responder<schema::NewSessionResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let (handler, lanes) = (handler.clone(), lanes.clone());
                    async move {
                        let cancellation = responder.cancellation();
                        connection.spawn(async move {
                            let request = match new_session_to_v1(&request) {
                                Ok(request) => request,
                                Err(error) => return responder.respond_with_error(error),
                            };
                            match handler.create_session_on(request, cancellation, None).await {
                                Ok(session_id) => {
                                    lanes.open(&session_id);
                                    let (config_options, commands) =
                                        session_lists(&handler, &session_id).await;
                                    responder.respond(
                                        schema::NewSessionResponse::new(session_id.0.to_string())
                                            .config_options(config_options)
                                            .available_commands(commands),
                                    )
                                }
                                Err(error) => {
                                    responder.respond_with_error(to_protocol_error(error))
                                }
                            }
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let handler = handler.clone();
                move |request: schema::ListSessionsRequest,
                      responder: v2::Responder<schema::ListSessionsResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let handler = handler.clone();
                    async move {
                        connection.spawn(async move {
                            if let Some(cwd) = &request.cwd
                                && !cwd.0.is_absolute()
                            {
                                return responder.respond_with_error(invalid_params(
                                    "`cwd` must be an absolute path",
                                ));
                            }
                            let archived = request.archived.as_ref().map(|filter| match filter {
                                schema::SessionArchiveFilter::Include => {
                                    v1::SessionArchiveFilter::Include
                                }
                                schema::SessionArchiveFilter::Only => {
                                    v1::SessionArchiveFilter::Only
                                }
                                _ => v1::SessionArchiveFilter::Exclude,
                            });
                            let v1_request = v1::ListSessionsRequest::new()
                                .cwd(request.cwd.as_ref().map(|cwd| cwd.0.clone()))
                                .cursor(request.cursor.as_ref().map(|cursor| cursor.0.to_string()))
                                .archived(archived);
                            match handler.list_sessions(v1_request).await {
                                Ok(page) => responder.respond(
                                    schema::ListSessionsResponse::new(
                                        page.sessions
                                            .iter()
                                            .map(convert::session_info_to_v2)
                                            .collect(),
                                    )
                                    .next_cursor(
                                        page.next_cursor.map(schema::SessionListCursor::from),
                                    ),
                                ),
                                Err(error) => {
                                    responder.respond_with_error(to_protocol_error(error))
                                }
                            }
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let (handler, lanes) = (handler.clone(), lanes.clone());
                move |request: schema::ResumeSessionRequest,
                      responder: v2::Responder<schema::ResumeSessionResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let (handler, lanes) = (handler.clone(), lanes.clone());
                    async move {
                        let cancellation = responder.cancellation();
                        let spawned = connection.clone();
                        connection.spawn(async move {
                            match resume(&handler, &lanes, request, cancellation, &spawned).await {
                                Ok(response) => responder.respond(response),
                                Err(error) => responder.respond_with_error(error),
                            }
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let (handler, lanes) = (handler.clone(), lanes.clone());
                move |request: schema::CloseSessionRequest,
                      responder: v2::Responder<schema::CloseSessionResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let (handler, lanes) = (handler.clone(), lanes.clone());
                    async move {
                        connection.spawn(async move {
                            match close(&handler, &lanes, &request.session_id).await {
                                Ok(()) => responder.respond(schema::CloseSessionResponse::new()),
                                Err(error) => responder.respond_with_error(error),
                            }
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let (handler, lanes) = (handler.clone(), lanes.clone());
                move |request: schema::DeleteSessionRequest,
                      responder: v2::Responder<schema::DeleteSessionResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let (handler, lanes) = (handler.clone(), lanes.clone());
                    async move {
                        connection.spawn(async move {
                            // Deleting an active session closes it first.
                            if let Err(error) = close(&handler, &lanes, &request.session_id).await {
                                return responder.respond_with_error(error);
                            }
                            match handler.delete_session(&v1_session_id(&request.session_id)).await
                            {
                                Ok(()) => responder.respond(schema::DeleteSessionResponse::new()),
                                Err(error) => {
                                    responder.respond_with_error(to_protocol_error(error))
                                }
                            }
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let handler = handler.clone();
                move |request: schema::SetSessionConfigOptionRequest,
                      responder: v2::Responder<schema::SetSessionConfigOptionResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let handler = handler.clone();
                    async move {
                        connection.spawn(async move {
                            let Some(value) = convert::config_value_to_v1(&request.value) else {
                                return responder.respond_with_error(invalid_params(
                                    "unsupported configuration value",
                                ));
                            };
                            let session_id = v1_session_id(&request.session_id);
                            let config_id =
                                v1::SessionConfigId::new(request.config_id.0.to_string());
                            match handler.store_config_option(&session_id, &config_id, value).await
                            {
                                Ok(options) => {
                                    responder.respond(schema::SetSessionConfigOptionResponse::new(
                                        convert::config_options_to_v2(&options),
                                    ))
                                }
                                Err(error) => {
                                    responder.respond_with_error(to_protocol_error(error))
                                }
                            }
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let (handler, lanes) = (handler.clone(), lanes.clone());
                move |request: schema::PromptRequest,
                      responder: v2::Responder<schema::PromptResponse>,
                      connection: v2::ConnectionTo<Client>| {
                    let (handler, lanes) = (handler.clone(), lanes.clone());
                    async move {
                        let spawned = connection.clone();
                        connection.spawn(async move {
                            prompt(&handler, &lanes, request, responder, spawned).await
                        })
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let handler = handler.clone();
                async move |notification: schema::CancelSessionNotification,
                            _connection: v2::ConnectionTo<Client>| {
                    lanes.cancel(&notification.session_id);
                    handler.cancel_session(&v1_session_id(&notification.session_id)).await;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
}

/// The `initialize` answer for a v2 client: the server's identity and the v2 form of its
/// capabilities, limited to what this adapter serves.
fn initialize_response(
    settings: &ConnectionSettings,
    request: &schema::InitializeRequest,
) -> schema::InitializeResponse {
    let mut implementation = schema::Implementation::new(&settings.name, env!("CARGO_PKG_VERSION"));
    if !settings.title.is_empty() {
        implementation = implementation.title(settings.title.clone());
    }
    // The SDK's v2 guard has negotiated the version: a request for a newer version arrives as
    // v2, and one below v2 never reaches this handler.
    schema::InitializeResponse::new(request.protocol_version, implementation)
        .capabilities(v2_capabilities(&settings.capabilities))
}

/// The v2 capabilities of a server advertising `capabilities` over v1.
pub(super) fn v2_capabilities(capabilities: &v1::AgentCapabilities) -> schema::AgentCapabilities {
    let prompt = &capabilities.prompt_capabilities;
    let mut prompt_capabilities = schema::PromptCapabilities::new();
    if prompt.image {
        prompt_capabilities = prompt_capabilities.image(schema::PromptImageCapabilities::new());
    }
    if prompt.audio {
        prompt_capabilities = prompt_capabilities.audio(schema::PromptAudioCapabilities::new());
    }
    if prompt.embedded_context {
        prompt_capabilities =
            prompt_capabilities.embedded_context(schema::PromptEmbeddedContextCapabilities::new());
    }
    let v1_session = &capabilities.session_capabilities;
    // Session-scoped MCP servers start over stdio only, as over v1.
    let mut session = schema::SessionCapabilities::new()
        .prompt(prompt_capabilities)
        .mcp(schema::McpCapabilities::new().stdio(schema::McpStdioCapabilities::new()));
    if v1_session.delete.is_some() {
        session = session.delete(schema::SessionDeleteCapabilities::new());
    }
    if v1_session.additional_directories.is_some() {
        session =
            session.additional_directories(schema::SessionAdditionalDirectoriesCapabilities::new());
    }
    // The system prompt capability has the same shape in both versions.
    if let Some(system_prompt) = &v1_session.system_prompt
        && let Some(system_prompt) =
            convert::convert_json::<_, schema::SessionSystemPromptCapabilities>(system_prompt)
    {
        session = session.system_prompt(system_prompt);
    }
    schema::AgentCapabilities::new().session(session)
}

/// The v1 form of a v2 session id.
fn v1_session_id(session_id: &schema::SessionId) -> v1::SessionId {
    v1::SessionId::new(session_id.0.clone())
}

/// The v2 form of a v1 session id.
fn v2_session_id(session_id: &v1::SessionId) -> schema::SessionId {
    schema::SessionId::new(session_id.0.clone())
}

fn invalid_params(message: impl ToString) -> Error {
    Error::invalid_params().data(message.to_string())
}

/// The session's configuration options and commands in their v2 form, which the setup
/// responses carry.
async fn session_lists(
    handler: &AcpSessionHandler,
    session_id: &v1::SessionId,
) -> (Vec<schema::SessionConfigOption>, Vec<schema::AvailableCommand>) {
    let (_, config_options) = handler.session_controls_snapshot(session_id).await;
    let config_options = convert::config_options_to_v2(&config_options.unwrap_or_default());
    let commands = handler.declared_commands().iter().map(convert::command_to_v2).collect();
    (config_options, commands)
}

/// v2 MCP server declarations as v1, rejecting those the server cannot start.
fn mcp_servers_to_v1(servers: &[schema::McpServer]) -> Result<Vec<v1::McpServer>, Error> {
    servers
        .iter()
        .map(|server| {
            convert::mcp_server_to_v1(server)
                .ok_or_else(|| invalid_params("this agent supports the stdio MCP transport only"))
        })
        .collect()
}

/// A v2 system prompt as v1.
fn system_prompt_to_v1(
    prompt: Option<&schema::SystemPrompt>,
) -> Result<Option<v1::SystemPrompt>, Error> {
    prompt
        .map(|prompt| {
            convert::system_prompt_to_v1(prompt)
                .ok_or_else(|| invalid_params("unsupported system prompt"))
        })
        .transpose()
}

fn initial_config_to_v1(
    initial: &[schema::InitialSessionConfigOption],
) -> Result<Vec<v1::InitialSessionConfigOption>, Error> {
    convert::initial_config_to_v1(initial)
        .ok_or_else(|| invalid_params("unsupported initial configuration value"))
}

fn new_session_to_v1(request: &schema::NewSessionRequest) -> Result<v1::NewSessionRequest, Error> {
    Ok(v1::NewSessionRequest::new(request.cwd.0.clone())
        .additional_directories(convert::paths_to_v1(&request.additional_directories))
        .mcp_servers(mcp_servers_to_v1(&request.mcp_servers)?)
        .system_prompt(system_prompt_to_v1(request.system_prompt.as_ref())?)
        .config_options(initial_config_to_v1(&request.config_options)?))
}

/// `session/resume`: reactivate the session, replay its history when asked, then report the
/// session's current state before answering.
async fn resume(
    handler: &AcpSessionHandler,
    lanes: &Lanes,
    request: schema::ResumeSessionRequest,
    cancellation: RequestCancellation,
    connection: &v2::ConnectionTo<Client>,
) -> Result<schema::ResumeSessionResponse, Error> {
    let replay = match &request.replay_from {
        None => false,
        Some(schema::ReplayFrom::Start(_)) => true,
        Some(_) => return Err(invalid_params("unsupported replay cursor")),
    };
    let session_id = v1_session_id(&request.session_id);
    let v1_request = v1::ResumeSessionRequest::new(session_id.clone(), request.cwd.0.clone())
        .additional_directories(convert::paths_to_v1(&request.additional_directories))
        .mcp_servers(mcp_servers_to_v1(&request.mcp_servers)?)
        .system_prompt(system_prompt_to_v1(request.system_prompt.as_ref())?)
        .config_options(initial_config_to_v1(&request.config_options)?);
    handler.resume_session_on(v1_request, cancellation, None).await.map_err(to_protocol_error)?;
    lanes.open(&session_id);

    let send = |update: schema::SessionUpdate| {
        connection.send_notification(schema::UpdateSessionNotification::new(
            request.session_id.clone(),
            update,
        ))
    };
    if replay {
        let history = handler.history(&session_id).await.map_err(to_protocol_error)?;
        let mut mapper = UpdateMapper::new(request.session_id.0.to_string());
        for event in history {
            mapper.begin_event(&event.id, &event.invocation_id);
            for update in ResponseStreamer::map_event(&event) {
                if let Some(update) = mapper.map(update) {
                    send(update)?;
                }
            }
        }
    }
    // Every resume synchronizes the session's current state after any replay: its title,
    // then its foreground state. A resumed session has no foreground work yet.
    if let Some(title) = handler.session_title(&session_id).await {
        send(schema::SessionUpdate::SessionInfoUpdate(
            schema::SessionInfoUpdate::new().title(title),
        ))?;
    }
    send(schema::SessionUpdate::StateUpdate(schema::StateUpdate::Idle(
        schema::IdleStateUpdate::new(),
    )))?;
    let (config_options, commands) = session_lists(handler, &session_id).await;
    Ok(schema::ResumeSessionResponse::new()
        .config_options(config_options)
        .available_commands(commands))
}

/// `session/close`, which is idempotent: cancel the session's work and held prompts, release
/// it, and return once the cancelled work has reported `idle`.
async fn close(
    handler: &AcpSessionHandler,
    lanes: &Lanes,
    session_id: &schema::SessionId,
) -> Result<(), Error> {
    lanes.cancel(session_id);
    match handler.close_session(&v1_session_id(session_id)).await {
        Ok(()) | Err(AcpServerError::SessionNotFound(_)) => {}
        Err(error) => return Err(to_protocol_error(error)),
    }
    lanes.wait_until_idle(session_id).await;
    lanes.remove(session_id);
    Ok(())
}

/// `session/prompt`: validate the prompt, hold it while the session is busy, then run it as
/// one turn. The response answers at insertion; the turn ends with an `idle` update.
async fn prompt(
    handler: &AcpSessionHandler,
    lanes: &Lanes,
    request: schema::PromptRequest,
    responder: v2::Responder<schema::PromptResponse>,
    connection: v2::ConnectionTo<Client>,
) -> Result<(), Error> {
    let session_id = v1_session_id(&request.session_id);
    if !handler.is_active(&session_id).await {
        return responder.respond_with_error(
            schema::Error::session_not_found(request.session_id.clone()).into(),
        );
    }
    if request.prompt.is_empty() {
        return responder.respond_with_error(invalid_params("prompt must not be empty"));
    }
    let Some(blocks) = convert::blocks_to_v1(&request.prompt) else {
        return responder.respond_with_error(invalid_params("unsupported prompt content block"));
    };
    if let Err(error) = lanes.admit(&request.session_id, responder.cancellation()).await {
        return responder.respond_with_error(error);
    }
    // Held until the idle update is sent, so a shutdown drain waits for the turn's end.
    let _running = handler.running_prompt();
    let turn = V2Turn::new(connection, request.session_id.clone(), request.prompt, responder);
    let result = handler.run_turn(&session_id, &blocks, &turn).await;
    let finished = turn.finish(result);
    lanes.release(&request.session_id);
    finished
}

/// Per-session foreground lanes: one turn runs at a time, and prompts that arrive meanwhile
/// wait in received order.
#[derive(Default)]
struct Lanes {
    sessions: Mutex<HashMap<schema::SessionId, Lane>>,
    changed: Notify,
    next_ticket: AtomicU64,
}

#[derive(Default)]
struct Lane {
    busy: bool,
    held: VecDeque<u64>,
    /// Incremented by each `session/cancel` and `session/close`, which cancel every prompt
    /// held before them.
    epoch: u64,
}

impl Lanes {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<schema::SessionId, Lane>> {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn open(&self, session_id: &v1::SessionId) {
        self.lock().entry(v2_session_id(session_id)).or_default();
    }

    fn remove(&self, session_id: &schema::SessionId) {
        self.lock().remove(session_id);
        self.changed.notify_waiters();
    }

    fn cancel(&self, session_id: &schema::SessionId) {
        if let Some(lane) = self.lock().get_mut(session_id) {
            lane.epoch += 1;
        }
        self.changed.notify_waiters();
    }

    /// Wait for the session's foreground to be free, in received order, and claim it. A
    /// cancel, a close or the request's own cancellation withdraws a held prompt.
    async fn admit(
        &self,
        session_id: &schema::SessionId,
        withdrawn: RequestCancellation,
    ) -> Result<(), Error> {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let epoch = {
            let mut sessions = self.lock();
            let lane = sessions.entry(session_id.clone()).or_default();
            if !lane.busy && lane.held.is_empty() {
                lane.busy = true;
                return Ok(());
            }
            lane.held.push_back(ticket);
            lane.epoch
        };
        debug!(session_id = %session_id, "holding an ACP v2 prompt behind running work");
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut sessions = self.lock();
                let Some(lane) = sessions.get_mut(session_id) else {
                    return Err(Error::request_cancelled());
                };
                if lane.epoch != epoch || withdrawn.is_cancelled() {
                    lane.held.retain(|held| *held != ticket);
                    drop(sessions);
                    self.changed.notify_waiters();
                    return Err(Error::request_cancelled());
                }
                if !lane.busy && lane.held.front() == Some(&ticket) {
                    lane.held.pop_front();
                    lane.busy = true;
                    return Ok(());
                }
            }
            tokio::select! {
                () = &mut notified => {}
                () = withdrawn.cancelled() => {}
            }
        }
    }

    fn release(&self, session_id: &schema::SessionId) {
        if let Some(lane) = self.lock().get_mut(session_id) {
            lane.busy = false;
        }
        self.changed.notify_waiters();
    }

    async fn wait_until_idle(&self, session_id: &schema::SessionId) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.lock().get(session_id).is_some_and(|lane| lane.busy) {
                return;
            }
            notified.await;
        }
    }
}

/// Maps the v1 updates the session handler produces to v2 updates, assigning message ids:
/// consecutive chunks of one kind form one message, and a tool call ends the messages before
/// it. It also tracks which tool calls have not reached a final status.
struct UpdateMapper {
    prefix: String,
    counter: u64,
    agent: Option<schema::MessageId>,
    thought: Option<schema::MessageId>,
    user: Option<schema::MessageId>,
    unfinished: Vec<String>,
}

impl UpdateMapper {
    fn new(prefix: String) -> Self {
        Self { prefix, counter: 0, agent: None, thought: None, user: None, unfinished: Vec::new() }
    }

    /// Replay: the updates of one stored event form its own messages. A user message keeps
    /// the id its prompt was answered with, which is its invocation's id.
    fn begin_event(&mut self, event_id: &str, invocation_id: &str) {
        self.agent = Some(schema::MessageId::new(format!("{event_id}-agent")));
        self.thought = Some(schema::MessageId::new(format!("{event_id}-thought")));
        self.user = Some(schema::MessageId::new(invocation_id.to_string()));
    }

    fn next_id(&mut self, kind: &str) -> schema::MessageId {
        self.counter += 1;
        schema::MessageId::new(format!("{}-{kind}-{}", self.prefix, self.counter))
    }

    fn map(&mut self, update: v1::SessionUpdate) -> Option<schema::SessionUpdate> {
        Some(match update {
            v1::SessionUpdate::AgentMessageChunk(chunk) => {
                self.thought = None;
                let id = match self.agent.clone() {
                    Some(id) => id,
                    None => {
                        let id = self.next_id("agent");
                        self.agent = Some(id.clone());
                        id
                    }
                };
                schema::SessionUpdate::AgentMessageChunk(schema::ContentChunk::new(
                    convert::block_to_v2(&chunk.content)?,
                    id,
                ))
            }
            v1::SessionUpdate::AgentThoughtChunk(chunk) => {
                self.agent = None;
                let id = match self.thought.clone() {
                    Some(id) => id,
                    None => {
                        let id = self.next_id("thought");
                        self.thought = Some(id.clone());
                        id
                    }
                };
                schema::SessionUpdate::AgentThoughtChunk(schema::ContentChunk::new(
                    convert::block_to_v2(&chunk.content)?,
                    id,
                ))
            }
            v1::SessionUpdate::UserMessageChunk(chunk) => {
                let id = match self.user.clone() {
                    Some(id) => id,
                    None => {
                        let id = self.next_id("user");
                        self.user = Some(id.clone());
                        id
                    }
                };
                schema::SessionUpdate::UserMessageChunk(schema::ContentChunk::new(
                    convert::block_to_v2(&chunk.content)?,
                    id,
                ))
            }
            v1::SessionUpdate::ToolCall(call) => {
                self.end_messages();
                let id = call.tool_call_id.0.to_string();
                self.track(&id, Some(&convert::tool_status_to_v2(call.status)));
                schema::SessionUpdate::ToolCallUpdate(convert::tool_call_to_v2(&call))
            }
            v1::SessionUpdate::ToolCallUpdate(update) => {
                self.end_messages();
                let id = update.tool_call_id.0.to_string();
                let status = update.fields.status.map(convert::tool_status_to_v2);
                self.track(&id, status.as_ref());
                schema::SessionUpdate::ToolCallUpdate(convert::tool_call_update_to_v2(&update))
            }
            v1::SessionUpdate::UsageUpdate(update) => {
                schema::SessionUpdate::UsageUpdate(convert::usage_update_to_v2(&update))
            }
            v1::SessionUpdate::AvailableCommandsUpdate(update) => {
                schema::SessionUpdate::AvailableCommandsUpdate(
                    schema::AvailableCommandsUpdate::new(
                        update.available_commands.iter().map(convert::command_to_v2).collect(),
                    ),
                )
            }
            v1::SessionUpdate::ConfigOptionUpdate(update) => {
                schema::SessionUpdate::ConfigOptionUpdate(schema::ConfigOptionUpdate::new(
                    convert::config_options_to_v2(&update.config_options),
                ))
            }
            v1::SessionUpdate::SessionInfoUpdate(update) => {
                let mut info = schema::SessionInfoUpdate::new();
                if let Some(title) = update.title.value() {
                    info = info.title(title.clone());
                }
                schema::SessionUpdate::SessionInfoUpdate(info)
            }
            other => {
                debug!(update = ?other, "an ACP v1 session update has no v2 form; not sent");
                return None;
            }
        })
    }

    fn end_messages(&mut self) {
        self.agent = None;
        self.thought = None;
    }

    fn track(&mut self, tool_call_id: &str, status: Option<&schema::ToolCallStatus>) {
        let known = self.unfinished.iter().position(|id| id == tool_call_id);
        match (status.is_some_and(convert::tool_status_is_final), known) {
            (true, Some(index)) => {
                self.unfinished.remove(index);
            }
            (false, None) if status.is_some() => self.unfinished.push(tool_call_id.to_string()),
            _ => {}
        }
    }
}

/// One v2 prompt turn: answers the prompt at insertion, then streams the turn's updates and
/// permission requests on the v2 connection.
struct V2Turn {
    connection: v2::ConnectionTo<Client>,
    session_id: schema::SessionId,
    prompt: Vec<schema::ContentBlock>,
    responder: Mutex<Option<v2::Responder<schema::PromptResponse>>>,
    mapper: Mutex<UpdateMapper>,
}

impl V2Turn {
    fn new(
        connection: v2::ConnectionTo<Client>,
        session_id: schema::SessionId,
        prompt: Vec<schema::ContentBlock>,
        responder: v2::Responder<schema::PromptResponse>,
    ) -> Self {
        let mapper = UpdateMapper::new(session_id.0.to_string());
        Self {
            connection,
            session_id,
            prompt,
            responder: Mutex::new(Some(responder)),
            mapper: Mutex::new(mapper),
        }
    }

    fn send(&self, update: schema::SessionUpdate) -> Result<(), Error> {
        self.connection.send_notification(schema::UpdateSessionNotification::new(
            self.session_id.clone(),
            update,
        ))
    }

    fn mapper(&self) -> std::sync::MutexGuard<'_, UpdateMapper> {
        self.mapper.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn take_responder(&self) -> Option<v2::Responder<schema::PromptResponse>> {
        self.responder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    /// End the turn. Before insertion a failure is the prompt's error response; after it,
    /// unfinished tool calls are finalized and an `idle` update reports how the turn ended.
    fn finish(
        &self,
        result: Result<(v1::StopReason, Option<v1::Usage>), AcpServerError>,
    ) -> Result<(), Error> {
        if let Some(responder) = self.take_responder() {
            return match result {
                Err(error) => responder.respond_with_error(to_protocol_error(error)),
                // A turn always starts its runner before it ends, so this does not happen;
                // answer rather than leave the prompt pending.
                Ok(_) => responder.respond_with_error(
                    Error::internal_error().data("the turn ended before its prompt was inserted"),
                ),
            };
        }
        let (stop_reason, usage, final_status) = match result {
            Ok((stop_reason, usage)) => {
                let status = if stop_reason == v1::StopReason::Cancelled {
                    schema::ToolCallStatus::Cancelled
                } else {
                    schema::ToolCallStatus::Failed
                };
                (
                    convert::stop_reason_to_v2(stop_reason),
                    usage.as_ref().map(convert::usage_to_v2),
                    status,
                )
            }
            Err(error) => {
                warn!(%error, session_id = %self.session_id, "ACP v2 turn failed after its prompt was inserted");
                let error = schema::Error::from(to_protocol_error(error));
                (
                    schema::StopReason::Error(schema::ErrorStopReason::new().error(error)),
                    None,
                    schema::ToolCallStatus::Failed,
                )
            }
        };
        let unfinished = std::mem::take(&mut self.mapper().unfinished);
        for tool_call_id in unfinished {
            self.send(schema::SessionUpdate::ToolCallUpdate(
                schema::ToolCallUpdate::new(tool_call_id).status(final_status.clone()),
            ))?;
        }
        self.send(schema::SessionUpdate::StateUpdate(schema::StateUpdate::Idle(
            schema::IdleStateUpdate::new().stop_reason(stop_reason).usage(usage),
        )))
    }
}

#[async_trait::async_trait]
impl TurnSink for V2Turn {
    fn started(&self, invocation_id: &str) -> Result<(), AcpServerError> {
        let Some(responder) = self.take_responder() else {
            return Ok(());
        };
        let transport = |error: Error| AcpServerError::Transport(error.to_string());
        // The user message's id is its invocation's, so a replay reports the same id.
        let message_id = schema::MessageId::new(invocation_id.to_string());
        responder.respond(schema::PromptResponse::new(message_id.clone())).map_err(transport)?;
        self.send(schema::SessionUpdate::UserMessage(
            schema::UserMessage::new(message_id).content(self.prompt.clone()),
        ))
        .map_err(transport)?;
        self.send(schema::SessionUpdate::StateUpdate(schema::StateUpdate::Running(
            schema::RunningStateUpdate::new(),
        )))
        .map_err(transport)
    }

    fn update(&self, update: v1::SessionUpdate) -> Result<(), AcpServerError> {
        let Some(update) = self.mapper().map(update) else {
            return Ok(());
        };
        self.send(update).map_err(|error| AcpServerError::Transport(error.to_string()))
    }

    async fn request_permission(
        &self,
        confirmation: &ToolConfirmationRequest,
        offer: Option<&StandingOffer>,
    ) -> Result<PermissionAnswer, AcpServerError> {
        let title = format!("Run tool '{}'", confirmation.tool_name);
        let tool_call = schema::ToolCallUpdate::new(PermissionBridge::call_id(confirmation))
            .kind(convert::tool_kind_to_v2(infer_tool_kind(&confirmation.tool_name)))
            .status(schema::ToolCallStatus::Pending)
            .title(title.clone())
            .raw_input(confirmation.args.clone());
        let options = PermissionBridge::options(offer)
            .into_iter()
            .map(|option| {
                schema::PermissionOption::new(
                    option.option_id.0.to_string(),
                    option.name,
                    convert::permission_kind_to_v2(option.kind),
                )
            })
            .collect();
        let request =
            schema::RequestPermissionRequest::new(self.session_id.clone(), title, options).subject(
                schema::RequestPermissionSubject::ToolCall(Box::new(
                    schema::ToolCallPermissionSubject::new(tool_call),
                )),
            );
        let response = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(|error| AcpServerError::Transport(error.to_string()))?;
        let outcome = match &response.outcome {
            schema::RequestPermissionOutcome::Selected(selected) => {
                v1::RequestPermissionOutcome::Selected(v1::SelectedPermissionOutcome::new(
                    selected.option_id.0.to_string(),
                ))
            }
            // A cancelled or unknown outcome is not an approval.
            _ => v1::RequestPermissionOutcome::Cancelled,
        };
        Ok(PermissionBridge::answer_for(&outcome, offer.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> v1::SessionUpdate {
        v1::SessionUpdate::AgentMessageChunk(v1::ContentChunk::new(v1::ContentBlock::from(value)))
    }

    fn message_id(update: &schema::SessionUpdate) -> Option<String> {
        match update {
            schema::SessionUpdate::AgentMessageChunk(chunk)
            | schema::SessionUpdate::AgentThoughtChunk(chunk)
            | schema::SessionUpdate::UserMessageChunk(chunk) => {
                Some(chunk.message_id.0.to_string())
            }
            _ => None,
        }
    }

    #[test]
    fn chunks_form_one_message_until_a_tool_call_ends_it() {
        let mut mapper = UpdateMapper::new("s".into());
        let first = mapper.map(text("a")).expect("chunk");
        let second = mapper.map(text("b")).expect("chunk");
        assert_eq!(message_id(&first), message_id(&second));
        mapper.map(v1::SessionUpdate::ToolCall(
            v1::ToolCall::new("call-1", "search").status(v1::ToolCallStatus::InProgress),
        ));
        let third = mapper.map(text("c")).expect("chunk");
        assert_ne!(message_id(&first), message_id(&third), "a tool call starts a new message");
        assert_eq!(mapper.unfinished, vec!["call-1".to_string()]);
        mapper.map(v1::SessionUpdate::ToolCallUpdate(v1::ToolCallUpdate::new(
            "call-1",
            v1::ToolCallUpdateFields::new().status(v1::ToolCallStatus::Completed),
        )));
        assert!(mapper.unfinished.is_empty(), "a completed call is final");
    }

    #[test]
    fn replayed_user_messages_keep_their_invocation_id() {
        let mut mapper = UpdateMapper::new("s".into());
        mapper.begin_event("event-1", "inv-1");
        let user = mapper
            .map(v1::SessionUpdate::UserMessageChunk(v1::ContentChunk::new("hi".into())))
            .expect("chunk");
        assert_eq!(message_id(&user).as_deref(), Some("inv-1"));
    }

    #[test]
    fn v2_capabilities_advertise_the_served_surface_only() {
        let capabilities = v2_capabilities(
            &v1::AgentCapabilities::new()
                .prompt_capabilities(v1::PromptCapabilities::new().image(true))
                .session_capabilities(
                    v1::SessionCapabilities::new().delete(v1::SessionDeleteCapabilities::new()),
                ),
        );
        let session = capabilities.session.expect("session capabilities");
        assert!(session.prompt.as_ref().is_some_and(|prompt| prompt.image.is_some()));
        assert!(session.prompt.as_ref().is_some_and(|prompt| prompt.audio.is_none()));
        assert!(session.mcp.as_ref().is_some_and(|mcp| mcp.stdio.is_some() && mcp.http.is_none()));
        assert!(session.delete.is_some());
        assert!(session.fork.is_none() && session.archive.is_none() && session.title.is_none());
        assert!(capabilities.auth.is_none());
    }
}
