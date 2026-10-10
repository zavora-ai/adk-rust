//! A crashed MCP server is detected and reconnected, `SimpleClient` and
//! `ConnectionRefresher` send requests concurrently, and a call interrupted by a
//! crash is never replayed by default.
#![cfg(feature = "mcp")]

use adk_core::{ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{
    ConnectionFactory, ConnectionRefresher, McpToolset, RefreshConfig, SimpleClient,
};
use rmcp::model::*;
use rmcp::service::{RequestContext, RunningService};
use rmcp::{RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Barrier, Notify};
use tokio::task::JoinHandle;

/// Long enough that only a deadlock or a missed reconnect reaches it.
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct Server {
    /// `rendezvous` calls wait here until two of them are in flight at once.
    rendezvous: Arc<Barrier>,
    /// Notified when a `stall` call or a stalled `tools/list` reaches the server.
    entered: Arc<Notify>,
    /// `tools/list` stalls instead of answering.
    stall_listing: bool,
}

impl Server {
    fn new() -> Self {
        Self {
            rendezvous: Arc::new(Barrier::new(2)),
            entered: Arc::new(Notify::new()),
            stall_listing: false,
        }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.stall_listing {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![Tool::new("echo", "Echo.", schema)]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match params.name.as_ref() {
            "rendezvous" => {
                self.rendezvous.wait().await;
            }
            "stall" => {
                self.entered.notify_one();
                std::future::pending::<()>().await;
            }
            "echo" => {}
            other => return Err(ErrorData::invalid_params(format!("unknown tool {other}"), None)),
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(params.name.to_string())]).into())
    }
}

/// Connects to `server` through a byte proxy. Aborting the returned task cuts both
/// directions at once, the way a crashed stdio server process closes its pipes.
async fn connect(server: Server) -> (RunningService<RoleClient, ()>, JoinHandle<()>) {
    let (client_io, mut client_side) = tokio::io::duplex(64 * 1024);
    let (mut server_side, server_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let proxy = tokio::spawn(async move {
        let _ = tokio::io::copy_bidirectional(&mut client_side, &mut server_side).await;
    });
    (().serve(client_io).await.unwrap(), proxy)
}

/// Cuts the connection and waits until the client has seen it close.
async fn crash(proxy: JoinHandle<()>, peer: &rmcp::service::Peer<RoleClient>) {
    proxy.abort();
    tokio::time::timeout(DEADLINE, async {
        while !peer.is_transport_closed() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the client never saw the transport close");
}

/// Hands out connections to a healthy server and counts them.
#[derive(Default)]
struct Factory {
    connections: AtomicUsize,
}

#[async_trait::async_trait]
impl ConnectionFactory<()> for Factory {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        Ok(connect(Server::new()).await.0)
    }
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-reconnect-test"))
}

fn fast_refresh() -> RefreshConfig {
    RefreshConfig::default().with_retry_delay_ms(0).without_logging()
}

fn toolset(client: RunningService<RoleClient, ()>, factory: Arc<Factory>) -> McpToolset {
    McpToolset::new(client)
        .with_connection_factory(factory)
        .with_refresh_config(fast_refresh())
        .with_tool_list_cache_ttl(Duration::ZERO)
}

#[tokio::test]
async fn is_closed_reports_a_server_process_that_exited() {
    let (client, proxy) = connect(Server::new()).await;
    let peer = client.peer().clone();
    let toolset = McpToolset::new(client);
    assert!(!toolset.is_closed().await);

    crash(proxy, &peer).await;

    assert!(toolset.is_closed().await, "a closed transport must read as a closed connection");
}

#[tokio::test]
async fn a_toolset_reconnects_after_its_server_process_exits() {
    let (client, proxy) = connect(Server::new()).await;
    let peer = client.peer().clone();
    let factory = Arc::new(Factory::default());
    let toolset = toolset(client, factory.clone());
    crash(proxy, &peer).await;

    let tools = toolset.tools(ctx()).await.expect("discovery reconnects");
    assert_eq!(tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["echo"]);
    // Without replay opt-in: the call was never sent on the dead connection.
    let value = toolset.call_tool_value("echo", Default::default()).await.unwrap();
    assert_eq!(value, json!({ "output": "echo" }));
    assert_eq!(factory.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn discovery_interrupted_by_a_server_crash_reconnects() {
    let server = Server { stall_listing: true, ..Server::new() };
    let entered = server.entered.clone();
    let (client, proxy) = connect(server).await;
    let peer = client.peer().clone();
    let factory = Arc::new(Factory::default());
    let toolset = toolset(client, factory.clone());

    let discovery = tokio::spawn({
        let toolset = toolset.clone();
        async move { toolset.tools(ctx()).await.map(|tools| tools.len()) }
    });
    entered.notified().await;
    crash(proxy, &peer).await;

    let found = tokio::time::timeout(DEADLINE, discovery).await.unwrap().unwrap();
    assert_eq!(found.expect("`Transport closed` must trigger a reconnect"), 1);
    assert_eq!(factory.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_tool_call_interrupted_by_a_server_crash_is_not_replayed() {
    let server = Server::new();
    let entered = server.entered.clone();
    let (client, proxy) = connect(server).await;
    let peer = client.peer().clone();
    let factory = Arc::new(Factory::default());
    let toolset = toolset(client, factory.clone());

    let call = tokio::spawn({
        let toolset = toolset.clone();
        async move { toolset.call_tool_value("stall", Default::default()).await }
    });
    entered.notified().await;
    crash(proxy, &peer).await;

    let error = tokio::time::timeout(DEADLINE, call).await.unwrap().unwrap().unwrap_err();
    assert!(error.to_string().contains("result is uncertain and was not replayed"), "{error}");
    assert_eq!(factory.connections.load(Ordering::SeqCst), 0, "the call must not be resent");

    // The next call goes out on a new connection.
    let value = toolset.call_tool_value("echo", Default::default()).await.unwrap();
    assert_eq!(value, json!({ "output": "echo" }));
    assert_eq!(factory.connections.load(Ordering::SeqCst), 1);
}

fn call(name: &str) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
}

fn text(response: CallToolResponse) -> String {
    match response {
        CallToolResponse::Complete(result) => result.content[0].as_text().unwrap().text.clone(),
        other => panic!("expected a complete result, got {other:?}"),
    }
}

#[tokio::test]
async fn simple_client_requests_do_not_wait_for_each_other() {
    let client = SimpleClient::new(connect(Server::new()).await.0);

    // Each call returns only once the other one has reached the server.
    let (first, second) = tokio::time::timeout(
        DEADLINE,
        futures::future::join(
            client.call_tool(call("rendezvous")),
            client.call_tool(call("rendezvous")),
        ),
    )
    .await
    .expect("the second call waited for the first to finish");

    assert_eq!(text(first.unwrap()), "rendezvous");
    assert_eq!(text(second.unwrap()), "rendezvous");
}

#[tokio::test]
async fn connection_refresher_requests_do_not_wait_for_each_other() {
    let refresher =
        ConnectionRefresher::new(connect(Server::new()).await.0, Arc::new(Factory::default()));

    let (first, second) = tokio::time::timeout(
        DEADLINE,
        futures::future::join(
            refresher.call_tool(call("rendezvous")),
            refresher.call_tool(call("rendezvous")),
        ),
    )
    .await
    .expect("the second call waited for the first to finish");

    assert_eq!(text(first.unwrap().value), "rendezvous");
    assert_eq!(text(second.unwrap().value), "rendezvous");
}

#[tokio::test]
async fn connection_refresher_reconnects_once_after_a_crash_interrupts_discovery() {
    let server = Server { stall_listing: true, ..Server::new() };
    let entered = server.entered.clone();
    let (client, proxy) = connect(server).await;
    let peer = client.peer().clone();
    let factory = Arc::new(Factory::default());
    let refresher = ConnectionRefresher::new(client, factory.clone()).with_config(fast_refresh());

    let discovery = futures::future::join(refresher.list_tools(), refresher.list_tools());
    let crash_after_both_arrive = async {
        entered.notified().await;
        entered.notified().await;
        crash(proxy, &peer).await;
    };
    let ((first, second), ()) =
        tokio::time::timeout(DEADLINE, futures::future::join(discovery, crash_after_both_arrive))
            .await
            .expect("discovery did not recover from the crash");

    for listed in [first, second] {
        let listed = listed.expect("`Transport closed` must trigger a reconnect");
        assert!(listed.reconnected);
        assert_eq!(listed.value.len(), 1);
    }
    assert_eq!(factory.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn connection_refresher_does_not_replay_a_call_a_crash_interrupted() {
    let server = Server::new();
    let entered = server.entered.clone();
    let (client, proxy) = connect(server).await;
    let peer = client.peer().clone();
    let factory = Arc::new(Factory::default());
    let refresher = ConnectionRefresher::new(client, factory.clone()).with_config(fast_refresh());

    let (result, ()) = futures::future::join(refresher.call_tool(call("stall")), async {
        entered.notified().await;
        crash(proxy, &peer).await;
    })
    .await;

    let error = result.unwrap_err();
    assert!(error.contains("result is uncertain and was not replayed"), "{error}");
    assert_eq!(factory.connections.load(Ordering::SeqCst), 0);
    // A closed connection is replaced before the next call is sent.
    let response = refresher.call_tool(call("echo")).await.unwrap();
    assert_eq!(text(response.value), "echo");
    assert_eq!(factory.connections.load(Ordering::SeqCst), 1);
}
