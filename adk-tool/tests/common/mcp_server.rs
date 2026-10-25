//! A live in-process MCP server for the `mcp_server_*` tests: its tools, an executor that
//! records and gates calls, a bearer-token caller resolver, and a server task stopped on drop.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, ReadonlyContext, Result, Tool, ToolContext,
    ToolEffect, Url,
};
use adk_tool::mcp::server::{
    DirectExecutor, McpCallExecutor, McpCaller, McpCallerResolver, McpHttpRequest,
    McpRouterBuilder, McpServer, McpToolCall, McpToolOutcome,
};
use adk_tool::{FunctionTool, SimpleToolContext};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The arguments of `get_quote`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct QuoteArgs {
    /// The installation's size in kilowatts.
    pub kw: u32,
}

/// The result of `get_quote`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Quote {
    pub kw: u32,
    pub price: u32,
}

/// Books an installation: non-idempotent, but declares additive and closed-world hints.
pub struct BookInstall;

static BOOK_HINTS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(
    || json!({ "title": "Book an installation", "destructiveHint": false, "openWorldHint": false }),
);

#[async_trait]
impl Tool for BookInstall {
    fn name(&self) -> &str {
        "book_install"
    }
    fn description(&self) -> &str {
        "Books an installation date"
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "date": { "type": "string" } },
            "required": ["date"],
        }))
    }
    fn annotations(&self) -> Option<&Value> {
        Some(&BOOK_HINTS)
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        Ok(json!({ "booking": format!("B-{}", args["date"].as_str().unwrap_or_default()) }))
    }
}

/// Every published tool.
pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(
            FunctionTool::new(
                "get_quote",
                "Quotes a solar installation",
                |_ctx, args| async move {
                    let kw = args["kw"].as_u64().unwrap_or_default() as u32;
                    Ok(serde_json::to_value(Quote { kw, price: kw * 1000 }).unwrap())
                },
            )
            .with_parameters_schema::<QuoteArgs>()
            .with_response_schema::<Quote>()
            .with_read_only(true),
        ),
        Arc::new(
            FunctionTool::new(
                "set_reminder",
                "Sets the follow-up reminder",
                |_ctx, args| async move { Ok(json!(format!("reminder set for {}", args["day"]))) },
            )
            .with_effect(ToolEffect::Idempotent),
        ),
        Arc::new(BookInstall),
        Arc::new(FunctionTool::new("lookup_order", "Looks up an order", |_ctx, _args| async {
            Err(AdkError::unavailable(
                ErrorComponent::Tool,
                "orders.db",
                "connect postgres://admin:hunter2@db.internal/prod failed",
            ))
        })),
        Arc::new(FunctionTool::new("cancel_order", "Cancels an order", |_ctx, _args| async {
            Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Conflict,
                "orders.shipped",
                "order A-1 has shipped and can no longer be cancelled",
            ))
        })),
        Arc::new(
            FunctionTool::new("admin_report", "Reports revenue", |_ctx, _args| async {
                Ok(json!({ "revenue": 1 }))
            })
            .with_read_only(true),
        ),
    ]
}

/// One call the executor saw.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub tool: String,
    pub caller: String,
    pub arguments: Value,
    pub request_id: String,
    pub session_id: Option<String>,
}

/// Records every call; anonymous callers cannot see `admin_report` and are refused
/// `book_install`.
#[derive(Clone)]
pub struct GateExecutor {
    pub seen: Arc<Mutex<Vec<Seen>>>,
    inner: DirectExecutor,
}

impl GateExecutor {
    pub fn new() -> Self {
        Self { seen: Arc::default(), inner: DirectExecutor::new("com.example/solar") }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl McpCallExecutor for GateExecutor {
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
        self.seen.lock().unwrap().push(Seen {
            tool: call.tool.name().to_string(),
            caller: call.caller.id().to_string(),
            arguments: call.arguments.clone(),
            request_id: call.request_id.clone(),
            session_id: call.session_id.clone(),
        });
        if call.tool.name() == "book_install" && !call.caller.is_authenticated() {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "solar.booking.known_tier",
                "booking needs a caller the site knows",
            ));
        }
        self.inner.execute(call).await
    }

    fn is_visible(&self, tool: &dyn Tool, caller: &McpCaller) -> bool {
        tool.name() != "admin_report" || caller.is_authenticated()
    }
}

/// Callers holding a token the site issued; others are anonymous, or refused when `required`.
pub struct Tokens {
    pub holders: HashMap<String, String>,
    pub required: bool,
}

impl Tokens {
    pub fn new(required: bool) -> Self {
        let holders = HashMap::from([
            ("token-acme".to_string(), "partner:acme".to_string()),
            ("token-globex".to_string(), "partner:globex".to_string()),
        ]);
        Self { holders, required }
    }
}

#[async_trait]
impl McpCallerResolver for Tokens {
    async fn resolve(&self, request: &McpHttpRequest<'_>) -> Result<McpCaller> {
        match request.bearer_token().map(|token| self.holders.get(token)) {
            Some(Some(holder)) => Ok(McpCaller::authenticated(holder.clone())),
            Some(None) | None if self.required => Err(AdkError::unauthorized(
                ErrorComponent::Auth,
                "solar.caller.token",
                "a bearer token issued by Example Solar is required",
            )),
            Some(None) => Err(AdkError::unauthorized(
                ErrorComponent::Auth,
                "solar.caller.token",
                "the bearer token is not one Example Solar issued",
            )),
            None => Ok(McpCaller::anonymous()),
        }
    }
}

/// The test server: an `McpServer` over the `GateExecutor`.
pub fn server(executor: GateExecutor) -> McpServer {
    McpServer::builder("com.example/solar", "1.0.0")
        .description("Quotes and bookings for Example Solar")
        .title("Example Solar")
        .open_world(false)
        .tools(tools())
        .executor(Arc::new(executor))
        .build()
        .unwrap()
}

/// A running server, stopped when dropped.
pub struct Running {
    pub base: String,
    task: tokio::task::JoinHandle<()>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Running {
    /// The MCP endpoint.
    pub fn endpoint(&self) -> String {
        format!("{}/mcp", self.base)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

/// Binds a loopback port and serves the router `configure` builds for its public URL.
pub async fn start(
    server: McpServer,
    configure: impl FnOnce(McpRouterBuilder) -> McpRouterBuilder,
) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = tokio_util::sync::CancellationToken::new();
    let builder = McpRouterBuilder::new(server)
        .public_url(Url::parse(&format!("{base}/mcp")).unwrap())
        .cancellation_token(cancel.clone());
    let router = configure(builder).build().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    Running { base, task, cancel }
}

/// A context for listing tools.
pub fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-server-tests"))
}

/// A context for executing a tool.
pub fn tool_ctx() -> Arc<dyn ToolContext> {
    Arc::new(SimpleToolContext::new("mcp-server-tests"))
}
