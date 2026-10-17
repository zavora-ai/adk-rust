//! A scripted OpenAI-compatible Chat Completions server on 127.0.0.1.
//!
//! The real `OpenAIClient` is pointed at it through its base URL. Each request body is recorded,
//! and a script decides the reply from the request, so a scenario can play a reviewer that asks
//! for changes once, a model that calls a tool twice, or a provider that answers `429`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// What the scripted model answers to one request.
pub enum Reply {
    Text(String),
    /// One tool call: id, name, and JSON arguments as written.
    Call(String, String, String),
    /// An HTTP error status with an OpenAI-style error body.
    Status(u16, String),
}

/// Decides the reply from the request body and the zero-based request number.
pub type Script = Box<dyn Fn(&Value, usize) -> Reply + Send + Sync>;

struct Shared {
    script: Script,
    requests: Mutex<Vec<Value>>,
}

/// A running scripted server.
pub struct ScriptedServer {
    pub base_url: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    pub async fn start(script: Script) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared { script, requests: Mutex::new(Vec::new()) });
        let app =
            Router::new().route("/v1/chat/completions", post(complete)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url, shared, task })
    }

    /// How many requests reached the server.
    pub fn request_count(&self) -> usize {
        self.shared.requests.lock().unwrap().len()
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn complete(State(shared): State<Arc<Shared>>, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let turn = {
        let mut requests = shared.requests.lock().unwrap();
        requests.push(request.clone());
        requests.len() - 1
    };
    let reply = (shared.script)(&request, turn);
    if let Reply::Status(status, message) = &reply {
        let status = StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = json!({"error": {"message": message, "type": "rate_limit_error", "code": null}});
        return (status, axum::Json(body)).into_response();
    }
    if request.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let mut body = String::new();
        for chunk in stream_chunks(&reply) {
            body.push_str(&format!("data: {chunk}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
    } else {
        axum::Json(unary(&reply)).into_response()
    }
}

fn usage() -> Value {
    json!({"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16})
}

fn message(reply: &Reply) -> (Value, &'static str) {
    match reply {
        Reply::Text(text) => (json!({"role": "assistant", "content": text}), "stop"),
        Reply::Call(id, name, arguments) => (
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                }],
            }),
            "tool_calls",
        ),
        Reply::Status(..) => (Value::Null, "stop"),
    }
}

fn unary(reply: &Reply) -> Value {
    let (message, finish) = message(reply);
    json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion",
        "created": 0,
        "model": "scripted-model",
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": usage(),
    })
}

fn stream_chunks(reply: &Reply) -> Vec<Value> {
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-scripted",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "scripted-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let mut chunks = vec![chunk(json!({"role": "assistant"}), Value::Null)];
    let finish = match reply {
        Reply::Text(text) => {
            chunks.push(chunk(json!({"content": text}), Value::Null));
            "stop"
        }
        Reply::Call(id, name, arguments) => {
            chunks.push(chunk(
                json!({"tool_calls": [{
                    "index": 0,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                }]}),
                Value::Null,
            ));
            "tool_calls"
        }
        Reply::Status(..) => "stop",
    };
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage();
    chunks.push(last);
    chunks
}

/// The text of the last user message in `request`.
pub fn last_user_text(request: &Value) -> String {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|message| match message.get("content") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(other) => Some(other.to_string()),
            None => None,
        })
        .unwrap_or_default()
}

/// The content of the tool message that answers `call_id` in `request`, if any.
pub fn tool_message(request: &Value, call_id: &str) -> Option<String> {
    request.get("messages")?.as_array()?.iter().find_map(|message| {
        (message.get("role")?.as_str()? == "tool"
            && message.get("tool_call_id")?.as_str()? == call_id)
            .then(|| match message.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            })
    })
}
