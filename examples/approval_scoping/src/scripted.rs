//! A scripted OpenAI-compatible Chat Completions server on 127.0.0.1.
//!
//! The real `OpenAIClient` is pointed at it through its base URL. A script decides each reply from
//! the request, so two agents can issue the same call with the same arguments, under the same
//! call ID when a scenario plays a provider that reuses IDs.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// One tool call in a scripted reply; `arguments` is sent exactly as written.
pub struct ScriptedCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// What the scripted model answers to one request.
pub enum Reply {
    Text(String),
    Calls(Vec<ScriptedCall>),
}

/// Decides the reply from the request body and the zero-based request number.
pub type Script = Box<dyn Fn(&Value, usize) -> Reply + Send + Sync>;

struct Shared {
    script: Script,
    requests: Mutex<usize>,
}

/// A running scripted server.
pub struct ScriptedServer {
    pub base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    pub async fn start(script: Script) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared { script, requests: Mutex::new(0) });
        let app =
            Router::new().route("/v1/chat/completions", post(complete)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url, task })
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
        *requests += 1;
        *requests - 1
    };
    let reply = (shared.script)(&request, turn);
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

fn unary(reply: &Reply) -> Value {
    let (message, finish) = match reply {
        Reply::Text(text) => (json!({"role": "assistant", "content": text}), "stop"),
        Reply::Calls(calls) => (
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": calls.iter().map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": call.arguments},
                })).collect::<Vec<_>>(),
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
        Reply::Calls(calls) => {
            for (index, call) in calls.iter().enumerate() {
                chunks.push(chunk(
                    json!({"tool_calls": [{
                        "index": index,
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    }]}),
                    Value::Null,
                ));
            }
            "tool_calls"
        }
    };
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage();
    chunks.push(last);
    chunks
}

pub fn call(id: &str, name: &str, arguments: &str) -> Reply {
    Reply::Calls(vec![ScriptedCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: arguments.to_string(),
    }])
}
