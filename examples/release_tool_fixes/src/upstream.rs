//! A scripted OpenAI-compatible Chat Completions endpoint that records every request.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_model::openai::OpenAIClient;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::Response;
use serde_json::{Value, json};

/// What the endpoint answers to the next request.
pub enum Reply {
    /// The model calls `name` with `args`.
    ToolCall { name: &'static str, args: Value },
    /// The model answers with text.
    Text(&'static str),
    /// An HTTP error with this status and JSON body.
    Error(u16, Value),
}

#[derive(Default)]
struct Shared {
    replies: Mutex<VecDeque<Reply>>,
    captured: Mutex<Vec<Value>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A local server answering each request with the next scripted reply.
pub struct Upstream {
    pub base: String,
    shared: Arc<Shared>,
}

impl Upstream {
    pub async fn start(replies: Vec<Reply>) -> anyhow::Result<Self> {
        let shared =
            Arc::new(Shared { replies: Mutex::new(replies.into()), ..Shared::default() });
        let app = Router::new().fallback(scripted).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base, shared })
    }

    /// The real OpenAI-compatible client pointed at this endpoint.
    pub fn client(&self) -> anyhow::Result<OpenAIClient> {
        Ok(OpenAIClient::compatible("test-key", &self.base, "scripted-model")?)
    }

    /// Every request body received so far.
    pub fn captured(&self) -> Vec<Value> {
        lock(&self.shared.captured).clone()
    }
}

fn chunk(delta: Value, finish_reason: Option<&str>) -> String {
    let body = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
        "model": "scripted-model",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    });
    format!("data: {body}\n\n")
}

async fn scripted(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let stream = request["stream"].as_bool().unwrap_or(false);
    lock(&shared.captured).push(request);
    let reply = lock(&shared.replies).pop_front().unwrap_or(Reply::Text("done"));
    let (message, finish) = match reply {
        Reply::Error(status, body) => {
            return Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("valid response");
        }
        Reply::ToolCall { name, args } => (
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": {"name": name, "arguments": args.to_string()}
            }]}),
            "tool_calls",
        ),
        Reply::Text(text) => (json!({"role": "assistant", "content": text}), "stop"),
    };
    let usage = json!({"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7});
    let (content_type, body) = if stream {
        let mut events = chunk(message, None);
        events.push_str(&chunk(json!({}), Some(finish)));
        events.push_str("data: [DONE]\n\n");
        ("text/event-stream", events)
    } else {
        let body = json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 1,
            "model": "scripted-model",
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": usage,
        });
        ("application/json", body.to_string())
    };
    Response::builder()
        .status(200)
        .header("content-type", content_type)
        .body(Body::from(body))
        .expect("valid response")
}

/// The content of every `tool` message in a recorded request, joined.
pub fn tool_messages(request: &Value) -> String {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "tool")
        .map(|message| match &message["content"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
