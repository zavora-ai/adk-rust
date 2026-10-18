//! Local scripted HTTP upstreams: a server that never answers, a counting token endpoint,
//! and an OpenAI-compatible Chat Completions server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// Accepts connections and reads requests but never writes a byte back.
pub async fn hung_server() -> anyhow::Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    Ok(base_url)
}

/// An RFC 8693 token endpoint that answers after 300 ms and counts its requests.
pub async fn token_endpoint() -> anyhow::Result<(String, Arc<AtomicUsize>)> {
    let count = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/token",
            post(|State(count): State<Arc<AtomicUsize>>| async move {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(300)).await;
                axum::Json(json!({
                    "access_token": "exchanged-token",
                    "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
                    "token_type": "Bearer",
                    "expires_in": 3600
                }))
            }),
        )
        .with_state(count.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((base_url, count))
}

/// A scripted OpenAI-compatible server: the first request is answered with one
/// `reserve_seats` call whose arguments are sent verbatim, every later one with text.
pub struct ScriptedOpenAi {
    pub base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl ScriptedOpenAi {
    pub async fn start(arguments: &'static str) -> anyhow::Result<Self> {
        let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
        let state = (requests.clone(), arguments);
        let app = Router::new().route("/v1/chat/completions", post(complete)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base_url, requests })
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().map(|requests| requests.clone()).unwrap_or_default()
    }
}

/// The content of the `tool` message answering `call_id`, if `request` carries one.
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

async fn complete(
    State((requests, arguments)): State<(Arc<Mutex<Vec<Value>>>, &'static str)>,
    body: axum::body::Bytes,
) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let turn = match requests.lock() {
        Ok(mut requests) => {
            requests.push(request.clone());
            requests.len() - 1
        }
        Err(_) => 0,
    };
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-scripted",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "scripted-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let (delta, finish) = if turn == 0 {
        (
            json!({"tool_calls": [{
                "index": 0,
                "id": "call_seats",
                "type": "function",
                "function": {"name": "reserve_seats", "arguments": arguments},
            }]}),
            "tool_calls",
        )
    } else {
        (json!({"content": "Done."}), "stop")
    };
    let usage = json!({"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16});
    if request.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let mut last = chunk(json!({}), json!(finish));
        last["usage"] = usage;
        let mut body = String::new();
        for item in
            [chunk(json!({"role": "assistant"}), Value::Null), chunk(delta, Value::Null), last]
        {
            body.push_str(&format!("data: {item}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
    } else {
        let mut message = delta;
        message["role"] = json!("assistant");
        axum::Json(json!({
            "id": "chatcmpl-scripted",
            "object": "chat.completion",
            "created": 0,
            "model": "scripted-model",
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": usage,
        }))
        .into_response()
    }
}
