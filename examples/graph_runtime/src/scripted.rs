//! A scripted OpenAI-compatible Chat Completions server on 127.0.0.1.
//!
//! The real `OpenAIClient` is pointed at it through its base URL. Each request body is recorded
//! with the instant it arrived, and a script decides the reply from the request, so a scenario
//! can play a model that calls a tool, a slow model, or a provider that answers `429` or `503`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// The inner reply, sent after a delay.
    Slow(Duration, Box<Reply>),
}

/// Decides the reply from the request body and the zero-based request number.
pub type Script = Box<dyn Fn(&Value, usize) -> Reply + Send + Sync>;

struct Shared {
    script: Script,
    requests: Mutex<Vec<(Instant, Value)>>,
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

    /// The bodies of every request that reached the server, in arrival order.
    pub fn requests(&self) -> Vec<Value> {
        self.shared.requests.lock().unwrap().iter().map(|(_, body)| body.clone()).collect()
    }

    /// When each request arrived, in arrival order.
    pub fn arrivals(&self) -> Vec<Instant> {
        self.shared.requests.lock().unwrap().iter().map(|(at, _)| *at).collect()
    }

    /// How many requests carried `tag` in their system instruction.
    pub fn count_tagged(&self, tag: &str) -> usize {
        self.requests().iter().filter(|request| system_text(request).contains(tag)).count()
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
        requests.push((Instant::now(), request.clone()));
        requests.len() - 1
    };
    let mut reply = (shared.script)(&request, turn);
    while let Reply::Slow(delay, inner) = reply {
        tokio::time::sleep(delay).await;
        reply = *inner;
    }
    if let Reply::Status(status, message) = &reply {
        let status = StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let kind = if status == StatusCode::TOO_MANY_REQUESTS {
            "rate_limit_error"
        } else {
            "server_error"
        };
        let body = json!({"error": {"message": message, "type": kind, "code": null}});
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
        Reply::Status(..) | Reply::Slow(..) => (Value::Null, "stop"),
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
        Reply::Status(..) | Reply::Slow(..) => "stop",
    };
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage();
    chunks.push(last);
    chunks
}

fn content_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

/// The system (or developer) instruction of `request`.
pub fn system_text(request: &Value) -> String {
    request
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|m| {
                    matches!(m.get("role").and_then(Value::as_str), Some("system" | "developer"))
                })
                .map(content_text)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The text of the last user message in `request`.
pub fn last_user_text(request: &Value) -> String {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        })
        .map(content_text)
        .unwrap_or_default()
}

/// Whether `request` already carries a tool result, so the model has called its tool.
pub fn has_tool_result(request: &Value) -> bool {
    request.get("messages").and_then(Value::as_array).is_some_and(|messages| {
        messages.iter().any(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
    })
}
