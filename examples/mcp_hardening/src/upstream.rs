//! Local scripted upstreams: a Gemini endpoint for the agent and a Streamable HTTP MCP server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_model::GeminiModel;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

async fn serve(router: Router) -> anyhow::Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{address}"))
}

#[derive(Default)]
struct GeminiState {
    /// The function names declared in each `generateContent` request, in order.
    declared: Mutex<Vec<Vec<String>>>,
    /// The function the model calls on its first turn, when the request declares it.
    call: Option<(String, Value)>,
}

/// A Gemini `generateContent` endpoint that calls one scripted function, then answers
/// with the function's response as text.
#[derive(Clone)]
pub struct ScriptedGemini {
    base_url: String,
    state: Arc<GeminiState>,
}

impl ScriptedGemini {
    pub async fn start(call: Option<(&str, Value)>) -> anyhow::Result<Self> {
        let state = Arc::new(GeminiState {
            declared: Mutex::default(),
            call: call.map(|(name, args)| (name.to_string(), args)),
        });
        let router = Router::new().fallback(gemini).with_state(Arc::clone(&state));
        let base_url = format!("{}/v1beta/", serve(router).await?);
        Ok(Self { base_url, state })
    }

    /// The real Gemini client, pointed at this endpoint.
    pub fn model(&self) -> anyhow::Result<GeminiModel> {
        Ok(GeminiModel::new_with_base_url("scripted", "gemini-3.7-flash", &self.base_url)?)
    }

    pub fn requests(&self) -> usize {
        self.state.declared.lock().map(|declared| declared.len()).unwrap_or(0)
    }

    /// Function names declared in the first request.
    pub fn first_declared(&self) -> Vec<String> {
        self.state
            .declared
            .lock()
            .ok()
            .and_then(|declared| declared.first().cloned())
            .unwrap_or_default()
    }
}

fn declared_names(body: &Value) -> Vec<String> {
    let mut names = Vec::new();
    for tool in body["tools"].as_array().into_iter().flatten() {
        let declarations =
            tool.get("functionDeclarations").or_else(|| tool.get("function_declarations"));
        for declaration in declarations.and_then(Value::as_array).into_iter().flatten() {
            if let Some(name) = declaration["name"].as_str() {
                names.push(name.to_string());
            }
        }
    }
    names
}

async fn gemini(State(state): State<Arc<GeminiState>>, uri: Uri, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let names = declared_names(&body);
    if let Ok(mut declared) = state.declared.lock() {
        declared.push(names.clone());
    }
    let last_parts = body["contents"]
        .as_array()
        .and_then(|contents| contents.last())
        .map(|content| content["parts"].clone())
        .unwrap_or(Value::Null);
    let function_response = last_parts.as_array().into_iter().flatten().find_map(|part| {
        part.get("functionResponse").or_else(|| part.get("function_response")).cloned()
    });
    let part = match (function_response, &state.call) {
        (Some(response), _) => json!({ "text": format!("Tool result: {}", response["response"]) }),
        (None, Some((name, args))) if names.contains(name) => {
            json!({ "functionCall": { "name": name, "args": args } })
        }
        (None, _) => json!({ "text": "No tool was needed." }),
    };
    let reply = json!({
        "candidates": [{
            "content": { "role": "model", "parts": [part] },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": { "promptTokenCount": 8, "candidatesTokenCount": 4, "totalTokenCount": 12 },
        "modelVersion": "gemini-3.7-flash"
    });
    if uri.path().ends_with(":streamGenerateContent") {
        ([(header::CONTENT_TYPE, "text/event-stream")], format!("data: {reply}\r\n\r\n"))
            .into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/json")], reply.to_string()).into_response()
    }
}

#[derive(Default)]
pub struct HttpMcpCounters {
    pub initializes: AtomicUsize,
    pub lists: AtomicUsize,
    /// The `Mcp-Session-Id` each `tools/list` arrived on.
    pub list_sessions: Mutex<Vec<String>>,
}

/// A Streamable HTTP MCP server whose first `tools/list` fails with HTTP 500 and a
/// plain-text body that contains the letters "eof" ("geoffrey").
pub async fn flaky_http_mcp() -> anyhow::Result<(String, Arc<HttpMcpCounters>)> {
    let counters = Arc::new(HttpMcpCounters::default());
    let router = Router::new()
        .route(
            "/mcp",
            post(http_mcp)
                .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .delete(|| async { StatusCode::OK }),
        )
        .with_state(Arc::clone(&counters));
    Ok((format!("{}/mcp", serve(router).await?), counters))
}

async fn http_mcp(
    State(counters): State<Arc<HttpMcpCounters>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let json_reply = |result: Value, session: Option<String>| {
        let mut response = (
            [(header::CONTENT_TYPE, "application/json")],
            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        )
            .into_response();
        if let Some(session) = session.and_then(|session| session.parse().ok()) {
            response.headers_mut().insert("mcp-session-id", session);
        }
        response
    };
    match message["method"].as_str() {
        Some("initialize") => {
            let session = counters.initializes.fetch_add(1, Ordering::SeqCst) + 1;
            let version = message["params"]["protocolVersion"].clone();
            json_reply(
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "flaky-http", "version": "1.0.0" }
                }),
                Some(format!("s{session}")),
            )
        }
        Some("tools/list") => {
            let listing = counters.lists.fetch_add(1, Ordering::SeqCst) + 1;
            let session = headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("none")
                .to_string();
            if let Ok(mut sessions) = counters.list_sessions.lock() {
                sessions.push(session);
            }
            if listing == 1 {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    "upstream geoffrey is restarting",
                )
                    .into_response();
            }
            json_reply(
                json!({ "tools": [{
                    "name": "echo",
                    "description": "Echo.",
                    "inputSchema": { "type": "object" }
                }] }),
                None,
            )
        }
        _ => json_reply(json!({}), None),
    }
}
