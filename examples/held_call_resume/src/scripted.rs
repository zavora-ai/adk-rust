//! A scripted model upstream on 127.0.0.1 serving two wire formats.
//!
//! - `/v1/chat/completions` — OpenAI-compatible Chat Completions, for the real `OpenAIClient`.
//!   Every tool call carries an ID.
//! - `/api/chat` — Ollama's chat API, for the real `OllamaModel`. Tool calls carry no ID, as
//!   Ollama sends them.
//!
//! Each request is recorded with the agent that sent it (read from its `You are <name>.`
//! instruction), so a scenario counts model calls per agent and asserts on what each request
//! carried. A script decides the reply from that view of the request.

use std::collections::HashMap;
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
    /// Tool calls as `(tool name, arguments)`, in order.
    Calls(Vec<(String, Value)>),
}

/// One request as the script sees it.
pub struct Turn {
    /// The agent that sent it: `You are <name>.` in its instruction, `summarizer` for a
    /// compaction summary, `classifier` for a routing request.
    pub agent: String,
    /// The role of the last message: `user`, `assistant` or `tool`.
    pub last_role: String,
    /// The text of the last user message.
    pub last_user_text: String,
    /// How many requests from this agent, this one included, did not end on a tool result.
    pub fresh: usize,
}

/// Decides the reply for one request.
pub type Script = Box<dyn Fn(&Turn) -> Reply + Send + Sync>;

/// One recorded request.
#[derive(Clone)]
pub struct Recorded {
    pub agent: String,
    pub last_role: String,
    pub body: Value,
}

struct Shared {
    script: Script,
    requests: Mutex<Vec<Recorded>>,
    fresh: Mutex<HashMap<String, usize>>,
    call_ids: Mutex<usize>,
}

/// A running scripted upstream.
pub struct ScriptedServer {
    /// Base URL for `OpenAIClient::compatible` (`http://127.0.0.1:<port>/v1`).
    pub openai_base: String,
    /// Host for `OllamaConfig::with_host` (`http://127.0.0.1:<port>`).
    pub ollama_host: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    pub async fn start(script: Script) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            script,
            requests: Mutex::new(Vec::new()),
            fresh: Mutex::new(HashMap::new()),
            call_ids: Mutex::new(0),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(openai))
            .route("/api/chat", post(ollama))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            openai_base: format!("http://{address}/v1"),
            ollama_host: format!("http://{address}"),
            shared,
            task,
        })
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// How many requests `agent` sent.
    pub fn calls(&self, agent: &str) -> usize {
        self.requests().iter().filter(|request| request.agent == agent).count()
    }
}

impl Drop for ScriptedServer {
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

/// Records the request and asks the script for its reply.
fn decide(shared: &Shared, body: &Value) -> Reply {
    let messages = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let role =
        |message: &Value| message.get("role").and_then(Value::as_str).unwrap_or("").to_string();
    let last_user_text = messages
        .iter()
        .rev()
        .find(|message| role(message) == "user")
        .map(message_text)
        .unwrap_or_default();
    let instruction = messages
        .iter()
        .filter(|message| role(message) == "system")
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n");
    let agent = if let Some(rest) = instruction.split("You are ").nth(1) {
        rest.split('.').next().unwrap_or_default().trim().to_string()
    } else if last_user_text.contains("conversation history between a user") {
        "summarizer".to_string()
    } else if last_user_text.contains("Classify") {
        "classifier".to_string()
    } else {
        "unknown".to_string()
    };
    let last_role = messages.last().map(role).unwrap_or_default();
    let fresh = {
        let mut counts = shared.fresh.lock().unwrap();
        let count = counts.entry(agent.clone()).or_default();
        if last_role != "tool" {
            *count += 1;
        }
        *count
    };
    shared.requests.lock().unwrap().push(Recorded {
        agent: agent.clone(),
        last_role: last_role.clone(),
        body: body.clone(),
    });
    (shared.script)(&Turn { agent, last_role, last_user_text, fresh })
}

fn next_call_id(shared: &Shared) -> String {
    let mut next = shared.call_ids.lock().unwrap();
    *next += 1;
    format!("call-{next}")
}

async fn openai(State(shared): State<Arc<Shared>>, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let reply = decide(&shared, &request);
    let tool_calls = |calls: &[(String, Value)]| {
        calls
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
            .collect::<Vec<_>>()
    };
    let (message, delta, finish) = match &reply {
        Reply::Text(text) => {
            (json!({"role": "assistant", "content": text}), json!({"content": text}), "stop")
        }
        Reply::Calls(calls) => {
            let calls = tool_calls(calls);
            (
                json!({"role": "assistant", "content": null, "tool_calls": calls}),
                json!({"tool_calls": calls}),
                "tool_calls",
            )
        }
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
    let mut last = chunk(json!({}), json!(finish));
    last["usage"] = usage;
    let mut body = String::new();
    for chunk in [chunk(json!({"role": "assistant"}), Value::Null), chunk(delta, Value::Null), last]
    {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

async fn ollama(State(shared): State<Arc<Shared>>, body: axum::body::Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let reply = decide(&shared, &request);
    let message = match &reply {
        Reply::Text(text) => json!({"role": "assistant", "content": text}),
        Reply::Calls(calls) => json!({
            "role": "assistant",
            "content": "",
            "tool_calls": calls
                .iter()
                .map(|(name, args)| json!({"function": {"name": name, "arguments": args}}))
                .collect::<Vec<_>>(),
        }),
    };
    let line = |message: Value, done: bool| {
        let mut line = json!({
            "model": "scripted-model",
            "created_at": "2026-10-17T00:00:00Z",
            "message": message,
            "done": done,
        });
        if done {
            for (key, value) in [
                ("total_duration", 1),
                ("load_duration", 1),
                ("prompt_eval_count", 12),
                ("prompt_eval_duration", 1),
                ("eval_count", 4),
                ("eval_duration", 1),
            ] {
                line[key] = json!(value);
            }
            line["done_reason"] = json!("stop");
        }
        line
    };
    let done = line(json!({"role": "assistant", "content": ""}), true);
    if !request.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let mut single = done;
        single["message"] = message;
        return axum::Json(single).into_response();
    }
    let body = format!("{}\n{}\n", line(message, false), done);
    ([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}
