//! A scripted local upstream and a capturing proxy in front of a live provider.
//!
//! Both record every request body, so a scenario can assert on what a client actually sent.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use futures::StreamExt;
use serde_json::Value;

/// One request a client sent.
#[derive(Debug, Clone)]
pub struct Captured {
    pub path: String,
    pub body: Value,
}

/// What the scripted upstream answers to the next request.
pub enum Reply {
    /// A server-sent event stream; each item is one complete event, including its blank line.
    Sse(Vec<String>),
    /// Newline-delimited JSON, as Ollama streams.
    Ndjson(Vec<Value>),
    /// Sends `first`, then keeps the connection open without sending anything else.
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

fn record(shared: &Shared, uri: &Uri, body: &Bytes) {
    let body = serde_json::from_slice(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
    lock(&shared.captured).push(Captured { path: uri.path().to_string(), body });
}

/// A local HTTP server that answers each request with the next scripted reply.
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

async fn scripted(State(shared): State<Arc<Shared>>, uri: Uri, body: Bytes) -> Response {
    record(&shared, &uri, &body);
    let Some(reply) = lock(&shared.replies).pop_front() else {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("{\"error\":{\"message\":\"no scripted reply left\"}}"))
            .expect("static response");
    };
    let (content_type, body) = match reply {
        Reply::Sse(events) => ("text/event-stream", Body::from(events.concat())),
        Reply::Ndjson(lines) => {
            let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
            ("application/x-ndjson", Body::from(text))
        }
        Reply::Stall { content_type, first } => {
            let stream =
                futures::stream::once(async move { Ok::<_, Infallible>(Bytes::from(first)) })
                    .chain(futures::stream::pending());
            (content_type, Body::from_stream(stream))
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .body(body)
        .expect("valid response")
}

/// A local proxy that records each request and forwards it to a live provider.
pub struct Proxy {
    pub base: String,
    shared: Arc<Shared>,
}

struct ProxyState {
    shared: Arc<Shared>,
    upstream: String,
    http: reqwest::Client,
}

impl Proxy {
    /// Starts a proxy forwarding every path to `upstream` (scheme and host, no path).
    pub async fn start(upstream: &str) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::default());
        let state = Arc::new(ProxyState {
            shared: shared.clone(),
            upstream: upstream.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder().build()?,
        });
        let app = Router::new().fallback(forward).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base, shared })
    }

    pub fn captured(&self) -> Vec<Captured> {
        lock(&self.shared.captured).clone()
    }
}

async fn forward(
    State(state): State<Arc<ProxyState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(&state.shared, &uri, &body);
    let path = uri.path_and_query().map(|value| value.as_str()).unwrap_or("/");
    let url = format!("{}{path}", state.upstream);
    let mut request = state.http.request(method, url).body(body);
    for (name, value) in &headers {
        // Identity encoding keeps the forwarded body readable by the client as sent.
        if matches!(name.as_str(), "host" | "content-length" | "connection" | "accept-encoding") {
            continue;
        }
        request = request.header(name, value);
    }
    match request.send().await {
        Ok(upstream) => {
            let mut response = Response::builder().status(upstream.status().as_u16());
            for (name, value) in upstream.headers() {
                if matches!(name.as_str(), "content-length" | "transfer-encoding" | "connection") {
                    continue;
                }
                response = response.header(name, value);
            }
            response
                .body(Body::from_stream(upstream.bytes_stream()))
                .expect("valid proxied response")
        }
        Err(error) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("{{\"error\":{{\"message\":\"proxy: {error}\"}}}}")))
            .expect("static response"),
    }
}

/// OpenAI Chat Completions stream events.
pub mod chat {
    use serde_json::{Value, json};

    fn event(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    /// A streamed assistant turn: optional text, then tool calls `(id, name, arguments)`.
    pub fn turn(text: Option<&str>, calls: &[(&str, &str, Value)]) -> Vec<String> {
        let mut events = Vec::new();
        events.push(event(json!({
            "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
        })));
        if let Some(text) = text {
            events.push(event(json!({
                "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
                "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
            })));
        }
        for (index, (id, name, arguments)) in calls.iter().enumerate() {
            events.push(event(json!({
                "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": index, "id": id, "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}
                }]}, "finish_reason": null}]
            })));
        }
        let finish = if calls.is_empty() { "stop" } else { "tool_calls" };
        events.push(event(json!({
            "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
            "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        })));
        events.push("data: [DONE]\n\n".to_string());
        events
    }

    /// The first chunk of a stream that then stalls.
    pub fn first_chunk() -> String {
        event(json!({
            "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Thinking"}, "finish_reason": null}]
        }))
    }
}

/// Anthropic Messages stream events.
pub mod anthropic {
    use serde_json::{Value, json};

    fn event(name: &str, value: Value) -> String {
        format!("event: {name}\ndata: {value}\n\n")
    }

    /// A streamed assistant turn whose content blocks are `blocks` (text blocks only).
    pub fn turn(texts: &[&str]) -> Vec<String> {
        let mut events = vec![event(
            "message_start",
            json!({"type": "message_start", "message": {
                "id": "msg_scripted", "type": "message", "role": "assistant", "model": "claude-sonnet-4-6",
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 10, "output_tokens": 1}
            }}),
        )];
        for (index, text) in texts.iter().enumerate() {
            events.push(event(
                "content_block_start",
                json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}),
            ));
            if !text.is_empty() {
                events.push(event(
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
                ));
            }
            events.push(event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
        }
        events.push(event(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                   "usage": {"output_tokens": 2}}),
        ));
        events.push(event("message_stop", json!({"type": "message_stop"})));
        events
    }
}

/// Ollama `/api/chat` stream lines.
pub mod ollama {
    use serde_json::{Value, json};

    /// A streamed assistant turn: text, then tool calls `(name, arguments)`.
    pub fn turn(text: &str, calls: &[(&str, Value)]) -> Vec<Value> {
        let tool_calls: Vec<Value> = calls
            .iter()
            .map(|(name, arguments)| json!({"function": {"name": name, "arguments": arguments}}))
            .collect();
        vec![
            json!({"model": "scripted", "created_at": "2026-10-18T00:00:00Z",
                   "message": {"role": "assistant", "content": text, "tool_calls": tool_calls}, "done": false}),
            json!({"model": "scripted", "created_at": "2026-10-18T00:00:00Z",
                   "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop",
                   "prompt_eval_count": 10, "eval_count": 5}),
        ]
    }
}
