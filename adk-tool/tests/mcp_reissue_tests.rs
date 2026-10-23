//! Parked MCP calls survive a restart by re-issue (spec 3.2 §7.7, §10 T2): an MRTR call
//! parked at `input_required` completes on a new toolset from its stored `requestState` with
//! one execution, a parked task resumes `tasks/get` and is answered through `tasks/update`,
//! and a legacy call is run again only when the tool is retry-safe.
//!
//! Every server is an in-process rmcp server; a "restart" serializes the parked call, drops
//! the toolset and its connection, and re-issues the call on a new connection.
#![cfg(feature = "mcp")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ElicitationHandler, McpCallOutcome,
    McpTaskConfig, McpToolset, ParkedCallKind, ParkedMcpCall,
};
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// Names the mode when this binary runs as the stdio MCP server [`Mailer`].
const SERVER_ENV: &str = "ADK_TOOL_MCP_REISSUE_TEST_SERVER";

/// File the stdio [`Mailer`] appends a line to for each execution.
const EXECUTION_LOG_ENV: &str = "ADK_TOOL_MCP_REISSUE_EXECUTION_LOG";

/// `2026-07-28`, which MRTR and the tasks extension require, is reached through
/// `server/discover`.
fn discover_2026() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }
}

fn question(message: &str) -> InputRequest {
    InputRequest::Elicitation(ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: message.into(),
        requested_schema: serde_json::from_value(
            json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } }),
        )
        .unwrap(),
    }))
}

fn accept() -> Value {
    serde_json::to_value(ElicitResult::new(ElicitationAction::Accept)).unwrap()
}

fn arguments(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// Serves `server` on one end of an in-process pipe and returns the other.
fn spawn_server<H: ServerHandler>(server: H) -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    client_io
}

/// A toolset named `name` on a new connection to `server`, on ADK's client handler.
async fn connect<H: ServerHandler>(
    name: &str,
    server: H,
    handler: Arc<dyn ElicitationHandler>,
    lifecycle: ClientLifecycleMode,
    tasks: bool,
) -> McpToolset<AdkClientHandler> {
    let mut client_handler = AdkClientHandler::new(handler);
    if tasks {
        client_handler = client_handler.with_tasks();
    }
    let client =
        client_handler.serve_with_lifecycle(spawn_server(server), lifecycle).await.unwrap();
    McpToolset::new(client).with_name(name).with_task_support(
        McpTaskConfig::enabled().poll_interval(Duration::from_millis(1)).max_input_rounds(3),
    )
}

/// What a restart keeps of a parked call: its JSON.
fn across_a_restart(parked: &ParkedMcpCall) -> ParkedMcpCall {
    serde_json::from_str(&serde_json::to_string(parked).unwrap()).unwrap()
}

fn parked(outcome: McpCallOutcome) -> ParkedMcpCall {
    match outcome {
        McpCallOutcome::Parked(parked) => parked,
        McpCallOutcome::Completed(value) => panic!("the call completed: {value}"),
    }
}

/// Records the parked call each elicitation it answers belongs to, and accepts.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<Option<ParkedMcpCall>>>,
}

#[async_trait::async_trait]
impl ElicitationHandler for Recorder {
    async fn handle_form_elicitation(
        &self,
        _message: &str,
        _schema: &ElicitationSchema,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        self.seen.lock().unwrap().push(ParkedMcpCall::current());
        Ok(ElicitResult::new(ElicitationAction::Accept))
    }

    async fn handle_url_elicitation(
        &self,
        _message: &str,
        _url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        self.seen.lock().unwrap().push(ParkedMcpCall::current());
        Ok(ElicitResult::new(ElicitationAction::Accept))
    }
}

// --- MRTR -------------------------------------------------------------------------------

/// A stateless MRTR server: `send_email` asks `confirm` and sends only when an accepted
/// answer arrives with the state it issued. A state numbered `lapses` or lower has lapsed,
/// and the server asks again under the next one.
#[derive(Clone, Default)]
struct Mailer {
    calls: Arc<AtomicUsize>,
    executions: Arc<AtomicUsize>,
    lapses: usize,
}

impl ServerHandler for Mailer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "send_email",
            "Sends an email.",
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let ask = |state: usize| {
            InputRequiredResult::new(
                Some([("confirm".to_string(), question("Send the email?"))].into()),
                Some(format!("state-{state}")),
            )
            .into()
        };
        let (Some(state), Some(responses)) = (&params.request_state, &params.input_responses)
        else {
            return Ok(ask(1));
        };
        let state: usize = state.trim_start_matches("state-").parse().unwrap();
        if state <= self.lapses {
            return Ok(ask(state + 1));
        }
        if responses["confirm"]["action"] != "accept" {
            return Ok(CallToolResult::success(vec![ContentBlock::text("not sent")]).into());
        }
        self.executions.fetch_add(1, Ordering::SeqCst);
        // A server in its own process records each execution where the test can count it.
        if let Ok(path) = std::env::var(EXECUTION_LOG_ENV) {
            use std::io::Write;
            let mut log = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            writeln!(log, "{}", std::process::id()).unwrap();
        }
        let to = params.arguments.as_ref().and_then(|args| args["to"].as_str()).unwrap_or("");
        Ok(CallToolResult::success(vec![ContentBlock::text(format!("sent to {to}"))]).into())
    }
}

async fn mailer_toolset(server: Mailer) -> McpToolset<AdkClientHandler> {
    connect("mail", server, Arc::new(AutoDeclineElicitationHandler), discover_2026(), false).await
}

#[tokio::test]
async fn an_mrtr_call_parked_before_a_restart_completes_on_a_new_toolset_with_one_execution() {
    let server = Mailer::default();
    let before = mailer_toolset(server.clone()).await;

    let outcome = before
        .call_tool_resumable(Some("call-1"), "send_email", arguments(json!({ "to": "ops" })))
        .await
        .unwrap();
    let parked = parked(outcome);
    let expected_requests: InputRequests =
        [("confirm".to_string(), question("Send the email?"))].into();
    assert_eq!(parked.server_id, "mail");
    assert_eq!(parked.tool_name, "send_email");
    assert_eq!(parked.arguments, Some(arguments(json!({ "to": "ops" }))));
    assert_eq!(parked.call_id.as_deref(), Some("call-1"));
    assert_eq!(parked.request_state.as_deref(), Some("state-1"));
    assert_eq!(parked.input_requests, expected_requests);
    assert_eq!(
        parked.input_requests_digest,
        ParkedMcpCall::digest_input_requests(&expected_requests)
    );
    assert_eq!(parked.input_rounds, 1);
    assert_eq!(parked.kind, ParkedCallKind::Mrtr);

    // The process restarts: only the parked call's JSON survives.
    let checkpoint = serde_json::to_string(&parked).unwrap();
    drop(before);
    let after = mailer_toolset(server.clone()).await;
    let restored: ParkedMcpCall = serde_json::from_str(&checkpoint).unwrap();

    let outcome =
        after.reissue(&restored, [("confirm".to_string(), accept())].into()).await.unwrap();

    assert_eq!(outcome, McpCallOutcome::Completed(json!({ "output": "sent to ops" })));
    assert_eq!(server.executions.load(Ordering::SeqCst), 1);
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_lapsed_request_state_parks_the_call_again_under_max_input_rounds() {
    let server = Mailer { lapses: 1, ..Mailer::default() };
    let toolset = mailer_toolset(server.clone()).await;
    let first = parked(
        toolset
            .call_tool_resumable(None, "send_email", arguments(json!({ "to": "ops" })))
            .await
            .unwrap(),
    );

    // `state-1` lapsed: the server asks again under `state-2`.
    let answer = || [("confirm".to_string(), accept())].into();
    let second = parked(toolset.reissue(&across_a_restart(&first), answer()).await.unwrap());
    assert_eq!(second.request_state.as_deref(), Some("state-2"));
    assert_eq!(second.input_rounds, 2);
    assert_eq!(second.kind, ParkedCallKind::Mrtr);

    let done = toolset.reissue(&across_a_restart(&second), answer()).await.unwrap();
    assert_eq!(done, McpCallOutcome::Completed(json!({ "output": "sent to ops" })));
    assert_eq!(server.executions.load(Ordering::SeqCst), 1);

    // A server whose state always lapses stops at the toolset's three input rounds.
    let always = Mailer { lapses: usize::MAX, ..Mailer::default() };
    let toolset = mailer_toolset(always.clone()).await;
    let mut call =
        parked(toolset.call_tool_resumable(None, "send_email", Default::default()).await.unwrap());
    for round in 2..=3 {
        call = parked(toolset.reissue(&across_a_restart(&call), answer()).await.unwrap());
        assert_eq!(call.input_rounds, round);
    }
    let error = toolset.reissue(&across_a_restart(&call), answer()).await.unwrap_err();
    assert!(error.message.contains("exceeded 3 MRTR input rounds"), "{}", error.message);
    assert_eq!(always.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_handler_answering_in_process_sees_the_parked_call() {
    let server = Mailer::default();
    let recorder = Arc::new(Recorder::default());
    let toolset = connect("mail", server.clone(), recorder.clone(), discover_2026(), false).await;
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(SimpleToolContext::new("reissue"));
    let tools = toolset.tools(ctx).await.unwrap();
    let send = tools.iter().find(|tool| tool.name() == "send_email").unwrap();

    // An agent's call: the handler answers in process, inside the call's parked form.
    let call = Arc::new(SimpleToolContext::new("reissue").with_function_call_id("call-3"));
    let value = send.execute(call, json!({ "to": "ops" })).await.unwrap();

    assert_eq!(value, json!({ "output": "sent to ops" }));
    let seen = recorder.seen.lock().unwrap().clone();
    let [Some(parked)] = seen.as_slice() else { panic!("{seen:?}") };
    assert_eq!(parked.kind, ParkedCallKind::Mrtr);
    assert_eq!(parked.tool_name, "send_email");
    assert_eq!(parked.call_id.as_deref(), Some("call-3"));
    assert_eq!(parked.request_state.as_deref(), Some("state-1"));
    assert_eq!(parked.input_requests.keys().collect::<Vec<_>>(), ["confirm"]);
    assert_eq!(parked.input_rounds, 1);
    assert_eq!(server.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_reissue_refuses_another_server_a_changed_question_and_an_unasked_answer() {
    let server = Mailer::default();
    let toolset = mailer_toolset(server.clone()).await;
    let parked =
        parked(toolset.call_tool_resumable(None, "send_email", Default::default()).await.unwrap());
    let answer = || [("confirm".to_string(), accept())].into();

    let elsewhere = connect(
        "calendar",
        server.clone(),
        Arc::new(AutoDeclineElicitationHandler),
        discover_2026(),
        false,
    )
    .await;
    let error = elsewhere.reissue(&parked, answer()).await.unwrap_err();
    assert_eq!(error.code, "tool.mcp.reissue_refused");
    assert!(error.message.contains("toolset 'mail'"), "{}", error.message);

    let mut changed = parked.clone();
    changed.input_requests.insert("confirm".to_string(), question("Delete every email?"));
    let error = toolset.reissue(&changed, answer()).await.unwrap_err();
    assert!(error.message.contains("do not match their digest"), "{}", error.message);

    let unasked = [("wire_money".to_string(), accept())].into();
    let error = toolset.reissue(&parked, unasked).await.unwrap_err();
    assert!(error.message.contains("'wire_money' has no input request"), "{}", error.message);

    assert_eq!(server.calls.load(Ordering::SeqCst), 1, "a refused re-issue sends nothing");
}

// --- Tasks ------------------------------------------------------------------------------

/// `approve_refund` runs as a task that asks `confirm` and refunds once answered. The task
/// manager is shared by every connection, as a durable task store is.
#[derive(Clone, Default)]
struct Refunds {
    tasks: Arc<rmcp::task_manager::TaskManager>,
    executions: Arc<AtomicUsize>,
    updates: Arc<AtomicUsize>,
    cancels: Arc<AtomicUsize>,
}

impl ServerHandler for Refunds {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let executions = Arc::clone(&self.executions);
        let task = self.tasks.spawn(Default::default(), move |ctx| {
            Box::pin(async move {
                let answer = ctx.request_input("confirm", question("Refund 40 EUR?")).await?;
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(CallToolResult::structured(json!({ "refunded": answer["action"] == "accept" })))
            })
        });
        Ok(CreateTaskResult::new(task).into())
    }

    async fn get_task(
        &self,
        params: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        Ok(GetTaskResult::new(self.tasks.get_task(&params.task_id)?))
    }

    async fn update_task(
        &self,
        params: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        self.tasks.update_task(&params.task_id, params.input_responses)
    }

    async fn cancel_task(
        &self,
        params: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.tasks.cancel_task(&params.task_id)
    }
}

async fn refunds_toolset(server: Refunds) -> McpToolset<AdkClientHandler> {
    connect("refunds", server, Arc::new(AutoDeclineElicitationHandler), discover_2026(), true).await
}

#[tokio::test]
async fn a_parked_task_resumes_on_a_new_toolset_and_answers_through_tasks_update() {
    let server = Refunds::default();
    let before = refunds_toolset(server.clone()).await;

    let parked = parked(
        before
            .call_tool_resumable(Some("call-9"), "approve_refund", Default::default())
            .await
            .unwrap(),
    );
    let ParkedCallKind::Task { task_id } = &parked.kind else { panic!("{parked:?}") };
    assert_eq!(parked.input_requests.keys().collect::<Vec<_>>(), ["confirm"]);
    assert_eq!(parked.request_state, None);
    assert_eq!(parked.call_id.as_deref(), Some("call-9"));
    assert_eq!(parked.input_rounds, 1);

    // A parked task is not the toolset's to cancel: closing it leaves the task waiting.
    before.close().await;
    assert_eq!(server.cancels.load(Ordering::SeqCst), 0);
    let waiting = server.tasks.get_task(task_id).unwrap();
    assert_eq!(waiting.task.status, TaskStatus::InputRequired);

    let after = refunds_toolset(server.clone()).await;
    let outcome = after
        .reissue(&across_a_restart(&parked), [("confirm".to_string(), accept())].into())
        .await
        .unwrap();

    let refunded = json!({ "output": { "refunded": true }, "text": [r#"{"refunded":true}"#] });
    assert_eq!(outcome, McpCallOutcome::Completed(refunded));
    assert_eq!(server.executions.load(Ordering::SeqCst), 1);
    assert_eq!(server.updates.load(Ordering::SeqCst), 1);
    assert_eq!(server.cancels.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_task_resumes_only_on_a_toolset_with_task_support() {
    let server = Refunds::default();
    let toolset = refunds_toolset(server.clone()).await;
    let parked = parked(
        toolset.call_tool_resumable(None, "approve_refund", Default::default()).await.unwrap(),
    );

    let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
        .serve_with_lifecycle(spawn_server(server.clone()), discover_2026())
        .await
        .unwrap();
    let without_tasks = McpToolset::new(client).with_name("refunds");
    let error = without_tasks.reissue(&parked, Default::default()).await.unwrap_err();

    assert_eq!(error.code, "tool.mcp.reissue_refused");
    assert!(error.message.contains("needs task support"), "{}", error.message);
    assert_eq!(server.updates.load(Ordering::SeqCst), 0);
}

// --- Legacy -----------------------------------------------------------------------------

/// A server before 2026-07-28: each call asks with `elicitation/create` on the live
/// connection, then runs. `delete_file` declares no hints; `lookup` is idempotent.
#[derive(Clone, Default)]
struct Files {
    calls: Arc<AtomicUsize>,
    executions: Arc<AtomicUsize>,
}

impl ServerHandler for Files {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("delete_file", "Deletes a file.", Arc::clone(&schema)),
            Tool::new("lookup", "Looks a file up.", schema)
                .with_annotations(ToolAnnotations::new().idempotent(true)),
        ]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = context
            .peer
            .create_elicitation(ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: format!("Run {}?", params.name),
                requested_schema: ElicitationSchema::new(Default::default()),
            })
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        self.executions.fetch_add(1, Ordering::SeqCst);
        let text = format!("{:?}", answer.action);
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

async fn files_toolset(server: Files, recorder: Arc<Recorder>) -> McpToolset<AdkClientHandler> {
    connect("files", server, recorder, ClientLifecycleMode::Initialize, false).await
}

/// Calls `tool` once, answered in process, and returns the parked call the handler saw.
async fn legacy_park(server: &Files, tool: &str) -> ParkedMcpCall {
    let recorder = Arc::new(Recorder::default());
    let toolset = files_toolset(server.clone(), recorder.clone()).await;
    let args = arguments(json!({ "path": "notes.txt" }));
    let outcome = toolset.call_tool_resumable(Some("call-7"), tool, args.clone()).await.unwrap();
    assert_eq!(outcome, McpCallOutcome::Completed(json!({ "output": "Accept" })));
    let seen = recorder.seen.lock().unwrap().clone();
    let [Some(parked)] = seen.as_slice() else { panic!("{seen:?}") };
    assert_eq!(parked.kind, ParkedCallKind::Legacy);
    assert_eq!(parked.server_id, "files");
    assert_eq!(parked.tool_name, tool);
    assert_eq!(parked.arguments, Some(args));
    assert_eq!(parked.call_id.as_deref(), Some("call-7"));
    assert_eq!(parked.request_state, None);
    assert_eq!(parked.input_requests.len(), 1);
    assert_eq!(
        parked.input_requests_digest,
        ParkedMcpCall::digest_input_requests(&parked.input_requests)
    );
    parked.clone()
}

#[tokio::test]
async fn a_parked_legacy_call_that_is_not_retry_safe_is_not_run() {
    let server = Files::default();
    let parked = legacy_park(&server, "delete_file").await;
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);

    let recorder = Arc::new(Recorder::default());
    let after = files_toolset(server.clone(), recorder.clone()).await;
    let error = after.reissue(&across_a_restart(&parked), Default::default()).await.unwrap_err();

    assert_eq!(error.code, "tool.mcp.not_run");
    assert!(error.message.contains("'delete_file' was not run"), "{}", error.message);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1, "the call was not sent again");
    assert_eq!(server.executions.load(Ordering::SeqCst), 1);
    assert!(recorder.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_parked_legacy_call_that_is_retry_safe_runs_again_and_its_question_parks_again() {
    let server = Files::default();
    let parked = legacy_park(&server, "lookup").await;

    let recorder = Arc::new(Recorder::default());
    let after = files_toolset(server.clone(), recorder.clone()).await;
    let outcome = after.reissue(&across_a_restart(&parked), Default::default()).await.unwrap();

    assert_eq!(outcome, McpCallOutcome::Completed(json!({ "output": "Accept" })));
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    // The re-issued call's new question reached the handler with the call's parked form.
    let seen = recorder.seen.lock().unwrap().clone();
    let [Some(again)] = seen.as_slice() else { panic!("{seen:?}") };
    assert_eq!(again.kind, ParkedCallKind::Legacy);
    assert_eq!(again.tool_name, "lookup");
    assert_eq!(again.call_id.as_deref(), Some("call-7"));
}

#[tokio::test]
async fn the_handler_answering_a_task_in_process_sees_its_parked_call() {
    let server = Refunds::default();
    let recorder = Arc::new(Recorder::default());
    let toolset = connect("refunds", server.clone(), recorder.clone(), discover_2026(), true).await;

    let value = toolset.call_tool_value("approve_refund", Default::default()).await.unwrap();

    assert_eq!(value["output"], json!({ "refunded": true }));
    let seen = recorder.seen.lock().unwrap().clone();
    let [Some(parked)] = seen.as_slice() else { panic!("{seen:?}") };
    assert!(matches!(parked.kind, ParkedCallKind::Task { .. }), "{parked:?}");
    assert_eq!(parked.input_requests.keys().collect::<Vec<_>>(), ["confirm"]);
    assert_eq!(parked.input_rounds, 1);
}

// --- A managed server in its own process --------------------------------------------------

/// Serves [`Mailer`] over stdio when [`SERVER_ENV`] is set.
#[tokio::test]
#[ignore = "runs only as the stdio MCP server the manager test starts"]
async fn stdio_server() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running = Mailer::default().serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

/// Records the parked call it is asked about and never answers, as a handler that parks the
/// question in an approval inbox waits.
#[derive(Default)]
struct Inbox {
    parked: Mutex<Option<ParkedMcpCall>>,
    asked: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ElicitationHandler for Inbox {
    async fn handle_form_elicitation(
        &self,
        _message: &str,
        _schema: &ElicitationSchema,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        *self.parked.lock().unwrap() = ParkedMcpCall::current();
        self.asked.notify_one();
        std::future::pending().await
    }

    async fn handle_url_elicitation(
        &self,
        _message: &str,
        _url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        std::future::pending().await
    }
}

fn managed_mailer(log: &std::path::Path) -> adk_tool::mcp::McpServerManager {
    use adk_tool::mcp::manager::McpLifecycleMode;
    let config = adk_tool::mcp::McpServerConfig {
        command: std::env::current_exe().unwrap().display().to_string(),
        args: ["stdio_server", "--exact", "--ignored", "--quiet", "--test-threads=1"]
            .map(str::to_string)
            .to_vec(),
        env: [
            (SERVER_ENV.to_string(), "mailer".to_string()),
            (EXECUTION_LOG_ENV.to_string(), log.display().to_string()),
        ]
        .into(),
        lifecycle: McpLifecycleMode::Discover,
        ..Default::default()
    };
    adk_tool::mcp::McpServerManager::new([("mail".to_string(), config)].into())
}

#[tokio::test]
async fn a_managed_call_parked_in_the_inbox_completes_once_after_a_restart() {
    let dir = std::env::temp_dir().join(format!("adk-tool-reissue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("executions.log");
    let inbox = Arc::new(Inbox::default());
    let manager = managed_mailer(&log).with_elicitation_handler(inbox.clone());
    manager.start_server("mail").await.unwrap();
    let tools = manager.tools(Arc::new(SimpleToolContext::new("reissue"))).await.unwrap();
    let send = tools.iter().find(|tool| tool.name() == "send_email").unwrap().clone();

    // The agent's call waits while the question sits in the inbox.
    let call = tokio::spawn(async move {
        let ctx = Arc::new(SimpleToolContext::new("reissue").with_function_call_id("call-5"));
        send.execute(ctx, json!({ "to": "ops" })).await
    });
    tokio::time::timeout(Duration::from_secs(10), inbox.asked.notified()).await.unwrap();
    let checkpoint = serde_json::to_string(&inbox.parked.lock().unwrap().clone().unwrap()).unwrap();

    // The process stops: the call, its connection and the server process are gone.
    call.abort();
    manager.shutdown().await.unwrap();
    drop(manager);

    let restarted = managed_mailer(&log);
    restarted.start_server("mail").await.unwrap();
    let parked: ParkedMcpCall = serde_json::from_str(&checkpoint).unwrap();
    assert_eq!(parked.server_id, "mail");
    assert_eq!(parked.call_id.as_deref(), Some("call-5"));
    let outcome = restarted
        .reissue_parked_call(&parked, [("confirm".to_string(), accept())].into())
        .await
        .unwrap();

    assert_eq!(outcome, McpCallOutcome::Completed(json!({ "output": "sent to ops" })));
    let executions = std::fs::read_to_string(&log).unwrap();
    assert_eq!(executions.lines().count(), 1, "one execution across both processes");
    restarted.shutdown().await.unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
