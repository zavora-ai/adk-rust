//! MCP sampling requests count against the run whose tool call triggered them.
#![cfg(feature = "mcp-sampling")]
#![allow(deprecated)]

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{
    Artifacts, BudgetLimit, BudgetTracker, CallbackContext, Content, EventActions,
    InMemorySpendLedger, Llm, LlmRequest, LlmResponse, LlmResponseStream, MemoryEntry,
    ModelCallScope, ReadonlyContext, RunBudget, RunConfig, SpendKey, SpendLedger, SpendPeriod,
    Tool, ToolContext, UsageMetadata,
};
use adk_tool::mcp::{AutoDeclineElicitationHandler, McpToolset};
use adk_tool::sampling::LlmSamplingHandler;
use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, CreateMessageRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, SamplingMessage, ServerCapabilities,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// Asks the client to sample once per `tools/call` and returns the sampled text.
#[derive(Clone)]
struct SamplingServer;

impl ServerHandler for SamplingServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let schema = json!({ "type": "object" }).as_object().cloned().unwrap_or_default();
        Ok(ListToolsResult::with_all_items(vec![rmcp::model::Tool::new(
            "summarize",
            "Summarizes with the client's model.",
            Arc::new(schema),
        )]))
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let sampled = context
            .peer
            .create_message(CreateMessageRequestParams::new(
                vec![SamplingMessage::user_text("summarize this")],
                64,
            ))
            .await;
        Ok(match sampled {
            Ok(result) => {
                let text = result.message.content.first().and_then(|block| match block {
                    rmcp::model::SamplingMessageContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                });
                CallToolResult::success(vec![ContentBlock::text(text.unwrap_or_default())])
            }
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        }
        .into())
    }
}

fn spawn_sampling_server() -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(running) = SamplingServer.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    client_io
}

/// Answers with fixed usage and cost, counting how often it is called.
struct PricedModel {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Llm for PricedModel {
    fn name(&self) -> &str {
        "sampler-model"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("short summary")),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 20,
                candidates_token_count: 10,
                total_token_count: 30,
                cost: Some(0.0015),
                ..Default::default()
            }),
            provider: Some("sampler-vendor".to_string()),
            model: Some("sampler-model".to_string()),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

/// A tool context inside a run: it exposes the run's configuration.
struct RunToolContext {
    run_config: Option<RunConfig>,
    content: Content,
    actions: Mutex<EventActions>,
}

impl RunToolContext {
    fn new(run_config: Option<RunConfig>) -> Arc<Self> {
        Arc::new(Self {
            run_config,
            content: Content::new("user").with_text("go"),
            actions: Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait]
impl ReadonlyContext for RunToolContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "researcher"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "acme"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for RunToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for RunToolContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }
}

async fn sampling_toolset(calls: Arc<AtomicUsize>) -> McpToolset<adk_tool::mcp::AdkClientHandler> {
    McpToolset::with_sampling_handler(
        spawn_sampling_server(),
        Arc::new(AutoDeclineElicitationHandler),
        Arc::new(LlmSamplingHandler::new(Arc::new(PricedModel { calls }))),
    )
    .await
    .expect("connect to the sampling server")
}

async fn summarize_tool(toolset: &McpToolset<adk_tool::mcp::AdkClientHandler>) -> Arc<dyn Tool> {
    let ctx = RunToolContext::new(None) as Arc<dyn ReadonlyContext>;
    let tools = adk_core::Toolset::tools(toolset, ctx).await.expect("list tools");
    tools.into_iter().find(|tool| tool.name() == "summarize").expect("summarize tool")
}

async fn call(tool: &Arc<dyn Tool>, run_config: Option<RunConfig>) -> adk_core::Result<Value> {
    tool.execute(RunToolContext::new(run_config), json!({})).await
}

#[tokio::test]
async fn sampling_during_a_run_counts_against_its_budget_and_ledger() {
    let calls = Arc::new(AtomicUsize::new(0));
    let toolset = sampling_toolset(calls.clone()).await;
    let tool = summarize_tool(&toolset).await;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(5)));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let mut config = RunConfig::builder().spend_ledger(ledger.clone()).build();
    config.budget_tracker = Some(tracker.clone());

    let value = call(&tool, Some(config)).await.expect("tool call succeeds");

    assert!(value.to_string().contains("short summary"), "{value}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let usage = tracker.usage();
    assert_eq!((usage.model_calls, usage.total_tokens, usage.cost_micro_usd), (1, 30, 1_500));
    let vendor_key = SpendKey::org("acme").with_agent("researcher").with_vendor("sampler-vendor");
    assert_eq!(ledger.spent(&vendor_key, SpendPeriod::Lifetime).await.unwrap(), 1_500);
}

#[tokio::test]
async fn sampling_over_the_run_budget_is_refused_before_the_model_is_called() {
    let calls = Arc::new(AtomicUsize::new(0));
    let toolset = sampling_toolset(calls.clone()).await;
    let tool = summarize_tool(&toolset).await;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(1)));
    drop(tracker.begin_model_call("main-model").unwrap());
    let config = RunConfig { budget_tracker: Some(tracker.clone()), ..RunConfig::default() };

    let error = call(&tool, Some(config)).await.expect_err("the sampled call is refused");

    assert!(error.to_string().contains("budget"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a refused call is never sent");
    assert_eq!(tracker.usage().model_calls, 1);
    assert_eq!(tracker.check().map_err(|e| e.limit()), Ok(()));
    assert_eq!(
        tracker.begin_model_call("main-model").map(drop).map_err(|e| e.limit()),
        Err(BudgetLimit::ModelCalls)
    );
}

#[tokio::test]
async fn sampling_over_the_spend_cap_is_refused() {
    let calls = Arc::new(AtomicUsize::new(0));
    let toolset = sampling_toolset(calls.clone()).await;
    let tool = summarize_tool(&toolset).await;
    let ledger = Arc::new(InMemorySpendLedger::new(
        adk_core::SpendLimits::new().limit(SpendKey::org("acme"), 1_000),
    ));
    let config = RunConfig::builder().spend_ledger(ledger.clone()).build();

    let error = call(&tool, Some(config)).await.expect_err("the hold exceeds the cap");

    assert!(error.to_string().contains("spend"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap(), 0);
}

#[tokio::test]
async fn sampling_outside_a_run_uses_the_attached_scope_or_is_unmetered() {
    let calls = Arc::new(AtomicUsize::new(0));
    let toolset = sampling_toolset(calls.clone()).await;
    let tool = summarize_tool(&toolset).await;

    call(&tool, None).await.expect("unmetered sampling still runs");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let toolset = toolset.with_sampling_scope(
        ModelCallScope::new()
            .with_budget(tracker.clone())
            .with_spend_ledger(ledger.clone(), "acme")
            .with_agent("background-sync"),
    );
    let tool = summarize_tool(&toolset).await;
    call(&tool, None).await.expect("sampling under the attached scope");

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(tracker.usage().model_calls, 1);
    let key = SpendKey::org("acme").with_agent("background-sync");
    assert_eq!(ledger.spent(&key, SpendPeriod::Lifetime).await.unwrap(), 1_500);
}
