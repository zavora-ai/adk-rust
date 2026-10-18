//! A2A clients decode streams on whole lines and treat a stream that stops early as an error.

use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader,
};
use adk_server::a2a::{
    A2aClient, AgentCapabilities, AgentCard, Artifact, JsonRpcResponse, Message, Role,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent, UpdateEvent,
};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use futures::StreamExt;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

const TEXT: &str = "Grüße aus Köln — café ☕";

async fn serve(build: impl FnOnce(&str) -> Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = build(&base);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// An SSE body sent in pieces, with `ü` split across two of them.
fn split_sse(frames: Vec<String>) -> axum::response::Response {
    let mut chunks = Vec::new();
    for frame in frames {
        let frame = format!("data: {frame}\n\n").into_bytes();
        match frame.windows(2).position(|pair| pair == "ü".as_bytes()) {
            Some(index) => {
                chunks.push(Bytes::copy_from_slice(&frame[..=index]));
                chunks.push(Bytes::copy_from_slice(&frame[index + 1..]));
            }
            None => chunks.push(Bytes::from(frame)),
        }
    }
    let body = futures::stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let chunk = chunks.next()?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        Some((Ok::<_, Infallible>(chunk), chunks))
    });
    ([("content-type", "text/event-stream")], Body::from_stream(body)).into_response()
}

fn legacy_card(base: &str) -> AgentCard {
    AgentCard::builder()
        .name("peer".to_string())
        .description("scripted peer".to_string())
        .url(format!("{base}/a2a"))
        .version("1.0.0".to_string())
        .capabilities(AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: false,
            extensions: None,
        })
        .skills(vec![])
        .build()
        .unwrap()
}

/// A legacy peer that sends a working status and an artifact, then stops.
fn truncating_legacy_peer(base: &str) -> Router {
    let card = legacy_card(base);
    let working = serde_json::to_value(TaskStatusUpdateEvent {
        task_id: "task-1".to_string(),
        context_id: Some("ctx-1".to_string()),
        status: TaskStatus { state: TaskState::Working, message: None },
        final_update: false,
    })
    .unwrap();
    let artifact = serde_json::to_value(TaskArtifactUpdateEvent {
        task_id: "task-1".to_string(),
        context_id: Some("ctx-1".to_string()),
        artifact: Artifact {
            artifact_id: "artifact-1".to_string(),
            name: None,
            description: None,
            parts: vec![adk_server::a2a::Part::text(TEXT.to_string())],
            metadata: None,
            extensions: None,
        },
        append: false,
        last_chunk: true,
    })
    .unwrap();
    let frames: Vec<String> = [working, artifact]
        .into_iter()
        .map(|result| {
            serde_json::to_string(&JsonRpcResponse::success(Some(json!(1)), result)).unwrap()
        })
        .collect();
    Router::new()
        .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
        .route("/a2a/stream", post(move || async move { split_sse(frames) }))
}

fn hello() -> Message {
    Message::builder()
        .role(Role::User)
        .parts(vec![adk_server::a2a::Part::text("hello".to_string())])
        .build()
}

fn artifact_text(items: &[AdkResult<UpdateEvent>]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            Ok(UpdateEvent::TaskArtifactUpdate(update)) => Some(&update.artifact.parts),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            adk_server::a2a::Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_truncated_stream_ends_in_an_error_and_keeps_split_characters() {
    let base = serve(truncating_legacy_peer).await;
    let client = A2aClient::from_url(&base).await.unwrap();
    let items: Vec<_> = client.send_streaming_message(hello()).await.unwrap().collect().await;

    assert_eq!(artifact_text(&items), TEXT);
    let last = items.last().unwrap().as_ref().unwrap_err();
    assert!(last.message.contains("before the task reached a final status"), "{last}");
}

struct Greeter;

#[async_trait]
impl Agent for Greeter {
    fn name(&self) -> &str {
        "greeter"
    }

    fn description(&self) -> &str {
        "greets"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = "greeter".to_string();
        event.llm_response.content = Some(Content::new("model").with_text(TEXT));
        event.llm_response.turn_complete = true;
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

#[tokio::test]
async fn a_complete_stream_from_an_adk_server_has_no_error() {
    let base = serve(|base| {
        let config = ServerConfig::new(
            Arc::new(SingleAgentLoader::new(Arc::new(Greeter))),
            Arc::new(InMemorySessionService::new()),
        );
        ServerBuilder::new(config).with_a2a(base).build()
    })
    .await;
    let client = A2aClient::from_url(&base).await.unwrap();
    let items: Vec<_> = client.send_streaming_message(hello()).await.unwrap().collect().await;

    assert!(
        items.iter().all(Result::is_ok),
        "{:?}",
        items.iter().filter_map(|i| i.as_ref().err()).collect::<Vec<_>>()
    );
    assert_eq!(artifact_text(&items), TEXT);
}

#[cfg(feature = "a2a-v1")]
#[tokio::test]
async fn the_v1_remote_agent_reports_a_truncated_stream_and_keeps_split_characters() {
    use a2a_protocol_types::artifact::{Artifact as V1Artifact, ArtifactId};
    use a2a_protocol_types::events::TaskStatusUpdateEvent as V1Status;
    use a2a_protocol_types::events::{StreamResponse, TaskArtifactUpdateEvent as V1Artifacts};
    use a2a_protocol_types::task::{ContextId, TaskId, TaskState as V1State, TaskStatus as V1Task};
    use adk_server::a2a::client::v1_client::A2aV1Client;
    use adk_server::a2a::remote_agent::v1_remote::{RemoteA2aV1Agent, RemoteA2aV1Config};

    let base = serve(|base| {
        let card = adk_server::a2a::v1::card::build_v1_agent_card(
            "peer",
            "scripted v1 peer",
            &format!("{base}/a2a"),
            "1.0.0",
            vec![],
            a2a_protocol_types::AgentCapabilities::none().with_streaming(true),
        );
        let working = StreamResponse::StatusUpdate(V1Status {
            task_id: TaskId::new("t1"),
            context_id: ContextId::new("c1"),
            status: V1Task::new(V1State::Working),
            metadata: None,
        });
        let artifact = StreamResponse::ArtifactUpdate(V1Artifacts {
            task_id: TaskId::new("t1"),
            context_id: ContextId::new("c1"),
            artifact: V1Artifact::new(
                ArtifactId::new("a1"),
                vec![a2a_protocol_types::Part::text(TEXT)],
            ),
            append: None,
            last_chunk: Some(true),
            metadata: None,
        });
        let frames: Vec<String> = [working, artifact]
            .iter()
            .map(|event| json!({"jsonrpc": "2.0", "id": 1, "result": event}).to_string())
            .collect();
        Router::new()
            .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
            .route("/a2a", post(move || async move { split_sse(frames) }))
    })
    .await;

    let card = A2aV1Client::resolve_agent_card(&base).await.unwrap();
    let agent = RemoteA2aV1Agent::new(RemoteA2aV1Config {
        name: "peer".to_string(),
        description: "scripted v1 peer".to_string(),
        agent_card: card,
        streaming: Some(true),
        credential_broker: None,
        credential_scopes: vec![],
    });
    let sessions = Arc::new(InMemorySessionService::new());
    adk_session::SessionService::create(
        sessions.as_ref(),
        adk_session::CreateRequest {
            app_name: "client".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: Default::default(),
        },
    )
    .await
    .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("client")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()
        .unwrap();
    let events: Vec<Event> = runner
        .run_str("user", "session", Content::new("user").with_text("hello"))
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    let text: String = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(adk_core::Part::text)
        .collect();
    assert_eq!(text, TEXT);
    let error = events.iter().find_map(|event| event.llm_response.error_message.clone());
    assert!(error.is_some_and(|error| error.contains("before the task reached a final status")));
}
