//! Scripted MCP servers, served in-process over a pipe or as a stdio child of this binary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    Tool,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::{RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use tokio::sync::Notify;

/// The order status the healthy server returns; only a real tool call can produce it.
pub const TRACKING_CODE: &str = "TRK-7Q4";

/// Argument used by `--mcp-fixture <mode>` to run this binary as a stdio MCP server.
pub const FIXTURE_FLAG: &str = "--mcp-fixture";

fn object_schema() -> Arc<JsonObject> {
    Arc::new(
        json!({
            "type": "object",
            "properties": { "order_id": { "type": "string" } }
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    )
}

/// A tool with a small object schema.
pub fn tool(name: &str, description: &str) -> Tool {
    Tool::new(name.to_string(), description.to_string(), object_schema())
}

fn tools_capability() -> InitializeResult {
    InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
}

/// Serves `handler` in-process and returns a client connected to it.
pub async fn connect<H: ServerHandler>(
    handler: H,
) -> anyhow::Result<RunningService<RoleClient, ()>> {
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = handler.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    Ok(().serve(client_io).await?)
}

/// Returns a fresh `nextCursor` on every `tools/list` page, so the listing never ends.
#[derive(Clone, Default)]
pub struct EndlessCursor {
    pub lists: Arc<AtomicUsize>,
}

impl ServerHandler for EndlessCursor {
    fn get_info(&self) -> InitializeResult {
        tools_capability()
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let page = self.lists.fetch_add(1, Ordering::SeqCst) + 1;
        let mut result =
            ListToolsResult::with_all_items(vec![tool(&format!("page_{page}"), "One page.")]);
        result.next_cursor = Some(format!("cursor-{page}"));
        Ok(result)
    }
}

/// Pages `a -> b -> a` with a one-minute `ttlMs`, so a client that follows the cursor
/// loops on its own response cache.
#[derive(Clone, Default)]
pub struct CyclicCursor {
    pub lists: Arc<AtomicUsize>,
}

impl ServerHandler for CyclicCursor {
    fn get_info(&self) -> InitializeResult {
        tools_capability()
    }

    async fn list_tools(
        &self,
        params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let cursor = params.and_then(|params| params.cursor);
        let next = match cursor.as_deref() {
            Some("a") => "b",
            _ => "a",
        };
        let mut result =
            ListToolsResult::with_all_items(vec![tool(&format!("tool_{next}"), "Cycled.")]);
        result.next_cursor = Some(next.to_string());
        result.ttl_ms = Some(60_000);
        Ok(result)
    }
}

/// A fixed catalog whose tools answer with their own name, except `lookup_order`.
#[derive(Clone)]
pub struct Catalog {
    label: String,
    tools: Vec<Tool>,
    pub calls: Arc<Mutex<Vec<String>>>,
}

impl Catalog {
    pub fn new(label: &str, tools: Vec<Tool>) -> Self {
        Self { label: label.to_string(), tools, calls: Arc::default() }
    }

    /// The healthy order service: one `lookup_order` tool.
    pub fn orders() -> Self {
        Self::new(
            "orders",
            vec![tool(
                "lookup_order",
                "Looks up the shipping status and tracking code of an order.",
            )],
        )
    }

    pub fn calls_to(&self, name: &str) -> usize {
        self.calls
            .lock()
            .map(|calls| calls.iter().filter(|call| *call == name).count())
            .unwrap_or(0)
    }
}

impl ServerHandler for Catalog {
    fn get_info(&self) -> InitializeResult {
        tools_capability()
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = params.name.to_string();
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(name.clone());
        }
        let text = if name == "lookup_order" {
            format!("order A-1042 has shipped; tracking code {TRACKING_CODE}")
        } else {
            format!("{name} answered by fixture {}", self.label)
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

/// Never answers `tools/list`.
#[derive(Clone, Default)]
pub struct HungListing;

impl ServerHandler for HungListing {
    fn get_info(&self) -> InitializeResult {
        tools_capability()
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        std::future::pending::<()>().await;
        Ok(ListToolsResult::default())
    }
}

/// Publishes one tool named `v{version}`; the first listing waits for `release`, and
/// every listing carries a one-minute `ttlMs` when `ttl` is set.
#[derive(Clone, Default)]
pub struct Versioned {
    pub version: Arc<AtomicUsize>,
    pub lists: Arc<AtomicUsize>,
    pub hold_first: bool,
    pub ttl: bool,
    pub release: Arc<Notify>,
}

impl ServerHandler for Versioned {
    fn get_info(&self) -> InitializeResult {
        tools_capability()
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let version = self.version.load(Ordering::SeqCst);
        let listing = self.lists.fetch_add(1, Ordering::SeqCst) + 1;
        if self.hold_first && listing == 1 {
            self.release.notified().await;
        }
        let mut result =
            ListToolsResult::with_all_items(vec![tool(&format!("v{version}"), "Versioned.")]);
        if self.ttl {
            result.ttl_ms = Some(60_000);
        }
        Ok(result)
    }
}

/// Runs this binary as a stdio MCP server for `mode`.
pub async fn serve_stdio(mode: &str) -> anyhow::Result<()> {
    let stdio = (tokio::io::stdin(), tokio::io::stdout());
    match mode {
        "hung" => HungListing.serve(stdio).await?.waiting().await?,
        "orders" => Catalog::orders().serve(stdio).await?.waiting().await?,
        "a" => {
            Catalog::new("a", vec![tool("b__x", "Tool b__x.")])
                .serve(stdio)
                .await?
                .waiting()
                .await?
        }
        "a__b" => {
            Catalog::new("a__b", vec![tool("x", "Tool x.")]).serve(stdio).await?.waiting().await?
        }
        "z" => {
            Catalog::new("z", vec![tool("b__x", "Tool b__x."), tool("x", "Tool x.")])
                .serve(stdio)
                .await?
                .waiting()
                .await?
        }
        other => anyhow::bail!("unknown fixture mode {other}"),
    };
    Ok(())
}
