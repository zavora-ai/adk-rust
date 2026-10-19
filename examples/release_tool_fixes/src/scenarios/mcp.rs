//! Item 2: the text of an MCP `isError` result reaches the model, which can follow it.
//! Item 7: `McpHttpClientBuilder::connect` keeps the `mcp.oauth.token_fetch` code.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use adk_tool::McpToolset;
use adk_tool::mcp::{McpAuth, McpHttpClientBuilder, OAuth2Config};
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{Value, json};

use crate::common::{LIVE_MODEL, Mode, Report, Verdict, run_turn, skip, verdict};
use crate::upstream::{Reply, Upstream, tool_messages};

/// Runs this binary as a stdio MCP server.
pub const SERVER_FLAG: &str = "--mcp-server";

/// The only ID `lookup_order` accepts; the model learns it from the error text alone.
const MIGRATED_ID: &str = "MIG-55X2-7781";
/// What a successful lookup returns; only a corrected call can produce it.
const TRACKING_CODE: &str = "TRK-4Q8Z";

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

/// `lookup_order`, which answers an outdated order ID with `isError` and the new ID.
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
        Ok(ListToolsResult::with_all_items(vec![order_tool()]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let order = params
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("order_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = if order == MIGRATED_ID {
            CallToolResult::success(vec![ContentBlock::text(format!(
                "order {MIGRATED_ID} has shipped; tracking code {TRACKING_CODE}"
            ))])
        } else {
            CallToolResult::error(vec![ContentBlock::text(format!(
                "order '{order}' was migrated to the new order system. Its new ID is \
                 {MIGRATED_ID}; call lookup_order again with order_id \"{MIGRATED_ID}\"."
            ))])
        };
        Ok(result.into())
    }
}

/// Serves `Orders` over this process's stdin and stdout until the client hangs up.
pub async fn serve_stdio() -> anyhow::Result<()> {
    let running = Orders.serve((tokio::io::stdin(), tokio::io::stdout())).await?;
    running.waiting().await?;
    Ok(())
}

async fn toolset() -> anyhow::Result<McpToolset> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command.arg(SERVER_FLAG);
    let client = ().serve(TokioChildProcess::new(command)?).await?;
    Ok(McpToolset::new(client).with_name("orders"))
}

pub async fn run(gemini_key: Option<&str>, report: &mut Report) {
    report.record_result("mcp-is-error-text-reaches-model", 2, Mode::Scripted, scripted().await);
    match gemini_key {
        Some(key) => {
            report.record_result("mcp-is-error-live-self-correction", 2, Mode::Live, live(key).await)
        }
        None => report.record(
            "mcp-is-error-live-self-correction",
            2,
            Mode::Live,
            skip("no Gemini key"),
        ),
    }
    report.record_result("mcp-oauth-connect-error-code", 7, Mode::Scripted, oauth_code().await);
}

/// The function response the model receives carries the server's guidance.
async fn scripted() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start(vec![
        Reply::ToolCall { name: "lookup_order", args: json!({"order_id": "7781"}) },
        Reply::Text("I could not find it."),
    ])
    .await?;
    let toolset = toolset().await?;
    let agent = LlmAgentBuilder::new("support")
        .instruction("Look up orders with lookup_order.")
        .model(Arc::new(upstream.client()?))
        .toolset(Arc::new(toolset))
        .build()?;
    let turn = run_turn(Arc::new(agent), "Where is order 7781?").await?;
    let captured = upstream.captured();
    let tool_text = captured.get(1).map(tool_messages).unwrap_or_default();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("run error: {:?}", turn.error)),
        (captured.len() == 2, format!("{} model requests", captured.len())),
        (tool_text.contains(MIGRATED_ID), format!("tool message: {tool_text}")),
    ]))
}

/// A live model reads the guidance and calls the tool again with the new ID.
async fn live(key: &str) -> anyhow::Result<Verdict> {
    let toolset = toolset().await?;
    let agent = LlmAgentBuilder::new("support")
        .instruction(
            "Look up orders with lookup_order. When the tool reports an error, follow its \
             instructions. Reply with the tracking code.",
        )
        .model(Arc::new(GeminiModel::new(key, LIVE_MODEL)?))
        .toolset(Arc::new(toolset))
        .build()?;
    let turn = run_turn(Arc::new(agent), "What is the tracking code of order 7781?").await?;
    let calls = turn.calls_to("lookup_order");
    let retried = calls.iter().any(|args| args["order_id"] == MIGRATED_ID);
    let answer = turn.answer();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("run error: {:?}", turn.error)),
        (retried, format!("calls: {calls:?}")),
        (answer.contains(TRACKING_CODE), format!("answer: {}", answer.trim())),
    ]))
}

/// A token endpoint that rejects the client credentials fails `connect` with the OAuth code.
async fn oauth_code() -> anyhow::Result<Verdict> {
    let app = Router::new()
        .route("/token", post(|| async { (StatusCode::UNAUTHORIZED, "invalid_client") }))
        .route("/mcp", post(|| async { StatusCode::INTERNAL_SERVER_ERROR }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let oauth = OAuth2Config::new("client", format!("{base}/token")).with_secret("secret");
    let error = match McpHttpClientBuilder::new(format!("{base}/mcp"))
        .with_auth(McpAuth::oauth2(oauth))
        .connect()
        .await
    {
        Ok(_) => return Ok(Verdict::Fail("connect succeeded against a rejecting token endpoint".into())),
        Err(error) => error,
    };
    Ok(verdict(vec![
        (error.code == "mcp.oauth.token_fetch", format!("code {}", error.code)),
        (error.is_unauthorized(), format!("category {:?}", error.category)),
    ]))
}
