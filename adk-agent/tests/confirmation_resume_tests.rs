//! A call held for confirmation resumes deterministically once a decision exists.
//!
//! Run 1 holds the call and ends. Run 2 carries a decision and an empty user turn; the
//! agent dispatches the held call again — same tool, same arguments, same call ID —
//! before its first model call, so the decision applies whether or not the model would
//! issue the call again. Every test drives a real `LlmAgent` through a `Runner` with a
//! scripted model and asserts on what the tool received and what the model was sent.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, ApprovalScope, ApprovalStore, Content, Event,
    InMemoryActionLedger, InMemoryApprovalStore, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    Part, Result, RunBudget, RunConfig, SessionId, Tool, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest, ToolContext, ToolEffect, UserId, is_confirmation_pending,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Every function response in the latest request, as `(call id, response)`.
    fn function_responses(&self) -> Vec<(Option<String>, Value)> {
        let requests = self.requests.lock().unwrap();
        let Some(last) = requests.last() else { return Vec::new() };
        last.contents
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

/// Records the arguments of every execution.
struct RecordingTool {
    effect: ToolEffect,
    executions: Arc<Mutex<Vec<Value>>>,
}

impl RecordingTool {
    fn new(effect: ToolEffect) -> (Arc<Self>, Arc<Mutex<Vec<Value>>>) {
        let executions = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Self { effect, executions: executions.clone() }), executions)
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "moves money"
    }

    fn effect(&self) -> ToolEffect {
        self.effect
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "status": "sent" }))
    }
}

fn agent(model: Arc<ScriptedModel>, tool: Arc<RecordingTool>) -> LlmAgent {
    LlmAgentBuilder::new("ops")
        .model(model)
        .tool(tool)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap()
}

struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

async fn harness(agent: LlmAgent, config: Option<RunConfig>) -> Harness {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "governed".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name("governed")
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>);
    if let Some(config) = config {
        builder = builder.run_config(config);
    }
    Harness { runner: builder.build().unwrap(), sessions }
}

impl Harness {
    async fn run(&self, text: &str, config: Option<RunConfig>) -> Vec<Result<Event>> {
        let stream = self
            .runner
            .run_with_config(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                Content::new("user").with_text(text),
                config,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish")
    }

    async fn events(&self) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: "governed".to_string(),
                user_id: "user".to_string(),
                session_id: "session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all()
    }
}

fn confirmation_requests(events: &[Result<Event>]) -> Vec<ToolConfirmationRequest> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.actions.tool_confirmation.clone())
        .collect()
}

/// The function responses carried by `events`, as `(call id, response)`.
fn responses_in(events: &[Event]) -> Vec<(Option<String>, Value)> {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

fn ok_events(events: Vec<Result<Event>>) -> Vec<Event> {
    events.into_iter().map(|event| event.expect("no run error")).collect()
}

fn args() -> Value {
    json!({ "amount": 40 })
}
const DENIED: &str = "Tool 'transfer' execution denied by confirmation policy";

/// Run 1 holds `transfer` as `call-1`; later model turns answer with text only.
async fn held(
    config: Option<RunConfig>,
    effect: ToolEffect,
) -> (Harness, Arc<ScriptedModel>, Arc<Mutex<Vec<Value>>>, ToolConfirmationRequest) {
    let model = ScriptedModel::new(vec![call("transfer", args(), "call-1")]);
    let (tool, executions) = RecordingTool::new(effect);
    let harness = harness(agent(model.clone(), tool), config).await;
    let first = harness.run("send 40", None).await;
    let held = confirmation_requests(&first);
    assert_eq!(held.len(), 1, "{first:?}");
    assert!(executions.lock().unwrap().is_empty());
    (harness, model, executions, held[0].clone())
}

// --- Approve ---

#[tokio::test]
async fn an_approved_held_call_executes_once_on_resume_without_the_model_reissuing_it() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let config = RunConfig::builder().approval_store(store.clone()).build();
    let (harness, model, executions, request) = held(Some(config), ToolEffect::NonIdempotent).await;
    let scope = ApprovalScope::new("governed", "user", "session");
    store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await.unwrap();

    let second = ok_events(harness.run("", None).await);

    assert_eq!(*executions.lock().unwrap(), vec![args()]);
    assert_eq!(model.request_count(), 2, "the resumed run calls the model once");
    // The empty resume turn is not sent: some providers reject empty text blocks.
    let last_request = model.requests.lock().unwrap().last().cloned().unwrap();
    // The confirmation notice is not model output, so the request ends on the result.
    let roles: Vec<&str> =
        last_request.contents.iter().map(|content| content.role.as_str()).collect();
    assert_eq!(roles, ["user", "model", "function"]);
    assert!(
        last_request.contents.iter().all(|content| content
            .parts
            .iter()
            .any(|part| { !matches!(part, Part::Text { text } if text.trim().is_empty()) })),
        "{:?}",
        last_request.contents
    );
    // The model sees exactly one response for the call: the real result.
    assert_eq!(
        model.function_responses(),
        vec![(Some("call-1".to_string()), json!({ "status": "sent" }))]
    );
    // The resumed result streams to the caller and persists in the session.
    assert!(
        responses_in(&second).contains(&(Some("call-1".to_string()), json!({ "status": "sent" })))
    );
    let persisted = responses_in(&harness.events().await);
    assert_eq!(persisted.len(), 2, "the pending response and the real result: {persisted:?}");
    assert!(is_confirmation_pending(&persisted[0].1));
    assert_eq!(persisted[1], (Some("call-1".to_string()), json!({ "status": "sent" })));
    // The approval was consumed by the execution.
    assert_eq!(store.take_decision(&scope, &request.fingerprint()).await.unwrap(), None);

    // A later run projects the history with the real result in place of the pending one.
    harness.run("thanks", None).await;
    assert_eq!(*executions.lock().unwrap(), vec![args()], "a resumed call never runs twice");
    assert_eq!(
        model.function_responses(),
        vec![(Some("call-1".to_string()), json!({ "status": "sent" }))]
    );
}

#[tokio::test]
async fn a_fingerprint_approval_resumes_the_held_call_under_its_original_id() {
    let (harness, model, executions, request) = held(None, ToolEffect::NonIdempotent).await;

    let config =
        RunConfig::builder().tool_approval(request.fingerprint(), ToolApproval::approve()).build();
    let second = harness.run("", Some(config)).await;

    assert!(confirmation_requests(&second).is_empty(), "{second:?}");
    assert_eq!(*executions.lock().unwrap(), vec![args()]);
    assert_eq!(
        model.function_responses(),
        vec![(Some("call-1".to_string()), json!({ "status": "sent" }))]
    );
}

#[tokio::test]
async fn a_call_id_decision_resumes_the_held_call() {
    let (harness, model, executions, request) = held(None, ToolEffect::NonIdempotent).await;

    let call_id = request.function_call_id.clone().unwrap();
    let config = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(call_id, ToolConfirmationDecision::Approve)]))
        .build();
    harness.run("", Some(config)).await;

    assert_eq!(*executions.lock().unwrap(), vec![args()]);
    assert_eq!(
        model.function_responses(),
        vec![(Some("call-1".to_string()), json!({ "status": "sent" }))]
    );
}

#[tokio::test]
async fn a_model_that_reissues_the_call_after_resume_does_not_execute_it_again() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let config = RunConfig::builder().approval_store(store.clone()).build();
    let model = ScriptedModel::new(vec![
        call("transfer", args(), "call-1"),
        // On the resumed run the model repeats the call it already saw succeed.
        call("transfer", args(), "call-2"),
    ]);
    let (tool, executions) = RecordingTool::new(ToolEffect::NonIdempotent);
    let harness = harness(agent(model.clone(), tool), Some(config)).await;
    let request = confirmation_requests(&harness.run("send 40", None).await).remove(0);
    let scope = ApprovalScope::new("governed", "user", "session");
    store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await.unwrap();

    let second = harness.run("", None).await;

    assert_eq!(*executions.lock().unwrap(), vec![args()], "one approval, one execution");
    let held_again = confirmation_requests(&second);
    assert_eq!(held_again.len(), 1);
    assert_eq!(held_again[0].function_call_id.as_deref(), Some("call-2"));
}

/// Counts the ledger operations of the calls it records.
#[derive(Debug, Default)]
struct CountingLedger {
    inner: InMemoryActionLedger,
    begun: AtomicUsize,
    completed: AtomicUsize,
}

#[async_trait]
impl ActionLedger for CountingLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        self.begun.fetch_add(1, Ordering::SeqCst);
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        self.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

#[tokio::test]
async fn a_resumed_non_idempotent_call_goes_through_the_action_ledger_once() {
    let ledger = Arc::new(CountingLedger::default());
    let config = RunConfig::builder().action_ledger(ledger.clone()).build();
    let (harness, _model, executions, request) =
        held(Some(config), ToolEffect::NonIdempotent).await;
    assert_eq!(ledger.begun.load(Ordering::SeqCst), 0, "a held call never begins");

    // A run config passed to one run replaces the runner's, so it carries the ledger too.
    let config = RunConfig::builder()
        .action_ledger(ledger.clone())
        .tool_approval(request.fingerprint(), ToolApproval::approve())
        .build();
    harness.run("", Some(config)).await;

    assert_eq!(*executions.lock().unwrap(), vec![args()]);
    assert_eq!(ledger.begun.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.completed.load(Ordering::SeqCst), 1);
}

// --- Deny ---

#[tokio::test]
async fn a_denied_held_call_is_answered_with_the_refusal_and_does_not_execute() {
    let (harness, model, executions, request) = held(None, ToolEffect::NonIdempotent).await;

    let call_id = request.function_call_id.clone().unwrap();
    let config = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(call_id, ToolConfirmationDecision::Deny)]))
        .build();
    let second = harness.run("", Some(config)).await;

    assert!(executions.lock().unwrap().is_empty());
    assert!(confirmation_requests(&second).is_empty());
    assert_eq!(
        model.function_responses(),
        vec![(Some("call-1".to_string()), json!({ "error": DENIED }))]
    );

    // The refusal answers the call, so a later approval does not resume it.
    let config =
        RunConfig::builder().tool_approval(request.fingerprint(), ToolApproval::approve()).build();
    harness.run("", Some(config)).await;
    assert!(executions.lock().unwrap().is_empty());
}

// --- No decision ---

#[tokio::test]
async fn an_undecided_held_call_stays_held_until_a_decision_arrives() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let config = RunConfig::builder().approval_store(store.clone()).build();
    let (harness, model, executions, request) = held(Some(config), ToolEffect::NonIdempotent).await;

    // No decision: the run proceeds to the model, which sees the pending response.
    harness.run("any news?", None).await;
    assert!(executions.lock().unwrap().is_empty());
    let responses = model.function_responses();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].0.as_deref(), Some("call-1"));
    assert!(is_confirmation_pending(&responses[0].1));

    // The hold survives that run: a decision recorded now resumes the call.
    let scope = ApprovalScope::new("governed", "user", "session");
    store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await.unwrap();
    harness.run("", None).await;
    assert_eq!(*executions.lock().unwrap(), vec![args()]);
    assert_eq!(
        model.function_responses(),
        vec![(Some("call-1".to_string()), json!({ "status": "sent" }))]
    );
}

#[tokio::test]
async fn a_resume_refused_by_the_run_budget_leaves_the_call_held() {
    let (harness, model, executions, request) = held(None, ToolEffect::NonIdempotent).await;

    let approval =
        || RunConfig::builder().tool_approval(request.fingerprint(), ToolApproval::approve());
    let refused =
        harness.run("", Some(approval().budget(RunBudget::new().max_tool_calls(0)).build())).await;
    assert!(refused.last().unwrap().is_err(), "the run ends with the budget error");
    assert!(executions.lock().unwrap().is_empty());
    assert_eq!(model.request_count(), 1, "the model is not called");

    harness.run("", Some(approval().build())).await;
    assert_eq!(*executions.lock().unwrap(), vec![args()]);
}
