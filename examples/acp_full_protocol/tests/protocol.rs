//! End-to-end validation of the ACP v1 server-direction features through a
//! real [`Runner`](adk_runner::Runner)-backed `AcpServer`, driven by the
//! official SDK `Client` over an in-process [`Channel::duplex`] — no subprocess
//! and no API key.
//!
//! The first tests pair the SDK's v1 `Client` with the server that
//! [`AcpServer::serve`] runs — the same wiring `AcpServer::run` serves over
//! stdio — exactly as an IDE would, and assert:
//!
//! - embedded-resource prompts reach the agent as `Part::EmbeddedResource`;
//! - image / audio prompts are accepted and reach the agent as
//!   `Part::InlineData`;
//! - the permission bridge fires for a confirmation-gated tool (allow executes,
//!   deny skips), and the outer prompt completes in both cases;
//! - `session/load` replays stored updates in chronological order;
//! - `UsageUpdate` and an enriched `ToolCallUpdate` are surfaced.
//!
//! The ACP v5 tests at the end drive the same server with adk-acp's own client,
//! [`AcpConnection`], covering version negotiation, client system prompts,
//! end-of-turn usage, session titles, status, archiving, options, modes and
//! config, fork and resume, and authentication.

use std::sync::{Arc, Mutex};

use acp_full_protocol::{
    AGENT_INSTRUCTION, AGENT_NAME, CONFIRM_CALL_ID, ReferenceControls, ScriptedAgent, TOKEN_METHOD,
    WorkspaceToken, build_delete_tool, build_session_service,
};
use adk_acp::agent_client_protocol::schema::ProtocolVersion;
use adk_acp::agent_client_protocol::v1::schema::{
    AudioContent, AuthState, CloseSessionRequest, ContentBlock,
    EmbeddedResource as AcpEmbeddedResource, EmbeddedResourceResource, ImageContent,
    InitializeRequest, ListSessionsRequest, LoadSessionRequest, NewSessionRequest,
    PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionArchiveFilter, SessionConfigKind,
    SessionNotification, SessionStatus, SessionUpdate, StopReason, TextContent,
    TextResourceContents as AcpTextResourceContents,
};
use adk_acp::agent_client_protocol::v1::{ConnectionTo, Responder};
use adk_acp::agent_client_protocol::{Agent, Channel, Client};
use adk_acp::server::{AcpServer, AcpServerConfigBuilder, ResponseStreamer};
use adk_acp::{AcpAgentConfig, AcpConnection, AcpError, OutputChunk, PermissionPolicy};
use adk_core::Agent as AdkAgent;
use adk_session::{GetRequest, SessionService};
use base64::{Engine as _, engine::general_purpose};

/// Serve the reference agent over one end of an in-process SDK channel —
/// exactly as `AcpServer::run` serves stdio — and return the other end.
fn serve(
    agent: Arc<dyn AdkAgent>,
    session_service: Arc<dyn SessionService>,
    configure: impl FnOnce(AcpServerConfigBuilder) -> AcpServerConfigBuilder,
) -> Channel {
    let config = configure(
        AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .agent_name(AGENT_NAME)
            .agent_description("ACP full-protocol reference agent")
            .session_controls(Arc::new(ReferenceControls)),
    )
    .build()
    .expect("valid config");
    let (server_channel, client_channel) = Channel::duplex();
    tokio::spawn(AcpServer::serve(config, server_channel));
    client_channel
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// How the test client answers a `session/request_permission` request.
#[derive(Clone, Copy)]
enum PermissionAnswer {
    Allow,
    Deny,
    Cancel,
}

/// Result of driving one prompt turn end to end.
struct DriveResult {
    stop_reason: StopReason,
    updates: Vec<SessionUpdate>,
    permission_call_id: Option<String>,
}

/// Text of an `AgentMessageChunk`, if the update is one carrying text.
fn message_chunk_text(update: &SessionUpdate) -> Option<String> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Drive `initialize` → `session/new` → `session/prompt` against the server,
/// answering any `session/request_permission` with `answer`, and capture every
/// `session/update` plus the stop reason.
async fn drive_prompt(
    agent: Arc<dyn AdkAgent>,
    session_service: Arc<dyn SessionService>,
    prompt: Vec<ContentBlock>,
    answer: PermissionAnswer,
) -> DriveResult {
    let client_channel = serve(agent, session_service, |config| config);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let updates_for_client = updates.clone();
    let permission_call_id = Arc::new(Mutex::new(None::<String>));
    let permission_for_client = permission_call_id.clone();

    let client = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                updates_for_client.lock().expect("updates lock").push(notification.update);
                Ok(())
            },
            adk_acp::agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            move |request: RequestPermissionRequest,
                  responder: Responder<RequestPermissionResponse>,
                  _connection: ConnectionTo<Agent>| {
                let captured = permission_for_client.clone();
                async move {
                    *captured.lock().expect("permission lock") =
                        Some(request.tool_call.tool_call_id.to_string());
                    let outcome = match answer {
                        PermissionAnswer::Allow => {
                            let option = request
                                .options
                                .iter()
                                .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                                .expect("allow option offered");
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                            ))
                        }
                        PermissionAnswer::Deny => {
                            let option = request
                                .options
                                .iter()
                                .find(|option| option.kind == PermissionOptionKind::RejectOnce)
                                .expect("reject option offered");
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                            ))
                        }
                        PermissionAnswer::Cancel => RequestPermissionOutcome::Cancelled,
                    };
                    responder.respond(RequestPermissionResponse::new(outcome))
                }
            },
            adk_acp::agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_channel, move |connection: ConnectionTo<Agent>| async move {
            let initialized = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
            let cwd = std::env::current_dir().expect("absolute cwd");
            let session = connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
            let prompt = connection
                .send_request(PromptRequest::new(session.session_id.clone(), prompt))
                .block_task()
                .await?;
            Ok(prompt.stop_reason)
        });

    let stop_reason = tokio::time::timeout(std::time::Duration::from_secs(5), client)
        .await
        .expect("prompt completed before timeout")
        .expect("official ACP client completed");

    let updates = updates.lock().expect("updates lock").clone();
    let permission_call_id = permission_call_id.lock().expect("permission lock").clone();
    DriveResult { stop_reason, updates, permission_call_id }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text))
}

fn embedded_text_block(uri: &str, text: &str) -> ContentBlock {
    ContentBlock::Resource(AcpEmbeddedResource::new(
        EmbeddedResourceResource::TextResourceContents(
            AcpTextResourceContents::new(text, uri).mime_type(Some("text/markdown".to_string())),
        ),
    ))
}

fn image_block(bytes: &[u8]) -> ContentBlock {
    ContentBlock::Image(ImageContent::new(general_purpose::STANDARD.encode(bytes), "image/png"))
}

fn audio_block(bytes: &[u8]) -> ContentBlock {
    ContentBlock::Audio(AudioContent::new(general_purpose::STANDARD.encode(bytes), "audio/wav"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// Embedded-resource prompt round-trip **and** usage / tool-call surfacing.
///
/// Sending a `session/prompt` carrying an `EmbeddedResource` block makes the
/// agent echo the resource's uri and text — which is only possible if the block
/// reached the agent as `Part::EmbeddedResource`. The same rich turn emits a
/// `UsageUpdate` and an enriched `ToolCallUpdate`.
#[tokio::test]
async fn embedded_resource_prompt_round_trips_and_surfaces_usage_and_tool_update() {
    let session_service = build_session_service();
    let agent: Arc<dyn AdkAgent> = Arc::new(ScriptedAgent::new(build_delete_tool()));
    let prompt = vec![text_block("inspect"), embedded_text_block("file:///notes.md", "# Notes")];

    let result = drive_prompt(agent, session_service, prompt, PermissionAnswer::Cancel).await;
    assert_eq!(result.stop_reason, StopReason::EndTurn);

    // The embedded resource reached the agent (echoed uri + text).
    let echoed: Vec<String> = result.updates.iter().filter_map(message_chunk_text).collect();
    assert!(
        echoed.iter().any(|text| text == "embedded-resource:file:///notes.md:# Notes"),
        "embedded resource must reach the agent as Part::EmbeddedResource, got {echoed:?}"
    );

    // A single UsageUpdate reflects the reported token count.
    let usage: Vec<_> = result
        .updates
        .iter()
        .filter_map(|update| match update {
            SessionUpdate::UsageUpdate(usage) => Some(usage.used),
            _ => None,
        })
        .collect();
    assert_eq!(usage, vec![50], "exactly one UsageUpdate with the reported total tokens");

    // An enriched ToolCallUpdate surfaces the tool result with a file location.
    let has_enriched_tool_update = result.updates.iter().any(|update| match update {
        SessionUpdate::ToolCallUpdate(update) => {
            update.fields.locations.as_ref().is_some_and(|locations| !locations.is_empty())
        }
        _ => false,
    });
    assert!(has_enriched_tool_update, "an enriched ToolCallUpdate with locations must be emitted");
}

/// Multimodal prompt acceptance: image and audio blocks are accepted and reach
/// the agent as `Part::InlineData` (echoed with mime type and byte length).
#[tokio::test]
async fn multimodal_prompt_is_accepted_as_inline_data() {
    let session_service = build_session_service();
    let agent: Arc<dyn AdkAgent> = Arc::new(ScriptedAgent::new(build_delete_tool()));
    let prompt = vec![text_block("look"), image_block(&[1, 2, 3, 4]), audio_block(&[5, 6, 7])];

    let result = drive_prompt(agent, session_service, prompt, PermissionAnswer::Cancel).await;
    assert_eq!(result.stop_reason, StopReason::EndTurn, "multimodal prompt must be accepted");

    let echoed: Vec<String> = result.updates.iter().filter_map(message_chunk_text).collect();
    assert!(
        echoed.iter().any(|text| text == "inline-data:image/png:4"),
        "image must reach the agent as Part::InlineData, got {echoed:?}"
    );
    assert!(
        echoed.iter().any(|text| text == "inline-data:audio/wav:3"),
        "audio must reach the agent as Part::InlineData, got {echoed:?}"
    );
}

/// Permission bridge — approval executes the gated tool, and the outer prompt
/// still completes after the nested `session/request_permission` round trip.
#[tokio::test]
async fn permission_allow_executes_gated_tool_and_completes_turn() {
    let session_service = build_session_service();
    let scripted = ScriptedAgent::new(build_delete_tool());
    let executed = scripted.executed_flag();
    let agent: Arc<dyn AdkAgent> = Arc::new(scripted);

    let result = drive_prompt(
        agent,
        session_service,
        vec![text_block("delete the report")],
        PermissionAnswer::Allow,
    )
    .await;

    assert_eq!(result.stop_reason, StopReason::EndTurn, "outer prompt must complete");
    assert!(*executed.lock().expect("executed lock"), "tool must execute on allow");
    assert_eq!(
        result.permission_call_id.as_deref(),
        Some(CONFIRM_CALL_ID),
        "permission request must correlate to the paused tool call"
    );
    let echoed: Vec<String> = result.updates.iter().filter_map(message_chunk_text).collect();
    assert!(
        echoed.iter().any(|text| text.contains("tool executed")),
        "executed message must be streamed, got {echoed:?}"
    );
}

/// Permission bridge — denial skips the gated tool, and the outer prompt still
/// completes.
#[tokio::test]
async fn permission_deny_skips_gated_tool_and_completes_turn() {
    let session_service = build_session_service();
    let scripted = ScriptedAgent::new(build_delete_tool());
    let executed = scripted.executed_flag();
    let agent: Arc<dyn AdkAgent> = Arc::new(scripted);

    let result = drive_prompt(
        agent,
        session_service,
        vec![text_block("delete the report")],
        PermissionAnswer::Deny,
    )
    .await;

    assert_eq!(result.stop_reason, StopReason::EndTurn, "outer prompt must complete");
    assert!(!*executed.lock().expect("executed lock"), "tool must not execute on deny");
    assert_eq!(result.permission_call_id.as_deref(), Some(CONFIRM_CALL_ID));
    let echoed: Vec<String> = result.updates.iter().filter_map(message_chunk_text).collect();
    assert!(
        echoed.iter().any(|text| text.contains("skipped")),
        "skipped message must be streamed, got {echoed:?}"
    );
}

/// `session/load` replays the stored conversation in chronological order before
/// the load request completes.
#[tokio::test]
async fn session_load_replays_history_in_chronological_order() {
    let session_service = build_session_service();
    let session_service_probe = session_service.clone();
    let agent: Arc<dyn AdkAgent> = Arc::new(ScriptedAgent::new(build_delete_tool()));
    let client_channel = serve(agent, session_service, |config| config);

    let updates = Arc::new(Mutex::new(Vec::new()));
    let updates_for_client = updates.clone();

    let client = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                updates_for_client.lock().expect("updates lock").push(notification.update);
                Ok(())
            },
            adk_acp::agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(client_channel, move |connection: ConnectionTo<Agent>| {
            let updates = updates.clone();
            let session_service_probe = session_service_probe.clone();
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
                        vec![text_block("inspect"), embedded_text_block("file:///a.md", "# A")],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(prompt.stop_reason, StopReason::EndTurn);

                // Expected replay order derived directly from the stored events.
                let persisted = session_service_probe
                    .get(GetRequest {
                        app_name: AGENT_NAME.to_string(),
                        user_id: "acp-client".to_string(),
                        session_id: session.session_id.to_string(),
                        num_recent_events: None,
                        after: None,
                    })
                    .await
                    .expect("persisted session");
                let expected: Vec<String> = persisted
                    .events()
                    .all()
                    .iter()
                    .flat_map(ResponseStreamer::map_event)
                    .filter_map(|update| message_chunk_text(&update))
                    .collect();
                assert!(
                    expected.iter().any(|text| text == "turn complete"),
                    "stored history must contain the agent's response, got {expected:?}"
                );

                // Drop the active session and clear captured prompt-turn updates,
                // so only replay updates remain.
                connection
                    .send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?;
                updates.lock().expect("updates lock").clear();

                connection
                    .send_request(LoadSessionRequest::new(session.session_id.clone(), cwd.clone()))
                    .block_task()
                    .await?;

                let replayed: Vec<String> = updates
                    .lock()
                    .expect("updates lock")
                    .iter()
                    .filter_map(message_chunk_text)
                    .collect();
                assert_eq!(replayed, expected, "replay must match stored chronological order");
                Ok(())
            }
        });

    tokio::time::timeout(std::time::Duration::from_secs(5), client)
        .await
        .expect("session load completed before timeout")
        .expect("official ACP client completed");
}

// ─────────────────────────────────────────────────────────────────────────────
// ACP v5: adk-acp's client against the same server
// ─────────────────────────────────────────────────────────────────────────────

async fn connect(
    client: AcpAgentConfig,
    configure: impl FnOnce(AcpServerConfigBuilder) -> AcpServerConfigBuilder,
) -> AcpConnection {
    let agent: Arc<dyn AdkAgent> = Arc::new(ScriptedAgent::new(build_delete_tool()));
    let channel = serve(agent, build_session_service(), configure);
    AcpConnection::connect_to(channel, client, Arc::new(PermissionPolicy::DenyAll))
        .await
        .expect("ACP handshake")
}

fn client() -> AcpAgentConfig {
    AcpAgentConfig::new("in-process").working_dir(std::env::current_dir().expect("absolute cwd"))
}

/// The handshake negotiates protocol v1 and reports the agent and the session
/// capabilities the server implements.
#[tokio::test]
async fn v5_handshake_negotiates_the_protocol_version() {
    let connection = connect(client(), |config| config).await;
    let info = connection.agent_info();
    assert_eq!(info.protocol_version, ProtocolVersion::V1);
    assert_eq!(info.agent.as_ref().map(|agent| agent.name.as_str()), Some(AGENT_NAME));
    let session = &info.capabilities.session_capabilities;
    assert!(session.fork.is_some() && session.archive.is_some() && session.status.is_some());
    assert!(
        session.options.is_some() && session.title.is_some() && session.system_prompt.is_some()
    );
    connection.close().await.expect("close");
}

/// A client system prompt reaches the turn's model requests: `append` after the
/// agent's instruction, `override` instead of it.
#[tokio::test]
async fn v5_system_prompts_reach_the_turn() {
    let connection =
        connect(client().append_system_prompt("Answer in French."), |config| config).await;
    let mut session = connection.new_session().await.expect("session");
    let reply = session.prompt("instructions?").await.expect("prompt");
    assert_eq!(reply.text, format!("instructions: {AGENT_INSTRUCTION} | Answer in French."));
    session.close().await.expect("close");

    let connection = connect(client().override_system_prompt("Only this."), |config| config).await;
    let mut session = connection.new_session().await.expect("session");
    let reply = session.prompt("instructions?").await.expect("prompt");
    assert_eq!(reply.text, "instructions: Only this.");
    session.close().await.expect("close");
}

/// `PromptResponse.usage` carries the turn's token counts, and the stream
/// reports the stop reason.
#[tokio::test]
async fn v5_prompt_responses_carry_usage() {
    let connection = connect(client(), |config| config).await;
    let mut session = connection.new_session().await.expect("session");
    let mut chunks = Vec::new();
    let reply = session
        .prompt_blocks(vec![text_block("inspect")], |chunk| chunks.push(chunk))
        .await
        .expect("prompt");
    assert_eq!(reply.stop_reason, StopReason::EndTurn);
    let usage = reply.usage.expect("usage");
    assert_eq!((usage.input_tokens, usage.output_tokens, usage.total_tokens), (42, 8, 50));
    assert!(chunks.iter().any(|chunk| matches!(chunk, OutputChunk::ToolUpdate { .. })));
    session.close().await.expect("close");
}

/// Titles, status, archiving, listing, fork, load and resume.
#[tokio::test]
async fn v5_session_lifecycle() {
    let connection = connect(client(), |config| config).await;
    let mut session = connection.new_session().await.expect("session");
    session.prompt("inspect").await.expect("prompt");
    session.set_title("Inspect the notes").await.expect("title");
    assert_eq!(session.status().await.expect("status"), SessionStatus::Open);

    let mut fork = session.fork().await.expect("fork");
    assert_eq!(fork.status().await.expect("status"), SessionStatus::Open);
    fork.archive().await.expect("archive");
    let listed = connection.list_sessions(None).await.expect("list");
    assert!(listed.iter().any(|info| info.title.as_deref() == Some("Inspect the notes")));
    assert!(
        listed.iter().all(|info| info.session_id != *fork.session_id()),
        "archived fork hidden"
    );
    let archived = connection
        .list_sessions_page(ListSessionsRequest::new().archived(SessionArchiveFilter::Only))
        .await
        .expect("archived list");
    assert_eq!(archived.sessions.len(), 1);
    fork.close().await.expect("close fork");

    let id = session.session_id().clone();
    session.close().await.expect("close");
    assert_eq!(connection.session_status(id.clone()).await.expect("status"), SessionStatus::Saved);

    let mut loaded = connection.load_session(id.clone()).await.expect("load");
    assert!(
        loaded
            .pending_updates()
            .iter()
            .any(|chunk| matches!(chunk, OutputChunk::Text(text) if text == "turn complete"))
    );
    loaded.close().await.expect("close");
    let mut resumed = connection.resume_session(id).await.expect("resume");
    resumed.prompt("inspect").await.expect("prompt after resume");
    resumed.close().await.expect("close");
}

/// Session options before creation, initial config values, modes and config.
#[tokio::test]
async fn v5_modes_and_config_options() {
    let connection =
        connect(client().initial_config_option("verbose", true), |config| config).await;
    let options = connection.session_options(None).await.expect("options");
    assert_eq!(options.modes.expect("modes").available_modes.len(), 2);

    let mut session = connection.new_session().await.expect("session");
    let verbose =
        |options: &[adk_acp::agent_client_protocol::v1::schema::SessionConfigOption]| match &options
            [0]
        .kind
        {
            SessionConfigKind::Boolean(boolean) => boolean.current_value,
            _ => panic!("boolean option"),
        };
    assert!(verbose(&session.config_options().expect("options")), "initial value applied");
    session.set_mode("code").await.expect("mode");
    assert_eq!(session.modes().expect("modes").current_mode_id.to_string(), "code");
    assert!(!verbose(&session.set_config_option("verbose", false).await.expect("config")));
    session.close().await.expect("close");
}

/// With an authenticator, sessions need `authenticate`; `auth/status` and
/// `logout` follow the state.
#[tokio::test]
async fn v5_authentication() {
    let authenticated = |config: AcpServerConfigBuilder| {
        config.authenticator(Arc::new(WorkspaceToken::new("secret")))
    };
    let connection = connect(client(), authenticated).await;
    assert!(connection.new_session().await.is_err(), "auth_required before authenticating");
    assert_eq!(connection.auth_status().await.expect("status").state, AuthState::Unauthenticated);
    connection.authenticate(TOKEN_METHOD).await.expect("authenticate");
    assert_eq!(connection.auth_status().await.expect("status").state, AuthState::Authenticated);
    let mut session = connection.new_session().await.expect("session");
    session.close().await.expect("close");
    connection.logout().await.expect("logout");
    assert!(connection.new_session().await.is_err(), "logout ends the authenticated state");

    let connection = connect(client().auth_method(TOKEN_METHOD), authenticated).await;
    let mut session = connection.new_session().await.expect("session after handshake auth");
    session.close().await.expect("close");

    // A method the agent does not offer fails the handshake, and is refused
    // locally on a live connection.
    let agent: Arc<dyn AdkAgent> = Arc::new(ScriptedAgent::new(build_delete_tool()));
    let channel = serve(agent, build_session_service(), authenticated);
    let refused = AcpConnection::connect_to(
        channel,
        client().auth_method("unknown"),
        Arc::new(PermissionPolicy::DenyAll),
    )
    .await;
    assert!(refused.is_err(), "an unoffered auth method fails the handshake");
    assert!(matches!(connection.authenticate("unknown").await, Err(AcpError::Unsupported(_))));
}
