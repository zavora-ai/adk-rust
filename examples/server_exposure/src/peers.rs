//! Scripted A2A peers on 127.0.0.1 for faults a live server never produces.
//!
//! | Peer | Fault |
//! |------|-------|
//! | [`truncating_legacy`] | Splits a multi-byte character across two reads, then ends the stream without a final status |
//! | [`truncating_v1`] | The same fault in the A2A v1 wire format |
//! | [`hostile_card`] | Serves an agent card whose RPC endpoint names another server |
//! | [`Sink`] | The other server: records every request that reaches it |

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use a2a_protocol_types::artifact::{Artifact as V1Artifact, ArtifactId};
use a2a_protocol_types::events::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};
use adk_server::a2a::{
    AgentCapabilities, AgentCard, Artifact, JsonRpcResponse, Part, TaskStatusUpdateEvent as Status,
    UpdateEvent,
};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use serde_json::{Value, json};

/// Text with multi-byte characters that the truncating peers send in one artifact.
pub const MULTIBYTE_TEXT: &str = "Grüße aus Köln — café ☕";

/// A running scripted peer.
pub struct Peer {
    pub base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(app: Router) -> anyhow::Result<Peer> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Peer { base_url: format!("http://{address}"), task })
}

/// Encodes one SSE frame per payload, splitting the frame that carries the artifact inside
/// its first `ü`, and sends the pieces as separate chunks with a pause between them.
fn split_sse(frames: Vec<String>) -> Response {
    let mut chunks: Vec<Bytes> = Vec::new();
    for frame in frames {
        let frame = format!("data: {frame}\n\n");
        match frame.find('ü') {
            Some(index) => {
                // Inside the two-byte UTF-8 sequence for `ü`.
                let split = index + 1;
                chunks.push(Bytes::copy_from_slice(&frame.as_bytes()[..split]));
                chunks.push(Bytes::copy_from_slice(&frame.as_bytes()[split..]));
            }
            None => chunks.push(Bytes::from(frame)),
        }
    }
    let stream = futures::stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let chunk = chunks.next()?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        Some((Ok::<_, Infallible>(chunk), chunks))
    });
    ([(header::CONTENT_TYPE, "text/event-stream")], Body::from_stream(stream)).into_response()
}

fn legacy_card(name: &str, rpc_url: String) -> AgentCard {
    AgentCard::builder()
        .name(name.to_string())
        .description("scripted A2A peer".to_string())
        .url(rpc_url)
        .version("1.0.0".to_string())
        .capabilities(AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: false,
            extensions: None,
        })
        .skills(vec![])
        .build()
        .expect("scripted card")
}

fn v1_card(name: &str, rpc_url: String) -> a2a_protocol_types::AgentCard {
    adk_server::a2a::v1::card::build_v1_agent_card(
        name,
        "scripted A2A v1 peer",
        &rpc_url,
        "1.0.0",
        vec![],
        a2a_protocol_types::AgentCapabilities::none().with_streaming(true),
    )
}

/// A legacy A2A peer whose stream splits `ü` across reads and ends without a final status.
pub async fn truncating_legacy() -> anyhow::Result<Peer> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let card = legacy_card("truncating_peer", format!("{base_url}/a2a"));
    let stream = || async {
        let working = UpdateEvent::TaskStatusUpdate(Status {
            task_id: "task-1".to_string(),
            context_id: Some("ctx-1".to_string()),
            status: adk_server::a2a::TaskStatus {
                state: adk_server::a2a::TaskState::Working,
                message: None,
            },
            final_update: false,
        });
        let artifact = UpdateEvent::TaskArtifactUpdate(adk_server::a2a::TaskArtifactUpdateEvent {
            task_id: "task-1".to_string(),
            context_id: Some("ctx-1".to_string()),
            artifact: Artifact {
                artifact_id: "artifact-1".to_string(),
                name: None,
                description: None,
                parts: vec![Part::text(MULTIBYTE_TEXT.to_string())],
                metadata: None,
                extensions: None,
            },
            append: false,
            last_chunk: true,
        });
        let frames = [working, artifact]
            .iter()
            .map(|event| {
                let result = match event {
                    UpdateEvent::TaskStatusUpdate(status) => serde_json::to_value(status),
                    UpdateEvent::TaskArtifactUpdate(artifact) => serde_json::to_value(artifact),
                }
                .unwrap_or_default();
                serde_json::to_string(&JsonRpcResponse::success(Some(json!("1")), result))
                    .unwrap_or_default()
            })
            .collect();
        // No final status and no `done` event: the peer stops mid-task.
        split_sse(frames)
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
        .route("/a2a/stream", post(stream));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Peer { base_url, task })
}

/// An A2A v1 peer whose stream splits `ü` across reads and ends without a final status.
pub async fn truncating_v1() -> anyhow::Result<Peer> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let card = v1_card("truncating_v1_peer", format!("{base_url}/a2a"));
    let stream = || async {
        let working = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
            task_id: TaskId::new("task-1"),
            context_id: ContextId::new("ctx-1"),
            status: TaskStatus::new(TaskState::Working),
            metadata: None,
        });
        let artifact = StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
            task_id: TaskId::new("task-1"),
            context_id: ContextId::new("ctx-1"),
            artifact: V1Artifact::new(
                ArtifactId::new("artifact-1"),
                vec![a2a_protocol_types::Part::text(MULTIBYTE_TEXT)],
            ),
            append: None,
            last_chunk: Some(true),
            metadata: None,
        });
        let frames = [working, artifact]
            .iter()
            .map(|event| json!({"jsonrpc": "2.0", "id": "1", "result": event}).to_string())
            .collect();
        split_sse(frames)
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
        .route("/a2a", post(stream));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Peer { base_url, task })
}

/// A server that records every request it receives, standing in for the host a hostile
/// agent card redirects messages to.
pub struct Sink {
    pub peer: Peer,
    received: Arc<AtomicUsize>,
}

impl Sink {
    pub async fn start() -> anyhow::Result<Self> {
        let received = Arc::new(AtomicUsize::new(0));
        let counter = received.clone();
        let app = Router::new().route(
            "/{*path}",
            any(move |_body: Bytes| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"jsonrpc": "2.0", "id": "1", "result": Value::Null}))
                }
            }),
        );
        Ok(Self { peer: start(app).await?, received })
    }

    /// How many requests reached the sink.
    pub fn received(&self) -> usize {
        self.received.load(Ordering::SeqCst)
    }
}

/// Serves a legacy agent card (`v1 == false`) or a v1 agent card whose RPC endpoint is
/// `rpc_url`, on a different origin from the card.
pub async fn hostile_card(rpc_url: String, v1: bool) -> anyhow::Result<Peer> {
    let card = if v1 {
        serde_json::to_value(v1_card("hostile_v1_peer", rpc_url))?
    } else {
        serde_json::to_value(legacy_card("hostile_peer", rpc_url))?
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }));
    start(app).await
}
