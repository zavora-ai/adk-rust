//! Official ACP v1 JSON-RPC transport over stdin/stdout.

use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, CloseSessionResponse, DeleteSessionRequest,
    DeleteSessionResponse, ForkSessionRequest, ForkSessionResponse, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, ResumeSessionRequest, ResumeSessionResponse, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
};
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Error, Responder, Stdio};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::super::capabilities::{AgentCapabilities, CapabilitiesBuilder};
use super::super::config::AcpServerConfig;
use super::super::error::AcpServerError;
use super::super::handler::AcpSessionHandler;
use super::Transport;

/// ACP's standard local-process transport. The official SDK owns JSON-RPC
/// framing, request IDs, typed message decoding, cancellation, and stdio I/O.
pub struct StdioTransport {
    capabilities: AgentCapabilities,
    agent_name: String,
    agent_title: String,
}

impl StdioTransport {
    /// Create a stdio transport whose initialization response reflects the
    /// handlers registered below.
    pub fn new(config: &AcpServerConfig) -> Self {
        Self {
            capabilities: CapabilitiesBuilder::build(config),
            agent_name: config.agent_name.clone(),
            agent_title: config.agent_description.clone(),
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn serve(
        &self,
        handler: Arc<AcpSessionHandler>,
        shutdown: CancellationToken,
    ) -> Result<(), AcpServerError> {
        info!(agent = %self.agent_name, "official ACP v1 stdio transport started");
        let connection = serve_connection(
            handler,
            self.capabilities.clone(),
            self.agent_name.clone(),
            self.agent_title.clone(),
            Stdio::new(),
        );

        tokio::select! {
            result = connection => result.map_err(|error| AcpServerError::Transport(error.to_string())),
            _ = shutdown.cancelled() => Ok(()),
        }
    }
}

pub(crate) async fn serve_connection<C>(
    handler: Arc<AcpSessionHandler>,
    initialize_capabilities: AgentCapabilities,
    initialize_name: String,
    initialize_title: String,
    component: C,
) -> Result<(), Error>
where
    C: ConnectTo<Agent> + 'static,
{
    let new_handler = handler.clone();
    let prompt_handler = handler.clone();
    let cancel_handler = handler.clone();
    let close_handler = handler.clone();
    let resume_handler = handler.clone();
    let load_handler = handler.clone();
    let fork_handler = handler.clone();
    let set_mode_handler = handler.clone();
    let set_config_handler = handler.clone();
    let list_handler = handler.clone();
    let delete_handler = handler;

    Agent
        .builder()
        .name(initialize_name.clone())
        .on_receive_request(
            move |request: InitializeRequest,
                  responder: Responder<InitializeResponse>,
                  _connection: ConnectionTo<Client>| {
                let capabilities = initialize_capabilities.clone();
                let name = initialize_name.clone();
                let title = initialize_title.clone();
                async move {
                    let version = match request.protocol_version {
                        ProtocolVersion::V1 => ProtocolVersion::V1,
                        _ => ProtocolVersion::V1,
                    };
                    let mut implementation = Implementation::new(name, env!("CARGO_PKG_VERSION"));
                    if !title.is_empty() {
                        implementation = implementation.title(title);
                    }
                    responder.respond(
                        InitializeResponse::new(version)
                            .agent_capabilities(capabilities)
                            .agent_info(implementation),
                    )
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: NewSessionRequest,
                  responder: Responder<NewSessionResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = new_handler.clone();
                async move {
                    let cancellation = responder.cancellation();
                    let spawned_connection = connection.clone();
                    connection.spawn(async move {
                        let response = match handler.create_session(request, cancellation).await {
                            Ok(session_id) => {
                                emit_activation_updates(&handler, &session_id, &spawned_connection)
                                    .await;
                                let (modes, config_options) =
                                    handler.session_controls_snapshot(&session_id).await;
                                Ok(NewSessionResponse::new(session_id)
                                    .modes(modes)
                                    .config_options(config_options))
                            }
                            Err(error) => Err(to_protocol_error(error)),
                        };
                        responder.respond_with_result(response)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: PromptRequest,
                  responder: Responder<PromptResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = prompt_handler.clone();
                async move {
                    let cancellation = responder.cancellation();
                    let spawned_connection = connection.clone();
                    connection.spawn(async move {
                        let session_id = request.session_id.clone();
                        let cancellation_handler = handler.clone();
                        let work = handler.handle_prompt(request, spawned_connection);
                        tokio::pin!(work);
                        let result = tokio::select! {
                            result = &mut work => result,
                            _ = cancellation.cancelled() => {
                                cancellation_handler.cancel_session(&session_id).await;
                                work.await
                            }
                        };
                        responder.respond_with_result(
                            result.map(PromptResponse::new).map_err(to_protocol_error),
                        )
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: CancelNotification, _connection: ConnectionTo<Client>| {
                cancel_handler.cancel_session(&notification.session_id).await;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: CloseSessionRequest,
                        responder: Responder<CloseSessionResponse>,
                        _connection: ConnectionTo<Client>| {
                match close_handler.close_session(&request.session_id).await {
                    Ok(()) => responder.respond(CloseSessionResponse::new()),
                    Err(error) => responder.respond_with_error(to_protocol_error(error)),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: ResumeSessionRequest,
                  responder: Responder<ResumeSessionResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = resume_handler.clone();
                async move {
                    let cancellation = responder.cancellation();
                    let spawned_connection = connection.clone();
                    connection.spawn(async move {
                        let session_id = request.session_id.clone();
                        let result = handler
                            .resume_session(
                                &request.session_id,
                                request.cwd,
                                request.additional_directories,
                                request.mcp_servers,
                                cancellation,
                            )
                            .await;
                        let response = match result {
                            Ok(()) => {
                                emit_activation_updates(&handler, &session_id, &spawned_connection)
                                    .await;
                                let (modes, config_options) =
                                    handler.session_controls_snapshot(&session_id).await;
                                Ok(ResumeSessionResponse::new()
                                    .modes(modes)
                                    .config_options(config_options))
                            }
                            Err(error) => Err(to_protocol_error(error)),
                        };
                        responder.respond_with_result(response)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: LoadSessionRequest,
                  responder: Responder<LoadSessionResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = load_handler.clone();
                async move {
                    let cancellation = responder.cancellation();
                    let replay_connection = connection.clone();
                    let activation_connection = connection.clone();
                    connection.spawn(async move {
                        let session_id = request.session_id.clone();
                        let result = handler
                            .load_session(
                                &request.session_id,
                                request.cwd,
                                request.additional_directories,
                                request.mcp_servers,
                                cancellation,
                                replay_connection,
                            )
                            .await;
                        let response = match result {
                            Ok(()) => {
                                emit_activation_updates(
                                    &handler,
                                    &session_id,
                                    &activation_connection,
                                )
                                .await;
                                let (modes, config_options) =
                                    handler.session_controls_snapshot(&session_id).await;
                                Ok(LoadSessionResponse::new()
                                    .modes(modes)
                                    .config_options(config_options))
                            }
                            Err(error) => Err(to_protocol_error(error)),
                        };
                        responder.respond_with_result(response)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: ForkSessionRequest,
                  responder: Responder<ForkSessionResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = fork_handler.clone();
                async move {
                    let cancellation = responder.cancellation();
                    let spawned_connection = connection.clone();
                    connection.spawn(async move {
                        let result = handler
                            .fork_session(
                                &request.session_id,
                                request.cwd,
                                request.additional_directories,
                                request.mcp_servers,
                                cancellation,
                            )
                            .await;
                        let response = match result {
                            Ok(new_session_id) => {
                                emit_activation_updates(
                                    &handler,
                                    &new_session_id,
                                    &spawned_connection,
                                )
                                .await;
                                let (modes, config_options) =
                                    handler.session_controls_snapshot(&new_session_id).await;
                                Ok(ForkSessionResponse::new(new_session_id)
                                    .modes(modes)
                                    .config_options(config_options))
                            }
                            Err(error) => Err(to_protocol_error(error)),
                        };
                        responder.respond_with_result(response)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: SetSessionModeRequest,
                  responder: Responder<SetSessionModeResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = set_mode_handler.clone();
                async move {
                    let spawned_connection = connection.clone();
                    connection.spawn(async move {
                        responder.respond_with_result(
                            handler
                                .set_mode(&request.session_id, &request.mode_id, spawned_connection)
                                .await
                                .map(|()| SetSessionModeResponse::new())
                                .map_err(to_protocol_error),
                        )
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: SetSessionConfigOptionRequest,
                  responder: Responder<SetSessionConfigOptionResponse>,
                  connection: ConnectionTo<Client>| {
                let handler = set_config_handler.clone();
                async move {
                    let spawned_connection = connection.clone();
                    connection.spawn(async move {
                        let session_id = request.session_id.clone();
                        let result = handler
                            .set_config_option(
                                &request.session_id,
                                &request.config_id,
                                request.value,
                                spawned_connection,
                            )
                            .await;
                        let response = match result {
                            Ok(()) => {
                                let (_, config_options) =
                                    handler.session_controls_snapshot(&session_id).await;
                                Ok(SetSessionConfigOptionResponse::new(
                                    config_options.unwrap_or_default(),
                                ))
                            }
                            Err(error) => Err(to_protocol_error(error)),
                        };
                        responder.respond_with_result(response)
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ListSessionsRequest,
                        responder: Responder<ListSessionsResponse>,
                        _connection: ConnectionTo<Client>| {
                match list_handler.list_sessions(request).await {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(to_protocol_error(error)),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: DeleteSessionRequest,
                        responder: Responder<DeleteSessionResponse>,
                        _connection: ConnectionTo<Client>| {
                match delete_handler.delete_session(&request.session_id).await {
                    Ok(()) => responder.respond(DeleteSessionResponse::new()),
                    Err(error) => responder.respond_with_error(to_protocol_error(error)),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(component)
        .await
}

/// Best-effort emission of the session-activation `session/update`
/// notifications (available commands and session info) for a freshly-active
/// session.
///
/// A transport hiccup while sending these notifications must not fail the
/// surrounding session request, so any error is logged and swallowed rather
/// than propagated.
async fn emit_activation_updates(
    handler: &Arc<AcpSessionHandler>,
    session_id: &agent_client_protocol::schema::v1::SessionId,
    connection: &ConnectionTo<Client>,
) {
    if let Err(error) = handler.emit_session_activation_updates(session_id, connection).await {
        warn!(%error, session_id = %session_id, "failed to emit session activation updates");
    }
}

fn to_protocol_error(error: AcpServerError) -> Error {
    match error {
        AcpServerError::MalformedMessage(message)
        | AcpServerError::SessionNotFound(message)
        | AcpServerError::UnsupportedVersion { requested: message, .. } => {
            Error::invalid_params().data(message)
        }
        AcpServerError::MaxSessionsReached(max) => {
            Error::invalid_params().data(format!("maximum active sessions reached: {max}"))
        }
        other => Error::internal_error().data(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use adk_core::{
        Agent as AdkAgent, Content, Event, EventStream, InvocationContext, Result as AdkResult,
        ToolConfirmationDecision, ToolConfirmationRequest,
    };
    use agent_client_protocol::schema::v1::{
        AudioContent, AvailableCommand, CancelNotification, ContentBlock, DeleteSessionRequest,
        EmbeddedResource as AcpEmbeddedResource, EmbeddedResourceResource, ForkSessionRequest,
        ImageContent, InitializeRequest, ListSessionsRequest, LoadSessionRequest,
        NewSessionRequest, PermissionOptionKind, PromptRequest, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
        SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
        SessionConfigOptionValue, SessionConfigSelectOption, SessionMode, SessionModeState,
        SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest,
        StopReason, TextContent, TextResourceContents as AcpTextResourceContents,
    };
    use agent_client_protocol::{Channel, Client, Responder};
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose};
    use tokio::sync::Notify;

    use super::*;
    use crate::server::config::AcpServerConfigBuilder;
    use crate::server::test_helpers::mock_agent_and_session;

    struct PendingAgent {
        started: Arc<Notify>,
    }

    struct FirstPendingThenResponds {
        started: Arc<Notify>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AdkAgent for PendingAgent {
        fn name(&self) -> &str {
            "pending-agent"
        }

        fn description(&self) -> &str {
            "Waits until the ACP client cancels the turn"
        }

        fn sub_agents(&self) -> &[Arc<dyn AdkAgent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            self.started.notify_one();
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    #[async_trait]
    impl AdkAgent for FirstPendingThenResponds {
        fn name(&self) -> &str {
            "request-cancellation-agent"
        }

        fn description(&self) -> &str {
            "Waits on its first turn and responds on its second"
        }

        fn sub_agents(&self) -> &[Arc<dyn AdkAgent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                return Ok(Box::pin(futures::stream::pending()));
            }
            let mut event = Event::new("second-turn");
            event.set_content(Content::new("model").with_text("session recovered"));
            Ok(Box::pin(futures::stream::once(async move { Ok(event) })))
        }
    }

    #[tokio::test]
    async fn official_client_completes_initialize_session_prompt_and_close() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .agent_description("Deterministic ACP test agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Deterministic ACP test agent".into(),
            server_channel,
        );
        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    updates_for_client.lock().expect("updates lock").push(notification.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, |connection: ConnectionTo<Agent>| async move {
                let initialized = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
                assert!(initialized.agent_capabilities.session_capabilities.close.is_some());
                assert!(initialized.agent_capabilities.session_capabilities.list.is_some());

                let cwd = std::env::current_dir().expect("absolute cwd");
                let session = connection
                    .send_request(NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await?;
                let prompt = connection
                    .send_request(PromptRequest::new(
                        session.session_id.clone(),
                        vec![ContentBlock::Text(TextContent::new("hello"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(prompt.stop_reason, StopReason::EndTurn);
                connection
                    .send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?;
                let listed =
                    connection.send_request(ListSessionsRequest::new()).block_task().await?;
                assert_eq!(listed.sessions.len(), 1);
                assert_eq!(listed.sessions[0].session_id, session.session_id);

                connection
                    .send_request(ResumeSessionRequest::new(session.session_id.clone(), cwd))
                    .block_task()
                    .await?;
                connection
                    .send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?;
                connection
                    .send_request(DeleteSessionRequest::new(session.session_id))
                    .block_task()
                    .await?;
                let listed =
                    connection.send_request(ListSessionsRequest::new()).block_task().await?;
                assert!(listed.sessions.is_empty());
                Ok(())
            });

        let server_task = tokio::spawn(server);
        client.await.expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
        let updates = updates.lock().expect("updates lock");
        assert!(matches!(updates.as_slice(), [SessionUpdate::AgentMessageChunk(_)]));
    }

    #[tokio::test]
    async fn official_client_cancels_a_running_prompt() {
        let started = Arc::new(Notify::new());
        let agent: Arc<dyn AdkAgent> = Arc::new(PendingAgent { started: started.clone() });
        let session_service: Arc<dyn adk_session::SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("pending-agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "pending-agent".into(),
            "Cancellation test agent".into(),
            server_channel,
        );
        let client = Client.builder().connect_with(
            client_channel,
            move |connection: ConnectionTo<Agent>| async move {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let cwd = std::env::current_dir().expect("absolute cwd");
                let session =
                    connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
                let pending_prompt = connection.send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new("wait"))],
                ));
                started.notified().await;
                connection.send_notification(CancelNotification::new(session.session_id))?;
                let response = pending_prompt.block_task().await?;
                assert_eq!(response.stop_reason, StopReason::Cancelled);
                Ok(())
            },
        );

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(2), client)
            .await
            .expect("cancellation completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn jsonrpc_request_cancellation_cleans_up_the_session() {
        let started = Arc::new(Notify::new());
        let agent: Arc<dyn AdkAgent> = Arc::new(FirstPendingThenResponds {
            started: started.clone(),
            calls: AtomicUsize::new(0),
        });
        let session_service: Arc<dyn adk_session::SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("request-cancellation-agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "request-cancellation-agent".into(),
            "JSON-RPC cancellation test agent".into(),
            server_channel,
        );
        let client = Client.builder().connect_with(
            client_channel,
            move |connection: ConnectionTo<Agent>| async move {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let cwd = std::env::current_dir().expect("absolute cwd");
                let session =
                    connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
                let first = connection.send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new("wait"))],
                ));
                started.notified().await;
                first.cancel()?;
                let _ = first.block_task().await;

                let second = connection
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![ContentBlock::Text(TextContent::new("continue"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(second.stop_reason, StopReason::EndTurn);
                Ok(())
            },
        );

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(2), client)
            .await
            .expect("request cancellation completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// Extract the text of an `AgentMessageChunk` text content block, if any.
    fn message_chunk_text(update: &SessionUpdate) -> Option<String> {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// **Feature: acp-v1-full-support, Property 7: Load replay ordering**
    /// *For any* persisted session, the `SessionUpdate`s replayed by
    /// `session/load` appear in the same chronological order as the stored
    /// events, and are delivered before the load request completes.
    /// **Validates: Requirements 5.1, 5.4**
    #[tokio::test]
    async fn session_load_replays_history_in_chronological_order() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .agent_description("Deterministic ACP test agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Deterministic ACP test agent".into(),
            server_channel,
        );
        let client =
            Client
                .builder()
                .on_receive_notification(
                    async move |notification: SessionNotification,
                                _connection: ConnectionTo<Agent>| {
                        updates_for_client.lock().expect("updates lock").push(notification.update);
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
                    let updates = updates.clone();
                    async move {
                        let initialized = connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;
                        assert!(
                            initialized.agent_capabilities.load_session,
                            "load_session capability must be advertised"
                        );

                        let cwd = std::env::current_dir().expect("absolute cwd");
                        let session = connection
                            .send_request(NewSessionRequest::new(cwd.clone()))
                            .block_task()
                            .await?;
                        let prompt = connection
                            .send_request(PromptRequest::new(
                                session.session_id.clone(),
                                vec![ContentBlock::Text(TextContent::new("hello"))],
                            ))
                            .block_task()
                            .await?;
                        assert_eq!(prompt.stop_reason, StopReason::EndTurn);

                        // Drop the connection's active session so it can be loaded
                        // fresh, then clear the updates captured during the prompt
                        // turn so only replay updates remain.
                        connection
                            .send_request(CloseSessionRequest::new(session.session_id.clone()))
                            .block_task()
                            .await?;
                        updates.lock().expect("updates lock").clear();

                        connection
                            .send_request(LoadSessionRequest::new(
                                session.session_id.clone(),
                                cwd.clone(),
                            ))
                            .block_task()
                            .await?;

                        // All replay notifications are delivered before the load
                        // response returns, so the captured order matches the
                        // stored chronological order exactly.
                        let replayed: Vec<(&str, String)> = updates
                            .lock()
                            .expect("updates lock")
                            .iter()
                            .filter_map(|update| match update {
                                SessionUpdate::UserMessageChunk(chunk) => match &chunk.content {
                                    ContentBlock::Text(text) => Some(("user", text.text.clone())),
                                    _ => None,
                                },
                                SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                                    ContentBlock::Text(text) => Some(("agent", text.text.clone())),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect();
                        assert_eq!(
                            replayed,
                            vec![
                                ("user", "hello".to_string()),
                                ("agent", "mock response".to_string()),
                            ],
                            "load must replay the stored user turn before the agent response",
                        );
                        Ok(())
                    }
                });

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("session load completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// A test agent that models ADK's pause/resume tool-confirmation flow. On a
    /// run with no decision for `delete_file`, it emits a confirmation interrupt
    /// (`event.actions.tool_confirmation`) and ends the run. On a resume run
    /// carrying a decision in `RunConfig::tool_confirmation_decisions`, it
    /// records the decision and emits either an "executed" or "skipped" message,
    /// standing in for real tool execution being gated on the decision.
    struct ConfirmingAgent {
        applied_decision: Arc<Mutex<Option<ToolConfirmationDecision>>>,
        executed: Arc<Mutex<bool>>,
    }

    #[async_trait]
    impl AdkAgent for ConfirmingAgent {
        fn name(&self) -> &str {
            "confirming-agent"
        }

        fn description(&self) -> &str {
            "Pauses for tool confirmation, then executes or skips on resume"
        }

        fn sub_agents(&self) -> &[Arc<dyn AdkAgent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            let decision = ctx.run_config().tool_confirmation_decisions.get("call-1").copied();
            let applied_decision = self.applied_decision.clone();
            let executed = self.executed.clone();
            let s = async_stream::stream! {
                match decision {
                    Some(decision) => {
                        *applied_decision.lock().expect("decision lock") = Some(decision);
                        let mut event = Event::new("confirm-resume");
                        event.author = "confirming-agent".to_string();
                        let text = match decision {
                            ToolConfirmationDecision::Approve => {
                                *executed.lock().expect("executed lock") = true;
                                "tool executed: /tmp/report.csv"
                            }
                            ToolConfirmationDecision::Deny => {
                                *executed.lock().expect("executed lock") = false;
                                "tool call skipped"
                            }
                        };
                        event.set_content(Content::new("model").with_text(text));
                        yield Ok(event);
                    }
                    None => {
                        let mut event = Event::new("confirm-pause");
                        event.author = "confirming-agent".to_string();
                        event.llm_response.interrupted = true;
                        event.llm_response.turn_complete = true;
                        event.set_content(
                            Content::new("model").with_text("Tool confirmation required"),
                        );
                        event.actions.tool_confirmation = Some(ToolConfirmationRequest {
                            tool_name: "delete_file".to_string(),
                            function_call_id: Some("call-1".to_string()),
                            args: serde_json::json!({"path": "/tmp/report.csv"}),
                        });
                        yield Ok(event);
                    }
                }
            };
            Ok(Box::pin(s))
        }
    }

    /// How the test client answers the `session/request_permission` request.
    #[derive(Clone, Copy)]
    enum Answer {
        Allow,
        Deny,
        Cancel,
    }

    /// Result of driving one full permission-gated prompt turn.
    struct PermissionFlowResult {
        stop_reason: StopReason,
        streamed_text: Vec<String>,
        applied_decision: Option<ToolConfirmationDecision>,
        executed: bool,
        permission_tool_call_id: Option<String>,
    }

    /// Drive a complete permission-gated prompt turn against the ACP server
    /// through the in-process [`Channel::duplex`] harness, answering the
    /// server's `session/request_permission` request with `answer`.
    async fn drive_permission_turn(answer: Answer) -> PermissionFlowResult {
        let applied_decision = Arc::new(Mutex::new(None));
        let executed = Arc::new(Mutex::new(false));
        let agent: Arc<dyn AdkAgent> = Arc::new(ConfirmingAgent {
            applied_decision: applied_decision.clone(),
            executed: executed.clone(),
        });
        let session_service: Arc<dyn adk_session::SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("confirming-agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));

        let streamed = Arc::new(Mutex::new(Vec::<String>::new()));
        let streamed_for_client = streamed.clone();
        let captured_call_id = Arc::new(Mutex::new(None::<String>));
        let captured_for_client = captured_call_id.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "confirming-agent".into(),
            "Permission bridge test agent".into(),
            server_channel,
        );

        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    if let Some(text) = message_chunk_text(&notification.update) {
                        streamed_for_client.lock().expect("streamed lock").push(text);
                    }
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                move |request: RequestPermissionRequest,
                      responder: Responder<RequestPermissionResponse>,
                      _connection: ConnectionTo<Agent>| {
                    let captured = captured_for_client.clone();
                    async move {
                        *captured.lock().expect("captured lock") =
                            Some(request.tool_call.tool_call_id.to_string());
                        let outcome = match answer {
                            Answer::Allow => {
                                let option = request
                                    .options
                                    .iter()
                                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                                    .expect("allow option offered");
                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                    option.option_id.clone(),
                                ))
                            }
                            Answer::Deny => {
                                let option = request
                                    .options
                                    .iter()
                                    .find(|option| option.kind == PermissionOptionKind::RejectOnce)
                                    .expect("reject option offered");
                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                    option.option_id.clone(),
                                ))
                            }
                            Answer::Cancel => RequestPermissionOutcome::Cancelled,
                        };
                        responder.respond(RequestPermissionResponse::new(outcome))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(client_channel, move |connection: ConnectionTo<Agent>| async move {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let cwd = std::env::current_dir().expect("absolute cwd");
                let session =
                    connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
                // The outer prompt response must still arrive after the nested
                // `session/request_permission` round trip — this is the guard
                // against the known SDK nested-request regression.
                let prompt = connection
                    .send_request(PromptRequest::new(
                        session.session_id.clone(),
                        vec![ContentBlock::Text(TextContent::new("delete the report"))],
                    ))
                    .block_task()
                    .await?;
                Ok(prompt.stop_reason)
            });

        let server_task = tokio::spawn(server);
        let stop_reason = tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("permission turn completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;

        let streamed_text = streamed.lock().expect("streamed lock").clone();
        let applied_decision = *applied_decision.lock().expect("decision lock");
        let executed = *executed.lock().expect("executed lock");
        let permission_tool_call_id = captured_call_id.lock().expect("captured lock").clone();
        PermissionFlowResult {
            stop_reason,
            streamed_text,
            applied_decision,
            executed,
            permission_tool_call_id,
        }
    }

    /// **Feature: acp-v1-full-support, Property 8: Permission correlation**
    /// An `allow` outcome resumes the runner with an `Approve` decision for the
    /// paused tool call, the tool executes, and the outer `PromptResponse` still
    /// completes after the nested `session/request_permission` round trip. The
    /// permission request is correlated to the tool call by its function-call id.
    /// **Validates: Requirements 7.1, 7.2, 7.5**
    #[tokio::test]
    async fn permission_allow_executes_tool_and_completes_turn() {
        let result = drive_permission_turn(Answer::Allow).await;
        assert_eq!(result.stop_reason, StopReason::EndTurn, "outer prompt response must complete");
        assert_eq!(result.applied_decision, Some(ToolConfirmationDecision::Approve));
        assert!(result.executed, "tool must execute on allow");
        assert!(
            result.streamed_text.iter().any(|text| text.contains("tool executed")),
            "executed message must be streamed, got {:?}",
            result.streamed_text
        );
        assert_eq!(
            result.permission_tool_call_id.as_deref(),
            Some("call-1"),
            "permission request must correlate to the tool call by function-call id"
        );
    }

    /// **Feature: acp-v1-full-support, Property 8: Permission correlation**
    /// A `deny` outcome resumes the runner with a `Deny` decision, the tool is
    /// skipped, and the outer `PromptResponse` still completes.
    /// **Validates: Requirements 7.1, 7.3, 7.5**
    #[tokio::test]
    async fn permission_deny_skips_tool_and_completes_turn() {
        let result = drive_permission_turn(Answer::Deny).await;
        assert_eq!(result.stop_reason, StopReason::EndTurn, "outer prompt response must complete");
        assert_eq!(result.applied_decision, Some(ToolConfirmationDecision::Deny));
        assert!(!result.executed, "tool must not execute on deny");
        assert!(
            result.streamed_text.iter().any(|text| text.contains("skipped")),
            "skipped message must be streamed, got {:?}",
            result.streamed_text
        );
        assert_eq!(result.permission_tool_call_id.as_deref(), Some("call-1"));
    }

    /// **Feature: acp-v1-full-support, Property 8: Permission correlation**
    /// A cancelled permission request maps to a `Deny` decision (Requirement
    /// 7.4): the tool is not executed and the turn continues to completion, so
    /// the outer `PromptResponse` still arrives.
    /// **Validates: Requirements 7.4, 7.5**
    #[tokio::test]
    async fn permission_cancel_denies_tool_and_completes_turn() {
        let result = drive_permission_turn(Answer::Cancel).await;
        assert_eq!(result.stop_reason, StopReason::EndTurn, "outer prompt response must complete");
        assert_eq!(
            result.applied_decision,
            Some(ToolConfirmationDecision::Deny),
            "cancellation must map to deny"
        );
        assert!(!result.executed, "tool must not execute when the permission request is cancelled");
        assert_eq!(result.permission_tool_call_id.as_deref(), Some("call-1"));
    }

    /// Session controls used by the mode/config integration tests: two modes
    /// (`ask` default, `code`) and two config options (a `model` select
    /// defaulting to `fast`, and a `verbose` boolean defaulting to `false`).
    struct TestControls;

    impl crate::server::modes::SessionControls for TestControls {
        fn modes(&self) -> Option<SessionModeState> {
            Some(SessionModeState::new(
                "ask",
                vec![SessionMode::new("ask", "Ask"), SessionMode::new("code", "Code")],
            ))
        }

        fn config_options(&self) -> Vec<SessionConfigOption> {
            vec![
                SessionConfigOption::select(
                    "model",
                    "Model",
                    "fast",
                    vec![
                        SessionConfigSelectOption::new("fast", "Fast"),
                        SessionConfigSelectOption::new("smart", "Smart"),
                    ],
                ),
                SessionConfigOption::boolean("verbose", "Verbose", false),
            ]
        }
    }

    /// Extract the `current_value` id of the `model` select option from a
    /// `ConfigOptionUpdate`, if present.
    fn model_current_value(update: &SessionUpdate) -> Option<String> {
        let SessionUpdate::ConfigOptionUpdate(config) = update else {
            return None;
        };
        let option =
            config.config_options.iter().find(|option| option.id.to_string() == "model")?;
        match &option.kind {
            SessionConfigKind::Select(select) => Some(select.current_value.to_string()),
            _ => None,
        }
    }

    /// **Feature: acp-v1-full-support, Property 9: Mode validity**
    /// `session/set_mode` with an advertised id records the new current mode and
    /// emits a `CurrentModeUpdate`; a subsequent `session/load` surfaces the
    /// persisted mode. `session/set_mode` with an unknown id returns an error
    /// and leaves the current mode unchanged.
    /// **Validates: Requirements 8.1, 8.2, 8.3, 8.4**
    #[tokio::test]
    async fn set_mode_updates_advertised_mode_and_rejects_unknown() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .session_controls(Arc::new(TestControls))
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Session mode test agent".into(),
            server_channel,
        );
        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    updates_for_client.lock().expect("updates lock").push(notification.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
                let updates = updates.clone();
                async move {
                    connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    let cwd = std::env::current_dir().expect("absolute cwd");
                    let session = connection
                        .send_request(NewSessionRequest::new(cwd.clone()))
                        .block_task()
                        .await?;
                    // The new-session response advertises the default mode.
                    let modes = session.modes.as_ref().expect("modes advertised");
                    assert_eq!(modes.current_mode_id.to_string(), "ask");

                    // Setting an advertised mode succeeds and emits a CurrentModeUpdate.
                    connection
                        .send_request(SetSessionModeRequest::new(
                            session.session_id.clone(),
                            "code",
                        ))
                        .block_task()
                        .await?;
                    let saw_code_update = updates.lock().expect("updates lock").iter().any(|u| {
                        matches!(u, SessionUpdate::CurrentModeUpdate(update)
                            if update.current_mode_id.to_string() == "code")
                    });
                    assert!(saw_code_update, "a CurrentModeUpdate(code) must be emitted");

                    // Setting an unknown mode returns an error (P9).
                    let unknown = connection
                        .send_request(SetSessionModeRequest::new(
                            session.session_id.clone(),
                            "autonomous",
                        ))
                        .block_task()
                        .await;
                    assert!(unknown.is_err(), "unknown mode must be rejected");

                    // The persisted mode survives reload and reflects only the
                    // successful change, not the rejected one.
                    connection
                        .send_request(CloseSessionRequest::new(session.session_id.clone()))
                        .block_task()
                        .await?;
                    let loaded = connection
                        .send_request(LoadSessionRequest::new(session.session_id.clone(), cwd))
                        .block_task()
                        .await?;
                    let loaded_modes = loaded.modes.as_ref().expect("modes advertised on load");
                    assert_eq!(
                        loaded_modes.current_mode_id.to_string(),
                        "code",
                        "the persisted mode must be the successfully-set mode"
                    );
                    Ok(())
                }
            });

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("set-mode flow completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// **Feature: acp-v1-full-support, Property 9 (config analog)**
    /// `session/set_config_option` with a valid value records it and emits a
    /// `ConfigOptionUpdate` reflecting the new value; a subsequent `session/load`
    /// surfaces the persisted value. An invalid value and an unknown option are
    /// both rejected and leave the option unchanged.
    /// **Validates: Requirements 9.1, 9.2, 9.3**
    #[tokio::test]
    async fn set_config_option_updates_valid_value_and_rejects_invalid() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .session_controls(Arc::new(TestControls))
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Session config test agent".into(),
            server_channel,
        );
        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    updates_for_client.lock().expect("updates lock").push(notification.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
                let updates = updates.clone();
                async move {
                    connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    let cwd = std::env::current_dir().expect("absolute cwd");
                    let session = connection
                        .send_request(NewSessionRequest::new(cwd.clone()))
                        .block_task()
                        .await?;
                    // The new-session response advertises the config options with defaults.
                    let options = session.config_options.as_ref().expect("config advertised");
                    assert!(options.iter().any(|option| option.id.to_string() == "model"));

                    // Setting a valid value succeeds and emits a ConfigOptionUpdate.
                    connection
                        .send_request(SetSessionConfigOptionRequest::new(
                            session.session_id.clone(),
                            "model",
                            SessionConfigOptionValue::value_id("smart"),
                        ))
                        .block_task()
                        .await?;
                    let saw_smart = updates
                        .lock()
                        .expect("updates lock")
                        .iter()
                        .filter_map(model_current_value)
                        .any(|value| value == "smart");
                    assert!(
                        saw_smart,
                        "a ConfigOptionUpdate reflecting model=smart must be emitted"
                    );

                    // An invalid value for a known option is rejected.
                    let invalid_value = connection
                        .send_request(SetSessionConfigOptionRequest::new(
                            session.session_id.clone(),
                            "model",
                            SessionConfigOptionValue::value_id("genius"),
                        ))
                        .block_task()
                        .await;
                    assert!(invalid_value.is_err(), "an invalid option value must be rejected");

                    // An unknown option is rejected.
                    let unknown_option = connection
                        .send_request(SetSessionConfigOptionRequest::new(
                            session.session_id.clone(),
                            "nonexistent",
                            SessionConfigOptionValue::value_id("x"),
                        ))
                        .block_task()
                        .await;
                    assert!(unknown_option.is_err(), "an unknown option must be rejected");

                    // The persisted value survives reload and reflects only the
                    // successful change.
                    connection
                        .send_request(CloseSessionRequest::new(session.session_id.clone()))
                        .block_task()
                        .await?;
                    let loaded = connection
                        .send_request(LoadSessionRequest::new(session.session_id.clone(), cwd))
                        .block_task()
                        .await?;
                    let loaded_options =
                        loaded.config_options.as_ref().expect("config advertised on load");
                    let model = loaded_options
                        .iter()
                        .find(|option| option.id.to_string() == "model")
                        .expect("model option present");
                    match &model.kind {
                        SessionConfigKind::Select(select) => {
                            assert_eq!(
                                select.current_value.to_string(),
                                "smart",
                                "the persisted config value must be the successfully-set value"
                            );
                        }
                        other => panic!("expected a select option, got {other:?}"),
                    }
                    Ok(())
                }
            });

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("set-config flow completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// **Feature: acp-v1-full-support, Property 10: Fork isolation**
    /// *For any* fork, the fork's stored history is a copy of the source's
    /// history and the source session's persisted events are byte-for-byte
    /// unchanged after the fork completes. Here a session is created, a prompt
    /// persists agent history, the session is forked, and we assert (a) the
    /// fork's persisted events equal the source's (equal serialized event list)
    /// and (b) the source's persisted events are identical before and after the
    /// fork.
    /// **Validates: Requirements 10.1, 10.3**
    #[tokio::test]
    async fn session_fork_copies_history_and_leaves_source_unchanged() {
        let (agent, session_service) = mock_agent_and_session();
        let session_service_probe = session_service.clone();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .agent_description("Deterministic ACP test agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Deterministic ACP test agent".into(),
            server_channel,
        );

        // Read a session's persisted events as a comparable JSON value.
        async fn events_json(
            service: &Arc<dyn adk_session::SessionService>,
            session_id: &str,
        ) -> serde_json::Value {
            let session = service
                .get(adk_session::GetRequest {
                    app_name: "test-agent".to_string(),
                    user_id: "acp-client".to_string(),
                    session_id: session_id.to_string(),
                    num_recent_events: None,
                    after: None,
                })
                .await
                .expect("persisted session");
            serde_json::to_value(session.events().all()).expect("serialize events")
        }

        let client = Client.builder().connect_with(
            client_channel,
            move |connection: ConnectionTo<Agent>| {
                let session_service_probe = session_service_probe.clone();
                async move {
                    let initialized = connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    assert!(
                        initialized.agent_capabilities.session_capabilities.fork.is_some(),
                        "fork capability must be advertised"
                    );

                    let cwd = std::env::current_dir().expect("absolute cwd");
                    let session = connection
                        .send_request(NewSessionRequest::new(cwd.clone()))
                        .block_task()
                        .await?;
                    let prompt = connection
                        .send_request(PromptRequest::new(
                            session.session_id.clone(),
                            vec![ContentBlock::Text(TextContent::new("hello"))],
                        ))
                        .block_task()
                        .await?;
                    assert_eq!(prompt.stop_reason, StopReason::EndTurn);

                    // Capture the source's persisted history and non-empty state
                    // immediately before the fork.
                    let source_id = session.session_id.to_string();
                    let source_events_before =
                        events_json(&session_service_probe, &source_id).await;
                    assert!(
                        !source_events_before.as_array().expect("event array").is_empty(),
                        "source must have persisted history before forking"
                    );

                    // Fork the session.
                    let forked = connection
                        .send_request(ForkSessionRequest::new(
                            session.session_id.clone(),
                            cwd.clone(),
                        ))
                        .block_task()
                        .await?;
                    assert_ne!(
                        forked.session_id, session.session_id,
                        "fork must produce a new session id"
                    );

                    // (a) The fork's history is a copy of the source's.
                    let fork_events =
                        events_json(&session_service_probe, &forked.session_id.to_string()).await;
                    assert_eq!(
                        fork_events, source_events_before,
                        "fork history must equal the source's history at fork time"
                    );

                    // (b) The source's persisted events are unchanged (P10).
                    let source_events_after = events_json(&session_service_probe, &source_id).await;
                    assert_eq!(
                        source_events_after, source_events_before,
                        "the source session's persisted events must be unchanged by the fork"
                    );

                    // The fork carries the source's stored cwd state so a later
                    // load/resume validates against the same working directory.
                    let fork_session = session_service_probe
                        .get(adk_session::GetRequest {
                            app_name: "test-agent".to_string(),
                            user_id: "acp-client".to_string(),
                            session_id: forked.session_id.to_string(),
                            num_recent_events: None,
                            after: None,
                        })
                        .await
                        .expect("forked session");
                    assert_eq!(
                        fork_session
                            .state()
                            .get("acp:cwd")
                            .and_then(|value| value.as_str().map(str::to_string)),
                        Some(cwd.display().to_string()),
                        "the fork must inherit the source's stored cwd"
                    );
                    Ok(())
                }
            },
        );

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("fork flow completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// Session controls that declare two slash commands and nothing else, used
    /// to exercise the `AvailableCommandsUpdate` activation emission.
    struct CommandControls;

    impl crate::server::modes::SessionControls for CommandControls {
        fn available_commands(&self) -> Vec<AvailableCommand> {
            vec![
                AvailableCommand::new("plan", "Draft an execution plan"),
                AvailableCommand::new("review", "Review the current changes"),
            ]
        }
    }

    /// Collect the command names carried by every `AvailableCommandsUpdate` in a
    /// slice of updates.
    fn available_command_names(updates: &[SessionUpdate]) -> Vec<String> {
        updates
            .iter()
            .filter_map(|update| match update {
                SessionUpdate::AvailableCommandsUpdate(update) => Some(&update.available_commands),
                _ => None,
            })
            .flatten()
            .map(|command| command.name.clone())
            .collect()
    }

    /// Extract the title carried by the first `SessionInfoUpdate`, if any.
    fn session_info_title(updates: &[SessionUpdate]) -> Option<String> {
        updates.iter().find_map(|update| match update {
            SessionUpdate::SessionInfoUpdate(info) => info.title.value().cloned(),
            _ => None,
        })
    }

    /// **Feature: acp-v1-full-support, Requirement 11.1**
    /// An agent whose `SessionControls` declares commands emits an
    /// `AvailableCommandsUpdate` carrying exactly those commands when a session
    /// becomes active (here, on `session/new`).
    /// **Validates: Requirements 11.1**
    #[tokio::test]
    async fn available_commands_update_emitted_on_activation_when_declared() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .session_controls(Arc::new(CommandControls))
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Available commands test agent".into(),
            server_channel,
        );
        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    updates_for_client.lock().expect("updates lock").push(notification.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
                let updates = updates.clone();
                async move {
                    connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    let cwd = std::env::current_dir().expect("absolute cwd");
                    connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
                    let names = available_command_names(&updates.lock().expect("updates lock"));
                    assert_eq!(
                        names,
                        vec!["plan".to_string(), "review".to_string()],
                        "activation must emit the declared commands in order"
                    );
                    Ok(())
                }
            });

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("available-commands flow completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// **Feature: acp-v1-full-support, Requirement 11.4**
    /// An agent that declares no commands emits no `AvailableCommandsUpdate` on
    /// activation.
    /// **Validates: Requirements 11.4**
    #[tokio::test]
    async fn no_available_commands_update_when_none_declared() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "No-commands test agent".into(),
            server_channel,
        );
        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    updates_for_client.lock().expect("updates lock").push(notification.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
                let updates = updates.clone();
                async move {
                    connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    let cwd = std::env::current_dir().expect("absolute cwd");
                    connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
                    assert!(
                        available_command_names(&updates.lock().expect("updates lock")).is_empty(),
                        "no commands declared means no AvailableCommandsUpdate"
                    );
                    Ok(())
                }
            });

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("no-commands flow completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// **Feature: acp-v1-full-support, Requirements 11.2, 11.4**
    /// A session with no recorded title emits no `SessionInfoUpdate` on
    /// activation; once a title is recorded (`acp:title`), a later activation
    /// (here, `session/load`) emits a `SessionInfoUpdate` carrying it.
    /// **Validates: Requirements 11.2, 11.4**
    #[tokio::test]
    async fn session_info_update_emitted_on_activation_only_when_title_present() {
        let (agent, session_service) = mock_agent_and_session();
        let session_service_probe = session_service.clone();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let updates = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
        let updates_for_client = updates.clone();
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Session info test agent".into(),
            server_channel,
        );
        let client = Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                    updates_for_client.lock().expect("updates lock").push(notification.update);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
                let updates = updates.clone();
                let session_service_probe = session_service_probe.clone();
                async move {
                    connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    let cwd = std::env::current_dir().expect("absolute cwd");
                    let session = connection
                        .send_request(NewSessionRequest::new(cwd.clone()))
                        .block_task()
                        .await?;
                    // No title recorded yet: activation emits no SessionInfoUpdate.
                    assert!(
                        session_info_title(&updates.lock().expect("updates lock")).is_none(),
                        "a session with no title must not emit a SessionInfoUpdate"
                    );

                    // Record a title directly in the session state, then reload
                    // the session so the activation path re-reads it.
                    let mut event = adk_core::Event::new(session.session_id.to_string());
                    event.actions.state_delta.insert(
                        "acp:title".to_string(),
                        serde_json::Value::String("My Session".to_string()),
                    );
                    session_service_probe
                        .append_event(&session.session_id.to_string(), event)
                        .await
                        .expect("record title");
                    connection
                        .send_request(CloseSessionRequest::new(session.session_id.clone()))
                        .block_task()
                        .await?;
                    updates.lock().expect("updates lock").clear();

                    connection
                        .send_request(LoadSessionRequest::new(session.session_id.clone(), cwd))
                        .block_task()
                        .await?;
                    assert_eq!(
                        session_info_title(&updates.lock().expect("updates lock")).as_deref(),
                        Some("My Session"),
                        "activation must emit a SessionInfoUpdate carrying the recorded title"
                    );
                    Ok(())
                }
            });

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("session-info flow completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }

    /// **Feature: acp-v1-full-support, Property 11: Capability accuracy (final audit)**
    /// *For any* build configuration, every advertised capability is backed by a
    /// working handler or an enabled content mapping (advertised ⇒ implemented),
    /// and nothing is advertised that the server cannot handle. This end-to-end
    /// audit drives *every* advertised capability through the in-process
    /// [`Channel::duplex`] harness and asserts each is actually handled — never
    /// answered with a method-not-found / invalid-params error (Requirement
    /// 13.3):
    ///
    /// - the `embedded_context` / `image` / `audio` prompt capabilities are
    ///   exercised by a single prompt carrying all three content types, which is
    ///   accepted and completes with [`StopReason::EndTurn`];
    /// - `load_session`, session `fork`, `resume`, `list`, `close`, and `delete`
    ///   are each invoked and succeed;
    /// - session modes / config options (advertised because a
    ///   [`crate::server::modes::SessionControls`] provider is configured) are
    ///   exercised via `session/set_mode` and `session/set_config_option`.
    ///
    /// This complements the unit-level bidirectional content-mapping audit in
    /// [`crate::server::capabilities`] and the per-content-type acceptance test
    /// in [`crate::server::handler`], closing the P11 loop across the full
    /// advertised surface.
    ///
    /// **Validates: Requirements 13.1, 13.3**
    #[tokio::test]
    async fn every_advertised_capability_is_backed_by_a_working_handler() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name("test-agent")
            .agent_description("Capability audit agent")
            .session_controls(Arc::new(TestControls))
            .build()
            .expect("valid config");
        let capabilities = CapabilitiesBuilder::build(&config);
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let (server_channel, client_channel) = Channel::duplex();

        let server = serve_connection(
            handler,
            capabilities,
            "test-agent".into(),
            "Capability audit agent".into(),
            server_channel,
        );
        let client = Client.builder().connect_with(
            client_channel,
            move |connection: ConnectionTo<Agent>| async move {
                // initialize advertises the full capability set.
                let initialized = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let caps = &initialized.agent_capabilities;
                assert!(caps.prompt_capabilities.embedded_context);
                assert!(caps.prompt_capabilities.image);
                assert!(caps.prompt_capabilities.audio);
                assert!(caps.load_session);
                assert!(caps.session_capabilities.list.is_some());
                assert!(caps.session_capabilities.delete.is_some());
                assert!(caps.session_capabilities.resume.is_some());
                assert!(caps.session_capabilities.close.is_some());
                assert!(caps.session_capabilities.fork.is_some());
                assert!(caps.session_capabilities.additional_directories.is_some());

                let cwd = std::env::current_dir().expect("absolute cwd");
                let session = connection
                    .send_request(NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await?;

                // Modes + config are advertised (a SessionControls provider is
                // configured), so their set handlers must be present.
                assert!(session.modes.is_some(), "modes advertised => set_mode handled");
                assert!(
                    session.config_options.is_some(),
                    "config options advertised => set_config_option handled"
                );

                // embedded_context / image / audio advertised => a prompt
                // carrying all three content types is accepted and completes.
                let embedded = ContentBlock::Resource(AcpEmbeddedResource::new(
                    EmbeddedResourceResource::TextResourceContents(AcpTextResourceContents::new(
                        "fn main() {}",
                        "file:///main.rs",
                    )),
                ));
                let prompt = connection
                    .send_request(PromptRequest::new(
                        session.session_id.clone(),
                        vec![
                            ContentBlock::Text(TextContent::new("audit")),
                            ContentBlock::Image(ImageContent::new(
                                general_purpose::STANDARD.encode([0x89, 0x50, 0x4E, 0x47]),
                                "image/png",
                            )),
                            ContentBlock::Audio(AudioContent::new(
                                general_purpose::STANDARD.encode([1u8, 2, 3, 4]),
                                "audio/mp3",
                            )),
                            embedded,
                        ],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(
                    prompt.stop_reason,
                    StopReason::EndTurn,
                    "a prompt carrying every advertised content type must be accepted"
                );

                // load_session advertised => session/load handled.
                connection
                    .send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?;
                connection
                    .send_request(LoadSessionRequest::new(session.session_id.clone(), cwd.clone()))
                    .block_task()
                    .await?;

                // set_mode / set_config_option handled (modes + config advertised).
                connection
                    .send_request(SetSessionModeRequest::new(session.session_id.clone(), "code"))
                    .block_task()
                    .await?;
                connection
                    .send_request(SetSessionConfigOptionRequest::new(
                        session.session_id.clone(),
                        "model",
                        SessionConfigOptionValue::value_id("smart"),
                    ))
                    .block_task()
                    .await?;

                // fork advertised => session/fork handled and yields a new id.
                let forked = connection
                    .send_request(ForkSessionRequest::new(session.session_id.clone(), cwd.clone()))
                    .block_task()
                    .await?;
                assert_ne!(
                    forked.session_id, session.session_id,
                    "fork must produce a new session id"
                );

                // resume advertised => session/resume handled (close first so the
                // session is inactive before reactivation).
                connection
                    .send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?;
                connection
                    .send_request(ResumeSessionRequest::new(session.session_id.clone(), cwd))
                    .block_task()
                    .await?;

                // list advertised => session/list handled and reflects the session.
                let listed =
                    connection.send_request(ListSessionsRequest::new()).block_task().await?;
                assert!(
                    listed.sessions.iter().any(|entry| entry.session_id == session.session_id),
                    "list must reflect the live session"
                );

                // close + delete advertised => their handlers are present.
                connection
                    .send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?;
                connection
                    .send_request(DeleteSessionRequest::new(session.session_id))
                    .block_task()
                    .await?;
                Ok(())
            },
        );

        let server_task = tokio::spawn(server);
        tokio::time::timeout(std::time::Duration::from_secs(5), client)
            .await
            .expect("capability audit completed before timeout")
            .expect("official ACP client completed");
        server_task.abort();
        let _ = server_task.await;
    }
}
