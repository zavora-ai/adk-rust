//! The MCP servers and the scripted Gemini endpoint the scenarios run against.
//!
//! The stdio server is this binary itself, started with `--mcp-server <mode>`; the
//! Streamable HTTP server and the Gemini endpoint are local axum servers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_model::GeminiModel;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// Runs this binary as a stdio MCP server: `--mcp-server <mode>`.
pub const SERVER_FLAG: &str = "--mcp-server";

/// The tracking code `lookup_order` returns; only a real tool call can produce it.
pub const TRACKING_CODE: &str = "TRK-5J2";

fn order_tool() -> Tool {
    let schema: JsonObject = json!({
        "type": "object",
        "properties": { "order_id": { "type": "string", "description": "The order ID" } },
        "required": ["order_id"]
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    Tool::new(
        "lookup_order",
        "Looks up the shipping status and tracking code of an order.",
        Arc::new(schema),
    )
}

fn order_status(params: &CallToolRequestParams) -> String {
    let order = params
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get("order_id"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("order {order} has shipped; tracking code {TRACKING_CODE}")
}

fn pid_tool() -> Tool {
    let schema: JsonObject = json!({ "type": "object" }).as_object().cloned().unwrap_or_default();
    Tool::new("server_pid", "Returns the process ID of this MCP server.", Arc::new(schema))
}

/// `lookup_order`, and `server_pid` for the scenarios that cannot see the child process.
#[derive(Clone)]
struct Orders;

impl ServerHandler for Orders {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![order_tool(), pid_tool()]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let text = match params.name.as_ref() {
            "lookup_order" => order_status(&params),
            "server_pid" => std::process::id().to_string(),
            other => return Err(ErrorData::invalid_params(format!("unknown tool {other}"), None)),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

/// Serves `Orders` over this process's stdin and stdout until the client hangs up.
///
/// In mode `stubborn` the process keeps running after its input ends, like a server
/// that must be killed.
pub async fn serve_stdio(mode: &str) -> anyhow::Result<()> {
    let running = Orders.serve((tokio::io::stdin(), tokio::io::stdout())).await?;
    running.waiting().await?;
    if mode == "stubborn" {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    Ok(())
}

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
}

/// A Gemini `generateContent` endpoint that calls the declared `lookup_order` function
/// (under any prefix) once, then answers with that function's response as text.
#[derive(Clone)]
pub struct ScriptedGemini {
    base_url: String,
    state: Arc<GeminiState>,
}

impl ScriptedGemini {
    pub async fn start() -> anyhow::Result<Self> {
        let state = Arc::new(GeminiState::default());
        let router = Router::new().fallback(gemini).with_state(Arc::clone(&state));
        let base_url = format!("{}/v1beta/", serve(router).await?);
        Ok(Self { base_url, state })
    }

    /// The real Gemini client, pointed at this endpoint.
    pub fn model(&self) -> anyhow::Result<GeminiModel> {
        Ok(GeminiModel::new_with_base_url("scripted", "gemini-3.5-flash-lite", &self.base_url)?)
    }

    /// How many `generateContent` requests arrived.
    pub fn requests(&self) -> usize {
        self.state.declared.lock().map(|declared| declared.len()).unwrap_or(0)
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
    let lookup = names.iter().find(|name| name.ends_with("lookup_order"));
    let part = match (function_response, lookup) {
        (Some(response), _) => json!({ "text": format!("Tool result: {}", response["response"]) }),
        (None, Some(name)) => {
            json!({ "functionCall": { "name": name, "args": { "order_id": "A-1042" } } })
        }
        (None, None) => json!({ "text": "No lookup_order tool was declared." }),
    };
    let reply = json!({
        "candidates": [{
            "content": { "role": "model", "parts": [part] },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": { "promptTokenCount": 8, "candidatesTokenCount": 4, "totalTokenCount": 12 },
        "modelVersion": "gemini-3.5-flash-lite"
    });
    if uri.path().ends_with(":streamGenerateContent") {
        ([(header::CONTENT_TYPE, "text/event-stream")], format!("data: {reply}\r\n\r\n"))
            .into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/json")], reply.to_string()).into_response()
    }
}

/// What the Streamable HTTP MCP server saw.
#[derive(Default)]
pub struct HttpMcpLog {
    /// Sessions opened with `initialize`.
    pub initializes: AtomicUsize,
    /// Session IDs the client ended with `DELETE`.
    pub deleted: Mutex<Vec<String>>,
}

impl HttpMcpLog {
    pub fn initializes(&self) -> usize {
        self.initializes.load(Ordering::SeqCst)
    }

    pub fn deleted(&self) -> Vec<String> {
        self.deleted.lock().map(|deleted| deleted.clone()).unwrap_or_default()
    }
}

/// A Streamable HTTP MCP server publishing `lookup_order`, answering every request
/// with a JSON body.
pub async fn http_mcp() -> anyhow::Result<(String, Arc<HttpMcpLog>)> {
    let log = Arc::new(HttpMcpLog::default());
    let router = Router::new()
        .route(
            "/mcp",
            post(http_request).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(
                |State(log): State<Arc<HttpMcpLog>>, headers: HeaderMap| async move {
                    let session = headers
                        .get("mcp-session-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("none")
                        .to_string();
                    if let Ok(mut deleted) = log.deleted.lock() {
                        deleted.push(session);
                    }
                    StatusCode::OK
                },
            ),
        )
        .with_state(Arc::clone(&log));
    Ok((format!("{}/mcp", serve(router).await?), log))
}

async fn http_request(State(log): State<Arc<HttpMcpLog>>, body: Bytes) -> Response {
    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let reply = |result: Value, session: Option<String>| {
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
            let session = log.initializes.fetch_add(1, Ordering::SeqCst) + 1;
            reply(
                json!({
                    "protocolVersion": message["params"]["protocolVersion"].clone(),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "orders-http", "version": "1.0.0" }
                }),
                Some(format!("session-{session}")),
            )
        }
        Some("tools/list") => reply(json!({ "tools": [order_tool()] }), None),
        Some("tools/call") => {
            let params: CallToolRequestParams =
                serde_json::from_value(message["params"].clone()).unwrap_or_else(|_| {
                    CallToolRequestParams::new("lookup_order")
                });
            reply(
                json!({ "content": [{ "type": "text", "text": order_status(&params) }] }),
                None,
            )
        }
        _ => reply(json!({}), None),
    }
}
