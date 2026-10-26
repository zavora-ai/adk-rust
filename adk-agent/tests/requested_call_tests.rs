//! A call a person asked for through something the agent drew (spec 3.3 §7.4 rule 5, wave 4
//! decisions 12 and 13): `LlmAgent` holds it as itself with no model call, or refuses it, and
//! a later run resumes it once on the person's decision.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    ActingContext, ApprovalScope, ApprovalStore, Content, DeclarativePolicy, Event,
    InMemoryApprovalStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part,
    REQUESTED_CALL_METADATA_KEY, RequestContext, RequestedCall, RequestedVia, Result, RunConfig,
    SessionId, SubjectKind, SubjectRef, Tool, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationHandler, ToolConfirmationRequest, ToolContext, ToolOrigin, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "home";
const USER: &str = "ana";
const SESSION: &str = "thread-1";

/// Answers every request with text and counts the requests.
#[derive(Default)]
struct CountingModel {
    requests: Mutex<Vec<LlmRequest>>,
}

impl CountingModel {
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The function responses of the latest request, by call ID.
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
impl Llm for CountingModel {
    fn name(&self) -> &str {
        "counting"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("The invoice is on its way.")),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// `send_invoice`, which records every execution.
struct SendInvoice {
    name: &'static str,
    executions: Arc<Mutex<Vec<Value>>>,
}

impl SendInvoice {
    fn named(name: &'static str) -> (Arc<Self>, Arc<Mutex<Vec<Value>>>) {
        let executions = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Self { name, executions: executions.clone() }), executions)
    }
}

#[async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "sends an invoice"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "to": { "type": "string" },
                "amount": { "type": "integer" },
                "invoice": { "type": "string" }
            },
            "required": ["to", "amount", "invoice"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "status": "sent" }))
    }
}

/// Approves every call and counts how often it was asked.
#[derive(Debug, Default)]
struct CountingApproves(AtomicUsize);

#[async_trait]
impl ToolConfirmationHandler for CountingApproves {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolConfirmationDecision::Approve)
    }
}

fn invoice_args() -> Value {
    json!({ "to": "ana@example.com", "amount": 4200, "invoice": "INV-10" })
}

fn requested(tool: &str, args: Value) -> RequestedCall {
    RequestedCall::new(tool, args, SubjectRef::human(USER), RequestedVia::form("inv-10", 1, "send"))
}

/// The scope decisions are stored under.
fn scope() -> ApprovalScope {
    ApprovalScope::new(APP, USER, SESSION)
}

/// The acting context of `finance` in the test session.
fn finance() -> ActingContext {
    ActingContext::derived(APP, "finance", USER, SubjectKind::Human)
}

struct Fixture {
    runner: Runner,
    model: Arc<CountingModel>,
    tool: Arc<dyn Tool>,
    executions: Arc<Mutex<Vec<Value>>>,
    store: Arc<InMemoryApprovalStore>,
}

/// `finance`, whose policy allows `send_invoice` (Go ahead for it), with `extra` tools.
async fn fixture(extra: Vec<Arc<dyn Tool>>) -> Fixture {
    let model = Arc::new(CountingModel::default());
    let (tool, executions) = SendInvoice::named("send_invoice");
    let mut builder = LlmAgentBuilder::new("finance").model(model.clone()).tool(tool.clone());
    for extra in extra {
        builder = builder.tool(extra);
    }
    let agent: LlmAgent = builder.build().unwrap();
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
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .tool_policy(Arc::new(DeclarativePolicy::builder().allow("send_invoice").build()))
        .approval_store(store.clone())
        .build()
        .unwrap();
    Fixture { runner, model, tool, executions, store }
}

/// A run config for the signed-in person, carrying `call`.
fn signed_in(call: Option<RequestedCall>) -> RunConfig {
    RunConfig {
        request_context: Some(RequestContext {
            user_id: USER.to_string(),
            scopes: Vec::new(),
            metadata: HashMap::new(),
        }),
        requested_call: call,
        ..RunConfig::default()
    }
}

async fn run(runner: &Runner, text: &str, config: RunConfig) -> Vec<Event> {
    let stream = runner
        .run_with_config(
            UserId::new(USER).unwrap(),
            SessionId::new(SESSION).unwrap(),
            Content::new("user").with_text(text),
            Some(config),
        )
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    events.into_iter().map(|event| event.expect("the run must not fail")).collect()
}

/// The agent's events of `call`: the call, then its answer.
fn written(call: &RequestedCall, answer: Value) -> Vec<(String, Vec<Part>, Option<String>)> {
    vec![
        (
            "model".to_string(),
            vec![Part::FunctionCall {
                name: call.tool_name.clone(),
                args: call.args.clone(),
                id: Some(call.function_call_id.clone()),
                thought_signature: None,
            }],
            Some(call.metadata()),
        ),
        (
            "function".to_string(),
            vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    call.tool_name.clone(),
                    answer,
                ),
                id: Some(call.function_call_id.clone()),
                annotations: None,
            }],
            Some(call.metadata()),
        ),
    ]
}

/// What the agent wrote: each event's role, parts and requested-call metadata.
fn agent_writes(events: &[Event]) -> Vec<(String, Vec<Part>, Option<String>)> {
    events
        .iter()
        .filter(|event| event.author == "finance" && event.actions.tool_confirmation.is_none())
        .filter_map(|event| {
            let content = event.llm_response.content.clone()?;
            Some((
                content.role,
                content.parts,
                event.provider_metadata.get(REQUESTED_CALL_METADATA_KEY).cloned(),
            ))
        })
        .collect()
}

fn holds(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
}

/// Wave 4 decision 12: the press is never the approval. With the tool allowed by policy, a
/// fingerprint approval and a stored approval of the identical call, and a handler that
/// approves everything, the call is still held, as the agent, and no model call is made.
#[tokio::test]
async fn a_requested_call_is_held_as_the_agent_with_no_model_call() {
    let fixture = fixture(Vec::new()).await;
    let call = requested("send_invoice", invoice_args());
    let expected = ToolConfirmationRequest::new("send_invoice", invoice_args())
        .with_function_call_id(call.function_call_id.clone())
        .with_acting_context(finance())
        .with_origin(ToolOrigin::of(fixture.tool.as_ref()));
    fixture.store.decide(&scope(), &expected.fingerprint(), ToolApproval::approve()).await.unwrap();
    let handler = Arc::new(CountingApproves::default());
    let config = RunConfig {
        tool_confirmation_handler: Some(handler.clone()),
        tool_approvals: HashMap::from([(expected.fingerprint(), ToolApproval::approve())]),
        tool_confirmation_decisions: HashMap::from([(
            call.function_call_id.clone(),
            ToolConfirmationDecision::Approve,
        )]),
        ..signed_in(Some(call.clone()))
    };

    let events = run(&fixture.runner, "", config).await;

    assert_eq!(fixture.model.calls(), 0, "no model call");
    assert!(fixture.executions.lock().unwrap().is_empty(), "nothing ran");
    assert_eq!(handler.0.load(Ordering::SeqCst), 0, "the handler was never asked");
    assert_eq!(holds(&events), vec![expected.clone()], "held as finance");
    let reason = "Tool 'send_invoice' requires confirmation: a person asked for it";
    assert_eq!(
        agent_writes(&events),
        written(&call, adk_core::confirmation_pending_response(reason))
    );
    let marked =
        events.iter().filter(|event| event.actions.tool_confirmation.is_some()).all(|event| {
            event.provider_metadata.get(REQUESTED_CALL_METADATA_KEY) == Some(&call.metadata())
        });
    assert!(marked, "the hold says a person asked for it");
    let pending = fixture.store.pending(&scope()).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request, expected);
}

/// Allow once runs the held call exactly once, with the arguments it was held with, in the
/// run that resumes it; the same input woken again does not hold it a second time.
#[tokio::test]
async fn the_held_call_runs_exactly_once_after_allow_once() {
    let fixture = fixture(Vec::new()).await;
    let call = requested("send_invoice", invoice_args());
    let events = run(&fixture.runner, "", signed_in(Some(call.clone()))).await;
    let held = holds(&events).pop().expect("the call is held");

    fixture.store.decide(&scope(), &held.fingerprint(), ToolApproval::approve()).await.unwrap();
    // The woken run starts from the same input.
    let events = run(&fixture.runner, "", signed_in(Some(call.clone()))).await;

    assert_eq!(*fixture.executions.lock().unwrap(), vec![invoice_args()]);
    assert!(holds(&events).is_empty(), "the call is not held again");
    assert_eq!(fixture.model.calls(), 1, "the model sees the result once it ran");
    // The requested call's ID never reaches the provider.
    assert_eq!(fixture.model.function_responses(), vec![(None, json!({ "status": "sent" }))]);

    run(&fixture.runner, "", signed_in(Some(call))).await;
    assert_eq!(fixture.executions.lock().unwrap().len(), 1, "it ran exactly once");
}

/// A denied requested call never runs, even though the policy allows the tool.
#[tokio::test]
async fn a_denied_requested_call_never_runs() {
    let fixture = fixture(Vec::new()).await;
    let call = requested("send_invoice", invoice_args());
    let events = run(&fixture.runner, "", signed_in(Some(call.clone()))).await;
    let held = holds(&events).pop().expect("the call is held");

    fixture.store.decide(&scope(), &held.fingerprint(), ToolApproval::deny()).await.unwrap();
    run(&fixture.runner, "", signed_in(None)).await;

    assert!(fixture.executions.lock().unwrap().is_empty(), "a denied call never runs");
    assert_eq!(
        fixture.model.function_responses(),
        vec![(
            None,
            json!({ "error": "Tool 'send_invoice' execution denied by confirmation policy" })
        )]
    );
}

/// A tool the policy denies is refused: a response with the reason, no hold, no inbox entry
/// and no model call.
#[tokio::test]
async fn a_policy_denial_refuses_with_no_model_call() {
    let fixture = fixture(Vec::new()).await;
    let call = requested("send_invoice", invoice_args());
    let config = RunConfig {
        tool_policy: Some(Arc::new(
            DeclarativePolicy::builder().deny("send_invoice", "not from this home").build(),
        )),
        ..signed_in(Some(call.clone()))
    };

    let events = run(&fixture.runner, "", config).await;

    assert_eq!(
        agent_writes(&events),
        written(
            &call,
            json!({ "error": "Tool 'send_invoice' denied by policy: not from this home" })
        )
    );
    assert!(holds(&events).is_empty());
    assert!(fixture.store.pending(&scope()).await.unwrap().is_empty());
    assert_eq!(fixture.model.calls(), 0);
    assert!(fixture.executions.lock().unwrap().is_empty());
}

/// Unknown tools, the kernel's tools, the tools that draw and arguments that do not fit the
/// tool are refused before the governed path, with no model call.
#[tokio::test]
async fn unknown_kernel_and_drawing_tools_and_bad_arguments_are_refused() {
    let (kernel, kernel_runs) = SendInvoice::named("kernel_spawn");
    let (draw, draw_runs) = SendInvoice::named("render_form");
    let cases = [
        (
            requested("transfer_funds", json!({})),
            "Tool 'transfer_funds' was not run: this agent has no tool named 'transfer_funds'",
        ),
        (
            requested("kernel_spawn", invoice_args()),
            "Tool 'kernel_spawn' was not run: it is one of the kernel's own tools",
        ),
        (
            requested("render_form", invoice_args()),
            "Tool 'render_form' was not run: it draws on the person's screens",
        ),
    ];
    for (call, error) in cases {
        let fixture = fixture(vec![kernel.clone(), draw.clone()]).await;
        let events = run(&fixture.runner, "", signed_in(Some(call.clone()))).await;
        assert_eq!(agent_writes(&events), written(&call, json!({ "error": error })), "{error}");
        assert!(holds(&events).is_empty(), "{error}");
        assert_eq!(fixture.model.calls(), 0, "{error}");
    }

    let fixture = fixture(Vec::new()).await;
    let call = requested("send_invoice", json!({ "to": "ana@example.com", "amount": "lots" }));
    let events = run(&fixture.runner, "", signed_in(Some(call.clone()))).await;
    let writes = agent_writes(&events);
    let Some((_, parts, _)) = writes.get(1) else { panic!("{writes:?}") };
    let Part::FunctionResponse { function_response, .. } = &parts[0] else { panic!("{parts:?}") };
    let error = function_response.response["error"].as_str().unwrap_or_default();
    assert!(
        error.starts_with("Tool 'send_invoice' was not run: its arguments do not fit the tool"),
        "{error}"
    );
    assert!(holds(&events).is_empty());
    assert_eq!(fixture.model.calls(), 0);
    assert!(fixture.executions.lock().unwrap().is_empty());
    assert!(kernel_runs.lock().unwrap().is_empty() && draw_runs.lock().unwrap().is_empty());
}

/// Only an authenticated run of the person who asked can carry a requested call.
#[tokio::test]
async fn an_unauthenticated_run_or_another_persons_request_is_refused() {
    let anonymous_fixture = fixture(Vec::new()).await;
    let call = requested("send_invoice", invoice_args());
    let anonymous = RunConfig { request_context: None, ..signed_in(Some(call.clone())) };
    let events = run(&anonymous_fixture.runner, "", anonymous).await;
    assert_eq!(
        agent_writes(&events),
        written(
            &call,
            json!({
                "error": "Tool 'send_invoice' was not run: a call a person asks for needs a \
                          signed-in person"
            })
        )
    );

    let fixture = fixture(Vec::new()).await;
    let mut call = requested("send_invoice", invoice_args());
    call.requested_by = SubjectRef::human("mallory");
    let events = run(&fixture.runner, "", signed_in(Some(call.clone()))).await;
    assert_eq!(
        agent_writes(&events),
        written(
            &call,
            json!({
                "error": "Tool 'send_invoice' was not run: it was asked for by someone other \
                          than this session's person"
            })
        )
    );
    assert!(fixture.executions.lock().unwrap().is_empty());
    assert_eq!(fixture.model.calls(), 0);
}

/// Wave 4 decision 13: a `CodeActAgent` cannot hold a call, so it refuses requested calls
/// before any model call.
#[cfg(feature = "codeact")]
#[tokio::test]
async fn codeact_refuses_requested_calls() {
    use adk_agent::codeact::{CodeActAgent, CodeRuntime, ResumeWith, RunStep, RuntimeError};

    struct NeverRuns;
    impl CodeRuntime for NeverRuns {
        fn start(&self, _script: &str, _name: &str) -> std::result::Result<RunStep, RuntimeError> {
            panic!("no script runs for a requested call")
        }
        fn resume(
            &self,
            _snapshot: &[u8],
            _with: ResumeWith,
        ) -> std::result::Result<RunStep, RuntimeError> {
            panic!("no script runs for a requested call")
        }
    }

    let model = Arc::new(CountingModel::default());
    let (tool, executions) = SendInvoice::named("send_invoice");
    let agent = CodeActAgent::builder()
        .name("finance")
        .model(model.clone())
        .runtime(Arc::new(NeverRuns))
        .tool(tool)
        .build()
        .unwrap();
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
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let call = requested("send_invoice", invoice_args());

    let events = run(&runner, "", signed_in(Some(call.clone()))).await;

    assert_eq!(
        agent_writes(&events),
        written(
            &call,
            json!({
                "error": "Tool 'send_invoice' was not run: this agent runs code and cannot hold \
                          a call for a person's decision"
            })
        )
    );
    assert_eq!(model.calls(), 0);
    assert!(executions.lock().unwrap().is_empty());
}
