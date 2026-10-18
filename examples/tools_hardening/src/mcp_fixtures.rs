//! Hostile MCP servers, served in-process over a pipe or as stdio children of this binary.

use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    Tool,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::{RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

/// Argument used by `--mcp-fixture <mode>` to run this binary as a stdio MCP server.
pub const FIXTURE_FLAG: &str = "--mcp-fixture";

/// The tracking code `lookup_order` returns; only a real tool call can produce it.
pub const TRACKING_CODE: &str = "TRK-7Q4";

/// A 200-byte schema whose definition refers to itself three times.
pub fn self_referencing_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "root": { "$ref": "#/$defs/Node" } },
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "label": { "type": "string" },
                    "left": { "$ref": "#/$defs/Node" },
                    "right": { "$ref": "#/$defs/Node" },
                    "next": { "$ref": "#/$defs/Node" }
                }
            }
        }
    })
}

fn object(value: Value) -> Arc<JsonObject> {
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

/// A tool with one optional string argument.
pub fn simple_tool(name: &str, description: &str, argument: &str) -> Tool {
    Tool::new(
        name.to_string(),
        description.to_string(),
        object(json!({
            "type": "object",
            "properties": { argument: { "type": "string" } }
        })),
    )
}

/// A tool with the self-referencing schema.
pub fn tree_tool() -> Tool {
    Tool::new(
        "count_tree_nodes".to_string(),
        "Counts the nodes of a tree. Pass the tree as `root`; a node has a label and \
         optional left, right and next children."
            .to_string(),
        object(self_referencing_schema()),
    )
}

/// What a fixture's tool answers.
#[derive(Clone)]
pub enum Reply {
    Text(String),
    /// A successful result with no content blocks.
    Empty,
}

/// A fixed catalog that records every call.
#[derive(Clone)]
pub struct Catalog {
    tools: Vec<(Tool, Reply)>,
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Catalog {
    pub fn new(tools: Vec<(Tool, Reply)>) -> Self {
        Self { tools, calls: Arc::default() }
    }

    /// The healthy order service.
    pub fn orders() -> Self {
        Self::new(vec![(
            simple_tool(
                "lookup_order",
                "Looks up the shipping status and tracking code of an order.",
                "order_id",
            ),
            Reply::Text(format!("order A-1042 has shipped; tracking code {TRACKING_CODE}")),
        )])
    }

    pub fn calls_to(&self, name: &str) -> usize {
        self.calls.lock().map(|calls| calls.iter().filter(|(n, _)| n == name).count()).unwrap_or(0)
    }
}

impl ServerHandler for Catalog {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.tools.iter().map(|(tool, _)| tool.clone()).collect(),
        ))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = params.name.to_string();
        let args = params.arguments.map(Value::Object).unwrap_or(Value::Null);
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((name.clone(), args));
        }
        let reply = self.tools.iter().find(|(tool, _)| tool.name == name).map(|(_, r)| r.clone());
        let result = match reply {
            Some(Reply::Text(text)) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Some(Reply::Empty) => CallToolResult::success(vec![]),
            None => CallToolResult::error(vec![ContentBlock::text(format!("no tool {name}"))]),
        };
        Ok(result.into())
    }
}

/// Serves `handler` in-process and returns a client connected to it.
pub async fn connect<H: ServerHandler>(
    handler: H,
) -> anyhow::Result<RunningService<RoleClient, ()>> {
    let (server_io, client_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = handler.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    Ok(().serve(client_io).await?)
}

/// Runs this binary as a stdio MCP server for `mode`.
pub async fn serve_stdio(mode: &str) -> anyhow::Result<()> {
    match mode {
        "orders" => {
            let stdio = (tokio::io::stdin(), tokio::io::stdout());
            Catalog::orders().serve(stdio).await?.waiting().await?;
        }
        // Reads the client's `initialize` and never answers it; exits when stdin closes.
        "hung-handshake" => {
            let mut stdin = tokio::io::stdin();
            let mut buffer = [0u8; 4096];
            while stdin.read(&mut buffer).await? > 0 {}
        }
        other => anyhow::bail!("unknown fixture mode {other}"),
    }
    Ok(())
}
