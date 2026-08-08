//! An MCP server that supports protocol revision `2026-07-28`.
//!
//! It exists because, at the time of writing, no public MCP server advertises
//! `2026-07-28` on the wire — the official reference server answers
//! `server/discover` with `Method not found`. So the revision cannot be
//! demonstrated against a third party, and this server stands in.
//!
//! Two tools, chosen to show the one rule that matters most in SEP-2663:
//!
//! - `count_stock` answers immediately.
//! - `restock_warehouse` takes two seconds. The server returns a **task** for
//!   it, but only to a client that declared the tasks extension. A client that
//!   did not gets the same answer inline.
//!
//! The server never asks the client to use tasks. It decides per call.

use rmcp::handler::server::router::tool::ToolRouter;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CreateTaskResult,
    GetTaskParams, GetTaskResult, Implementation, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskManager, TaskOptions};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool, tool_router};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StockArgs {
    /// The item to count.
    pub item: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RestockArgs {
    /// The item to restock.
    pub item: String,
    /// How many units to add.
    pub units: u32,
}

pub struct Warehouse {
    tool_router: ToolRouter<Warehouse>,
    tasks: TaskManager,
    /// Real stock levels, so `count_stock` reflects what `restock_warehouse`
    /// did. A stateless server makes the agent report a contradiction.
    stock: Arc<Mutex<HashMap<String, u32>>>,
}

impl Default for Warehouse {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl Warehouse {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            tasks: TaskManager::new(),
            stock: Arc::new(Mutex::new(HashMap::from([("widgets".to_string(), 7)]))),
        }
    }

    #[tool(description = "Count how many units of an item are in stock. Answers immediately.")]
    async fn count_stock(
        &self,
        Parameters(StockArgs { item }): Parameters<StockArgs>,
    ) -> Result<CallToolResult, McpError> {
        let units = self.stock.lock().expect("stock mutex").get(&item).copied().unwrap_or(0);
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "{item}: {units} units in stock"
        ))]))
    }

    #[tool(description = "Restock an item. Takes about two seconds to complete.")]
    async fn restock_warehouse(
        &self,
        Parameters(RestockArgs { item, units }): Parameters<RestockArgs>,
    ) -> Result<CallToolResult, McpError> {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let total = {
            let mut stock = self.stock.lock().expect("stock mutex");
            let entry = stock.entry(item.clone()).or_insert(0);
            *entry += units;
            *entry
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "{item}: added {units} units, now {total} in stock"
        ))]))
    }
}

impl ServerHandler for Warehouse {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_server_info(Implementation::new("warehouse", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tool_router.list_all()))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // SEP-2663: never return a task to a client that did not declare the
        // extension. The client cannot poll for a result it does not expect.
        let client_declared_tasks =
            context.client_capabilities().is_some_and(|caps| caps.supports_tasks());

        if params.name == "restock_warehouse" && client_declared_tasks {
            let args: RestockArgs = serde_json::from_value(serde_json::Value::Object(
                params.arguments.clone().unwrap_or_default(),
            ))
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;

            let stock = Arc::clone(&self.stock);
            let task = self.tasks.spawn(TaskOptions::default(), move |ctx| {
                Box::pin(async move {
                    tokio::select! {
                        _ = ctx.cancelled() => Err(rmcp::task_manager::TaskExit::Cancelled),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                            let total = {
                                let mut stock = stock.lock().expect("stock mutex");
                                let entry = stock.entry(args.item.clone()).or_insert(0);
                                *entry += args.units;
                                *entry
                            };
                            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                                "{}: added {} units, now {total} in stock (completed as a task)",
                                args.item, args.units
                            ))]))
                        }
                    }
                })
            });
            return Ok(CallToolResponse::Task(CreateTaskResult::new(task)));
        }

        let call = rmcp::handler::server::tool::ToolCallContext::new(self, params, context);
        self.tool_router.call(call).await
    }

    async fn get_task(
        &self,
        params: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        Ok(GetTaskResult::new(self.tasks.get_task(&params.task_id)?))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stderr only: stdout carries the protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string()))
        .init();
    let running = Warehouse::new().serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
