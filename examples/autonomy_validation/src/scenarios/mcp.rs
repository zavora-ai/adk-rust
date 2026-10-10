//! `mcp_concurrency`: two tool calls from one model response share an MCP connection without
//! waiting for each other (#760).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm, Part, ToolExecutionStrategy};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::McpToolset;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use tokio::sync::Barrier;

use crate::common::{Config, Provider, Verdict, brief, fail, pair_calls, run_turn, session_events};

/// How long each tool waits at the server for the other one to arrive.
const OVERLAP_WINDOW: Duration = Duration::from_secs(8);

/// Each tool returns only once both are in flight or the window closes, so a client that
/// serializes requests on one connection cannot overlap them.
#[derive(Clone)]
struct OverlapServer {
    rendezvous: Arc<Barrier>,
    overlapped: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl OverlapServer {
    fn new() -> Self {
        Self {
            rendezvous: Arc::new(Barrier::new(2)),
            overlapped: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ServerHandler for OverlapServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(
            json!({ "type": "object", "properties": {} }).as_object().cloned().unwrap_or_default(),
        );
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new(
                "slow_report",
                "Builds the nightly report. Takes several seconds.",
                Arc::clone(&schema),
            ),
            Tool::new("quick_ping", "Checks that the reporting service is reachable.", schema),
        ]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = match params.name.as_ref() {
            name @ ("slow_report" | "quick_ping") => {
                let met =
                    tokio::time::timeout(OVERLAP_WINDOW, self.rendezvous.wait()).await.is_ok();
                if met {
                    self.overlapped.store(true, Ordering::SeqCst);
                }
                format!("{name} done (overlapped: {met})")
            }
            other => return Err(ErrorData::invalid_params(format!("unknown tool {other}"), None)),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

pub async fn run(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let server = OverlapServer::new();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let served = server.clone();
    tokio::spawn(async move {
        if let Ok(running) = served.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await?;
    let toolset = McpToolset::new(client);

    let agent = LlmAgentBuilder::new("reporter")
        .model(cfg.model(provider)? as Arc<dyn Llm>)
        .instruction(
            "You operate the reporting service. When asked for the nightly report, call slow_report \
             and quick_ping together in one response (parallel tool calls), then summarise both \
             results in one sentence.",
        )
        .toolset(Arc::new(toolset))
        .tool_execution_strategy(ToolExecutionStrategy::Parallel)
        .build()?;
    let app = "autonomy-mcp";
    let (user, session_id) = ("user-mcp", "mcp-overlap");
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .build()?;

    let turn = run_turn(
        &runner,
        user,
        session_id,
        "Produce the nightly report and check the service at the same time.",
    )
    .await;
    if let Some(error) = &turn.error {
        return Ok(Verdict::Fail(format!("turn failed: {}", brief(error))));
    }
    let events = session_events(sessions.as_ref(), app, user, session_id).await?;
    let same_response = events.iter().any(|event| {
        event.llm_response.content.as_ref().is_some_and(|content| {
            let names: Vec<&str> = content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::FunctionCall { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            names.contains(&"slow_report") && names.contains(&"quick_ping")
        })
    });
    if !same_response {
        return Ok(Verdict::Retry("model did not call both tools in one response".to_string()));
    }
    let (_, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
    }
    Ok(if server.overlapped.load(Ordering::SeqCst) {
        Verdict::Pass(format!(
            "slow_report and quick_ping from one response were in flight together on one MCP connection ({} tools/call requests)",
            server.calls.load(Ordering::SeqCst)
        ))
    } else {
        Verdict::Fail(format!(
            "calls from one response did not overlap on the MCP connection within {}s",
            OVERLAP_WINDOW.as_secs()
        ))
    })
}
