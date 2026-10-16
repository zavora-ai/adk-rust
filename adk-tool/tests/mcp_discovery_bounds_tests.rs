//! Catalog listings are bounded by page count, repeated cursors, and time; tool-list
//! invalidation reaches a fetch in flight and rmcp's response cache; and an HTTP 5xx
//! never reconnects, whatever its body says.
#![cfg(feature = "mcp")]

use adk_core::{ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{
    ConnectionFactory, ConnectionRefresher, McpDiscoveryLimits, McpToolset, RefreshConfig,
    SimpleClient,
};
use rmcp::model::*;
use rmcp::service::{RequestContext, RunningService};
use rmcp::{RoleClient, RoleServer, ServerHandler, ServiceExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-discovery-bounds-test"))
}

fn tool(name: String) -> Tool {
    Tool::new(name, "A tool.", Arc::new(JsonObject::new()))
}

async fn connect<H: ServerHandler>(handler: H) -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = handler.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    ().serve(client_io).await.unwrap()
}

/// How a scripted server pages its catalogs.
#[derive(Clone, Copy, Default)]
enum Paging {
    /// A fresh cursor on every page.
    #[default]
    Endless,
    /// `a -> b -> a`, each page fresh for a minute.
    Cyclic,
    /// Never answers.
    Hung,
}

/// Pages every catalog the same way and counts requests per method.
#[derive(Clone, Default)]
struct PagingServer {
    paging: Paging,
    tools: Arc<AtomicUsize>,
    resources: Arc<AtomicUsize>,
    templates: Arc<AtomicUsize>,
    prompts: Arc<AtomicUsize>,
}

impl PagingServer {
    fn new(paging: Paging) -> Self {
        Self { paging, ..Self::default() }
    }

    /// The cursor after `cursor`, counting the request in `counter`.
    async fn next(&self, counter: &AtomicUsize, cursor: Option<String>) -> String {
        let page = counter.fetch_add(1, Ordering::SeqCst) + 1;
        match self.paging {
            Paging::Endless => format!("cursor-{page}"),
            Paging::Cyclic => if cursor.as_deref() == Some("a") { "b" } else { "a" }.to_string(),
            Paging::Hung => std::future::pending().await,
        }
    }

    fn ttl(&self) -> Option<u64> {
        matches!(self.paging, Paging::Cyclic).then_some(60_000)
    }
}

impl ServerHandler for PagingServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let next = self.next(&self.tools, params.and_then(|params| params.cursor)).await;
        let mut result = ListToolsResult::with_all_items(vec![tool(format!("after_{next}"))]);
        result.next_cursor = Some(next);
        result.ttl_ms = self.ttl();
        Ok(result)
    }

    async fn list_resources(
        &self,
        params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let next = self.next(&self.resources, params.and_then(|params| params.cursor)).await;
        let mut result = ListResourcesResult::with_all_items(Vec::new());
        result.next_cursor = Some(next);
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let next = self.next(&self.templates, params.and_then(|params| params.cursor)).await;
        let mut result = ListResourceTemplatesResult::with_all_items(Vec::new());
        result.next_cursor = Some(next);
        Ok(result)
    }

    async fn list_prompts(
        &self,
        params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let next = self.next(&self.prompts, params.and_then(|params| params.cursor)).await;
        let mut result = ListPromptsResult::with_all_items(Vec::new());
        result.next_cursor = Some(next);
        Ok(result)
    }
}

#[tokio::test]
async fn tool_discovery_stops_at_the_default_page_limit() {
    let server = PagingServer::new(Paging::Endless);
    let requests = Arc::clone(&server.tools);
    let toolset = McpToolset::new(connect(server).await).with_name("endless");

    let error = toolset.tools(ctx()).await.err().expect("an endless listing must fail");

    let message = error.to_string();
    assert!(message.contains("'endless'") && message.contains("after 64 pages"), "{message}");
    assert_eq!(requests.load(Ordering::SeqCst), McpDiscoveryLimits::DEFAULT_MAX_PAGES);
}

/// The cycle repeats from rmcp's response cache without a request, so only the
/// repeated-cursor check ends it.
#[tokio::test]
async fn tool_discovery_stops_on_a_cursor_the_server_already_returned() {
    let server = PagingServer::new(Paging::Cyclic);
    let requests = Arc::clone(&server.tools);
    let toolset = McpToolset::new(connect(server).await);

    let error = toolset.tools(ctx()).await.err().expect("a cyclic listing must fail");

    assert!(error.to_string().contains("cursor \"a\" twice"), "{error}");
    assert!(requests.load(Ordering::SeqCst) <= 3);
}

#[tokio::test(start_paused = true)]
async fn tool_discovery_that_never_finishes_times_out() {
    let limits = McpDiscoveryLimits::default().with_timeout(Duration::from_secs(5));
    let toolset = McpToolset::new(connect(PagingServer::new(Paging::Hung)).await)
        .with_discovery_limits(limits);

    let started = tokio::time::Instant::now();
    let error = toolset.tools(ctx()).await.err().expect("a hung listing must fail");

    assert!(error.to_string().contains("did not finish within 5s"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(6));
}

#[tokio::test]
async fn resource_template_and_prompt_listings_stop_at_the_page_limit() {
    let server = PagingServer::new(Paging::Endless);
    let counters = server.clone();
    let toolset = McpToolset::new(connect(server).await)
        .with_discovery_limits(McpDiscoveryLimits::default().with_max_pages(3));

    let errors = [
        toolset.list_resources().await.err().map(|error| error.to_string()),
        toolset.list_resource_templates().await.err().map(|error| error.to_string()),
        toolset.list_prompts().await.err().map(|error| error.to_string()),
    ];

    for (error, method) in
        errors.iter().zip(["resources/list", "resources/templates/list", "prompts/list"])
    {
        let error = error.as_deref().expect("an endless listing must fail");
        assert!(error.contains(&format!("after 3 pages of {method}")), "{error}");
    }
    let requests = [&counters.resources, &counters.templates, &counters.prompts]
        .map(|counter| counter.load(Ordering::SeqCst));
    assert_eq!(requests, [3, 3, 3]);
}

/// Hands out connections to an endless server and counts them.
#[derive(Default)]
struct EndlessFactory {
    connections: AtomicUsize,
}

#[async_trait::async_trait]
impl ConnectionFactory<()> for EndlessFactory {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        Ok(connect(PagingServer::new(Paging::Endless)).await)
    }
}

#[tokio::test]
async fn simple_client_and_connection_refresher_listings_are_bounded() {
    let limits = McpDiscoveryLimits::default().with_max_pages(4);

    let server = PagingServer::new(Paging::Endless);
    let requests = Arc::clone(&server.tools);
    let simple = SimpleClient::new(connect(server).await).with_discovery_limits(limits);
    let error = simple.list_tools().await.expect_err("an endless listing must fail");
    assert!(error.contains("after 4 pages"), "{error}");
    assert_eq!(requests.load(Ordering::SeqCst), 4);

    let factory = Arc::new(EndlessFactory::default());
    let refresher = ConnectionRefresher::new(
        connect(PagingServer::new(Paging::Endless)).await,
        Arc::clone(&factory),
    )
    .with_config(RefreshConfig::default().with_retry_delay_ms(0).without_logging())
    .with_discovery_limits(limits);
    let error = refresher.list_tools().await.expect_err("an endless listing must fail");
    assert!(error.contains("after 4 pages"), "{error}");
    assert_eq!(
        factory.connections.load(Ordering::SeqCst),
        0,
        "a page limit is not a lost connection"
    );
}

/// Publishes one tool named `v{version}`. With `hold_first`, the first listing reads the
/// version on arrival and answers only after `release`.
#[derive(Clone, Default)]
struct Versioned {
    version: Arc<AtomicUsize>,
    lists: Arc<AtomicUsize>,
    hold_first: bool,
    ttl: bool,
    release: Arc<Notify>,
}

impl ServerHandler for Versioned {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let version = self.version.load(Ordering::SeqCst);
        if self.lists.fetch_add(1, Ordering::SeqCst) == 0 && self.hold_first {
            self.release.notified().await;
        }
        let mut result = ListToolsResult::with_all_items(vec![tool(format!("v{version}"))]);
        result.ttl_ms = self.ttl.then_some(60_000);
        Ok(result)
    }
}

async fn names(toolset: &McpToolset) -> Vec<String> {
    toolset.tools(ctx()).await.unwrap().iter().map(|tool| tool.name().to_string()).collect()
}

#[tokio::test]
async fn invalidating_during_a_fetch_keeps_its_result_out_of_the_cache() {
    let server = Versioned { hold_first: true, ..Versioned::default() };
    server.version.store(1, Ordering::SeqCst);
    let (version, lists, release) =
        (Arc::clone(&server.version), Arc::clone(&server.lists), Arc::clone(&server.release));
    let toolset = McpToolset::new(connect(server).await);

    let in_flight = tokio::spawn({
        let toolset = toolset.clone();
        async move { names(&toolset).await }
    });
    while lists.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    version.store(2, Ordering::SeqCst);
    toolset.invalidate_tool_list_cache().await;
    release.notify_one();

    assert_eq!(in_flight.await.unwrap(), vec!["v1"], "the call in flight returns what it fetched");
    assert_eq!(names(&toolset).await, vec!["v2"]);
    assert_eq!(lists.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn invalidation_also_clears_pages_the_server_marked_fresh() {
    let server = Versioned { ttl: true, ..Versioned::default() };
    server.version.store(1, Ordering::SeqCst);
    let (version, lists) = (Arc::clone(&server.version), Arc::clone(&server.lists));
    let toolset = McpToolset::new(connect(server).await);

    assert_eq!(names(&toolset).await, vec!["v1"]);
    version.store(2, Ordering::SeqCst);
    toolset.invalidate_tool_list_cache().await;

    assert_eq!(names(&toolset).await, vec!["v2"]);
    assert_eq!(lists.load(Ordering::SeqCst), 2);
}

#[cfg(feature = "http-transport")]
mod http {
    use super::*;
    use adk_tool::mcp::McpHttpClientBuilder;
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use serde_json::{Value, json};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Counters {
        initializes: AtomicUsize,
        lists: AtomicUsize,
        list_sessions: Mutex<Vec<String>>,
    }

    /// Answers the first `tools/list` with HTTP 500 and a body full of words the
    /// old text matching took for a lost connection.
    async fn mcp(
        State(counters): State<Arc<Counters>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let Some(id) = message.get("id").cloned() else {
            return StatusCode::ACCEPTED.into_response();
        };
        let reply = |result: Value| {
            (
                [(header::CONTENT_TYPE, "application/json")],
                json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
            )
                .into_response()
        };
        match message["method"].as_str() {
            Some("initialize") => {
                let session = counters.initializes.fetch_add(1, Ordering::SeqCst) + 1;
                let mut response = reply(json!({
                    "protocolVersion": message["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "flaky", "version": "1.0.0" }
                }));
                response
                    .headers_mut()
                    .insert("mcp-session-id", format!("s{session}").parse().unwrap());
                response
            }
            Some("tools/list") => {
                let session = headers.get("mcp-session-id").and_then(|value| value.to_str().ok());
                counters.list_sessions.lock().unwrap().push(session.unwrap_or("none").to_string());
                if counters.lists.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "text/plain")],
                        "upstream geoffrey: EOF; Transport closed; connection reset; session not found",
                    )
                        .into_response();
                }
                reply(json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }))
            }
            _ => reply(json!({})),
        }
    }

    #[tokio::test]
    async fn a_5xx_whose_body_mentions_eof_keeps_the_session() {
        let counters = Arc::new(Counters::default());
        let router = Router::new()
            .route(
                "/mcp",
                post(mcp)
                    .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                    .delete(|| async { StatusCode::OK }),
            )
            .with_state(Arc::clone(&counters));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });

        let toolset = McpHttpClientBuilder::new(url)
            .timeout(Duration::from_secs(5))
            .connect()
            .await
            .unwrap()
            .with_refresh_config(RefreshConfig::default().with_retry_delay_ms(0).without_logging())
            .with_tool_list_cache_ttl(Duration::ZERO);

        let error = toolset.tools(ctx()).await.err().expect("the 500 must surface");
        assert!(error.to_string().contains("HTTP 500"), "{error}");
        let tools = toolset.tools(ctx()).await.unwrap();

        assert_eq!(tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["echo"]);
        assert_eq!(counters.initializes.load(Ordering::SeqCst), 1, "a 5xx must not reconnect");
        assert_eq!(*counters.list_sessions.lock().unwrap(), vec!["s1", "s1"]);
    }
}
