//! An agent behind an `AgentTool` runs as a delegate of its caller.
//!
//! The delegate shares the caller's limits, governance, approvals and session scope, but not
//! the caller's handoff topology, invocation toolsets or context cache. A delegate that holds a
//! call for confirmation holds its caller's call with it: the caller's run ends on the
//! delegate's request, and once that request has a decision the caller resumes its call, the
//! delegate resumes its own, and the held call runs once. Every test drives real `LlmAgent`s
//! through a `Runner` with scripted models.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    ApprovalScope, ApprovalStore, CallbackContext, Content, ErrorCategory, Event,
    InMemoryApprovalStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    RunConfig, SessionId, Tool, ToolApproval, ToolConfirmationDecision, ToolConfirmationRequest,
    ToolContext, UserId, is_confirmation_pending,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::{AgentTool, BasicToolset};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "delegation";
const USER: &str = "user";
const SESSION: &str = "session";

// --- Harness ---

/// Replays scripted responses and records every request.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()), requests: Mutex::new(Vec::new()) })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req);
        let response =
            self.responses.lock().unwrap().pop_front().unwrap_or_else(|| text_response("done"));
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn text_response(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        turn_complete: true,
        ..Default::default()
    }
}

fn call(name: &str, args: Value, id: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args,
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// The function responses in `request`, as `(call id, response)`.
fn function_responses(request: &LlmRequest) -> Vec<(Option<String>, Value)> {
    request
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Records the arguments of every execution.
struct RecordingTool {
    name: &'static str,
    executions: Arc<Mutex<Vec<Value>>>,
}

impl RecordingTool {
    fn new(name: &'static str) -> (Arc<Self>, Arc<Mutex<Vec<Value>>>) {
        let executions = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Self { name, executions: executions.clone() }), executions)
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "deletes a file"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "deleted": true }))
    }
}

struct Harness {
    runner: Runner,
    store: Arc<InMemoryApprovalStore>,
}

async fn harness(root: LlmAgent, config: RunConfig) -> Harness {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let store = Arc::new(InMemoryApprovalStore::new());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(root))
        .session_service(sessions as Arc<dyn SessionService>)
        .approval_store(store.clone() as Arc<dyn ApprovalStore>)
        .run_config(config)
        .build()
        .unwrap();
    Harness { runner, store }
}

impl Harness {
    /// Runs one turn; `None` sends the empty turn that resumes held calls.
    async fn run(&self, text: Option<&str>, config: Option<RunConfig>) -> Vec<Event> {
        let content = match text {
            Some(text) => Content::new("user").with_text(text),
            None => Content::new("user"),
        };
        let stream = self
            .runner
            .run_with_config(
                UserId::new(USER).unwrap(),
                SessionId::new(SESSION).unwrap(),
                content,
                config,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish")
            .into_iter()
            .map(|event| event.expect("the run must not fail"))
            .collect()
    }

    fn scope(&self) -> ApprovalScope {
        ApprovalScope::new(APP, USER, SESSION)
    }
}

fn confirmation_requests(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
}

/// `ops` delegates to `helper` through an `AgentTool`; `helper` must confirm `delete_file`.
struct Delegation {
    ops_model: Arc<ScriptedModel>,
    helper_model: Arc<ScriptedModel>,
    deletions: Arc<Mutex<Vec<Value>>>,
    root: LlmAgent,
}

fn delegation() -> Delegation {
    let helper_model = ScriptedModel::new(vec![
        call("delete_file", json!({ "path": "a.txt" }), "inner-1"),
        text_response("deleted a.txt"),
    ]);
    let (delete_file, deletions) = RecordingTool::new("delete_file");
    let helper = LlmAgentBuilder::new("helper")
        .description("manages files")
        .model(helper_model.clone())
        .tool(delete_file)
        .require_tool_confirmation("delete_file")
        .build()
        .unwrap();
    let ops_model = ScriptedModel::new(vec![
        call("helper", json!({ "request": "delete a.txt" }), "call-1"),
        text_response("all done"),
    ]);
    let root = LlmAgentBuilder::new("ops")
        .model(ops_model.clone())
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .build()
        .unwrap();
    Delegation { ops_model, helper_model, deletions, root }
}

// --- Held calls ---

#[tokio::test]
async fn a_delegates_held_call_is_held_by_its_caller_and_runs_once_after_approval() {
    let Delegation { ops_model, helper_model, deletions, root } = delegation();
    let harness = harness(root, RunConfig::default()).await;

    let first = harness.run(Some("tidy up"), None).await;
    let requests = confirmation_requests(&first);
    assert_eq!(requests.len(), 1, "the caller surfaces the delegate's request");
    assert_eq!(requests[0].tool_name, "delete_file");
    assert_eq!(requests[0].args, json!({ "path": "a.txt" }));
    assert_eq!(requests[0].function_call_id.as_deref(), Some("call-1"));
    assert!(deletions.lock().unwrap().is_empty());
    assert_eq!(ops_model.requests().len(), 1, "the caller's run ends on the hold");
    let pending = harness.store.pending(&harness.scope()).await.unwrap();
    assert_eq!(pending.len(), 1, "the delegate's call waits in the caller's session scope");
    assert_eq!(pending[0].fingerprint, requests[0].fingerprint());

    harness
        .store
        .decide(&harness.scope(), &requests[0].fingerprint(), ToolApproval::approve())
        .await
        .unwrap();
    let second = harness.run(None, None).await;
    assert!(confirmation_requests(&second).is_empty());
    assert_eq!(*deletions.lock().unwrap(), vec![json!({ "path": "a.txt" })]);
    // The delegate resumed its held call instead of asking its model again.
    let helper_requests = helper_model.requests();
    assert_eq!(helper_requests.len(), 2);
    assert!(
        function_responses(&helper_requests[1])
            .iter()
            .any(|(id, response)| id.as_deref() == Some("inner-1")
                && *response == json!({ "deleted": true }))
    );
    let ops_requests = ops_model.requests();
    let delegated = function_responses(ops_requests.last().unwrap());
    assert_eq!(delegated, vec![(Some("call-1".into()), json!({ "response": "deleted a.txt" }))]);

    harness.run(Some("anything else?"), None).await;
    assert_eq!(deletions.lock().unwrap().len(), 1, "an approval authorizes one execution");
}

#[tokio::test]
async fn a_call_id_decision_for_the_surfaced_request_reaches_the_delegates_call() {
    let Delegation { deletions, root, .. } = delegation();
    let harness = harness(root, RunConfig::default()).await;

    let first = harness.run(Some("tidy up"), None).await;
    let request = confirmation_requests(&first).pop().expect("a request is surfaced");
    let call_id = request.function_call_id.clone().unwrap();
    let decided = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(
            call_id.clone(),
            ToolConfirmationDecision::Approve,
        )]))
        .tool_confirmation_fingerprints(HashMap::from([(call_id, request.fingerprint())]))
        .build();
    harness.run(None, Some(decided)).await;
    assert_eq!(deletions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_denied_delegated_call_is_reported_to_the_delegate_and_not_run() {
    let Delegation { helper_model, deletions, root, .. } = delegation();
    let harness = harness(root, RunConfig::default()).await;

    let first = harness.run(Some("tidy up"), None).await;
    let request = confirmation_requests(&first).pop().expect("a request is surfaced");
    harness
        .store
        .decide(&harness.scope(), &request.fingerprint(), ToolApproval::deny())
        .await
        .unwrap();
    harness.run(None, None).await;
    assert!(deletions.lock().unwrap().is_empty());
    let helper_requests = helper_model.requests();
    let answered = function_responses(helper_requests.last().unwrap());
    assert!(answered.iter().any(|(id, response)| id.as_deref() == Some("inner-1")
        && !is_confirmation_pending(response)
        && response["error"].as_str().is_some_and(|error| error.contains("denied"))));
}

#[tokio::test]
async fn a_hold_two_agent_tools_deep_reaches_the_outermost_caller() {
    let (delete_file, deletions) = RecordingTool::new("delete_file");
    let inner = LlmAgentBuilder::new("inner")
        .description("deletes files")
        .model(ScriptedModel::new(vec![
            call("delete_file", json!({ "path": "b.txt" }), "inner-1"),
            text_response("deleted b.txt"),
        ]))
        .tool(delete_file)
        .require_tool_confirmation("delete_file")
        .build()
        .unwrap();
    let middle = LlmAgentBuilder::new("middle")
        .description("delegates file work")
        .model(ScriptedModel::new(vec![
            call("inner", json!({ "request": "delete b.txt" }), "middle-1"),
            text_response("inner deleted b.txt"),
        ]))
        .tool(Arc::new(AgentTool::new(Arc::new(inner))))
        .build()
        .unwrap();
    let root = LlmAgentBuilder::new("ops")
        .model(ScriptedModel::new(vec![
            call("middle", json!({ "request": "clean b.txt" }), "call-1"),
            text_response("all done"),
        ]))
        .tool(Arc::new(AgentTool::new(Arc::new(middle))))
        .build()
        .unwrap();
    let harness = harness(root, RunConfig::default()).await;

    let first = harness.run(Some("tidy up"), None).await;
    let request = confirmation_requests(&first).pop().expect("a request is surfaced");
    assert_eq!(request.tool_name, "delete_file");
    assert_eq!(request.function_call_id.as_deref(), Some("call-1"));
    assert_eq!(
        request.acting.as_ref().map(|acting| acting.agent.id.as_str()),
        Some("urn:adk:agent:delegation/inner"),
        "the request names the agent that made the call"
    );

    let call_id = request.function_call_id.clone().unwrap();
    let decided = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(call_id, ToolConfirmationDecision::Approve)]))
        .build();
    let second = harness.run(None, Some(decided)).await;
    assert_eq!(*deletions.lock().unwrap(), vec![json!({ "path": "b.txt" })]);
    assert!(confirmation_requests(&second).is_empty());
}

// --- What the delegate inherits ---

#[tokio::test]
async fn the_delegate_gets_its_own_topology_not_its_callers() {
    let helper_model = ScriptedModel::new(vec![text_response("summary")]);
    let helper = LlmAgentBuilder::new("helper")
        .description("summarises")
        .model(helper_model.clone())
        .build()
        .unwrap();
    let specialist_model = ScriptedModel::new(vec![
        call("helper", json!({ "request": "summarise" }), "call-1"),
        text_response("done"),
    ]);
    let specialist = LlmAgentBuilder::new("specialist")
        .description("handles reports")
        .model(specialist_model.clone())
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .build()
        .unwrap();
    let peer = LlmAgentBuilder::new("peer")
        .description("handles the rest")
        .model(ScriptedModel::new(Vec::new()))
        .build()
        .unwrap();
    let root = LlmAgentBuilder::new("coordinator")
        .model(ScriptedModel::new(vec![call(
            "transfer_to_agent",
            json!({ "agent_name": "specialist" }),
            "transfer-1",
        )]))
        .sub_agent(Arc::new(specialist))
        .sub_agent(Arc::new(peer))
        .build()
        .unwrap();
    let (client_files, _) = RecordingTool::new("client_files");
    let config = RunConfig::builder()
        .cached_content("cachedContents/coordinator")
        .runtime_toolset(Arc::new(BasicToolset::new("client", vec![client_files as Arc<dyn Tool>])))
        .build();
    let harness = harness(root, config).await;
    harness.run(Some("summarise the report"), None).await;

    let helper_request = helper_model.requests().pop().expect("the helper ran");
    assert!(helper_request.tools.is_empty(), "{:?}", helper_request.tools.keys());
    assert_eq!(helper_request.config.and_then(|config| config.cached_content), None);
    let specialist_request = specialist_model.requests().pop().expect("the specialist ran");
    assert!(specialist_request.tools.contains_key("transfer_to_agent"));
    assert!(specialist_request.tools.contains_key("client_files"));
}

// --- Timeouts ---

/// Never answers.
struct StalledModel;

#[async_trait]
impl Llm for StalledModel {
    fn name(&self) -> &str {
        "stalled"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        Ok(Box::pin(futures::stream::pending()))
    }
}

#[tokio::test]
async fn an_agent_tool_timeout_is_a_failed_call() {
    for propagate in [false, true] {
        let helper = LlmAgentBuilder::new("helper")
            .description("never answers")
            .model(Arc::new(StalledModel))
            .build()
            .unwrap();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let seen = errors.clone();
        let root = LlmAgentBuilder::new("ops")
            .model(ScriptedModel::new(vec![
                call("helper", json!({ "request": "wait" }), "call-1"),
                text_response("gave up"),
            ]))
            .tool(Arc::new(
                AgentTool::new(Arc::new(helper))
                    .timeout(Duration::from_millis(20))
                    .propagate_failures(propagate),
            ))
            .on_tool_error(Box::new(
                move |ctx: Arc<dyn CallbackContext>, _tool, _args, message: String| {
                    let seen = seen.clone();
                    Box::pin(async move {
                        let category = ctx
                            .tool_outcome()
                            .and_then(|outcome| outcome.error.map(|error| error.category));
                        seen.lock().unwrap().push((message, category));
                        Ok(None)
                    })
                },
            ))
            .build()
            .unwrap();
        let harness = harness(root, RunConfig::default()).await;
        harness.run(Some("go"), None).await;

        let errors = errors.lock().unwrap().clone();
        assert_eq!(errors.len(), 1, "propagate = {propagate}");
        assert!(errors[0].0.contains("20ms"), "{}", errors[0].0);
        assert_eq!(errors[0].1, Some(ErrorCategory::Timeout));
    }
}
