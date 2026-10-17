//! A scripted OpenAI-compatible Chat Completions server on 127.0.0.1.
//!
//! The real `OpenAIClient` is pointed at it through its base URL. Each request body is recorded
//! verbatim, and a script decides the reply from the request, so a scenario can read exactly what
//! the agent and the compaction summarizer sent to the model.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// What the scripted model answers to one request.
pub enum Reply {
    Text(String),
    /// One tool call: call id, tool name, JSON arguments as written.
    Call(String, String, String),
}

/// Decides the reply from the request body.
pub type Script = Box<dyn Fn(&Value) -> Reply + Send + Sync>;

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

    /// Every request body received so far, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.shared.requests.lock().unwrap().clone()
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn complete(State(shared): State<Arc<Shared>>, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    shared.requests.lock().unwrap().push(request.clone());
    let reply = (shared.script)(&request);
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

fn tool_call(id: &str, name: &str, arguments: &str) -> Value {
    json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}})
}

fn unary(reply: &Reply) -> Value {
    let (message, finish) = match reply {
        Reply::Text(text) => (json!({"role": "assistant", "content": text}), "stop"),
        Reply::Call(id, name, arguments) => (
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [tool_call(id, name, arguments)],
            }),
            "tool_calls",
        ),
    };
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
            let mut call = tool_call(id, name, arguments);
            call["index"] = json!(0);
            chunks.push(chunk(json!({"tool_calls": [call]}), Value::Null));
            "tool_calls"
        }
    };
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage();
    chunks.push(last);
    chunks
}

/// The text of a chat message, whether its content is a string or an array of parts.
pub fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The messages of a request.
pub fn messages(request: &Value) -> Vec<Value> {
    request.get("messages").and_then(Value::as_array).cloned().unwrap_or_default()
}
