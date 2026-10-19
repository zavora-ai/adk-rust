//! A scripted OpenAI-compatible Chat Completions upstream on 127.0.0.1.
//!
//! The real `OpenAIClient` talks to it through `OpenAIClient::compatible`. Every request is
//! recorded with the agent that sent it, read from its `You are <name>.` instruction, so a
//! scenario asserts on exactly what each agent's model received. A script decides each reply
//! from that view of the request, and can answer with an HTTP error or end a stream on an
//! error frame.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// What the scripted model answers to one request.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    /// Text deltas, streamed in order.
    pub text: Vec<String>,
    /// Tool calls as `(tool name, arguments)`, in order.
    pub calls: Vec<(String, Value)>,
    /// Answers with this HTTP status and an error body instead of a completion.
    pub status: Option<u16>,
    /// Ends the stream with an `{"error": ...}` frame after the text deltas.
    pub error_frame: bool,
}

impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: vec![text.into()], ..Self::default() }
    }

    pub fn call(name: &str, args: Value) -> Self {
        Self { calls: vec![(name.to_string(), args)], ..Self::default() }
    }

    /// Text beside a tool call in one model turn.
    pub fn text_and_call(text: &str, name: &str, args: Value) -> Self {
        Self {
            text: vec![text.to_string()],
            calls: vec![(name.to_string(), args)],
            ..Self::default()
        }
    }

    pub fn http_error(status: u16) -> Self {
        Self { status: Some(status), ..Self::default() }
    }

    /// Streams `text`, then fails the stream with an error frame.
    pub fn broken_stream(text: &str) -> Self {
        Self { text: vec![text.to_string()], error_frame: true, ..Self::default() }
    }

    fn finish_reason(&self) -> &'static str {
        if self.calls.is_empty() { "stop" } else { "tool_calls" }
    }
}

/// One request as the script sees it.
pub struct Turn {
    /// The agent that sent it: `You are <name>.` in its instruction.
    pub agent: String,
    /// The role of the last message: `user`, `assistant` or `tool`.
    pub last_role: String,
    /// Tool results after the last user message.
    pub tool_results: usize,
    /// How many requests from this agent, this one included.
    pub count: usize,
}

/// Decides the reply for one request.
pub type Script = Box<dyn Fn(&Turn) -> Reply + Send + Sync>;

/// One recorded request.
#[derive(Clone)]
pub struct Recorded {
    pub agent: String,
    pub body: Value,
}

impl Recorded {
    pub fn messages(&self) -> Vec<Value> {
        self.body["messages"].as_array().cloned().unwrap_or_default()
    }

    /// The text of every message with `wanted` role, joined.
    pub fn texts(&self, wanted: &str) -> String {
        self.messages()
            .iter()
            .filter(|message| role(message) == wanted)
            .map(message_text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct Shared {
    script: Script,
    requests: Mutex<Vec<Recorded>>,
    counts: Mutex<HashMap<String, usize>>,
    call_ids: Mutex<usize>,
}

/// A running scripted upstream.
pub struct ScriptedServer {
    /// Base URL for `OpenAIClient::compatible` (`http://127.0.0.1:<port>/v1`).
    pub base: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    pub async fn start(script: Script) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            script,
            requests: Mutex::new(Vec::new()),
            counts: Mutex::new(HashMap::new()),
            call_ids: Mutex::new(0),
        });
        let app =
            Router::new().route("/v1/chat/completions", post(chat)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base: format!("http://{address}/v1"), shared, task })
    }

    /// Every request `agent` sent, in order.
    pub fn requests_from(&self, agent: &str) -> Vec<Recorded> {
        self.shared
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.agent == agent)
            .cloned()
            .collect()
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn role(message: &Value) -> String {
    message.get("role").and_then(Value::as_str).unwrap_or("").to_string()
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

/// Records the request and asks the script for its reply.
fn decide(shared: &Shared, body: &Value) -> Reply {
    let messages = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let instruction = messages
        .iter()
        .filter(|message| role(message) == "system")
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n");
    let agent = instruction
        .split("You are ")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .unwrap_or("unknown")
        .trim()
        .to_string();
    let last_role = messages.last().map(role).unwrap_or_default();
    let tool_results = messages
        .iter()
        .rev()
        .take_while(|message| role(message) != "user")
        .filter(|message| role(message) == "tool")
        .count();
    let count = {
        let mut counts = shared.counts.lock().unwrap();
        let count = counts.entry(agent.clone()).or_default();
        *count += 1;
        *count
    };
    shared.requests.lock().unwrap().push(Recorded { agent: agent.clone(), body: body.clone() });
    (shared.script)(&Turn { agent, last_role, tool_results, count })
}

fn next_call_id(shared: &Shared) -> String {
    let mut next = shared.call_ids.lock().unwrap();
    *next += 1;
    format!("call-{next}")
}

async fn chat(State(shared): State<Arc<Shared>>, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let reply = decide(&shared, &request);
    if let Some(status) = reply.status {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
        return (
            status,
            axum::Json(json!({
                "error": {"message": "the scripted model is unavailable", "type": "invalid_request_error"}
            })),
        )
            .into_response();
    }
    let calls: Vec<Value> = reply
        .calls
        .iter()
        .enumerate()
        .map(|(index, (name, args))| {
            json!({
                "index": index,
                "id": next_call_id(&shared),
                "type": "function",
                "function": {"name": name, "arguments": args.to_string()},
            })
        })
        .collect();
    let finish = reply.finish_reason();
    let usage = json!({"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16});
    if !request.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let mut message = json!({"role": "assistant", "content": reply.text.concat()});
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
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
    chunks.extend(reply.text.iter().map(|delta| chunk(json!({"content": delta}), Value::Null)));
    if reply.error_frame {
        chunks.push(json!({"error": {"message": "upstream overloaded", "type": "server_error"}}));
    } else {
        if !calls.is_empty() {
            chunks.push(chunk(json!({"tool_calls": calls}), Value::Null));
        }
        let mut last = chunk(json!({}), json!(finish));
        last["usage"] = usage;
        chunks.push(last);
    }
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    if !reply.error_frame {
        body.push_str("data: [DONE]\n\n");
    }
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}
