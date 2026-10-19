//! A scripted local upstream that records every request and answers with the next reply.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use futures::StreamExt;
use serde_json::Value;

/// One request a client sent.
#[derive(Debug, Clone)]
pub struct Captured {
    pub body: Value,
}

/// What the scripted upstream answers to the next request.
pub enum Reply {
    /// A JSON body with this status.
    Json(u16, Value),
    /// A server-sent event stream; each item is one complete event, including its blank line.
    Sse(Vec<String>),
    /// Newline-delimited JSON, as Ollama streams.
    Ndjson(Vec<Value>),
    /// Never sends the response headers.
    Hang,
    /// Sends the headers and `first`, then keeps the connection open without sending more.
    Stall { content_type: &'static str, first: String },
}

#[derive(Default)]
struct Shared {
    replies: Mutex<VecDeque<Reply>>,
    captured: Mutex<Vec<Captured>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A local HTTP server answering each request with the next scripted reply.
pub struct Upstream {
    pub base: String,
    shared: Arc<Shared>,
}

impl Upstream {
    pub async fn start() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::default());
        let app = Router::new().fallback(scripted).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base, shared })
    }

    pub fn push(&self, reply: Reply) {
        lock(&self.shared.replies).push_back(reply);
    }

    pub fn captured(&self) -> Vec<Captured> {
        lock(&self.shared.captured).clone()
    }
}

async fn scripted(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    let parsed = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
    lock(&shared.captured).push(Captured { body: parsed });
    let Some(reply) = lock(&shared.replies).pop_front() else {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(r#"{"error":{"message":"no scripted reply left"}}"#))
            .expect("static response");
    };
    let (status, content_type, body) = match reply {
        Reply::Json(status, value) => (status, "application/json", Body::from(value.to_string())),
        Reply::Sse(events) => (200, "text/event-stream", Body::from(events.concat())),
        Reply::Ndjson(lines) => {
            let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
            (200, "application/x-ndjson", Body::from(text))
        }
        Reply::Hang => {
            futures::future::pending::<()>().await;
            unreachable!("a pending future never completes")
        }
        Reply::Stall { content_type, first } => {
            let stream =
                futures::stream::once(async move { Ok::<_, Infallible>(Bytes::from(first)) })
                    .chain(futures::stream::pending());
            (200, content_type, Body::from_stream(stream))
        }
    };
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(body)
        .expect("valid response")
}

/// `data: <json>\n\n`.
pub fn sse(value: &Value) -> String {
    format!("data: {value}\n\n")
}

/// `event: <name>\ndata: <json>\n\n`.
pub fn named_sse(name: &str, value: &Value) -> String {
    format!("event: {name}\ndata: {value}\n\n")
}
