//! Direct calls preserve the connection's input policy, subscriptions, and
//! remote task lifecycle.
#![cfg(feature = "mcp")]

use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ConnectionFactory, McpTaskConfig, McpToolset,
};
use rmcp::model::*;
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::{IntoTransport, Transport};
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient, RoleServer, ServerHandler,
    ServiceExt,
};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Since rmcp 3.2 `initialize` negotiates only versions that still have it, so
/// `2026-07-28`, which MRTR requires, is reached through `server/discover`.
fn discover_2026() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }
}

#[derive(Clone, Default)]
struct InputClient(Arc<AtomicUsize>);

impl ClientHandler for InputClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::builder().enable_elicitation().enable_tasks().build(),
            Implementation::new("input-client", "1.0.0"),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn create_elicitation(
        &self,
        _params: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        assert_eq!(context.id, NumberOrString::String("answer".into()));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ElicitResult::new(ElicitationAction::Accept).with_content(json!({"name":"Ferris"})))
    }
}

#[derive(Clone, Default)]
struct InputServer(Arc<rmcp::task_manager::TaskManager>);

fn input_request() -> InputRequest {
    prompt("Name")
}

fn prompt(message: &str) -> InputRequest {
    InputRequest::Elicitation(ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: message.into(),
        requested_schema: serde_json::from_value(json!({
            "type":"object", "properties":{"name":{"type":"string"}}, "required":["name"]
        }))
        .unwrap(),
    }))
}

impl ServerHandler for InputServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if params.name == "task" {
            let task = self.0.spawn(Default::default(), |ctx| {
                Box::pin(async move {
                    let response = ctx.request_input("answer", input_request()).await?;
                    Ok(CallToolResult::structured(response))
                })
            });
            return Ok(CreateTaskResult::new(task).into());
        }
        if let Some(responses) = params.input_responses {
            assert_eq!(params.request_state.as_deref(), Some("round-1"));
            if params.name == "batch" {
                return Ok(
                    CallToolResult::structured(serde_json::to_value(responses).unwrap()).into()
                );
            }
            return Ok(CallToolResult::structured(responses["answer"].clone()).into());
        }
        let requests = if params.name == "batch" {
            [("first".into(), input_request()), ("second".into(), input_request())].into()
        } else {
            [("answer".into(), input_request())].into()
        };
        Ok(InputRequiredResult::new(Some(requests), Some("round-1".into())).into())
    }

    async fn get_task(
        &self,
        params: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        Ok(GetTaskResult::new(self.0.get_task(&params.task_id)?))
    }

    async fn update_task(
        &self,
        params: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.0.update_task(&params.task_id, params.input_responses)
    }
}

#[tokio::test]
async fn direct_calls_reuse_the_custom_handler_for_inline_and_task_input() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        InputServer::default().serve(server_io).await.unwrap().waiting().await.unwrap();
    });
    let handler = InputClient::default();
    let handled = handler.0.clone();
    let client = handler.serve_with_lifecycle(client_io, discover_2026()).await.unwrap();
    let toolset = McpToolset::new(client).with_task_support(
        McpTaskConfig::enabled().poll_interval(std::time::Duration::from_millis(1)),
    );
    for name in ["inline", "task"] {
        let value = toolset.call_tool_value(name, Default::default()).await.unwrap();
        assert_eq!(value["output"]["content"]["name"], "Ferris", "{value}");
    }
    assert_eq!(handled.load(Ordering::SeqCst), 2);
}

struct BatchClient(tokio::sync::Barrier);

impl ClientHandler for BatchClient {
    fn get_info(&self) -> InitializeRequestParams {
        InputClient::default().get_info()
    }

    async fn create_elicitation(
        &self,
        _params: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        // Each prompt must become visible before either answer is submitted.
        self.0.wait().await;
        Ok(ElicitResult::new(ElicitationAction::Accept).with_content(json!({"id": context.id})))
    }
}

#[tokio::test]
async fn input_batches_dispatch_custom_handlers_concurrently() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        InputServer::default().serve(server_io).await.unwrap().waiting().await.unwrap();
    });
    let client = BatchClient(tokio::sync::Barrier::new(2))
        .serve_with_lifecycle(client_io, discover_2026())
        .await
        .unwrap();
    let toolset = McpToolset::new(client);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        toolset.call_tool_value("batch", Default::default()),
    )
    .await
    .expect("all prompts must be dispatched without waiting for earlier answers")
    .unwrap();
    assert_eq!(result["output"]["first"]["content"]["id"], "first");
    assert_eq!(result["output"]["second"]["content"]["id"], "second");
}

#[derive(Clone, Default)]
struct SubscriptionServer {
    subscriptions: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

impl ServerHandler for SubscriptionServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(
            ServerCapabilities::builder().enable_tools().enable_resources().build(),
        )
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        params: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        assert_eq!(params.uri, "test://resource");
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ErrorData::internal_error("connection reset", None));
        }
        Ok(CallToolResult::structured(json!({"ok":true})).into())
    }
}

struct Factory(SubscriptionServer);

#[async_trait::async_trait]
impl ConnectionFactory<()> for Factory {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let server = self.0.clone();
        tokio::spawn(async move {
            server.serve(server_io).await.unwrap().waiting().await.unwrap();
        });
        ().serve(client_io).await.map_err(|error| error.to_string())
    }
}

#[tokio::test]
async fn direct_call_reconnection_restores_resource_subscriptions() {
    let server = SubscriptionServer::default();
    let subscriptions = server.subscriptions.clone();
    let factory = Arc::new(Factory(server));
    let client = factory.create_connection().await.unwrap();
    let toolset = McpToolset::new(client).with_connection_factory(factory).with_tool_call_retries();
    toolset.subscribe_resource("test://resource").await.unwrap();
    assert_eq!(subscriptions.load(Ordering::SeqCst), 1);
    let value = toolset.call_tool_value("read", Default::default()).await.unwrap();
    assert_eq!(value["output"], json!({"ok":true}));
    assert_eq!(subscriptions.load(Ordering::SeqCst), 2);
}

fn spawn_server<H: ServerHandler>(server: H) -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    client_io
}

fn fast_tasks() -> McpTaskConfig {
    McpTaskConfig::enabled().poll_interval(Duration::from_millis(1))
}

fn completed() -> TaskPayload {
    let result = serde_json::to_value(CallToolResult::structured(json!({"done": true}))).unwrap();
    TaskPayload::Completed { result: result.as_object().unwrap().clone() }
}

const TASK_ID: &str = "task-1";
const TIMESTAMP: &str = "2026-10-08T00:00:00Z";

/// Serves one task whose `tasks/get` answers follow a script. The last entry
/// repeats, and `None` is a task the server no longer knows.
#[derive(Clone)]
struct ScriptedTaskServer {
    statuses: Arc<std::sync::Mutex<VecDeque<Option<TaskPayload>>>>,
    gets: Arc<AtomicUsize>,
    cancels: Arc<AtomicUsize>,
    updates: Arc<AtomicUsize>,
    /// Never answers `tasks/cancel`.
    cancel_hangs: bool,
}

impl ScriptedTaskServer {
    fn new(statuses: impl IntoIterator<Item = Option<TaskPayload>>) -> Self {
        Self {
            statuses: Arc::new(std::sync::Mutex::new(statuses.into_iter().collect())),
            gets: Arc::default(),
            cancels: Arc::default(),
            updates: Arc::default(),
            cancel_hangs: false,
        }
    }
}

impl ServerHandler for ScriptedTaskServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let task = Task::new(TASK_ID, TaskStatus::Working, TIMESTAMP, TIMESTAMP);
        Ok(CreateTaskResult::new(task).into())
    }

    async fn get_task(
        &self,
        params: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let status = {
            let mut statuses = self.statuses.lock().unwrap();
            if statuses.len() > 1 { statuses.pop_front().unwrap() } else { statuses[0].clone() }
        };
        let payload = status.ok_or_else(|| {
            ErrorData::invalid_params(format!("unknown task: {}", params.task_id), None)
        })?;
        let task = Task::new(&params.task_id, TaskStatus::Working, TIMESTAMP, TIMESTAMP);
        Ok(GetTaskResult::new(DetailedTask::new(task, payload)))
    }

    async fn update_task(
        &self,
        _params: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// The task finishes as the cancel arrives, and the specification requires
    /// rejecting a cancel for a terminal task with `-32602`.
    async fn cancel_task(
        &self,
        params: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        if self.cancel_hangs {
            std::future::pending::<()>().await;
        }
        *self.statuses.lock().unwrap() = [Some(completed())].into();
        Err(ErrorData::invalid_params(format!("task {} is already terminal", params.task_id), None))
    }
}

#[tokio::test]
async fn cancel_pending_tasks_releases_a_task_the_server_no_longer_knows() {
    let server = ScriptedTaskServer::new([None]);
    let client = InputClient::default().serve(spawn_server(server.clone())).await.unwrap();
    let toolset = McpToolset::new(client).with_task_support(fast_tasks());
    let error = toolset.call_tool_value("job", Default::default()).await.unwrap_err();
    assert!(error.to_string().contains("unknown task"), "{error}");

    toolset.cancel_pending_tasks().await.unwrap();
    assert_eq!(server.cancels.load(Ordering::SeqCst), 0, "an unknown task needs no tasks/cancel");
    let gets = server.gets.load(Ordering::SeqCst);
    toolset.cancel_pending_tasks().await.unwrap();
    assert_eq!(server.gets.load(Ordering::SeqCst), gets, "a released task is not checked again");
}

#[tokio::test]
async fn cancel_pending_tasks_releases_a_task_whose_cancel_is_rejected_as_finished() {
    let server = ScriptedTaskServer::new([Some(TaskPayload::Working)]);
    let client = InputClient::default().serve(spawn_server(server.clone())).await.unwrap();
    let toolset = McpToolset::new(client).with_task_support(fast_tasks());
    // Dropping the call once it polls leaves its task tracked, as an abandoned turn does.
    tokio::select! {
        result = toolset.call_tool_value("job", Default::default()) => {
            panic!("a working task must not finish: {result:?}");
        }
        () = async {
            while server.gets.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        } => {}
    }

    toolset.cancel_pending_tasks().await.unwrap();
    assert_eq!(server.cancels.load(Ordering::SeqCst), 1);
    let gets = server.gets.load(Ordering::SeqCst);
    toolset.cancel_pending_tasks().await.unwrap();
    assert_eq!(server.gets.load(Ordering::SeqCst), gets, "a released task is not checked again");
}

#[tokio::test]
async fn a_task_returned_without_task_support_is_cancelled() {
    let server = ScriptedTaskServer::new([Some(TaskPayload::Working)]);
    let client = InputClient::default().serve(spawn_server(server.clone())).await.unwrap();
    let toolset = McpToolset::new(client);
    let error = toolset.call_tool_value("job", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("McpToolset::with_task_support(McpTaskConfig::enabled())"), "{error}");
    assert_eq!(server.cancels.load(Ordering::SeqCst), 1, "an unpolled task must be cancelled");

    toolset.cancel_pending_tasks().await.unwrap();
    assert_eq!(server.cancels.load(Ordering::SeqCst), 1, "a finished task needs no second cancel");
}

#[tokio::test(start_paused = true)]
async fn a_cancel_the_server_never_acknowledges_does_not_hang_the_call() {
    let server = ScriptedTaskServer {
        cancel_hangs: true,
        ..ScriptedTaskServer::new([Some(TaskPayload::Working)])
    };
    let client = InputClient::default().serve(spawn_server(server.clone())).await.unwrap();
    let toolset = McpToolset::new(client);
    let call = toolset.call_tool_value("job", Default::default());
    let error = tokio::time::timeout(Duration::from_secs(30), call)
        .await
        .expect("an unanswered tasks/cancel must not hold the call")
        .unwrap_err();
    assert!(error.to_string().contains("was cancelled because"), "{error}");
    assert_eq!(server.cancels.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn re_sent_task_answers_do_not_count_as_input_rounds() {
    let asks = Some(TaskPayload::InputRequired {
        input_requests: [("answer".into(), input_request())].into(),
    });
    // The server keeps listing the answered request for two more polls.
    let server = ScriptedTaskServer::new([asks.clone(), asks.clone(), asks, Some(completed())]);
    let handler = InputClient::default();
    let handled = handler.0.clone();
    let client = handler.serve(spawn_server(server.clone())).await.unwrap();
    let toolset = McpToolset::new(client).with_task_support(fast_tasks().max_input_rounds(1));
    let value = toolset.call_tool_value("job", Default::default()).await.unwrap();
    assert_eq!(value["output"], json!({"done": true}));
    assert_eq!(handled.load(Ordering::SeqCst), 1, "the person is asked once");
    assert_eq!(server.updates.load(Ordering::SeqCst), 3, "each listing receives the cached answer");
}

#[tokio::test]
async fn a_working_status_clears_answered_task_input() {
    let ask = |message| {
        Some(TaskPayload::InputRequired {
            input_requests: [("answer".into(), prompt(message))].into(),
        })
    };
    let server = ScriptedTaskServer::new([
        ask("Name"),
        Some(TaskPayload::Working),
        ask("Nickname"),
        Some(completed()),
    ]);
    let handler = InputClient::default();
    let handled = handler.0.clone();
    let client = handler.serve(spawn_server(server)).await.unwrap();
    let toolset = McpToolset::new(client).with_task_support(fast_tasks());
    toolset.call_tool_value("job", Default::default()).await.unwrap();
    assert_eq!(handled.load(Ordering::SeqCst), 2, "a reused key after `working` is a new question");
}

// Sampling and roots are deprecated upstream by SEP-2577; these tests pin how
// MRTR input still treats them, so the deprecated items are used on purpose.
#[allow(deprecated)]
fn sampling_request() -> InputRequest {
    InputRequest::CreateMessage(CreateMessageRequest::new(CreateMessageRequestParams::new(
        vec![SamplingMessage::user_text("hello")],
        16,
    )))
}

#[allow(deprecated)]
fn roots_request() -> InputRequest {
    InputRequest::ListRoots(ListRootsRequest::default())
}

/// Answers every `tools/call` with one MRTR input request, without the checks
/// rmcp's server applies, as a hostile peer may. It also ignores the
/// negotiated revision, which is what lets it reach ADK's own client handler.
fn spawn_hostile_server(key: &'static str, input: InputRequest) -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);
        while let Some(message) = server.receive().await {
            let ClientJsonRpcMessage::Request(request) = message else {
                continue;
            };
            let result = match request.request {
                ClientRequest::InitializeRequest(initialize) => {
                    let mut result =
                        InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
                    result.protocol_version = initialize.params.protocol_version;
                    ServerResult::InitializeResult(result)
                }
                ClientRequest::CallToolRequest(_) => {
                    let requests = [(key.to_string(), input.clone())].into();
                    ServerResult::InputRequiredResult(InputRequiredResult::new(
                        Some(requests),
                        None,
                    ))
                }
                _ => continue,
            };
            if server.send(ServerJsonRpcMessage::response(result, request.id)).await.is_err() {
                break;
            }
        }
    });
    client_io
}

/// Answers each tool with one scripted MRTR round, then echoes the responses.
#[derive(Clone, Default)]
struct MrtrServer;

impl ServerHandler for MrtrServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if params.input_responses.is_some() || params.request_state.is_some() {
            let responses = serde_json::to_value(params.input_responses).unwrap();
            return Ok(CallToolResult::structured(responses).into());
        }
        let requests: InputRequests = match &*params.name {
            "sample" => [("sample".into(), sampling_request())].into(),
            "roots" => [("roots".into(), roots_request())].into(),
            "flood" => (0..=64).map(|index| (format!("q{index}"), input_request())).collect(),
            "empty" => return Ok(InputRequiredResult::new(Some(InputRequests::new()), None).into()),
            "wait" => return Ok(InputRequiredResult::new(None, Some("wait".into())).into()),
            "fail" => {
                let mut result = CallToolResult::structured_error(json!({"code": "E_QUOTA"}));
                result.content.clear();
                return Ok(result.into());
            }
            _ => [("answer".into(), input_request())].into(),
        };
        Ok(InputRequiredResult::new(Some(requests), None).into())
    }
}

/// Declares elicitation only, and counts any sampling or roots request it receives.
#[derive(Clone, Default)]
struct ElicitationOnlyClient(Arc<AtomicUsize>);

#[allow(deprecated)]
impl ClientHandler for ElicitationOnlyClient {
    fn get_info(&self) -> InitializeRequestParams {
        InputClient::default().get_info()
    }

    async fn create_message(
        &self,
        _params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, ErrorData> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ErrorData::internal_error("sampling was not declared", None))
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ListRootsResult::default())
    }
}

#[tokio::test]
async fn mrtr_sampling_and_roots_need_a_declared_capability() {
    let handler = ElicitationOnlyClient::default();
    let reached = handler.0.clone();
    let client =
        handler.serve_with_lifecycle(spawn_server(MrtrServer), discover_2026()).await.unwrap();
    let toolset = McpToolset::new(client);
    for (tool, capability) in [("sample", "sampling"), ("roots", "roots")] {
        let error =
            toolset.call_tool_value(tool, Default::default()).await.unwrap_err().to_string();
        let expected = format!("asks for {capability}, which this MCP client did not declare");
        assert!(error.contains(&expected), "{error}");
    }
    assert_eq!(reached.load(Ordering::SeqCst), 0, "undeclared handlers must not run");
}

/// Declares roots and answers with one workspace root.
#[derive(Clone)]
struct RootsClient;

#[allow(deprecated)]
impl ClientHandler for RootsClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::builder().enable_elicitation().enable_roots().build(),
            Implementation::new("roots-client", "1.0.0"),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Ok(ListRootsResult::new(vec![Root::new("file:///workspace")]))
    }
}

#[tokio::test]
async fn mrtr_roots_reach_a_client_that_declared_them() {
    let client =
        RootsClient.serve_with_lifecycle(spawn_server(MrtrServer), discover_2026()).await.unwrap();
    let toolset = McpToolset::new(client);
    let value = toolset.call_tool_value("roots", Default::default()).await.unwrap();
    assert_eq!(value["output"]["roots"]["roots"][0]["uri"], "file:///workspace", "{value}");
}

#[tokio::test]
async fn adk_handler_toolsets_apply_the_adk_input_policy() {
    let toolset = McpToolset::with_elicitation_handler(
        spawn_hostile_server("roots", roots_request()),
        Arc::new(AutoDeclineElicitationHandler),
    )
    .await
    .unwrap();
    let error = toolset.call_tool_value("job", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("MRTR roots are deprecated and not enabled by ADK"), "{error}");
}

#[cfg(feature = "mcp-sampling")]
#[tokio::test]
async fn a_sampling_handler_is_not_reachable_through_mrtr() {
    use adk_tool::sampling::{SamplingContent, SamplingHandler, SamplingRequest, SamplingResponse};

    struct CountingSampler(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl SamplingHandler for CountingSampler {
        async fn handle_create_message(
            &self,
            _request: SamplingRequest,
        ) -> adk_core::Result<SamplingResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(SamplingResponse {
                content: SamplingContent::text("sampled"),
                model: "test-model".into(),
                stop_reason: "endTurn".into(),
            })
        }
    }

    let sampled = Arc::new(AtomicUsize::new(0));
    let toolset = McpToolset::with_sampling_handler(
        spawn_hostile_server("sample", sampling_request()),
        Arc::new(AutoDeclineElicitationHandler),
        Arc::new(CountingSampler(sampled.clone())),
    )
    .await
    .unwrap();
    let error = toolset.call_tool_value("job", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("MRTR sampling is deprecated and not enabled by ADK"), "{error}");

    // The documented `serve_with_lifecycle` path builds the client first.
    let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
        .with_sampling_handler(Arc::new(CountingSampler(sampled.clone())))
        .serve(spawn_hostile_server("sample", sampling_request()))
        .await
        .unwrap();
    let error = McpToolset::new(client)
        .call_tool_value("job", Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("MRTR sampling is deprecated and not enabled by ADK"), "{error}");
    assert_eq!(sampled.load(Ordering::SeqCst), 0, "the sampling handler must not run");
}

#[tokio::test]
async fn caller_built_adk_handler_toolsets_apply_the_adk_input_policy() {
    let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
        .serve(spawn_hostile_server("roots", roots_request()))
        .await
        .unwrap();
    let toolset = McpToolset::new(client);
    let error = toolset.call_tool_value("job", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("MRTR roots are deprecated and not enabled by ADK"), "{error}");
}

#[tokio::test]
async fn inline_input_batches_are_capped() {
    let handler = InputClient::default();
    let handled = handler.0.clone();
    let client =
        handler.serve_with_lifecycle(spawn_server(MrtrServer), discover_2026()).await.unwrap();
    let toolset = McpToolset::new(client);
    let error = toolset.call_tool_value("flood", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("sent 65 input requests in one round; at most 64"), "{error}");
    assert_eq!(handled.load(Ordering::SeqCst), 0, "no prompt is shown for a rejected batch");
}

#[tokio::test]
async fn input_required_without_requests_or_state_is_rejected() {
    let client = InputClient::default()
        .serve_with_lifecycle(spawn_server(MrtrServer), discover_2026())
        .await
        .unwrap();
    let toolset = McpToolset::new(client);
    let error = toolset.call_tool_value("empty", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("neither input requests nor request state"), "{error}");
}

#[tokio::test]
async fn state_only_input_rounds_back_off_before_retrying() {
    let client = InputClient::default()
        .serve_with_lifecycle(spawn_server(MrtrServer), discover_2026())
        .await
        .unwrap();
    let toolset = McpToolset::new(client);
    let started = Instant::now();
    toolset.call_tool_value("wait", Default::default()).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(50), "{:?}", started.elapsed());
}

#[tokio::test]
async fn structured_only_tool_errors_keep_their_detail() {
    let client = InputClient::default()
        .serve_with_lifecycle(spawn_server(MrtrServer), discover_2026())
        .await
        .unwrap();
    let toolset = McpToolset::new(client);
    let error = toolset.call_tool_value("fail", Default::default()).await.unwrap_err().to_string();
    assert!(error.contains("E_QUOTA"), "{error}");
}

/// Holds an elicitation open until the test releases it.
struct GatedClient {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl ClientHandler for GatedClient {
    fn get_info(&self) -> InitializeRequestParams {
        InputClient::default().get_info()
    }

    async fn create_elicitation(
        &self,
        _params: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ElicitResult::new(ElicitationAction::Accept).with_content(json!({"name": "Ferris"})))
    }
}

#[tokio::test]
async fn a_pending_elicitation_does_not_hold_the_connection_lock() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let handler = GatedClient { entered: entered.clone(), release: release.clone() };
    let client =
        handler.serve_with_lifecycle(spawn_server(MrtrServer), discover_2026()).await.unwrap();
    let toolset = McpToolset::new(client);
    let probe = async {
        entered.notified().await;
        let closed = tokio::time::timeout(Duration::from_secs(1), toolset.is_closed())
            .await
            .expect("the connection lock must be free while a person answers");
        release.notify_one();
        closed
    };
    let (value, closed) = tokio::join!(toolset.call_tool_value("ask", Default::default()), probe);
    assert!(!closed);
    assert_eq!(value.unwrap()["output"]["answer"]["content"]["name"], "Ferris");
}
