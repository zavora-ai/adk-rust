//! Official ACP v1 JSON-RPC transport over stdin/stdout.

use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, CloseSessionResponse, DeleteSessionRequest,
    DeleteSessionResponse, Implementation, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, ResumeSessionRequest, ResumeSessionResponse,
};
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Error, Responder, Stdio};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::info;

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
                    connection.spawn(async move {
                        responder.respond_with_result(
                            handler
                                .create_session(request, cancellation)
                                .await
                                .map(NewSessionResponse::new)
                                .map_err(to_protocol_error),
                        )
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
                    connection.spawn(async move {
                        responder.respond_with_result(
                            handler
                                .resume_session(
                                    &request.session_id,
                                    request.cwd,
                                    request.additional_directories,
                                    request.mcp_servers,
                                    cancellation,
                                )
                                .await
                                .map(|()| ResumeSessionResponse::new())
                                .map_err(to_protocol_error),
                        )
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
    };
    use agent_client_protocol::schema::v1::{
        CancelNotification, ContentBlock, DeleteSessionRequest, InitializeRequest,
        ListSessionsRequest, NewSessionRequest, PromptRequest, ResumeSessionRequest,
        SessionNotification, SessionUpdate, StopReason, TextContent,
    };
    use agent_client_protocol::{Channel, Client};
    use async_trait::async_trait;
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
}
