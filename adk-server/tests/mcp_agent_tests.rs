//! An agent served over MCP (`adk_server::mcp`): `adk-tool`'s own MCP client calls the agent
//! over Streamable HTTP, receives a task backed by a durable run, is asked to approve the call
//! the agent holds, answers, and sees the run finish — on in-memory stores, and on SQLite stores
//! across a server restart. The live run does the same on `claude-sonnet-5-5`.
//!
//! The live run needs `ANTHROPIC_API_KEY`, read from the repository's `.env` when present:
//!
//! ```text
//! cargo test -p adk-server --features mcp --test mcp_agent_tests -- --ignored --nocapture
//! ```

#![cfg(feature = "mcp")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, ApprovalStore, Content, DurableRunStore, ErrorCategory, ErrorComponent,
    InMemoryActionLedger, InMemoryApprovalStore, InMemoryRunStore, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, RunState, Tool, ToolContext, ToolEffect,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkerHandle};
use adk_server::mcp::{McpAgent, McpAgentHost};
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::rmcp::model::{ElicitResult, ElicitationAction, InputResponses, TaskPayload};
use adk_tool::mcp::server::{
    McpCaller, McpCallerResolver, McpHttpRequest, McpRouterBuilder, McpTaskHost,
};
use adk_tool::mcp::{
    ElicitationHandler, McpAuth, McpCallOutcome, McpHttpClientBuilder, McpTaskConfig, McpToolset,
    ParkedCallKind, ParkedMcpCall,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};

const APP: &str = "ops";

/// Issues `transfer` until it has a real (not pending) response, then reports it.
struct TransferModel;

#[async_trait]
impl Llm for TransferModel {
    fn name(&self) -> &str {
        "transfer-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let answered = req.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "transfer"
                    && !adk_core::is_confirmation_pending(&function_response.response) =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        });
        let content = match answered {
            Some(response) => Content::new("model").with_text(format!("transfer: {response}")),
            None => Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "transfer".to_string(),
                    args: json!({ "amountCents": 5000 }),
                    id: Some("call-transfer-1".to_string()),
                    thought_signature: None,
                }],
            },
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Moves money; counts its executions.
struct TransferTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for TransferTool {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "Moves money from the operating account. Takes amountCents, an integer."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "amountCents": { "type": "integer" } },
            "required": ["amountCents"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "sent" }))
    }
}

/// Bearer tokens to callers: `ana-token` is ana, `bo-token` is bo.
struct Tokens;

#[async_trait]
impl McpCallerResolver for Tokens {
    async fn resolve(&self, request: &McpHttpRequest<'_>) -> Result<McpCaller> {
        match request.bearer_token() {
            Some("ana-token") => Ok(McpCaller::authenticated("ana")),
            Some("bo-token") => Ok(McpCaller::authenticated("bo")),
            _ => Err(AdkError::unauthorized(
                ErrorComponent::Auth,
                "test.token",
                "a bearer token is required",
            )),
        }
    }
}

/// Answers every elicitation with `action`, recording the messages it was asked.
struct Answer {
    action: ElicitationAction,
    asked: std::sync::Mutex<Vec<String>>,
}

impl Answer {
    fn new(action: ElicitationAction) -> Arc<Self> {
        Arc::new(Self { action, asked: std::sync::Mutex::default() })
    }
}

#[async_trait]
impl ElicitationHandler for Answer {
    async fn handle_form_elicitation(
        &self,
        message: &str,
        _schema: &adk_tool::mcp::rmcp::model::ElicitationSchema,
        _metadata: Option<&Value>,
    ) -> std::result::Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        self.asked.lock().unwrap().push(message.to_string());
        Ok(ElicitResult::new(self.action))
    }

    async fn handle_url_elicitation(
        &self,
        _message: &str,
        _url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> std::result::Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ElicitResult::new(ElicitationAction::Decline))
    }
}

/// The stores a server runs on, which a restart keeps.
#[derive(Clone)]
struct Stores {
    runs: Arc<dyn DurableRunStore>,
    approvals: Arc<dyn ApprovalStore>,
    sessions: Arc<dyn SessionService>,
}

impl Stores {
    fn in_memory() -> Self {
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            approvals: Arc::new(InMemoryApprovalStore::new()),
            sessions: Arc::new(InMemorySessionService::new()),
        }
    }

    async fn sqlite(dir: &std::path::Path) -> Self {
        let url = |name: &str| format!("sqlite://{}?mode=rwc", dir.join(name).display());
        let runs = adk_session::SqliteRunStore::new(&url("runs.db")).await.unwrap();
        runs.migrate().await.unwrap();
        let approvals = adk_session::SqliteApprovalStore::new(&url("approvals.db")).await.unwrap();
        approvals.migrate().await.unwrap();
        let sessions = adk_session::SqliteSessionService::new(&url("sessions.db")).await.unwrap();
        sessions.migrate().await.unwrap();
        Self { runs: Arc::new(runs), approvals: Arc::new(approvals), sessions: Arc::new(sessions) }
    }
}

/// One running server: the worker, the HTTP endpoint and the host.
struct Served {
    host: McpAgentHost,
    worker: WorkerHandle,
    server: tokio::task::JoinHandle<()>,
    endpoint: String,
}

impl Served {
    /// Stops the worker and the endpoint, as a process exit would.
    async fn stop(self) {
        self.server.abort();
        let _ = self.server.await;
        self.worker.shutdown().await;
    }
}

/// The host of the agent `ops` with `model` on `stores`, and the worker that runs it.
fn host(
    stores: &Stores,
    model: Arc<dyn Llm>,
    tool: Arc<dyn Tool>,
    worker: &str,
) -> (McpAgentHost, WorkerHandle) {
    let agent = LlmAgentBuilder::new("ops")
        .description("Moves money for the operations team")
        .instruction("You move money with the transfer tool when asked. Report the outcome.")
        .model(model)
        .tool(tool)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&stores.sessions))
            .run_config(RunConfig::builder().approval_store(Arc::clone(&stores.approvals)).build())
            .action_ledger(Arc::new(InMemoryActionLedger::new()))
            .build()
            .unwrap(),
    );
    let host = McpAgentHost::builder(APP, Arc::clone(&stores.runs))
        .agent(McpAgent::new("ops", "Moves money for the operations team"))
        .approval_store(Arc::clone(&stores.approvals))
        .poll_interval(Duration::from_millis(50))
        .wait_limit(Duration::from_secs(120))
        .build()
        .unwrap();
    let worker = DurableWorker::new(Arc::clone(&stores.runs))
        .with_agent("ops", runner)
        .with_hooks(host.worker_hooks())
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id(worker)
                .with_lease_ttl(Duration::from_secs(2))
                .with_poll_interval(Duration::from_millis(20)),
        )
        .spawn();
    (host, worker)
}

/// Serves the agent `ops` with `model` on `stores` over Streamable HTTP, at `port` (0 picks
/// one).
async fn serve(stores: &Stores, model: Arc<dyn Llm>, tool: Arc<dyn Tool>, port: u16) -> Served {
    let (host, worker) = host(stores, model, tool, &format!("worker-{port}"));
    let server =
        host.server("com.example/ops", "1.0.0", "The operations team's agents").build().unwrap();
    let router = McpRouterBuilder::new(server).caller_resolver(Arc::new(Tokens)).build().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Served { host, worker, server, endpoint }
}

/// A client toolset for `token`, answering elicitations with `answer`, with or without tasks.
async fn client(
    endpoint: &str,
    token: &str,
    answer: Arc<Answer>,
    tasks: bool,
) -> McpToolset<adk_tool::mcp::AdkClientHandler> {
    let mut builder = McpHttpClientBuilder::new(endpoint)
        .with_auth(McpAuth::bearer(token))
        .with_elicitation_handler(answer);
    if tasks {
        builder = builder
            .with_task_support(McpTaskConfig::enabled().poll_interval(Duration::from_millis(50)));
    }
    builder.connect_with_elicitation().await.unwrap().with_name("ops-server")
}

fn message(text: &str) -> Map<String, Value> {
    json!({ "message": text }).as_object().unwrap().clone()
}

fn accept() -> Value {
    serde_json::to_value(ElicitResult::new(ElicitationAction::Accept)).unwrap()
}

/// The answers to every question `parked` asks, all `answer`.
fn answers(parked: &ParkedMcpCall, answer: &Value) -> InputResponses {
    parked.input_requests.keys().map(|key| (key.clone(), answer.clone())).collect()
}

#[tokio::test]
async fn a_task_asks_for_the_held_call_and_finishes_once_approved() {
    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let served = serve(&stores, Arc::new(TransferModel), tool, 0).await;
    let toolset =
        client(&served.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;

    // 1. The call returns a task, which parks on the held transfer and asks about it.
    let outcome =
        toolset.call_tool_resumable(Some("c-1"), "ops", message("send 50 USD")).await.unwrap();
    let McpCallOutcome::Parked(parked) = outcome else {
        panic!("expected a parked task: {outcome:?}")
    };
    let ParkedCallKind::Task { task_id } = &parked.kind else {
        panic!("expected a task: {parked:?}")
    };
    let [(key, request)] = parked.input_requests.iter().collect::<Vec<_>>()[..] else {
        panic!("expected one question: {parked:?}");
    };
    assert!(key.starts_with("approval-"), "{key}");
    let question = serde_json::to_value(request).unwrap();
    let text = question["params"]["message"].as_str().unwrap();
    assert!(text.contains("`transfer`") && text.contains("5000"), "{text}");
    let run = stores.runs.get(task_id).await.unwrap().unwrap();
    assert_eq!(
        (run.state, run.user_id.as_str(), run.authenticated),
        (RunState::Waiting, "ana", true)
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    // 2. Another caller cannot read, answer or cancel it.
    let bo = McpCaller::authenticated("bo");
    let unknown = served.host.get(task_id, &bo).await.unwrap_err();
    assert_eq!(
        (unknown.category, unknown.code),
        (ErrorCategory::NotFound, "mcp.agent.unknown_task")
    );
    assert!(served.host.update(task_id, &bo, answers(&parked, &accept())).await.is_err());
    assert!(served.host.cancel(task_id, &bo).await.is_err());

    // 3. Approving resumes the held call through the approval store; the run finishes.
    let finished = toolset.reissue(&parked, answers(&parked, &accept())).await.unwrap();
    let McpCallOutcome::Completed(value) = finished else {
        panic!("expected a result: {finished:?}")
    };
    assert_eq!(value["text"], format!("transfer: {}", json!({ "status": "sent" })), "{value}");
    assert_eq!(value["runId"], task_id.as_str());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(stores.runs.get(task_id).await.unwrap().unwrap().state, RunState::Completed);

    // 4. The same answer again is stale: the call it asked about was decided.
    let ana = McpCaller::authenticated("ana");
    let stale = served.host.update(task_id, &ana, answers(&parked, &accept())).await.unwrap_err();
    assert_eq!((stale.category, stale.code), (ErrorCategory::Conflict, "mcp.agent.stale_answer"));
    let task = served.host.get(task_id, &ana).await.unwrap();
    assert!(matches!(task.payload, TaskPayload::Completed { .. }), "{task:?}");
    assert_eq!(executions.load(Ordering::SeqCst), 1, "a stale answer runs nothing");
    served.stop().await;
}

#[tokio::test]
async fn a_declined_call_is_not_run_and_the_agent_hears_why() {
    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let served = serve(&stores, Arc::new(TransferModel), tool, 0).await;
    let toolset =
        client(&served.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;
    let outcome =
        toolset.call_tool_resumable(Some("c-1"), "ops", message("send 50 USD")).await.unwrap();
    let McpCallOutcome::Parked(parked) = outcome else { panic!("{outcome:?}") };
    let declined =
        json!({ "action": "accept", "content": { "approve": false, "reason": "over budget" } });
    let finished = toolset.reissue(&parked, answers(&parked, &declined)).await.unwrap();
    let McpCallOutcome::Completed(value) = finished else { panic!("{finished:?}") };
    let text = value["text"].as_str().unwrap();
    assert!(text.contains("over budget"), "the denial reaches the model: {text}");
    assert_eq!(executions.load(Ordering::SeqCst), 0, "a declined call never runs");
    served.stop().await;
}

#[tokio::test]
async fn a_client_without_tasks_is_asked_on_its_connection_and_gets_the_answer() {
    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let served = serve(&stores, Arc::new(TransferModel), tool, 0).await;
    let answer = Answer::new(ElicitationAction::Accept);
    let toolset = client(&served.endpoint, "ana-token", Arc::clone(&answer), false).await;
    let tools = adk_core::Toolset::tools(
        &toolset,
        Arc::new(adk_tool::SimpleToolContext::new("buyer")) as _,
    )
    .await
    .unwrap();
    let ops = tools.iter().find(|tool| tool.name() == "ops").unwrap();
    let value = ops
        .execute(
            Arc::new(adk_tool::SimpleToolContext::new("buyer")),
            json!({ "message": "send 50 USD" }),
        )
        .await
        .unwrap();
    assert_eq!(value["text"], format!("transfer: {}", json!({ "status": "sent" })), "{value}");
    let asked = answer.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].contains("`transfer`"), "{asked:?}");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    served.stop().await;
}

#[tokio::test]
async fn cancelling_a_task_cancels_its_run() {
    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let served = serve(&stores, Arc::new(TransferModel), tool, 0).await;
    let toolset =
        client(&served.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;
    let outcome =
        toolset.call_tool_resumable(Some("c-1"), "ops", message("send 50 USD")).await.unwrap();
    let McpCallOutcome::Parked(parked) = outcome else { panic!("{outcome:?}") };
    let ParkedCallKind::Task { task_id } = &parked.kind else { panic!("{parked:?}") };
    let ana = McpCaller::authenticated("ana");
    served.host.cancel(task_id, &ana).await.unwrap();
    let task = served.host.get(task_id, &ana).await.unwrap();
    assert!(matches!(task.payload, TaskPayload::Cancelled), "{task:?}");
    let again = served.host.cancel(task_id, &ana).await.unwrap_err();
    assert_eq!(again.code, "mcp.agent.task_finished");
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    served.stop().await;
}

#[tokio::test]
async fn an_anonymous_caller_starts_nothing() {
    let stores = Stores::in_memory();
    let host = McpAgentHost::builder(APP, Arc::clone(&stores.runs))
        .agent(McpAgent::new("ops", "Moves money"))
        .approval_store(Arc::clone(&stores.approvals))
        .build()
        .unwrap();
    let server = host.server("com.example/ops", "1.0.0", "Ops").build().unwrap();
    let call = adk_tool::mcp::server::McpToolCall::new(
        server.tools()[0].clone(),
        json!({ "message": "send 50 USD" }),
        McpCaller::anonymous(),
        "1",
    );
    let refused = adk_tool::mcp::server::McpCallExecutor::execute(&host, call).await.unwrap_err();
    assert_eq!(
        (refused.category, refused.code),
        (ErrorCategory::Forbidden, "mcp.agent.unauthenticated")
    );
    assert!(stores.runs.list(Default::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_task_survives_a_server_restart() {
    let dir = tempfile::tempdir().unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = || Arc::new(TransferTool { executions: Arc::clone(&executions) }) as Arc<dyn Tool>;

    // 1. The first server starts the run, which parks on the held call; then it stops.
    let first = serve(&Stores::sqlite(dir.path()).await, Arc::new(TransferModel), tool(), 0).await;
    let port: u16 =
        first.endpoint.rsplit(':').next().unwrap().trim_end_matches("/mcp").parse().unwrap();
    let toolset =
        client(&first.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;
    let outcome =
        toolset.call_tool_resumable(Some("c-1"), "ops", message("send 50 USD")).await.unwrap();
    let McpCallOutcome::Parked(parked) = outcome else { panic!("{outcome:?}") };
    let checkpoint = serde_json::to_string(&parked).unwrap();
    first.stop().await;
    drop(toolset);

    // 2. A new server on the same stores answers for the task; a new client approves it.
    let second =
        serve(&Stores::sqlite(dir.path()).await, Arc::new(TransferModel), tool(), port).await;
    let toolset =
        client(&second.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;
    let parked: ParkedMcpCall = serde_json::from_str(&checkpoint).unwrap();
    let finished = toolset.reissue(&parked, answers(&parked, &accept())).await.unwrap();
    let McpCallOutcome::Completed(value) = finished else { panic!("{finished:?}") };
    assert_eq!(value["text"], format!("transfer: {}", json!({ "status": "sent" })), "{value}");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    second.stop().await;
}

#[tokio::test]
async fn over_stdio_the_local_caller_owns_the_task() {
    use adk_tool::mcp::rmcp::ServiceExt;

    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let (host, worker) = host(&stores, Arc::new(TransferModel), tool, "stdio-worker");
    let server = host
        .server("com.example/ops", "1.0.0", "The operations team's agents")
        .local_caller(McpCaller::authenticated("local:ana"))
        .build()
        .unwrap();
    let (client_io, serving) = pipe(server);
    let handler =
        adk_tool::mcp::AdkClientHandler::new(Answer::new(ElicitationAction::Accept)).with_tasks();
    let client = handler.serve(tokio::io::split(client_io)).await.unwrap();
    let toolset = McpToolset::new(client)
        .with_name("ops-server")
        .with_task_support(McpTaskConfig::enabled().poll_interval(Duration::from_millis(50)));

    let outcome =
        toolset.call_tool_resumable(Some("c-1"), "ops", message("send 50 USD")).await.unwrap();
    let McpCallOutcome::Parked(parked) = outcome else { panic!("{outcome:?}") };
    let ParkedCallKind::Task { task_id } = &parked.kind else { panic!("{parked:?}") };
    let run = stores.runs.get(task_id).await.unwrap().unwrap();
    assert_eq!((run.user_id.as_str(), run.authenticated), ("local:ana", true));
    let finished = toolset.reissue(&parked, answers(&parked, &accept())).await.unwrap();
    let McpCallOutcome::Completed(value) = finished else { panic!("{finished:?}") };
    assert_eq!(value["text"], format!("transfer: {}", json!({ "status": "sent" })), "{value}");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    drop(toolset);
    serving.abort();
    worker.shutdown().await;
}

/// Serves `server` over an in-process pipe, as a stdio child process would be.
fn pipe(server: adk_tool::mcp::server::McpServer) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    use adk_tool::mcp::rmcp::ServiceExt;

    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let serving = tokio::spawn(async move {
        let running = server.serve(tokio::io::split(server_io)).await.unwrap();
        let _ = running.waiting().await;
    });
    (client_io, serving)
}

#[tokio::test]
async fn a_2026_07_28_client_without_tasks_is_asked_through_input_required() {
    use adk_tool::mcp::rmcp::model::ProtocolVersion;
    use adk_tool::mcp::{ClientLifecycleMode, ClientServiceExt};

    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let (host, worker) = host(&stores, Arc::new(TransferModel), tool, "mrtr-worker");
    let server = host
        .server("com.example/ops", "1.0.0", "The operations team's agents")
        .local_caller(McpCaller::authenticated("local:ana"))
        .build()
        .unwrap();
    let (client_io, serving) = pipe(server);
    let answer = Answer::new(ElicitationAction::Accept);
    let client = adk_tool::mcp::AdkClientHandler::new(Arc::clone(&answer) as Arc<dyn ElicitationHandler>)
        .serve_with_lifecycle(
            tokio::io::split(client_io),
            ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        )
        .await
        .unwrap();
    assert_eq!(
        client.peer_info().map(|info| info.protocol_version.clone()),
        Some(ProtocolVersion::V_2026_07_28)
    );
    let toolset = McpToolset::new(client).with_name("ops-server");
    let ctx = Arc::new(adk_tool::SimpleToolContext::new("buyer"));
    let tools = adk_core::Toolset::tools(&toolset, ctx.clone() as _).await.unwrap();
    let ops = tools.iter().find(|tool| tool.name() == "ops").unwrap();
    let value = ops.execute(ctx, json!({ "message": "send 50 USD" })).await.unwrap();
    assert_eq!(value["text"], format!("transfer: {}", json!({ "status": "sent" })), "{value}");
    assert_eq!(answer.asked.lock().unwrap().len(), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    drop(toolset);
    serving.abort();
    worker.shutdown().await;
}

/// An MCP client of an earlier revision that records the progress it hears and approves what it
/// is asked on the connection.
#[derive(Default)]
struct Listener {
    progress: Arc<std::sync::Mutex<Vec<String>>>,
    asked: Arc<std::sync::Mutex<Vec<String>>>,
}

impl adk_tool::mcp::rmcp::ClientHandler for Listener {
    fn get_info(&self) -> adk_tool::mcp::rmcp::model::ClientInfo {
        let mut info = adk_tool::mcp::rmcp::model::ClientInfo::default();
        info.capabilities =
            adk_tool::mcp::rmcp::model::ClientCapabilities::builder().enable_elicitation().build();
        info
    }

    async fn create_elicitation(
        &self,
        request: adk_tool::mcp::rmcp::model::ElicitRequestParams,
        _context: adk_tool::mcp::rmcp::service::RequestContext<adk_tool::mcp::rmcp::RoleClient>,
    ) -> std::result::Result<ElicitResult, adk_tool::mcp::rmcp::ErrorData> {
        if let adk_tool::mcp::rmcp::model::ElicitRequestParams::FormElicitationParams { message, .. } = request {
            self.asked.lock().unwrap().push(message);
        }
        Ok(ElicitResult::new(ElicitationAction::Accept))
    }

    async fn on_progress(
        &self,
        params: adk_tool::mcp::rmcp::model::ProgressNotificationParam,
        _context: adk_tool::mcp::rmcp::service::NotificationContext<adk_tool::mcp::rmcp::RoleClient>,
    ) {
        if let Some(message) = params.message {
            self.progress.lock().unwrap().push(message);
        }
    }
}

#[tokio::test]
async fn progress_notifications_stream_the_runs_steps() {
    use adk_tool::mcp::rmcp::ServiceExt;
    use adk_tool::mcp::rmcp::model::CallToolRequestParams;

    let stores = Stores::in_memory();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(TransferTool { executions: Arc::clone(&executions) });
    let (host, worker) = host(&stores, Arc::new(TransferModel), tool, "progress-worker");
    let server = host
        .server("com.example/ops", "1.0.0", "The operations team's agents")
        .local_caller(McpCaller::authenticated("local:ana"))
        .build()
        .unwrap();
    let (client_io, serving) = pipe(server);
    let listener = Listener::default();
    let (progress, asked) = (Arc::clone(&listener.progress), Arc::clone(&listener.asked));
    let client = listener.serve(tokio::io::split(client_io)).await.unwrap();
    let params = CallToolRequestParams::new("ops")
        .with_arguments(json!({ "message": "send 50 USD" }).as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await.unwrap();
    assert_eq!(
        result.structured_content.as_ref().unwrap()["text"],
        format!("transfer: {}", json!({ "status": "sent" }))
    );
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "asked once on the connection: {asked:?}");
    let started = tokio::time::Instant::now();
    let heard = loop {
        let heard = progress.lock().unwrap().clone();
        if heard.iter().any(|line| line.starts_with("ops: transfer:")) || started.elapsed() > Duration::from_secs(2) {
            break heard;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    for step in ["calling transfer", "waiting for approval to run transfer", "transfer returned"] {
        assert!(heard.iter().any(|line| line == step), "missing `{step}` in {heard:?}");
    }
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    drop(client);
    serving.abort();
    worker.shutdown().await;
}

/// Tells the live model to transfer what the principal asks for.
const LIVE_PROMPT: &str = "Send 25 USD (2500 cents) to the supplier with the transfer tool, then \
    tell me in one sentence whether it went through.";

#[tokio::test]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn live_anthropic_agent_over_mcp_with_a_restart_mid_task() {
    let _ = dotenvy::from_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model = || -> Arc<dyn Llm> {
        Arc::new(
            adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
                key.clone(),
                "claude-sonnet-5-5",
            ))
            .unwrap(),
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool = || Arc::new(TransferTool { executions: Arc::clone(&executions) }) as Arc<dyn Tool>;

    let first = serve(&Stores::sqlite(dir.path()).await, model(), tool(), 0).await;
    let port: u16 =
        first.endpoint.rsplit(':').next().unwrap().trim_end_matches("/mcp").parse().unwrap();
    let toolset =
        client(&first.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;
    let outcome =
        toolset.call_tool_resumable(Some("live-1"), "ops", message(LIVE_PROMPT)).await.unwrap();
    let McpCallOutcome::Parked(parked) = outcome else {
        panic!("expected a parked task: {outcome:?}")
    };
    let ParkedCallKind::Task { task_id } = parked.kind.clone() else { panic!("{parked:?}") };
    let asked: Vec<Value> = parked
        .input_requests
        .values()
        .map(|request| serde_json::to_value(request).unwrap())
        .collect();
    println!("task {task_id} asks: {}", serde_json::to_string_pretty(&asked).unwrap());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let checkpoint = serde_json::to_string(&parked).unwrap();
    first.stop().await;
    drop(toolset);
    println!("server stopped with the task parked; restarting on the same SQLite stores");

    let second = serve(&Stores::sqlite(dir.path()).await, model(), tool(), port).await;
    let toolset =
        client(&second.endpoint, "ana-token", Answer::new(ElicitationAction::Accept), true).await;
    let parked: ParkedMcpCall = serde_json::from_str(&checkpoint).unwrap();
    let finished = toolset.reissue(&parked, answers(&parked, &accept())).await.unwrap();
    let McpCallOutcome::Completed(value) = finished else { panic!("{finished:?}") };
    println!("task {task_id} completed: {value}");
    assert_eq!(executions.load(Ordering::SeqCst), 1, "the approved transfer ran once");
    assert_eq!(value["runId"], task_id.as_str());
    assert!(!value["text"].as_str().unwrap_or_default().is_empty());
    second.stop().await;
}
