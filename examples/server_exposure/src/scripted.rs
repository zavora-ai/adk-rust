//! A scripted OpenAI-compatible Chat Completions upstream on 127.0.0.1.
//!
//! The real `OpenAIClient` points at it through `OpenAIClient::compatible`, so every scenario
//! that needs a deterministic model drives the same client code path as a live provider. The
//! reply is chosen from the last user message:
//!
//! | Last user message contains | Reply |
//! |----------------------------|-------|
//! | `delete record` | a `delete_record` tool call |
//! | `start export` | a `start_export` tool call |
//! | `think` | `reasoning_content` with [`SECRET_REASONING`], then the answer text |
//! | anything else | `pong` |
//!
//! A request whose last message is a tool result is answered with `Done.`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// The reasoning the scripted model returns for a `think` request. It must never reach an
/// A2A peer.
pub const SECRET_REASONING: &str = "SECRET-REASONING-7f3a: the user cannot see this";

/// The answer the scripted model returns after its reasoning.
pub const THINKING_ANSWER: &str = "The answer is 42.";

enum Reply {
    Text(String),
    Call(&'static str, Value),
    Thinking { reasoning: &'static str, text: &'static str },
}

struct Shared {
    calls: AtomicUsize,
}

/// A running scripted upstream.
pub struct ScriptedModel {
    /// Base URL for `OpenAIClient::compatible` (`http://127.0.0.1:<port>/v1`).
    pub base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedModel {
    pub async fn start() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared { calls: AtomicUsize::new(0) });
        let app = Router::new().route("/v1/chat/completions", post(completions)).with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url: format!("http://{address}/v1"), task })
    }
}

impl Drop for ScriptedModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn decide(body: &Value) -> Reply {
    let messages = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    fn role(message: &Value) -> &str {
        message.get("role").and_then(Value::as_str).unwrap_or("")
    }
    if messages.last().map(role) == Some("tool") {
        return Reply::Text("Done.".to_string());
    }
    let last_user = messages
        .iter()
        .rev()
        .find(|message| role(message) == "user")
        .map(message_text)
        .unwrap_or_default()
        .to_lowercase();
    if last_user.contains("delete record") {
        Reply::Call("delete_record", json!({"record_id": "R-17"}))
    } else if last_user.contains("start export") {
        Reply::Call("start_export", json!({"export_id": "E-9"}))
    } else if last_user.contains("think") {
        Reply::Thinking { reasoning: SECRET_REASONING, text: THINKING_ANSWER }
    } else {
        Reply::Text("pong".to_string())
    }
}

async fn completions(State(shared): State<Arc<Shared>>, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let call_id = format!("call-{}", shared.calls.fetch_add(1, Ordering::SeqCst) + 1);
    let (message, deltas, finish) = match decide(&request) {
        Reply::Text(text) => {
            (json!({"role": "assistant", "content": text}), vec![json!({"content": text})], "stop")
        }
        Reply::Call(name, args) => {
            let call = json!({
                "index": 0,
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": args.to_string()},
            });
            (
                json!({"role": "assistant", "content": null, "tool_calls": [call.clone()]}),
                vec![json!({"tool_calls": [call]})],
                "tool_calls",
            )
        }
        Reply::Thinking { reasoning, text } => (
            json!({"role": "assistant", "reasoning_content": reasoning, "content": text}),
            vec![json!({"reasoning_content": reasoning}), json!({"content": text})],
            "stop",
        ),
    };
    let usage = json!({"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16});
    if !request.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        return axum::Json(json!({
            "id": "chatcmpl-scripted",
            "object": "chat.completion",
            "created": 0,
            "model": "scripted-model",
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": usage,
        }))
        .into_response();
    }
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
    chunks.extend(deltas.into_iter().map(|delta| chunk(delta, Value::Null)));
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage;
    chunks.push(last);
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}
