//! End-to-end tests of draft ACP protocol v2 (`unstable-protocol-v2`): the adk-acp server
//! serving v1 and v2 clients through the SDK's agent protocol router, the adk-acp client
//! negotiating v2 with it, and the client falling back to v1 with an agent that only speaks
//! v1.
#![cfg(all(feature = "server", feature = "unstable-protocol-v2"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_acp::agent_client_protocol::schema::ProtocolVersion;
use adk_acp::agent_client_protocol::v1::schema as v1;
use adk_acp::agent_client_protocol::v2::{self, InitializeOutcome, schema as v2s};
use adk_acp::agent_client_protocol::{Agent, Channel, Client, ErrorCode};
use adk_acp::server::{AcpServer, AcpServerConfig, AcpServerConfigBuilder, SessionControls};
use adk_acp::{AcpAgentConfig, AcpConnection, AcpToolServer, OutputChunk, PermissionPolicy};
use adk_core::{
    Agent as AdkAgent, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, UsageMetadata,
};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;

const LIMIT: Duration = Duration::from_secs(20);

/// Answers every request with fixed text and usage.
struct FixedLlm;

#[async_trait]
impl Llm for FixedLlm {
    fn name(&self) -> &str {
        "fixed"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let mut response = LlmResponse::new(Content::new("model").with_text("model answer"));
        response.turn_complete = true;
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 30,
            candidates_token_count: 12,
            total_token_count: 42,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
    }
}

/// One boolean option.
struct Controls;

impl SessionControls for Controls {
    fn config_options(&self) -> Vec<v1::SessionConfigOption> {
        vec![v1::SessionConfigOption::boolean("verbose", "Verbose", false)]
    }
}

/// A server configuration over one shared session store.
fn server_config() -> AcpServerConfig {
    let agent: Arc<dyn AdkAgent> = Arc::new(
        adk_agent::LlmAgentBuilder::new("v2-agent")
            .model(Arc::new(FixedLlm))
            .instruction("base instruction")
            .build()
            .expect("agent"),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    AcpServerConfigBuilder::new()
        .agent(agent)
        .session_service(sessions)
        .agent_name("v2-agent")
        .agent_description("ACP v2 test agent")
        .session_controls(Arc::new(Controls))
        .build()
        .expect("server config")
}

/// A transport to a new connection of the server.
fn serve(config: &AcpServerConfig) -> Channel {
    let (server_end, client_end) = Channel::duplex();
    tokio::spawn(AcpServer::serve(config.clone(), server_end));
    client_end
}

fn client_config() -> AcpAgentConfig {
    AcpAgentConfig::new("in-process").working_dir(std::env::current_dir().expect("cwd"))
}

#[tokio::test]
async fn a_v1_client_reaches_the_v2_capable_server_unchanged() {
    let config = server_config();
    // A single transport cannot be reopened for a fallback, so it speaks v1.
    let connection = tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_to(
            serve(&config),
            client_config(),
            Arc::new(PermissionPolicy::DenyAll),
        ),
    )
    .await
    .expect("connect hung")
    .expect("connect");
    assert_eq!(connection.agent_info().protocol_version, ProtocolVersion::V1);
    assert!(connection.sdk_connection().is_some());
    assert!(connection.agent_info().capabilities.load_session, "the v1 server offers session/load");
    let mut session = connection.new_session().await.expect("session");
    let result = tokio::time::timeout(LIMIT, session.prompt("hello"))
        .await
        .expect("prompt hung")
        .expect("prompt");
    assert_eq!(result.text, "model answer");
    assert_eq!(result.stop_reason, v1::StopReason::EndTurn);
    session.close().await.expect("close");
    connection.close().await.expect("close");
}

#[tokio::test]
async fn a_v2_client_runs_sessions_on_the_v2_server() {
    let config = server_config();
    let connections = Arc::new(AtomicUsize::new(0));
    let factory = {
        let (config, connections) = (config.clone(), connections.clone());
        move || {
            connections.fetch_add(1, Ordering::SeqCst);
            serve(&config)
        }
    };
    let connection = tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_with_factory(
            factory,
            client_config(),
            Arc::new(PermissionPolicy::DenyAll),
        ),
    )
    .await
    .expect("connect hung")
    .expect("connect");
    let info = connection.agent_info();
    assert_eq!(info.protocol_version, ProtocolVersion::V2);
    assert_eq!(info.agent.as_ref().map(|agent| agent.name.as_str()), Some("v2-agent"));
    assert!(!info.capabilities.load_session, "v2 has no session/load");
    assert!(info.capabilities.session_capabilities.delete.is_some());
    assert!(connection.sdk_connection().is_none());
    assert!(connection.sdk_connection_v2().is_some());
    assert_eq!(connections.load(Ordering::SeqCst), 1, "no fallback connection");

    let mut session = connection.new_session().await.expect("session");
    assert_eq!(
        session.config_options().map(|options| options.len()),
        Some(1),
        "the new-session response carries the options"
    );
    let mut chunks = Vec::new();
    let result = tokio::time::timeout(
        LIMIT,
        session.prompt_blocks(vec![v1::ContentBlock::from("hello")], |chunk| chunks.push(chunk)),
    )
    .await
    .expect("prompt hung")
    .expect("prompt");
    assert_eq!(result.text, "model answer");
    assert_eq!(result.stop_reason, v1::StopReason::EndTurn);
    let usage = result.usage.expect("the idle update carries the turn's usage");
    assert_eq!((usage.input_tokens, usage.output_tokens, usage.total_tokens), (30, 12, 42));
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, OutputChunk::Text(text) if text == "model answer"))
    );

    // A second turn on the same session.
    let again = tokio::time::timeout(LIMIT, session.prompt("again"))
        .await
        .expect("prompt hung")
        .expect("prompt");
    assert_eq!(again.prompt_count, 2);

    let options = session
        .set_config_option("verbose", v1::SessionConfigOptionValue::Boolean { value: true })
        .await
        .expect("set config option");
    assert!(matches!(
        &options[0].kind,
        v1::SessionConfigKind::Boolean(boolean) if boolean.current_value
    ));

    let session_id = session.session_id().clone();
    let listed = connection.list_sessions(None).await.expect("list");
    assert!(listed.iter().any(|info| info.session_id == session_id));
    session.close().await.expect("close");

    let mut resumed = connection.resume_session(session_id.clone()).await.expect("resume");
    let result = tokio::time::timeout(LIMIT, resumed.prompt("after resume"))
        .await
        .expect("prompt hung")
        .expect("prompt");
    assert_eq!(result.text, "model answer");
    resumed.close().await.expect("close");
    connection.delete_session(session_id).await.expect("delete");
    connection.close().await.expect("close");
}

/// An agent that speaks only v1: it answers `initialize`, `session/new` and
/// `session/prompt`, streaming one text chunk.
async fn v1_only_agent(transport: Channel) {
    let _ = Agent
        .v1()
        .on_receive_request(
            async |request: v1::InitializeRequest,
                   responder: adk_acp::agent_client_protocol::v1::Responder<v1::InitializeResponse>,
                   _connection: adk_acp::agent_client_protocol::v1::ConnectionTo<Client>| {
                responder.respond(
                    v1::InitializeResponse::new(request.protocol_version)
                        .agent_info(v1::Implementation::new("v1-only", "1.0.0")),
                )
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_request: v1::NewSessionRequest,
                   responder: adk_acp::agent_client_protocol::v1::Responder<v1::NewSessionResponse>,
                   _connection: adk_acp::agent_client_protocol::v1::ConnectionTo<Client>| {
                responder.respond(v1::NewSessionResponse::new("v1-session"))
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: v1::PromptRequest,
                   responder: adk_acp::agent_client_protocol::v1::Responder<v1::PromptResponse>,
                   connection: adk_acp::agent_client_protocol::v1::ConnectionTo<Client>| {
                connection.send_notification(v1::SessionNotification::new(
                    request.session_id.clone(),
                    v1::SessionUpdate::AgentMessageChunk(v1::ContentChunk::new(
                        "v1 answer".into(),
                    )),
                ))?;
                responder.respond(v1::PromptResponse::new(v1::StopReason::EndTurn))
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .connect_to(transport)
        .await;
}

#[tokio::test]
async fn a_v2_client_falls_back_to_a_v1_only_agent() {
    let connections = Arc::new(AtomicUsize::new(0));
    let factory = {
        let connections = connections.clone();
        move || {
            connections.fetch_add(1, Ordering::SeqCst);
            let (agent_end, client_end) = Channel::duplex();
            tokio::spawn(v1_only_agent(agent_end));
            client_end
        }
    };
    let connection = tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_with_factory(
            factory,
            client_config(),
            Arc::new(PermissionPolicy::DenyAll),
        ),
    )
    .await
    .expect("connect hung")
    .expect("connect");
    assert_eq!(connection.agent_info().protocol_version, ProtocolVersion::V1);
    assert_eq!(
        connection.agent_info().agent.as_ref().map(|agent| agent.name.as_str()),
        Some("v1-only")
    );
    // The v1 initialize advertises capabilities the v2 probe could not, so the fallback
    // opened a fresh connection.
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    let mut session = connection.new_session().await.expect("session");
    let result = tokio::time::timeout(LIMIT, session.prompt("hello"))
        .await
        .expect("prompt hung")
        .expect("prompt");
    assert_eq!(result.text, "v1 answer");
    session.close().await.expect("close");
    connection.close().await.expect("close");
}

/// What a raw v2 client saw on the connection.
#[derive(Default)]
struct Seen {
    updates: Mutex<Vec<v2s::SessionUpdate>>,
}

/// Initialize a raw v2 client with the server and run `body` on the connection.
async fn with_raw_v2_client<F, Fut>(config: &AcpServerConfig, seen: Arc<Seen>, body: F)
where
    F: FnOnce(v2::ConnectionTo<Agent>) -> Fut + Send,
    Fut: std::future::Future<Output = ()> + Send,
{
    let transport = serve(config);
    Client
        .v2()
        .on_receive_notification(
            {
                let seen = seen.clone();
                async move |notification: v2s::UpdateSessionNotification,
                            _connection: v2::ConnectionTo<Agent>| {
                    seen.updates.lock().unwrap().push(notification.update);
                    Ok(())
                }
            },
            adk_acp::agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(transport, async move |connection: v2::ConnectionTo<Agent>| {
            let outcome = connection
                .send_initialize(v2s::InitializeRequest::new(
                    ProtocolVersion::V2,
                    v2s::Implementation::new("raw-v2", "1.0.0"),
                ))
                .block_task()
                .await?;
            assert!(matches!(outcome, InitializeOutcome::Initialized(_)), "the server selects v2");
            body(connection).await;
            Ok(())
        })
        .await
        .expect("raw v2 client");
}

async fn new_session(connection: &v2::ConnectionTo<Agent>) -> v2s::SessionId {
    connection
        .send_request(v2s::NewSessionRequest::new(std::env::current_dir().expect("cwd")))
        .block_task()
        .await
        .expect("session/new")
        .session_id
}

/// Wait until `seen` holds an idle update with a stop reason.
async fn wait_for_idle(seen: &Seen) {
    tokio::time::timeout(LIMIT, async {
        loop {
            let ended = seen.updates.lock().unwrap().iter().any(|update| {
                matches!(
                    update,
                    v2s::SessionUpdate::StateUpdate(v2s::StateUpdate::Idle(idle))
                        if idle.stop_reason.is_some()
                )
            });
            if ended {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn never reported idle");
}

#[tokio::test]
async fn tool_call_and_terminal_cancel_name_nothing_cancellable() {
    let config = server_config();
    with_raw_v2_client(&config, Arc::default(), async |connection| {
        let session_id = new_session(&connection).await;
        let error = connection
            .send_request(v2s::CancelToolCallRequest::new(session_id.clone(), "call-1"))
            .block_task()
            .await
            .expect_err("no tool call is cancellable");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        let error = connection
            .send_request(v2s::CancelTerminalRequest::new(session_id, "terminal-1"))
            .block_task()
            .await
            .expect_err("the server owns no terminals");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    })
    .await;
}

#[tokio::test]
async fn a_resume_replays_the_user_message_under_its_prompt_id() {
    let config = server_config();
    let seen = Arc::new(Seen::default());
    let observed = seen.clone();
    with_raw_v2_client(&config, seen, async move |connection| {
        let session_id = new_session(&connection).await;
        let accepted = connection
            .send_request(v2s::PromptRequest::new(
                session_id.clone(),
                vec![v2s::ContentBlock::from("hello")],
            ))
            .block_task()
            .await
            .expect("prompt accepted");
        wait_for_idle(&observed).await;
        {
            let updates = observed.updates.lock().unwrap();
            assert!(updates.iter().any(|update| matches!(
                update,
                v2s::SessionUpdate::UserMessage(message) if message.message_id == accepted.message_id
            )));
            assert!(updates.iter().any(|update| matches!(
                update,
                v2s::SessionUpdate::StateUpdate(v2s::StateUpdate::Running(_))
            )));
        }
        connection
            .send_request(v2s::CloseSessionRequest::new(session_id.clone()))
            .block_task()
            .await
            .expect("close");
        observed.updates.lock().unwrap().clear();
        connection
            .send_request(
                v2s::ResumeSessionRequest::new(session_id, std::env::current_dir().expect("cwd"))
                    .replay_from(v2s::ReplayFrom::Start(v2s::ReplayFromStart::new())),
            )
            .block_task()
            .await
            .expect("resume");
        let updates = observed.updates.lock().unwrap();
        assert!(
            updates.iter().any(|update| matches!(
                update,
                v2s::SessionUpdate::UserMessageChunk(chunk) if chunk.message_id == accepted.message_id
            )),
            "the replayed user message keeps the id its prompt was answered with"
        );
        assert!(updates.iter().any(|update| matches!(
            update,
            v2s::SessionUpdate::AgentMessageChunk(_)
        )));
        assert!(
            matches!(
                updates.last(),
                Some(v2s::SessionUpdate::StateUpdate(v2s::StateUpdate::Idle(_)))
            ),
            "the resume ends with the session's current state"
        );
    })
    .await;
}

#[tokio::test]
async fn a_client_with_tool_servers_speaks_v1() {
    // MCP over ACP is served on v1 only, so a client with tool servers does not offer v2.
    let config = server_config();
    let connections = Arc::new(AtomicUsize::new(0));
    let factory = {
        let (config, connections) = (config.clone(), connections.clone());
        move || {
            connections.fetch_add(1, Ordering::SeqCst);
            serve(&config)
        }
    };
    let connection = tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_with_factory(
            factory,
            client_config().tool_server(AcpToolServer::new("app-tools")),
            Arc::new(PermissionPolicy::DenyAll),
        ),
    )
    .await
    .expect("connect hung")
    .expect("connect");
    assert_eq!(connection.agent_info().protocol_version, ProtocolVersion::V1);
    assert!(connection.sdk_connection().is_some());
    assert!(connection.sdk_connection_v2().is_none());
    assert_eq!(connections.load(Ordering::SeqCst), 1, "no fallback connection");
    connection.close().await.expect("close");
}
