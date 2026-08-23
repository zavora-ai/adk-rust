//! MCP warehouse server used by the advanced runtime gallery.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CreateTaskResult,
    GetTaskParams, GetTaskResult, Implementation, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskManager, TaskOptions};
use rmcp::transport::stdio;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool, tool_router,
};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StockArgs {
    /// Item to count.
    item: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RestockArgs {
    /// Item to restock.
    item: String,
    /// Number of units to add.
    units: u32,
}

struct Warehouse {
    tool_router: ToolRouter<Self>,
    tasks: TaskManager,
    stock: Arc<Mutex<HashMap<String, u32>>>,
}

#[tool_router]
impl Warehouse {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            tasks: TaskManager::new(),
            stock: Arc::new(Mutex::new(HashMap::from([("widgets".to_string(), 7)]))),
        }
    }

    #[tool(description = "Count the stock for an item. This completes immediately.")]
    async fn count_stock(
        &self,
        Parameters(StockArgs { item }): Parameters<StockArgs>,
    ) -> Result<CallToolResult, McpError> {
        let units = self.stock.lock().expect("stock mutex").get(&item).copied().unwrap_or(0);
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "{item}: {units} units in stock"
        ))]))
    }

    #[tool(description = "Restock an item. This may complete as an MCP task.")]
    async fn restock_warehouse(
        &self,
        Parameters(RestockArgs { item, units }): Parameters<RestockArgs>,
    ) -> Result<CallToolResult, McpError> {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        Ok(restock(&self.stock, &item, units, "inline"))
    }
}

impl ServerHandler for Warehouse {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_server_info(Implementation::new("advanced-warehouse", env!("CARGO_PKG_VERSION")))
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
        let client_supports_tasks =
            context.client_capabilities().is_some_and(|capabilities| capabilities.supports_tasks());
        if params.name == "restock_warehouse" && client_supports_tasks {
            let args: RestockArgs = serde_json::from_value(serde_json::Value::Object(
                params.arguments.clone().unwrap_or_default(),
            ))
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
            let stock = Arc::clone(&self.stock);
            let task = self.tasks.spawn(TaskOptions::default(), move |context| {
                Box::pin(async move {
                    tokio::select! {
                        _ = context.cancelled() => Err(rmcp::task_manager::TaskExit::Cancelled),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                            Ok(restock(&stock, &args.item, args.units, "SEP-2663 task"))
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

fn restock(
    stock: &Mutex<HashMap<String, u32>>,
    item: &str,
    units: u32,
    completion: &str,
) -> CallToolResult {
    let total = {
        let mut stock = stock.lock().expect("stock mutex");
        let entry = stock.entry(item.to_string()).or_insert(0);
        *entry += units;
        *entry
    };
    CallToolResult::success(vec![ContentBlock::text(format!(
        "{item}: added {units} units, now {total} in stock ({completion})"
    ))])
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string()))
        .init();
    let running = Warehouse::new().serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
