//! `Toolset::close` on MCP toolsets: the stdio server process exits, a Streamable HTTP
//! session ends, a closed toolset fails later requests instead of reconnecting, closing
//! is bounded and idempotent, a shared `McpServerManager` keeps its servers, and
//! `Runner::close` reaches an agent's MCP toolset.
//!
//! The stdio server is this test binary, re-run as a child with [`SERVER_ENV`] set. The
//! lines libtest prints before the server starts are not JSON-RPC, which the client skips.
//! Unix only: the process checks use `kill -0`.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use adk_core::{Content, ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::McpToolset;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::{RequestContext, RoleClient, RunningService};
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

/// Names the mode when this binary runs as the stdio MCP server.
const SERVER_ENV: &str = "ADK_TOOL_MCP_CLOSE_TEST_SERVER";

/// Publishes `lookup_order`, and `server_pid` returning this process's ID.
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
        let schema = Arc::new(serde_json::Map::new());
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("lookup_order", "Looks up an order.", Arc::clone(&schema)),
            Tool::new("server_pid", "Returns the server's process ID.", schema),
        ]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let text = match params.name.as_ref() {
            "server_pid" => std::process::id().to_string(),
            _ => "order A-1 has shipped".to_string(),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

/// Serves [`Orders`] over stdio when [`SERVER_ENV`] is set; in mode `stubborn` the process
/// keeps running for a minute after its input ends.
#[tokio::test]
#[ignore = "runs only as the stdio MCP server the other tests start"]
async fn stdio_server() {
    let Ok(mode) = std::env::var(SERVER_ENV) else {
        return;
    };
    let running = Orders.serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
    if mode == "stubborn" {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

fn server_args() -> Vec<String> {
    ["stdio_server", "--exact", "--ignored", "--quiet", "--test-threads=1"]
        .map(str::to_string)
        .to_vec()
}

/// Starts the stdio server in `mode` and completes the handshake with it.
async fn connect(mode: &str) -> (RunningService<RoleClient, ()>, u32) {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.args(server_args()).env(SERVER_ENV, mode);
    let transport = rmcp::transport::TokioChildProcess::new(command).unwrap();
    let pid = transport.id().unwrap();
    (().serve(transport).await.unwrap(), pid)
}

/// Whether process `pid` exists; an exited child nobody reaped still does.
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-close"))
}

/// The error of a request that must fail.
fn refused<T>(result: adk_core::Result<T>) -> adk_core::AdkError {
    match result {
        Ok(_) => panic!("a closed toolset served a request"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn close_ends_the_stdio_server_process() {
    let (client, pid) = connect("orders").await;
    let toolset = McpToolset::new(client).with_name("orders");
    assert_eq!(toolset.tools(ctx()).await.unwrap().len(), 2);
    assert!(process_exists(pid));

    toolset.close().await;

    assert!(!process_exists(pid), "the server process must be gone when close returns");
    assert!(toolset.is_closed().await);
}

#[tokio::test]
async fn a_closed_toolset_fails_every_request_and_closing_again_is_harmless() {
    let (client, _) = connect("orders").await;
    let toolset = McpToolset::new(client).with_name("orders");
    let tools = toolset.tools(ctx()).await.unwrap();
    let clone = toolset.clone();

    toolset.close().await;
    let started = Instant::now();
    toolset.close().await;
    clone.close().await;
    assert!(started.elapsed() < Duration::from_secs(1), "closing again only checks the close");

    let listed = refused(clone.tools(ctx()).await);
    let called = tools[0].execute(Arc::new(SimpleToolContext::new("mcp-close")), json!({})).await;
    let resources = toolset.list_resources().await.unwrap_err();
    let errors = [listed, called.unwrap_err(), resources];
    assert_eq!(errors.iter().map(|error| error.code).collect::<Vec<_>>(), ["tool.mcp.closed"; 3]);
    assert!(errors[0].message.contains("MCP toolset 'orders' is closed"), "{}", errors[0]);
}

/// Starts a new stdio server on every reconnect, and counts them.
struct Respawn(Arc<AtomicUsize>);

#[adk_core::async_trait]
impl adk_tool::mcp::ConnectionFactory<()> for Respawn {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(connect("orders").await.0)
    }
}

#[tokio::test]
async fn a_closed_toolset_does_not_reconnect_through_its_factory() {
    let spawned = Arc::new(AtomicUsize::new(0));
    let (client, _) = connect("orders").await;
    let toolset = McpToolset::new(client)
        .with_name("orders")
        .with_connection_factory(Arc::new(Respawn(Arc::clone(&spawned))));
    let tools = toolset.tools(ctx()).await.unwrap();

    toolset.close().await;

    let listed = refused(toolset.tools(ctx()).await);
    let called = tools[0].execute(Arc::new(SimpleToolContext::new("mcp-close")), json!({})).await;
    assert_eq!((listed.code, called.unwrap_err().code), ("tool.mcp.closed", "tool.mcp.closed"));
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "a closed toolset must not reconnect");
}

#[tokio::test]
async fn close_returns_within_its_timeout_when_the_server_ignores_its_input() {
    let (client, pid) = connect("stubborn").await;
    let toolset = McpToolset::new(client)
        .with_name("stubborn")
        .with_close_timeout(Duration::from_millis(300));

    let started = Instant::now();
    toolset.close().await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_millis(1500), "close took {elapsed:?}");
    assert!(toolset.tools(ctx()).await.is_err());
    // rmcp kills a stdio server that is still running 3 seconds after its input closed.
    let killed = tokio::time::timeout(Duration::from_secs(10), async {
        while process_exists(pid) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(killed.is_ok(), "the server process was never killed");
}

/// The PID the managed server reports through its `server_pid` tool.
async fn managed_pid(manager: &adk_tool::mcp::McpServerManager) -> u32 {
    let tools = manager.tools(ctx()).await.unwrap();
    let tool = tools.iter().find(|tool| tool.name() == "server_pid").unwrap();
    let output = tool.execute(Arc::new(SimpleToolContext::new("mcp-close")), json!({})).await;
    output.unwrap()["output"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn closing_a_manager_as_a_toolset_keeps_its_servers_and_shutdown_ends_them() {
    use adk_tool::mcp::{McpServerConfig, McpServerManager, ServerStatus};

    let config = McpServerConfig {
        command: std::env::current_exe().unwrap().display().to_string(),
        args: server_args(),
        env: HashMap::from([(SERVER_ENV.to_string(), "orders".to_string())]),
        ..Default::default()
    };
    let manager = McpServerManager::new(HashMap::from([("orders".to_string(), config)]))
        .with_grace_period(Duration::from_secs(3));
    manager.start_server("orders").await.unwrap();
    let pid = managed_pid(&manager).await;

    Toolset::close(&manager).await;

    assert_eq!(manager.server_status("orders").await.unwrap(), ServerStatus::Running);
    assert!(process_exists(pid), "closing the manager's toolset must not stop its servers");
    assert_eq!(managed_pid(&manager).await, pid);

    manager.shutdown().await.unwrap();

    assert!(!process_exists(pid), "the server process must be gone when shutdown returns");
}

/// Calls `lookup_order` until a function response arrives, then answers.
struct CallsLookupOrder;

#[adk_core::async_trait]
impl adk_core::Llm for CallsLookupOrder {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let answered = request
            .contents
            .iter()
            .flat_map(|content| &content.parts)
            .any(|part| matches!(part, adk_core::Part::FunctionResponse { .. }));
        let part = if answered {
            adk_core::Part::Text { text: "it has shipped".to_string() }
        } else {
            adk_core::Part::FunctionCall {
                name: "lookup_order".to_string(),
                args: json!({}),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }
        };
        let response = adk_core::LlmResponse {
            content: Some(Content { role: "model".to_string(), parts: vec![part] }),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

#[tokio::test]
async fn runner_close_ends_the_mcp_server_of_an_agent() {
    use adk_session::{CreateRequest, InMemorySessionService, SessionService};
    use futures::StreamExt;

    let (client, pid) = connect("orders").await;
    let toolset = Arc::new(McpToolset::new(client).with_name("orders"));
    let agent = adk_agent::LlmAgentBuilder::new("desk")
        .model(Arc::new(CallsLookupOrder))
        .toolset(toolset.clone())
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "mcp-close".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("mcp-close")
        .agent(Arc::new(agent) as Arc<dyn adk_core::Agent>)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let mut stream = runner
        .run_str("user", "session", Content::new("user").with_text("Where is order A-1?"))
        .await
        .unwrap();
    let mut answered = false;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        answered |=
            event.llm_response.content.iter().flat_map(|content| &content.parts).any(|part| {
                matches!(part, adk_core::Part::FunctionResponse { function_response, .. }
                if function_response.name == "lookup_order")
            });
    }
    assert!(answered, "the agent must have called the MCP tool");

    runner.close().await;

    assert!(!process_exists(pid), "the server process must be gone when Runner::close returns");
    assert!(toolset.is_closed().await);
}

#[cfg(feature = "http-transport")]
mod http {
    use super::*;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use std::sync::Mutex;

    /// The `initialize` requests and the session IDs ended with `DELETE`.
    #[derive(Clone, Default)]
    struct Log {
        initializes: Arc<AtomicUsize>,
        deleted: Arc<Mutex<Vec<String>>>,
    }

    async fn request(State(log): State<Log>, body: Bytes) -> Response {
        let message: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let Some(id) = message.get("id").cloned() else {
            return StatusCode::ACCEPTED.into_response();
        };
        let result = match message["method"].as_str() {
            Some("initialize") => {
                log.initializes.fetch_add(1, Ordering::SeqCst);
                json!({
                    "protocolVersion": message["params"]["protocolVersion"].clone(),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "close-http", "version": "1.0.0" }
                })
            }
            Some("tools/list") => {
                json!({ "tools": [{ "name": "lookup_order", "inputSchema": { "type": "object" } }] })
            }
            _ => json!({}),
        };
        (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::HeaderName::from_static("mcp-session-id"), "s1"),
            ],
            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        )
            .into_response()
    }

    async fn delete(State(log): State<Log>, headers: HeaderMap) -> StatusCode {
        let session = headers.get("mcp-session-id").and_then(|value| value.to_str().ok());
        log.deleted.lock().unwrap().push(session.unwrap_or("none").to_string());
        StatusCode::OK
    }

    #[tokio::test]
    async fn close_ends_a_streamable_http_session_and_does_not_reconnect() {
        let log = Log::default();
        let router = axum::Router::new()
            .route(
                "/mcp",
                axum::routing::post(request)
                    .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                    .delete(delete),
            )
            .with_state(log.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });
        let toolset = adk_tool::mcp::McpHttpClientBuilder::new(url).connect().await.unwrap();
        assert_eq!(toolset.tools(ctx()).await.unwrap().len(), 1);

        toolset.close().await;

        assert_eq!(*log.deleted.lock().unwrap(), ["s1"]);
        assert_eq!(refused(toolset.tools(ctx()).await).code, "tool.mcp.closed");
        assert_eq!(
            log.initializes.load(Ordering::SeqCst),
            1,
            "a closed toolset must not reconnect"
        );
    }
}
