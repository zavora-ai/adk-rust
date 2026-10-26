//! A durable run that carries a call a person asked for (spec 3.3 §7.4 rule 5, wave 4
//! decision 13): an authenticated run's start holds it for the person as the agent and
//! parks, the decision wakes it and the call runs once; any other run ignores it.

#![cfg(feature = "background")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::principal::SubjectRef;
use adk_core::run_store::RunCheckpointRef;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApproverSelector, Content,
    DeclarativePolicy, DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, InboxQuery, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, NewRun, RequestedCall, RequestedVia, Result,
    RunConfig, RunRecord, RunState, StoreApprovalService, Tool, ToolContext,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkerHandle};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "home";
const AGENT: &str = "finance";
const USER: &str = "ana";

/// Answers with text and counts its calls.
#[derive(Default)]
struct CountingModel(AtomicUsize);

#[async_trait]
impl Llm for CountingModel {
    fn name(&self) -> &str {
        "counting"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("The invoice is on its way.")),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

struct SendInvoice(Arc<AtomicUsize>);

#[async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "sends an invoice"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "sent" }))
    }
}

struct Fixture {
    runs: Arc<dyn DurableRunStore>,
    service: Arc<StoreApprovalService>,
    model: Arc<CountingModel>,
    executions: Arc<AtomicUsize>,
    worker: WorkerHandle,
}

/// `finance` under a policy that allows `send_invoice`, with an approval service the person
/// decides in.
fn fixture() -> Fixture {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let service = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            // A personal home: the person decides their agents' calls.
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("home")]).allow_self_approval(),
            ),
    );
    let model = Arc::new(CountingModel::default());
    let executions = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new(AGENT)
        .model(model.clone() as Arc<dyn Llm>)
        .tool(Arc::new(SendInvoice(Arc::clone(&executions))))
        .build()
        .unwrap();
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
            .tool_policy(Arc::new(DeclarativePolicy::builder().allow("send_invoice").build()))
            .run_config(RunConfig::builder().approval_service(service.clone()).build())
            .build()
            .unwrap(),
    );
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent(AGENT, runner)
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id("w")
                .with_lease_ttl(Duration::from_millis(300))
                .with_poll_interval(Duration::from_millis(20)),
        )
        .spawn();
    Fixture { runs, service, model, executions, worker }
}

fn requested() -> RequestedCall {
    RequestedCall::new(
        "send_invoice",
        json!({ "to": "ana@example.com", "amount": 4200, "invoice": "INV-10" }),
        SubjectRef::human(USER),
        RequestedVia::form("inv-10", 1, "send"),
    )
}

async fn enqueue(fixture: &Fixture, run: NewRun) -> String {
    fixture.runs.enqueue(run).await.unwrap().record().unwrap().run_id.clone()
}

async fn wait_for_run(
    runs: &Arc<dyn DurableRunStore>,
    run_id: &str,
    done: impl Fn(&RunRecord) -> bool,
) -> RunRecord {
    let started = tokio::time::Instant::now();
    loop {
        let run = runs.get(run_id).await.unwrap().unwrap();
        if done(&run) {
            return run;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "run never got there: {run:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// An authenticated run starts by holding the call with no model call and parks; Allow once
/// wakes it, and the call runs exactly once although the woken run starts from the same
/// input.
#[tokio::test]
async fn an_authenticated_run_holds_the_call_and_the_decision_runs_it_once() {
    let fixture = fixture();
    let call = requested();
    let run = NewRun::new(
        "default",
        APP,
        USER,
        AGENT,
        json!({ "text": "", "requestedCall": call.clone() }),
    )
    .with_session_id("thread-1")
    .with_authenticated_user();
    let run_id = enqueue(&fixture, run).await;

    let parked = wait_for_run(&fixture.runs, &run_id, |run| run.state == RunState::Waiting).await;
    assert!(matches!(parked.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. })));
    let inbox = fixture.service.inbox(InboxQuery::for_group("home")).await.unwrap();
    let [request] = inbox.items.as_slice() else {
        panic!("expected one waiting decision: {inbox:?}");
    };
    assert_eq!(
        (
            request.tool_name.as_str(),
            request.args_redacted.clone(),
            request.run_id.as_deref(),
            request.acting.as_ref().map(|acting| acting.agent.id.as_str()),
        ),
        (
            "send_invoice",
            call.args.clone(),
            Some(run_id.as_str()),
            Some("urn:adk:agent:home/finance")
        ),
        "held as finance, for this run"
    );
    assert_eq!(fixture.model.0.load(Ordering::SeqCst), 0, "no model call");
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0, "nothing ran");

    let decision = ApprovalDecision::approve(SubjectRef::human(USER)).with_groups(["home"]);
    fixture.service.decide(&request.id, request.version, decision).await.unwrap();
    let finished = wait_for_run(&fixture.runs, &run_id, |run| run.state.is_terminal()).await;
    fixture.worker.shutdown().await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 1, "Allow once ran it once");
    let inbox = fixture.service.inbox(InboxQuery::for_group("home")).await.unwrap();
    assert!(inbox.items.iter().all(|item| !item.is_open()), "nothing else waits: {inbox:?}");
}

/// A run no authenticated caller enqueued ignores the call: its text is answered and
/// nothing is held or run.
#[tokio::test]
async fn an_unauthenticated_run_ignores_the_requested_call() {
    let fixture = fixture();
    let run = NewRun::new(
        "default",
        APP,
        USER,
        AGENT,
        json!({ "text": "Send the invoice", "requestedCall": requested() }),
    )
    .with_session_id("thread-2");
    let run_id = enqueue(&fixture, run).await;

    let finished = wait_for_run(&fixture.runs, &run_id, |run| run.state.is_terminal()).await;
    fixture.worker.shutdown().await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(fixture.model.0.load(Ordering::SeqCst), 1, "the text was answered");
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
    let inbox = fixture.service.inbox(InboxQuery::for_group("home")).await.unwrap();
    assert!(inbox.items.is_empty(), "nothing waits: {inbox:?}");
}

/// An authenticated run whose `requestedCall` is not a requested call fails without retry.
#[tokio::test]
async fn a_malformed_requested_call_fails_the_run() {
    let fixture = fixture();
    let run = NewRun::new(
        "default",
        APP,
        USER,
        AGENT,
        json!({ "text": "", "requestedCall": { "toolName": "send_invoice" } }),
    )
    .with_session_id("thread-3")
    .with_authenticated_user()
    .with_max_attempts(1);
    let run_id = enqueue(&fixture, run).await;

    let finished = wait_for_run(&fixture.runs, &run_id, |run| run.state.is_terminal()).await;
    fixture.worker.shutdown().await;

    assert_eq!(finished.state, RunState::Failed, "{finished:?}");
    assert!(
        finished.error.as_deref().is_some_and(|error| error.contains("is not a requested call")),
        "{finished:?}"
    );
    assert_eq!(fixture.model.0.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
}
