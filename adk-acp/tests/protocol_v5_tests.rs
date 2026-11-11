//! End-to-end tests of the ACP v5 surface: the adk-acp client talking to the
//! adk-acp server over an in-process SDK channel, and the client talking to a
//! scripted SDK agent for the agent-initiated surfaces the server does not
//! produce (elicitation, notices, compactions, subagents).
#![cfg(feature = "server")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_acp::agent_client_protocol::schema::ProtocolVersion;
use adk_acp::agent_client_protocol::v1::schema::{
    AuthAccount, AuthMethod, AuthMethodAgent, AuthState, AuthenticateRequest, AvailableCommand,
    AvailableCommandsUpdate, CompactionStatus, CompactionSummaryChunk, CompactionUpdate,
    ContentBlock, ContentChunk, CreateElicitationRequest, CreateElicitationResponse,
    ElicitationAcceptAction, ElicitationAction, ElicitationContentValue, ElicitationFormMode,
    ElicitationMode, ElicitationSchema, ElicitationSessionScope, InitializeRequest,
    InitializeResponse, ListSessionsRequest, NewSessionRequest, NewSessionResponse, Notice,
    NoticeSeverity, PromptRequest, PromptResponse, SessionArchiveFilter, SessionConfigKind,
    SessionConfigOption, SessionMode, SessionModeState, SessionNotification, SessionStatus,
    SessionUpdate, StopReason, SubagentUpdate, Usage,
};
use adk_acp::agent_client_protocol::v1::{ConnectionTo, Responder};
use adk_acp::agent_client_protocol::{Agent, Builder, Channel, Client, Error};
use adk_acp::server::{
    AcpAuthenticator, AcpServer, AcpServerConfigBuilder, AcpServerError, SessionControls,
    SystemPromptSupport,
};
use adk_acp::{
    AcpAgentConfig, AcpConnection, AcpElicitation, AcpError, OutputChunk, PermissionPolicy,
};
use adk_core::{
    Agent as AdkAgent, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, UsageMetadata,
};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;

const LIMIT: Duration = Duration::from_secs(20);

// ─── Server side: an LlmAgent over a model that records its requests ────────────

/// Records every request and answers each with fixed text and usage.
#[derive(Default)]
struct RecordingLlm {
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait]
impl Llm for RecordingLlm {
    fn name(&self) -> &str {
        "recording"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request);
        let mut response = LlmResponse::new(Content::new("model").with_text("model answer"));
        response.turn_complete = true;
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 30,
            candidates_token_count: 12,
            total_token_count: 42,
            thinking_token_count: Some(5),
            cache_read_input_token_count: Some(7),
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
    }
}

impl RecordingLlm {
    /// The system texts of the last request, in order.
    fn last_system_texts(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap();
        let request = requests.last().expect("a model request");
        request
            .contents
            .iter()
            .filter(|content| content.role == "system")
            .map(|content| {
                content.parts.iter().filter_map(|part| part.text()).collect::<Vec<_>>().join("")
            })
            .collect()
    }
}

/// Two modes and a boolean option.
struct Controls;

impl SessionControls for Controls {
    fn modes(&self) -> Option<SessionModeState> {
        Some(SessionModeState::new(
            "ask",
            vec![SessionMode::new("ask", "Ask"), SessionMode::new("code", "Code")],
        ))
    }

    fn config_options(&self) -> Vec<SessionConfigOption> {
        vec![SessionConfigOption::boolean("verbose", "Verbose", false)]
    }
}

/// Requires the `token` method; supports logout and reports one account.
struct TokenAuth;

#[async_trait]
impl AcpAuthenticator for TokenAuth {
    fn methods(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::Agent(AuthMethodAgent::new("token", "Workspace token"))]
    }

    async fn authenticate(&self, request: &AuthenticateRequest) -> Result<(), AcpServerError> {
        assert_eq!(&*request.method_id.0, "token");
        Ok(())
    }

    fn supports_logout(&self) -> bool {
        true
    }

    async fn accounts(&self) -> Vec<AuthAccount> {
        vec![AuthAccount::new("Ada").email("ada@example.com".to_string())]
    }
}

struct Server {
    llm: Arc<RecordingLlm>,
    sessions: Arc<dyn SessionService>,
}

/// Serve an ADK-Rust LlmAgent and connect the adk-acp client to it.
async fn connect(
    client: AcpAgentConfig,
    configure: impl FnOnce(AcpServerConfigBuilder) -> AcpServerConfigBuilder,
) -> (AcpConnection, Server) {
    let llm = Arc::new(RecordingLlm::default());
    let agent: Arc<dyn AdkAgent> = Arc::new(
        adk_agent::LlmAgentBuilder::new("v5-agent")
            .model(llm.clone())
            .instruction("base instruction")
            .build()
            .expect("agent"),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let config = configure(
        AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(sessions.clone())
            .agent_name("v5-agent")
            .agent_description("ACP v5 test agent")
            .session_controls(Arc::new(Controls)),
    )
    .build()
    .expect("server config");
    let (server_end, client_end) = Channel::duplex();
    tokio::spawn(AcpServer::serve(config, server_end));
    let connection = tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_to(client_end, client, Arc::new(PermissionPolicy::DenyAll)),
    )
    .await
    .expect("connect hung")
    .expect("connect");
    (connection, Server { llm, sessions })
}

fn client_config() -> AcpAgentConfig {
    AcpAgentConfig::new("in-process").working_dir(std::env::current_dir().expect("cwd"))
}

#[tokio::test]
async fn the_handshake_negotiates_v1_and_reports_the_agent() {
    let (connection, _server) = connect(client_config(), |builder| builder).await;
    let info = connection.agent_info();
    assert_eq!(info.protocol_version, ProtocolVersion::V1);
    let agent = info.agent.as_ref().expect("agent info");
    assert_eq!(agent.name, "v5-agent");
    assert_eq!(agent.title.as_deref(), Some("ACP v5 test agent"));
    let session = &info.capabilities.session_capabilities;
    for (name, advertised) in [
        ("list", session.list.is_some()),
        ("delete", session.delete.is_some()),
        ("resume", session.resume.is_some()),
        ("close", session.close.is_some()),
        ("fork", session.fork.is_some()),
        ("archive", session.archive.is_some()),
        ("status", session.status.is_some()),
        ("options", session.options.is_some()),
        ("title", session.title.is_some()),
        ("systemPrompt", session.system_prompt.is_some()),
    ] {
        assert!(advertised, "{name} must be advertised");
    }
    assert!(info.capabilities.auth.status.is_some());
    assert!(info.auth_methods.is_empty(), "no authenticator, no methods");
    connection.close().await.expect("close");
}

#[tokio::test]
async fn a_prompt_returns_the_stop_reason_and_the_turns_usage() {
    let (connection, _server) = connect(client_config(), |builder| builder).await;
    let mut session = connection.new_session().await.expect("session");
    let mut chunks = Vec::new();
    let result = tokio::time::timeout(
        LIMIT,
        session.prompt_blocks(vec![ContentBlock::from("hello")], |chunk| chunks.push(chunk)),
    )
    .await
    .expect("prompt hung")
    .expect("prompt");
    assert_eq!(result.text, "model answer");
    assert_eq!(result.stop_reason, StopReason::EndTurn);
    let usage = result.usage.expect("usage");
    assert_eq!((usage.input_tokens, usage.output_tokens, usage.total_tokens), (30, 12, 42));
    assert_eq!(usage.thought_tokens, Some(5));
    assert_eq!(usage.cached_read_tokens, Some(7));
    assert!(chunks.iter().any(|chunk| matches!(chunk, OutputChunk::Usage { used: 42, .. })));
    session.close().await.expect("close");
    connection.close().await.expect("close");
}

#[tokio::test]
async fn an_appended_system_prompt_follows_the_agents_own_instructions() {
    let (connection, server) =
        connect(client_config().append_system_prompt("answer in French"), |builder| builder).await;
    let mut session = connection.new_session().await.expect("session");
    session.prompt("hi").await.expect("prompt");
    assert_eq!(server.llm.last_system_texts(), vec!["base instruction", "answer in French"]);
    session.close().await.expect("close");
}

#[tokio::test]
async fn an_override_system_prompt_replaces_the_agents_own_instructions() {
    let (connection, server) =
        connect(client_config().override_system_prompt("only this"), |builder| builder).await;
    let mut session = connection.new_session().await.expect("session");
    session.prompt("hi").await.expect("prompt");
    assert_eq!(server.llm.last_system_texts(), vec!["only this"]);
    session.close().await.expect("close");
}

#[tokio::test]
async fn system_prompts_the_agent_does_not_accept_are_never_sent() {
    let (connection, _server) = connect(client_config().override_system_prompt("x"), |builder| {
        builder.system_prompts(SystemPromptSupport::Append)
    })
    .await;
    let error = connection.new_session().await.err().expect("refused");
    assert!(matches!(error, AcpError::Unsupported(_)), "got {error:?}");

    let (connection, _server) = connect(client_config().append_system_prompt("x"), |builder| {
        builder.system_prompts(SystemPromptSupport::Disabled)
    })
    .await;
    assert!(connection.agent_info().capabilities.session_capabilities.system_prompt.is_none());
    let error = connection.new_session().await.err().expect("refused");
    assert!(matches!(error, AcpError::Unsupported(_)), "got {error:?}");
}

#[tokio::test]
async fn titles_status_and_archiving_round_trip() {
    let (connection, _server) = connect(client_config(), |builder| builder).await;
    let mut session = connection.new_session().await.expect("session");
    let id = session.session_id().clone();

    assert_eq!(session.set_title("Refactor parser").await.expect("title"), "Refactor parser");
    assert_eq!(session.title().as_deref(), Some("Refactor parser"));
    assert_eq!(session.status().await.expect("status"), SessionStatus::Open);

    let listed = connection.list_sessions(None).await.expect("list");
    let info = listed.iter().find(|info| info.session_id == id).expect("listed");
    assert_eq!(info.title.as_deref(), Some("Refactor parser"));
    assert_eq!(info.archived, Some(false));

    session.archive().await.expect("archive");
    let default_list = connection.list_sessions(None).await.expect("list");
    assert!(default_list.iter().all(|info| info.session_id != id), "archived sessions are hidden");
    let only_archived = connection
        .list_sessions_page(ListSessionsRequest::new().archived(SessionArchiveFilter::Only))
        .await
        .expect("archived list");
    assert_eq!(only_archived.sessions.len(), 1);
    assert_eq!(only_archived.sessions[0].archived, Some(true));
    session.unarchive().await.expect("unarchive");
    assert!(
        connection
            .list_sessions(None)
            .await
            .expect("list")
            .iter()
            .any(|info| info.session_id == id)
    );

    session.close().await.expect("close");
    assert_eq!(connection.session_status(id.clone()).await.expect("status"), SessionStatus::Saved);
    let missing = connection.session_status("no-such-session").await.expect_err("missing");
    assert!(missing.to_string().contains("no-such-session"), "got {missing}");
    connection.close().await.expect("close");
}

fn boolean_value(option: &SessionConfigOption) -> Option<bool> {
    match &option.kind {
        SessionConfigKind::Boolean(boolean) => Some(boolean.current_value),
        _ => None,
    }
}

#[tokio::test]
async fn options_initial_values_modes_and_config_round_trip() {
    let (connection, _server) =
        connect(client_config().initial_config_option("verbose", true), |builder| builder).await;

    let options = connection.session_options(None).await.expect("options");
    assert_eq!(
        options.modes.as_ref().map(|modes| modes.current_mode_id.to_string()).as_deref(),
        Some("ask")
    );
    assert_eq!(options.config_options.as_ref().map(Vec::len), Some(1));

    let mut session = connection.new_session().await.expect("session");
    let verbose = session.config_options().expect("config options");
    assert_eq!(
        boolean_value(&verbose[0]),
        Some(true),
        "the initial value applies to the new session"
    );
    session.set_mode("code").await.expect("set mode");
    assert_eq!(session.modes().expect("modes").current_mode_id.to_string(), "code");
    assert!(matches!(session.set_mode("nope").await, Err(AcpError::Unsupported(_))));
    let updated = session.set_config_option("verbose", false).await.expect("set option");
    assert_eq!(boolean_value(&updated[0]), Some(false));

    // The stored session's options reflect its selections.
    let stored =
        connection.session_options(Some(session.session_id().clone())).await.expect("options");
    assert_eq!(stored.modes.expect("modes").current_mode_id.to_string(), "code");
    session.close().await.expect("close");

    let (connection, _server) =
        connect(client_config().initial_config_option("no-such-option", true), |builder| builder)
            .await;
    assert!(connection.new_session().await.is_err(), "unknown initial options are refused");
}

#[tokio::test]
async fn fork_load_and_resume_open_working_sessions() {
    let (connection, server) = connect(client_config(), |builder| builder).await;
    let mut source = connection.new_session().await.expect("session");
    source.prompt("remember this").await.expect("prompt");

    let mut fork = source.fork().await.expect("fork");
    assert_ne!(fork.session_id(), source.session_id());
    fork.prompt("and this").await.expect("prompt on the fork");
    let source_id = source.session_id().clone();
    source.close().await.expect("close source");
    fork.close().await.expect("close fork");

    // Loading replays the stored conversation before the response.
    let mut loaded = connection.load_session(source_id.clone()).await.expect("load");
    let replay = loaded.pending_updates();
    assert!(
        replay
            .iter()
            .any(|chunk| matches!(chunk, OutputChunk::UserText(text) if text == "remember this")),
        "got {replay:?}"
    );
    assert!(
        replay
            .iter()
            .any(|chunk| matches!(chunk, OutputChunk::Text(text) if text == "model answer"))
    );
    loaded.close().await.expect("close");

    let mut resumed = connection.resume_session(source_id.clone()).await.expect("resume");
    assert!(
        resumed.pending_updates().iter().all(|chunk| !matches!(chunk, OutputChunk::UserText(_)))
    );
    resumed.prompt("again").await.expect("prompt after resume");
    resumed.close().await.expect("close");

    connection.delete_session(source_id.clone()).await.expect("delete");
    let deleted = server
        .sessions
        .get(adk_session::GetRequest {
            app_name: "v5-agent".into(),
            user_id: "acp-client".into(),
            session_id: source_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await;
    assert!(deleted.is_err(), "the deleted session is gone");
}

#[tokio::test]
async fn authentication_gates_sessions_and_reports_status() {
    // Without authenticating, session requests are refused with auth_required.
    let (connection, _server) =
        connect(client_config(), |builder| builder.authenticator(Arc::new(TokenAuth))).await;
    assert_eq!(connection.agent_info().auth_methods.len(), 1);
    assert!(connection.agent_info().capabilities.auth.logout.is_some());
    let refused = connection.new_session().await.err().expect("auth required");
    assert!(
        refused.to_string().contains("-32000")
            || refused.to_string().to_lowercase().contains("auth"),
        "got {refused}"
    );
    let status = connection.auth_status().await.expect("auth/status works unauthenticated");
    assert_eq!(status.state, AuthState::Unauthenticated);

    connection.authenticate("token").await.expect("authenticate");
    let status = connection.auth_status().await.expect("status");
    assert_eq!(status.state, AuthState::Authenticated);
    assert_eq!(status.accounts[0].label, "Ada");
    let mut session = connection.new_session().await.expect("session after authenticating");
    session.close().await.expect("close");

    connection.logout().await.expect("logout");
    assert_eq!(connection.auth_status().await.expect("status").state, AuthState::Unauthenticated);
    let update = connection.last_auth_status().expect("auth/status_update");
    assert_eq!(update.state, AuthState::Unauthenticated);
    assert!(connection.new_session().await.is_err(), "logout ends the authenticated state");
    assert!(matches!(connection.authenticate("other").await, Err(AcpError::Unsupported(_))));

    // A configured method authenticates during the handshake.
    let (connection, _server) = connect(client_config().auth_method("token"), |builder| {
        builder.authenticator(Arc::new(TokenAuth))
    })
    .await;
    let mut session = connection.new_session().await.expect("session");
    session.close().await.expect("close");
}

// ─── Client side: a scripted SDK agent ───────────────────────────────────────────

/// Answers forms with `main` for every field.
#[derive(Debug, Default)]
struct Branch {
    asked: Mutex<Vec<String>>,
}

#[async_trait]
impl AcpElicitation for Branch {
    async fn elicit(
        &self,
        request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, Error> {
        self.asked.lock().unwrap().push(request.message.clone());
        let ElicitationMode::Form(form) = &request.mode else {
            return Ok(CreateElicitationResponse::new(ElicitationAction::Decline));
        };
        let content: BTreeMap<String, ElicitationContentValue> = form
            .requested_schema
            .properties
            .keys()
            .map(|name| (name.clone(), ElicitationContentValue::from("main")))
            .collect();
        Ok(CreateElicitationResponse::new(ElicitationAction::Accept(
            ElicitationAcceptAction::new().content(content),
        )))
    }
}

fn update(session: &str, update: SessionUpdate) -> SessionNotification {
    SessionNotification::new(session.to_string(), update)
}

fn text(value: &str) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(value)))
}

/// An SDK agent that sends updates before answering `session/new`, and a
/// turn of notices, compactions, subagent output and an elicitation.
async fn scripted_agent(transport: Channel, protocol_version: Option<ProtocolVersion>) {
    let agent = Agent.v1();
    let result = agent
        .on_receive_request(
            async move |request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _connection: ConnectionTo<Client>| {
                responder.respond(InitializeResponse::new(
                    protocol_version.unwrap_or(request.protocol_version),
                ))
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_request: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        connection: ConnectionTo<Client>| {
                // Sent before the response: the client parks it until it knows the id.
                connection.send_notification(update(
                    "s1",
                    SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
                        AvailableCommand::new("review", "Review the diff"),
                    ])),
                ))?;
                responder.respond(NewSessionResponse::new("s1"))
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        connection: ConnectionTo<Client>| {
                let session = request.session_id.to_string();
                connection.clone().spawn(async move {
                    connection.send_notification(update(
                        &session,
                        SessionUpdate::Notice(Notice::new(NoticeSeverity::Warning, "rate limited")),
                    ))?;
                    connection.send_notification(update(
                        &session,
                        SessionUpdate::CompactionUpdate(CompactionUpdate::new(
                            "c1",
                            CompactionStatus::Completed,
                        )),
                    ))?;
                    connection.send_notification(update(
                        &session,
                        SessionUpdate::CompactionSummaryChunk(CompactionSummaryChunk::new(
                            "c1",
                            ContentBlock::from("summary"),
                        )),
                    ))?;
                    connection.send_notification(update(
                        &session,
                        SessionUpdate::SubagentUpdate(SubagentUpdate::new("child")),
                    ))?;
                    connection.send_notification(update("child", text("from the child")))?;
                    let answer = connection
                        .send_request(CreateElicitationRequest::new(
                            ElicitationFormMode::new(
                                ElicitationSessionScope::new(session.clone()),
                                ElicitationSchema::new().string("branch", true),
                            ),
                            "Which branch?",
                        ))
                        .block_task()
                        .await?;
                    let branch = match answer.action {
                        ElicitationAction::Accept(accept) => accept
                            .content
                            .and_then(|content| content.get("branch").cloned())
                            .map(|value| match value {
                                ElicitationContentValue::String(value) => value,
                                other => format!("{other:?}"),
                            })
                            .unwrap_or_default(),
                        other => format!("{other:?}"),
                    };
                    connection
                        .send_notification(update(&session, text(&format!("branch={branch}"))))?;
                    responder.respond(
                        PromptResponse::new(StopReason::EndTurn).usage(Usage::new(3, 2, 1)),
                    )
                })
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .connect_to(transport)
        .await;
    let _ = result;
}

#[tokio::test]
async fn agent_initiated_surfaces_reach_the_client() {
    let (agent_end, client_end) = Channel::duplex();
    tokio::spawn(scripted_agent(agent_end, None));
    let elicitation = Arc::new(Branch::default());
    let connection = AcpConnection::connect_to(
        client_end,
        client_config().elicitation(elicitation.clone()),
        Arc::new(PermissionPolicy::DenyAll),
    )
    .await
    .expect("connect");
    let mut session = connection.new_session().await.expect("session");

    let mut chunks = Vec::new();
    let result = tokio::time::timeout(
        LIMIT,
        session.prompt_blocks(vec![ContentBlock::from("go")], |chunk| chunks.push(chunk)),
    )
    .await
    .expect("prompt hung")
    .expect("prompt");

    // The available commands sent before `session/new` answered were parked.
    assert_eq!(session.available_commands().len(), 1);
    assert_eq!(result.text, "branch=main", "the elicitation answer reached the agent");
    assert_eq!(result.usage.map(|usage| usage.total_tokens), Some(3));
    assert_eq!(elicitation.asked.lock().unwrap().as_slice(), ["Which branch?"]);
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        OutputChunk::Notice { severity: NoticeSeverity::Warning, title, .. } if title == "rate limited"
    )));
    assert!(chunks.iter().any(|chunk| matches!(chunk, OutputChunk::Compaction(_))));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        OutputChunk::CompactionSummary { compaction_id, text } if compaction_id == "c1" && text == "summary"
    )));
    assert!(chunks.iter().any(|chunk| matches!(chunk, OutputChunk::Subagent(_))));
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        OutputChunk::SubagentOutput { session_id, chunk } if session_id == "child"
            && matches!(chunk.as_ref(), OutputChunk::Text(text) if text == "from the child")
    )));
    // The subagent's text is not part of the parent's answer.
    assert!(!result.text.contains("from the child"));
    session.close().await.expect("close");
    connection.close().await.expect("close");
}

#[tokio::test]
async fn an_agent_selecting_an_unsupported_version_is_rejected() {
    let (agent_end, client_end) = Channel::duplex();
    // An unguarded agent can answer with any version; the client's guard refuses v0.
    tokio::spawn(async move {
        let _ =
            Builder::new(Agent)
                .on_receive_request(
                    async move |_request: InitializeRequest,
                                responder: adk_acp::agent_client_protocol::Responder<
                        InitializeResponse,
                    >,
                                _connection: adk_acp::agent_client_protocol::ConnectionTo<
                        Client,
                    >| {
                        responder.respond(InitializeResponse::new(ProtocolVersion::V0))
                    },
                    adk_acp::agent_client_protocol::on_receive_request!(),
                )
                .connect_to(agent_end)
                .await;
    });
    let result = tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_to(client_end, client_config(), Arc::new(PermissionPolicy::DenyAll)),
    )
    .await
    .expect("connect hung");
    assert!(result.is_err(), "a v0 answer must not complete the handshake");
}

#[tokio::test]
async fn the_scripted_agent_uses_the_negotiated_version() {
    let (agent_end, client_end) = Channel::duplex();
    tokio::spawn(scripted_agent(agent_end, None));
    let connection =
        AcpConnection::connect_to(client_end, client_config(), Arc::new(PermissionPolicy::DenyAll))
            .await
            .expect("connect");
    assert_eq!(connection.agent_info().protocol_version, ProtocolVersion::V1);
    // No elicitation handler: the capability is not advertised and capabilities
    // the agent lacks are refused locally.
    assert!(matches!(connection.list_sessions(None).await, Err(AcpError::Unsupported(_))));
    assert!(matches!(connection.fork_session("s1").await, Err(AcpError::Unsupported(_))));
    connection.close().await.expect("close");
}
