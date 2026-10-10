//! A toolset shares one MCP connection across concurrent agent work: calls and
//! discovery proceed in parallel, a dropped call is cancelled on the server,
//! `tools/list` results are cached, and concurrent failures reconnect once.
#![cfg(feature = "mcp")]

use adk_core::{ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ConnectionFactory, McpToolset, RefreshConfig,
};
use rmcp::model::*;
use rmcp::service::{RequestContext, RunningService};
use rmcp::{RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Barrier, Notify};

/// Long enough that only a deadlock or a lost notification reaches it.
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct SlowServer {
    /// `rendezvous` calls wait here until two of them are in flight at once.
    rendezvous: Arc<Barrier>,
    /// `gate` calls wait here until the test releases them.
    gate: Arc<Notify>,
    /// Notified when the server sees a `hang` call cancelled.
    cancelled: Arc<Notify>,
    list_calls: Arc<AtomicUsize>,
    /// Fails `tools/list` the way a server that lost the session does.
    session_lost: bool,
}

impl SlowServer {
    fn new() -> Self {
        Self {
            rendezvous: Arc::new(Barrier::new(2)),
            gate: Arc::new(Notify::new()),
            cancelled: Arc::new(Notify::new()),
            list_calls: Arc::new(AtomicUsize::new(0)),
            session_lost: false,
        }
    }
}

impl ServerHandler for SlowServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(
            ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build(),
        )
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let generation = self.list_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.session_lost {
            return Err(ErrorData::invalid_request("session not found", None));
        }
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "rendezvous",
            format!("catalog {generation}"),
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match params.name.as_ref() {
            "rendezvous" => {
                self.rendezvous.wait().await;
            }
            "gate" => self.gate.notified().await,
            "hang" => {
                context.ct.cancelled().await;
                self.cancelled.notify_one();
            }
            other => return Err(ErrorData::invalid_params(format!("unknown tool {other}"), None)),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(params.name.to_string())]).into())
    }
}

async fn connect(server: SlowServer) -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    ().serve(client_io).await.unwrap()
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-concurrency-test"))
}

#[tokio::test]
async fn concurrent_tool_calls_share_the_connection_without_waiting_for_each_other() {
    let toolset = McpToolset::new(connect(SlowServer::new()).await);

    // Each call returns only once the other one has reached the server.
    let (first, second) = tokio::time::timeout(
        DEADLINE,
        futures::future::join(
            toolset.call_tool_value("rendezvous", Default::default()),
            toolset.call_tool_value("rendezvous", Default::default()),
        ),
    )
    .await
    .expect("the second call waited for the first to finish");

    assert_eq!(first.unwrap(), json!({ "output": "rendezvous" }));
    assert_eq!(second.unwrap(), json!({ "output": "rendezvous" }));
}

#[tokio::test]
async fn discovery_completes_while_a_tool_call_is_in_flight() {
    let server = SlowServer::new();
    let gate = server.gate.clone();
    let toolset = McpToolset::new(connect(server).await);

    let in_flight = tokio::spawn({
        let toolset = toolset.clone();
        async move { toolset.call_tool_value("gate", Default::default()).await }
    });
    // Let the call reach the server before discovery starts.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let tools = tokio::time::timeout(DEADLINE, toolset.tools(ctx()))
        .await
        .expect("discovery waited for an unrelated tool call")
        .unwrap();
    assert_eq!(tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["rendezvous"]);

    gate.notify_one();
    assert_eq!(in_flight.await.unwrap().unwrap(), json!({ "output": "gate" }));
}

#[tokio::test]
async fn a_tool_call_dropped_by_a_timeout_is_cancelled_on_the_server() {
    let server = SlowServer::new();
    let cancelled = server.cancelled.clone();
    let toolset = McpToolset::new(connect(server).await);

    let timed_out = tokio::time::timeout(
        Duration::from_millis(100),
        toolset.call_tool_value("hang", Default::default()),
    )
    .await;
    assert!(timed_out.is_err(), "the hanging call returned: {timed_out:?}");

    tokio::time::timeout(DEADLINE, cancelled.notified())
        .await
        .expect("the server never received notifications/cancelled");
}

#[tokio::test]
async fn tool_discovery_reuses_the_cached_list_until_invalidated() {
    let server = SlowServer::new();
    let list_calls = server.list_calls.clone();
    let toolset = McpToolset::new(connect(server).await);

    toolset.tools(ctx()).await.unwrap();
    toolset.tools(ctx()).await.unwrap();
    assert_eq!(list_calls.load(Ordering::SeqCst), 1);

    toolset.invalidate_tool_list_cache().await;
    toolset.tools(ctx()).await.unwrap();
    assert_eq!(list_calls.load(Ordering::SeqCst), 2);

    let uncached = toolset.with_tool_list_cache_ttl(Duration::ZERO);
    uncached.tools(ctx()).await.unwrap();
    uncached.tools(ctx()).await.unwrap();
    assert_eq!(list_calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_tool_list_changed_notification_invalidates_the_cached_list() {
    let server = SlowServer::new();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let running = tokio::spawn(server.serve(server_io));
    let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
        .serve(client_io)
        .await
        .unwrap();
    let running = running.await.unwrap().unwrap();
    let toolset = McpToolset::new(client);

    let description = |tools: Vec<Arc<dyn adk_core::Tool>>| tools[0].description().to_string();
    assert_eq!(description(toolset.tools(ctx()).await.unwrap()), "catalog 1");
    assert_eq!(description(toolset.tools(ctx()).await.unwrap()), "catalog 1");

    running.peer().notify_tool_list_changed().await.unwrap();
    tokio::time::timeout(DEADLINE, async {
        while description(toolset.tools(ctx()).await.unwrap()) == "catalog 1" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the cached list outlived notifications/tools/list_changed");
}

/// Hands out connections to a healthy server and counts them.
struct CountingFactory {
    connections: AtomicUsize,
}

#[async_trait::async_trait]
impl ConnectionFactory<()> for CountingFactory {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        // Slow enough that both failed callers are waiting on the reconnect.
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(connect(SlowServer::new()).await)
    }
}

#[tokio::test]
async fn concurrent_failures_on_one_connection_reconnect_once() {
    let lost = SlowServer { session_lost: true, ..SlowServer::new() };
    let factory = Arc::new(CountingFactory { connections: AtomicUsize::new(0) });
    let toolset = McpToolset::new(connect(lost).await)
        .with_connection_factory(factory.clone())
        .with_refresh_config(RefreshConfig::default().with_retry_delay_ms(0).without_logging())
        .with_tool_list_cache_ttl(Duration::ZERO);

    let (first, second) = tokio::time::timeout(
        DEADLINE,
        futures::future::join(toolset.tools(ctx()), toolset.tools(ctx())),
    )
    .await
    .expect("reconnect deadlocked");

    assert_eq!(first.unwrap().len(), 1);
    assert_eq!(second.unwrap().len(), 1);
    assert_eq!(factory.connections.load(Ordering::SeqCst), 1);
}
