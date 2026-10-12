//! Durable runtime gates: workers, crash resume, the scheduler, the replicated kill
//! switch, and cancellation across replicas.
//!
//! Replicas are separate Tokio tasks sharing in-memory stores; each owns its worker,
//! scheduler, and governance control, as a separate process would.

#![cfg(feature = "background")]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::governance::GovernanceMirrorConfig;
use adk_core::governance_state::{
    GovernanceScope, GovernanceState, GovernanceStateStore, InMemoryGovernanceStateStore,
};
use adk_core::principal::SubjectRef;
use adk_core::run_store::{RUN_STALE_EPOCH_CODE, RunCheckpointRef};
use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec};
use adk_core::{
    AdkError, ClaimedRun, Clock, Content, DurableRunStore, Enqueued, ErrorCategory, ErrorComponent,
    GovernanceControl, InMemoryActionLedger, InMemoryRunStore, InMemoryScheduleStore, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, NewRun, OnConflict, Part, Result, RunFilter,
    RunOutcome, RunRecord, RunState, ScheduleStore, Tool, ToolContext, ToolEffect,
};
use adk_runner::Runner;
use adk_server::background::{
    BackgroundState, DurableScheduler, DurableWorker, DurableWorkerConfig, WorkflowRegistry,
    background_runs_router_with_state,
};
use adk_session::{GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;

const TTL: Duration = Duration::from_millis(300);

fn worker_config(id: &str) -> DurableWorkerConfig {
    DurableWorkerConfig::default()
        .with_worker_id(id)
        .with_lease_ttl(TTL)
        .with_poll_interval(Duration::from_millis(20))
}

async fn wait_for<T>(
    bound: Duration,
    mut probe: impl FnMut() -> futures::future::BoxFuture<'static, Option<T>>,
) -> Option<T> {
    let started = tokio::time::Instant::now();
    while started.elapsed() <= bound {
        if let Some(value) = probe().await {
            return Some(value);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

async fn wait_terminal(store: &Arc<dyn DurableRunStore>, run_id: &str) -> RunRecord {
    let store = Arc::clone(store);
    let id = run_id.to_string();
    wait_for(Duration::from_secs(10), move || {
        let store = Arc::clone(&store);
        let run_id = id.clone();
        Box::pin(async move {
            store.get(&run_id).await.ok().flatten().filter(|run| run.state.is_terminal())
        })
    })
    .await
    .unwrap_or_else(|| panic!("run {run_id} never finished"))
}

// --- A scripted model whose answer depends only on the conversation it is sent ---

/// Charges once; once the charge has a response, reports its status and stops.
struct PaymentModel {
    calls: AtomicUsize,
}

#[async_trait]
impl Llm for PaymentModel {
    fn name(&self) -> &str {
        "payment-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let charge_status =
            req.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
                Part::FunctionResponse { function_response, .. }
                    if function_response.name == "charge_card" =>
                {
                    Some(
                        function_response
                            .response
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("ok")
                            .to_string(),
                    )
                }
                _ => None,
            });
        let content = match charge_status {
            Some(status) => Content::new("model").with_text(format!("charge status: {status}")),
            None => Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "charge_card".to_string(),
                    args: json!({ "amountCents": 1999 }),
                    id: Some("call-charge-1".to_string()),
                    thought_signature: None,
                }],
            },
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Charges, then hangs while `hang` is set — a process that dies inside the call.
struct ChargeTool {
    charges: Arc<AtomicUsize>,
    hang: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for ChargeTool {
    fn name(&self) -> &str {
        "charge_card"
    }

    fn description(&self) -> &str {
        "charges the customer's card"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.charges.fetch_add(1, Ordering::SeqCst);
        if self.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(json!({ "status": "charged" }))
    }
}

struct PaymentFixture {
    runs: Arc<dyn DurableRunStore>,
    sessions: Arc<InMemorySessionService>,
    charges: Arc<AtomicUsize>,
    hang: Arc<AtomicBool>,
    model: Arc<PaymentModel>,
    ledger: Arc<InMemoryActionLedger>,
}

impl PaymentFixture {
    fn new(runs: Arc<dyn DurableRunStore>) -> Self {
        Self {
            runs,
            sessions: Arc::new(InMemorySessionService::new()),
            charges: Arc::new(AtomicUsize::new(0)),
            hang: Arc::new(AtomicBool::new(true)),
            model: Arc::new(PaymentModel { calls: AtomicUsize::new(0) }),
            ledger: Arc::new(InMemoryActionLedger::new()),
        }
    }

    /// One replica's runner: its own agent and tool instances over the shared stores.
    fn runner(&self) -> Arc<Runner> {
        let agent = LlmAgentBuilder::new("billing")
            .model(Arc::clone(&self.model) as Arc<dyn Llm>)
            .tool(Arc::new(ChargeTool {
                charges: Arc::clone(&self.charges),
                hang: Arc::clone(&self.hang),
            }))
            .build()
            .unwrap();
        Arc::new(
            Runner::builder()
                .app_name("billing")
                .agent(Arc::new(agent))
                .session_service(Arc::clone(&self.sessions) as Arc<dyn SessionService>)
                .action_ledger(Arc::clone(&self.ledger) as Arc<dyn adk_core::ActionLedger>)
                .durable_run_store(Arc::clone(&self.runs))
                .build()
                .unwrap(),
        )
    }

    fn worker(&self, id: &str) -> DurableWorker {
        DurableWorker::new(Arc::clone(&self.runs))
            .with_agent("billing", self.runner())
            .with_config(worker_config(id))
    }

    async fn function_responses(&self, session_id: &str) -> Vec<Value> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: "billing".into(),
                user_id: "customer-1".into(),
                session_id: session_id.into(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        session
            .events()
            .all()
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.clone())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. } => {
                    Some(function_response.response)
                }
                _ => None,
            })
            .collect()
    }
}

async fn enqueue_payment(runs: &Arc<dyn DurableRunStore>) -> RunRecord {
    let run = NewRun::new("default", "billing", "customer-1", "billing", json!("charge my card"))
        .with_session_id("order-7");
    runs.enqueue(run).await.unwrap().record().cloned().unwrap()
}

/// Gate: a worker killed mid-payment is recovered by another after its lease expires,
/// and the customer is charged exactly once.
#[tokio::test]
async fn a_worker_killed_mid_payment_is_resumed_by_another_and_charges_once() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let fixture = PaymentFixture::new(Arc::clone(&runs));
    let run = enqueue_payment(&runs).await;

    let first = fixture.worker("replica-a").spawn();
    let charges = Arc::clone(&fixture.charges);
    let reached = wait_for(Duration::from_secs(5), move || {
        let charges = Arc::clone(&charges);
        Box::pin(async move { (charges.load(Ordering::SeqCst) == 1).then_some(()) })
    })
    .await;
    assert!(
        reached.is_some(),
        "replica a never reached the charge: {:?}",
        runs.get(&run.run_id).await
    );
    first.abort();
    let stopped = runs.get(&run.run_id).await.unwrap().unwrap();
    assert_eq!(stopped.state, RunState::Running, "the killed worker wrote no outcome");
    assert!(
        matches!(&stopped.checkpoint, Some(RunCheckpointRef::Session { session_id, invocation_id: Some(_) }) if session_id == "order-7"),
        "the session checkpoint was recorded before any progress: {:?}",
        stopped.checkpoint
    );

    // A recovered call must not hang again: the second replica would only reach it by
    // charging a second time.
    fixture.hang.store(false, Ordering::SeqCst);
    let second = fixture.worker("replica-b").spawn();
    let finished = wait_terminal(&runs, &run.run_id).await;
    second.shutdown().await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(finished.attempt, 2, "recovered at the next attempt");
    assert_eq!(fixture.charges.load(Ordering::SeqCst), 1, "exactly one charge");
    assert_eq!(finished.result.as_ref().unwrap()["text"], "charge status: outcome_unknown");
    let responses = fixture.function_responses("order-7").await;
    assert_eq!(responses.len(), 1, "the stopped call has exactly one response: {responses:?}");
    assert!(adk_core::is_outcome_unknown(&responses[0]));
}

/// A call that completed before the crash is answered from its ledger record, and a run
/// whose final answer already reached the session completes without a model call.
#[tokio::test]
async fn recovery_answers_completed_calls_from_the_ledger_and_reuses_a_final_answer() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let fixture = PaymentFixture::new(Arc::clone(&runs));
    fixture.hang.store(false, Ordering::SeqCst);
    let run = enqueue_payment(&runs).await;

    // Replica a runs the attempt to its end but "crashes" before writing the outcome:
    // claim by hand, drive the runner, never finish.
    let claimed = runs.claim("default", "replica-a", TTL).await.unwrap().unwrap();
    let runner = fixture.runner();
    fixture
        .sessions
        .create(adk_session::CreateRequest {
            app_name: "billing".into(),
            user_id: "customer-1".into(),
            session_id: Some("order-7".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let invocation = runner
        .start_with_config(
            "customer-1".try_into().unwrap(),
            "order-7".try_into().unwrap(),
            Content::new("user").with_text("charge my card"),
            None,
        )
        .await
        .unwrap();
    runs.set_checkpoint(
        &run.run_id,
        claimed.epoch,
        RunCheckpointRef::Session {
            session_id: "order-7".into(),
            invocation_id: Some(invocation.invocation_id().to_string()),
        },
    )
    .await
    .unwrap();
    use futures::StreamExt;
    let mut events = invocation.into_events();
    while events.next().await.is_some() {}
    let model_calls = fixture.model.calls.load(Ordering::SeqCst);
    assert_eq!(model_calls, 2);

    tokio::time::sleep(TTL + Duration::from_millis(50)).await;
    let finished = fixture.worker("replica-b").run_next().await.unwrap().unwrap();
    assert_eq!(finished.state, RunState::Completed);
    assert_eq!(finished.result.as_ref().unwrap()["text"], "charge status: charged");
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), model_calls, "no model call on resume");
    assert_eq!(fixture.charges.load(Ordering::SeqCst), 1);
}

/// The runner answers an unanswered call from a completed ledger record.
#[tokio::test]
async fn recover_unanswered_calls_reports_a_completed_charge_as_already_succeeded() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let fixture = PaymentFixture::new(runs);
    fixture
        .sessions
        .create(adk_session::CreateRequest {
            app_name: "billing".into(),
            user_id: "customer-1".into(),
            session_id: Some("s".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut call = adk_core::Event::new("inv-1");
    call.author = "billing".into();
    call.llm_response.content = Some(Content {
        role: "model".into(),
        parts: vec![Part::FunctionCall {
            name: "charge_card".into(),
            args: json!({}),
            id: Some("c1".into()),
            thought_signature: None,
        }],
    });
    fixture.sessions.append_event("s", call).await.unwrap();
    use adk_core::ActionLedger;
    let key = "billing/customer-1/s/inv-1/c1";
    fixture
        .ledger
        .begin(&adk_core::ActionRecord::new(
            key,
            "charge_card",
            &json!({}),
            ToolEffect::NonIdempotent,
        ))
        .await
        .unwrap();
    fixture
        .ledger
        .complete(key, adk_core::ActionOutcome::Succeeded { result_digest: "fnv1a128:01".into() })
        .await
        .unwrap();

    let runner = fixture.runner();
    assert_eq!(runner.recover_unanswered_calls("customer-1", "s").await.unwrap(), 1);
    assert_eq!(
        runner.recover_unanswered_calls("customer-1", "s").await.unwrap(),
        0,
        "answered once"
    );
    let responses = fixture
        .sessions
        .get(GetRequest {
            app_name: "billing".into(),
            user_id: "customer-1".into(),
            session_id: "s".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
        .iter()
        .filter_map(|event| event.llm_response.content.clone())
        .flat_map(|content| content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id, function_response.response))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].0.as_deref(), Some("c1"));
    assert_eq!(responses[0].1["status"], "already_succeeded");
}

// --- Fencing ---

/// A run store whose writes for one worker can be cut off, as a partition would.
struct PartitionedStore {
    inner: InMemoryRunStore,
    cut: Mutex<HashSet<String>>,
    owners: Mutex<HashMap<(String, u64), String>>,
}

impl PartitionedStore {
    fn new() -> Self {
        Self { inner: InMemoryRunStore::new(), cut: Mutex::default(), owners: Mutex::default() }
    }

    fn cut_off(&self, worker: &str) {
        self.cut.lock().unwrap().insert(worker.to_string());
    }

    fn check(&self, run_id: &str, epoch: u64) -> Result<()> {
        let owner = self.owners.lock().unwrap().get(&(run_id.to_string(), epoch)).cloned();
        if owner.is_some_and(|owner| self.cut.lock().unwrap().contains(&owner)) {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Unavailable,
                "test.partitioned",
                "run store unreachable",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl DurableRunStore for PartitionedStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        self.inner.enqueue(run).await
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        if self.cut.lock().unwrap().contains(worker) {
            return Ok(None);
        }
        let claimed = self.inner.claim(queue, worker, ttl).await?;
        if let Some(claimed) = &claimed {
            self.owners
                .lock()
                .unwrap()
                .insert((claimed.record.run_id.clone(), claimed.epoch), worker.to_string());
        }
        Ok(claimed)
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        self.check(run_id, epoch)?;
        self.inner.heartbeat(run_id, epoch, ttl).await
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        self.check(run_id, epoch)?;
        self.inner.set_checkpoint(run_id, epoch, checkpoint).await
    }

    async fn wait(
        &self,
        run_id: &str,
        epoch: u64,
        until: Option<chrono::DateTime<Utc>>,
    ) -> Result<()> {
        self.check(run_id, epoch)?;
        self.inner.wait(run_id, epoch, until).await
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        self.inner.wake(run_id).await
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        self.inner.finish(run_id, epoch, outcome).await
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        self.inner.request_cancel(run_id).await
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        self.inner.get(run_id).await
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        self.inner.list(filter).await
    }

    async fn prune(&self, finished_before: chrono::DateTime<Utc>) -> Result<u64> {
        self.inner.prune(finished_before).await
    }
}

/// Gate: a stalled worker's writes are refused once another worker recovered the run.
#[tokio::test]
async fn a_stalled_worker_cannot_write_after_recovery() {
    let runs = InMemoryRunStore::new();
    let run = runs
        .enqueue(NewRun::new("default", "app", "user", "flow", json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();
    let stalled = runs.claim("default", "a", TTL).await.unwrap().unwrap();
    tokio::time::sleep(TTL + Duration::from_millis(50)).await;
    let recovered = runs.claim("default", "b", TTL).await.unwrap().unwrap();
    assert_eq!(recovered.epoch, stalled.epoch + 1);

    let stale = [
        runs.heartbeat(&run.run_id, stalled.epoch, TTL).await.map(|_| ()),
        runs.set_checkpoint(
            &run.run_id,
            stalled.epoch,
            RunCheckpointRef::Graph { thread_id: "t".into(), checkpoint_id: None },
        )
        .await,
        runs.finish(&run.run_id, stalled.epoch, RunOutcome::Completed(json!("late")))
            .await
            .map(|_| ()),
    ];
    for result in stale {
        assert_eq!(result.unwrap_err().code, RUN_STALE_EPOCH_CODE);
    }
    runs.finish(&run.run_id, recovered.epoch, RunOutcome::Completed(json!("ok"))).await.unwrap();
    assert_eq!(runs.get(&run.run_id).await.unwrap().unwrap().result, Some(json!("ok")));
}

/// Gate: an agent worker cut off from the store stops at its lease fence; it neither
/// runs another tool nor appends another event, and the recovering worker charges nothing.
#[tokio::test]
async fn a_partitioned_agent_worker_fences_itself() {
    let partitioned = Arc::new(PartitionedStore::new());
    let runs: Arc<dyn DurableRunStore> = Arc::clone(&partitioned) as Arc<dyn DurableRunStore>;
    let fixture = PaymentFixture::new(Arc::clone(&runs));
    let run = enqueue_payment(&runs).await;

    let first = fixture.worker("replica-a").spawn();
    let charges = Arc::clone(&fixture.charges);
    wait_for(Duration::from_secs(5), move || {
        let charges = Arc::clone(&charges);
        Box::pin(async move { (charges.load(Ordering::SeqCst) == 1).then_some(()) })
    })
    .await
    .unwrap();
    partitioned.cut_off("replica-a");
    fixture.hang.store(false, Ordering::SeqCst);

    let second = fixture.worker("replica-b").spawn();
    let finished = wait_terminal(&runs, &run.run_id).await;
    second.shutdown().await;
    first.shutdown().await;

    assert_eq!(finished.state, RunState::Completed);
    assert_eq!(fixture.charges.load(Ordering::SeqCst), 1);
    let responses = fixture.function_responses("order-7").await;
    assert_eq!(responses.len(), 1, "the fenced worker appended nothing: {responses:?}");
}

// --- Workflows and graphs ---

#[tokio::test]
async fn a_failing_workflow_retries_until_its_attempts_run_out() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let registry = WorkflowRegistry::new().register("flaky", move |_input, _cancel| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Err("still failing".to_string())
        }
    });
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let run = runs
        .enqueue(NewRun::new("default", "app", "user", "flaky", json!({})).with_max_attempts(3))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_workflows(Arc::new(registry))
        .with_config(worker_config("w"))
        .spawn();
    let finished = wait_terminal(&runs, &run.run_id).await;
    worker.shutdown().await;
    assert_eq!(finished.state, RunState::Failed);
    assert_eq!(finished.attempt, 3);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(finished.error.as_deref(), Some("still failing"));
}

/// A graph workflow recovered after a crash resumes from its checkpointer: the step
/// that completed before the crash does not run again.
#[tokio::test]
async fn a_recovered_graph_run_resumes_from_its_checkpointer() {
    use adk_graph::checkpoint::MemoryCheckpointer;
    use adk_graph::edge::{END, START};
    use adk_graph::graph::StateGraph;
    use adk_graph::node::{ExecutionConfig, NodeOutput};
    use adk_graph::state::State;

    let summaries = Arc::new(AtomicUsize::new(0));
    let charges = Arc::new(AtomicUsize::new(0));
    let hang = Arc::new(AtomicBool::new(true));
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = {
        let summaries = Arc::clone(&summaries);
        let charges = Arc::clone(&charges);
        let hang = Arc::clone(&hang);
        Arc::new(
            StateGraph::with_channels(&["summary", "receipt"])
                .add_node_fn("summarize", move |_ctx| {
                    let summaries = Arc::clone(&summaries);
                    async move {
                        summaries.fetch_add(1, Ordering::SeqCst);
                        Ok(NodeOutput::new().with_update("summary", json!("double billed")))
                    }
                })
                .add_node_fn("charge", move |_ctx| {
                    let charges = Arc::clone(&charges);
                    let hang = Arc::clone(&hang);
                    async move {
                        if hang.load(Ordering::SeqCst) {
                            std::future::pending::<()>().await;
                        }
                        charges.fetch_add(1, Ordering::SeqCst);
                        Ok(NodeOutput::new().with_update("receipt", json!({ "cents": 1999 })))
                    }
                })
                .add_edge(START, "summarize")
                .add_edge("summarize", "charge")
                .add_edge("charge", END)
                .compile()
                .unwrap()
                .with_checkpointer_arc(checkpointer),
        )
    };
    let resumed_flags = Arc::new(Mutex::new(Vec::new()));
    let registry = {
        let graph = Arc::clone(&graph);
        let resumed_flags = Arc::clone(&resumed_flags);
        WorkflowRegistry::new().register_durable("settle", move |input, run| {
            let graph = Arc::clone(&graph);
            let resumed_flags = Arc::clone(&resumed_flags);
            async move {
                resumed_flags.lock().unwrap().push(run.resumed);
                let state: State = input.into_iter().collect();
                let out = graph
                    .invoke(state, ExecutionConfig::new(&run.thread_id))
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(json!({ "summary": out.get("summary"), "receipt": out.get("receipt") }))
            }
        })
    };
    let registry = Arc::new(registry);
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let run = runs
        .enqueue(NewRun::new("default", "app", "user", "settle", json!({ "ticket": "ORD-7" })))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();

    let first = DurableWorker::new(Arc::clone(&runs))
        .with_workflows(registry.clone())
        .with_config(worker_config("a"))
        .spawn();
    let summaries_probe = Arc::clone(&summaries);
    wait_for(Duration::from_secs(5), move || {
        let summaries = Arc::clone(&summaries_probe);
        Box::pin(async move { (summaries.load(Ordering::SeqCst) == 1).then_some(()) })
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    first.abort();
    hang.store(false, Ordering::SeqCst);

    let second = DurableWorker::new(Arc::clone(&runs))
        .with_workflows(registry)
        .with_config(worker_config("b"))
        .spawn();
    let finished = wait_terminal(&runs, &run.run_id).await;
    second.shutdown().await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(summaries.load(Ordering::SeqCst), 1, "the checkpointed step did not run again");
    assert_eq!(charges.load(Ordering::SeqCst), 1);
    assert_eq!(*resumed_flags.lock().unwrap(), vec![false, true]);
    assert_eq!(
        finished.checkpoint,
        Some(RunCheckpointRef::Graph { thread_id: run.run_id.clone(), checkpoint_id: None })
    );
}

// --- Scheduler ---

/// Gate: three scheduler replicas over N ticks enqueue each fire time exactly once.
#[tokio::test]
async fn three_scheduler_replicas_fire_each_occurrence_once() {
    let schedules: Arc<dyn ScheduleStore> = Arc::new(InMemoryScheduleStore::new());
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let clock = Arc::new(adk_core::ManualClock::new(Utc::now()));
    let start = clock.now();
    schedules
        .upsert(
            Schedule::new(
                "every-minute",
                ScheduleSpec::Every(Duration::from_secs(60)),
                NewRun::new("default", "ops", "system", "probe", json!({})),
                MissedFirePolicy::CatchUp(3),
            )
            .with_next_fire_at(start),
        )
        .await
        .unwrap();
    let replicas: Vec<DurableScheduler> = (0..3)
        .map(|_| {
            DurableScheduler::new(Arc::clone(&schedules), Arc::clone(&runs))
                .with_clock(clock.clone() as Arc<dyn adk_core::Clock>)
        })
        .collect();

    const TICKS: usize = 20;
    for _ in 0..TICKS {
        let ticks = futures::future::join_all(replicas.iter().map(DurableScheduler::tick)).await;
        assert!(ticks.iter().all(std::result::Result::is_ok));
        clock.advance(Duration::from_secs(60));
    }
    let fired = runs.list(RunFilter::default()).await.unwrap();
    assert_eq!(fired.len(), TICKS, "one run per fire time across three replicas");
    let keys: HashSet<_> = fired.iter().filter_map(|run| run.idempotency_key.clone()).collect();
    assert_eq!(keys.len(), TICKS);
    assert_eq!(
        schedules.get("every-minute").await.unwrap().unwrap().next_fire_at,
        Some(start + chrono::Duration::seconds(60 * TICKS as i64))
    );
}

/// The same with spawned, unsynchronised loops racing on real time.
#[tokio::test]
async fn racing_scheduler_loops_fire_each_occurrence_once() {
    let schedules: Arc<dyn ScheduleStore> = Arc::new(InMemoryScheduleStore::new());
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let start = Utc::now();
    schedules
        .upsert(
            Schedule::new(
                "fast",
                ScheduleSpec::Every(Duration::from_millis(50)),
                NewRun::new("default", "ops", "system", "probe", json!({})),
                MissedFirePolicy::CatchUp(100),
            )
            .with_next_fire_at(start),
        )
        .await
        .unwrap();
    let loops: Vec<_> = (0..3)
        .map(|_| {
            DurableScheduler::new(Arc::clone(&schedules), Arc::clone(&runs))
                .with_tick_interval(Duration::from_millis(7))
                .spawn()
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(600)).await;
    loops.iter().for_each(tokio::task::JoinHandle::abort);
    let schedule = schedules.get("fast").await.unwrap().unwrap();
    let fired = runs.list(RunFilter::default()).await.unwrap();
    let expected = ((schedule.next_fire_at.unwrap() - start).num_milliseconds() / 50) as usize;
    assert_eq!(fired.len(), expected, "every fire time before the cursor fired exactly once");
    let keys: HashSet<_> = fired.iter().filter_map(|run| run.idempotency_key.clone()).collect();
    assert_eq!(keys.len(), fired.len());
}

// --- Replicated kill switch ---

const POLL: Duration = Duration::from_millis(50);

fn mirror_config() -> GovernanceMirrorConfig {
    GovernanceMirrorConfig::default()
        .with_poll_interval(POLL)
        .with_max_staleness(Duration::from_millis(400))
}

/// Gate: a freeze reaches every replica within the poll bound, and no schedule fires on
/// any replica while it holds.
#[tokio::test]
async fn a_freeze_reaches_every_replica_and_no_schedule_fires_while_frozen() {
    let governance: Arc<dyn GovernanceStateStore> = Arc::new(InMemoryGovernanceStateStore::new());
    let schedules: Arc<dyn ScheduleStore> = Arc::new(InMemoryScheduleStore::new());
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let controls: Vec<GovernanceControl> = (0..3)
        .map(|_| GovernanceControl::mirrored(Arc::clone(&governance), mirror_config()))
        .collect();
    let mirrors: Vec<_> = controls.iter().filter_map(GovernanceControl::spawn_mirror).collect();
    for control in &controls {
        while control.check().is_err() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let schedulers: Vec<DurableScheduler> = controls
        .iter()
        .map(|control| {
            DurableScheduler::new(Arc::clone(&schedules), Arc::clone(&runs))
                .with_governance(control.clone())
        })
        .collect();

    controls[0].freeze_replicated("incident 9", Some(SubjectRef::human("oncall"))).await.unwrap();
    let frozen_at = tokio::time::Instant::now();
    for control in &controls {
        while !control.is_frozen() {
            assert!(
                frozen_at.elapsed() < POLL * 3,
                "a replica missed the freeze past the poll bound"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    let now = Utc::now();
    schedules
        .upsert(
            Schedule::new(
                "while-frozen",
                ScheduleSpec::Every(Duration::from_millis(20)),
                NewRun::new("default", "ops", "system", "probe", json!({})),
                MissedFirePolicy::Skip,
            )
            .with_next_fire_at(now),
        )
        .await
        .unwrap();
    for _ in 0..10 {
        for scheduler in &schedulers {
            assert!(scheduler.tick().await.unwrap().frozen);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        runs.list(RunFilter::default()).await.unwrap().is_empty(),
        "nothing fired while frozen"
    );
    assert_eq!(
        schedules.get("while-frozen").await.unwrap().unwrap().next_fire_at,
        Some(now),
        "a frozen scheduler does not advance"
    );

    controls[2].unfreeze_replicated(None).await.unwrap();
    for control in &controls {
        while control.is_frozen() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let ticks = futures::future::join_all(schedulers.iter().map(DurableScheduler::tick)).await;
    let enqueued: usize = ticks.into_iter().map(|tick| tick.unwrap().enqueued).sum();
    assert_eq!(enqueued, 1, "the latest missed occurrence fires once, within the skip grace");
    mirrors.iter().for_each(tokio::task::JoinHandle::abort);
}

/// A governance store that can be made unreachable.
#[derive(Default)]
struct FlakyGovernance {
    inner: InMemoryGovernanceStateStore,
    down: AtomicBool,
}

#[async_trait]
impl GovernanceStateStore for FlakyGovernance {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        if self.down.load(Ordering::SeqCst) {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Unavailable,
                "test.down",
                "governance store unreachable",
            ));
        }
        self.inner.get(scope).await
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        self.inner.set(scope, expected_epoch, frozen, reason, issued_by).await
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        self.inner.list().await
    }
}

/// Gate: past the staleness limit with the governance store unreachable, a replica
/// refuses new runs — submissions, claims, and runner invocations.
#[tokio::test]
async fn an_unreachable_governance_store_refuses_new_runs_past_staleness() {
    let governance = Arc::new(FlakyGovernance::default());
    let control = GovernanceControl::mirrored(governance.clone(), mirror_config());
    let mirror = control.spawn_mirror().unwrap();
    while control.check().is_err() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let registry = WorkflowRegistry::new()
        .register("noop", |_input, _cancel| async move { Ok(json!({ "ran": true })) });
    let state = BackgroundState::new()
        .with_executor(Arc::new(registry))
        .with_governance(control.clone())
        .with_embedded_worker(false);
    let runs = Arc::clone(state.run_store());
    let queued = runs
        .enqueue(NewRun::new("default", "background", "user", "noop", json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();

    governance.down.store(true, Ordering::SeqCst);
    while control.check().is_ok() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(control.check().unwrap_err().code, adk_core::GOVERNANCE_STALE_CODE);

    let response = background_runs_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runs")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "target": "noop", "input": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(state.worker().with_config(worker_config("w")).run_next().await.unwrap().is_none());
    assert_eq!(
        runs.get(&queued.run_id).await.unwrap().unwrap().state,
        RunState::Queued,
        "not claimed"
    );

    governance.down.store(false, Ordering::SeqCst);
    while control.check().is_err() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let finished =
        state.worker().with_config(worker_config("w")).run_next().await.unwrap().unwrap();
    assert_eq!(finished.state, RunState::Completed);
    mirror.abort();
}

// --- Cancellation across replicas ---

/// Gate: a cancel requested on one replica stops a run owned by another at its next
/// heartbeat — through `DELETE /runs/{id}` and through `Runner::interrupt`.
#[tokio::test]
async fn a_cancel_on_one_replica_stops_a_run_on_another() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let started = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&started);
    let registry = Arc::new(WorkflowRegistry::new().register("long", move |_input, cancel| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            cancel.cancelled().await;
            Err("cancelled".to_string())
        }
    }));
    // Replica a executes; replica b only serves the API.
    let replica_a = BackgroundState::new()
        .with_run_store(Arc::clone(&runs))
        .with_executor(registry.clone())
        .with_worker_config(worker_config("replica-a"));
    let replica_b = BackgroundState::new()
        .with_run_store(Arc::clone(&runs))
        .with_executor(registry)
        .with_embedded_worker(false);
    let worker = replica_a.worker().spawn();

    let run = runs
        .enqueue(NewRun::new("default", "background", "user", "long", json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();
    while started.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let response = background_runs_router_with_state(replica_b)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/runs/{}", run.run_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let requested_at = tokio::time::Instant::now();
    let finished = wait_terminal(&runs, &run.run_id).await;
    assert_eq!(finished.state, RunState::Cancelled);
    assert!(requested_at.elapsed() < TTL, "seen within one heartbeat interval");
    worker.shutdown().await;
}

#[tokio::test]
async fn runner_interrupt_requests_cancellation_through_the_run_store() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let fixture = PaymentFixture::new(Arc::clone(&runs));
    let run = enqueue_payment(&runs).await;
    let other = runs
        .enqueue(
            NewRun::new("default", "billing", "customer-1", "billing", json!("x"))
                .with_session_id("another-session"),
        )
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();

    let runner = fixture.runner();
    assert!(!runner.interrupt("order-7"), "nothing runs in this process");
    let runs_probe = Arc::clone(&runs);
    let run_id = run.run_id.clone();
    wait_for(Duration::from_secs(2), move || {
        let runs = Arc::clone(&runs_probe);
        let run_id = run_id.clone();
        Box::pin(async move {
            runs.get(&run_id).await.ok().flatten().filter(|run| run.state == RunState::Cancelled)
        })
    })
    .await
    .expect("the queued run was cancelled through the store");
    assert_eq!(runs.get(&other.run_id).await.unwrap().unwrap().state, RunState::Queued);
    assert_eq!(runner.cancel_durable_runs("customer-1", "another-session").await.unwrap(), 1);
}

// --- REST surface ---

async fn post_run(state: &BackgroundState, body: Value) -> (StatusCode, Value) {
    let response = background_runs_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runs")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn run_creation_honours_idempotency_and_double_texting() {
    let registry = WorkflowRegistry::new().register("slow", |_input, cancel| async move {
        cancel.cancelled().await;
        Ok(json!(null))
    });
    let state =
        BackgroundState::new().with_executor(Arc::new(registry)).with_embedded_worker(false);

    let first = json!({ "target": "slow", "input": {}, "idempotencyKey": "evt-1", "concurrencyKey": "thread-1", "onConflict": "reject" });
    let (status, created) = post_run(&state, first.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, again) = post_run(&state, first).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["runId"], created["runId"]);
    assert_eq!(again["existing"], true);

    let rejected = json!({ "target": "slow", "input": {}, "concurrencyKey": "thread-1", "onConflict": "reject" });
    let (status, body) = post_run(&state, rejected).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["blockingRunId"], created["runId"]);

    let interrupting = json!({ "target": "slow", "input": {}, "concurrencyKey": "thread-1", "onConflict": "interrupt" });
    let (status, _) = post_run(&state, interrupting).await;
    assert_eq!(status, StatusCode::CREATED);
    let first = state.run_store().get(created["runId"].as_str().unwrap()).await.unwrap().unwrap();
    assert_eq!(first.state, RunState::Cancelled, "the queued run was interrupted");

    let (status, _) = post_run(&state, json!({ "target": "missing", "input": {} })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = OnConflict::Interrupt;
}

// --- Parking on held tool calls ---

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

struct TransferTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for TransferTool {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "moves money"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "sent" }))
    }
}

/// A run that ends holding a tool call parks as `Waiting` instead of finishing; a wake
/// after the decision resumes the same session and the approved call executes once.
#[tokio::test]
async fn a_held_call_parks_the_run_and_a_wake_resumes_it() {
    use adk_core::{ApprovalScope, ApprovalStore, InMemoryApprovalStore, ToolApproval};

    let sessions = Arc::new(InMemorySessionService::new());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let executions = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(Arc::new(TransferModel) as Arc<dyn Llm>)
        .tool(Arc::new(TransferTool { executions: Arc::clone(&executions) }))
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let runner = Arc::new(
        Runner::builder()
            .app_name("ops")
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
            .approval_store(Arc::clone(&approvals) as Arc<dyn ApprovalStore>)
            .action_ledger(Arc::new(InMemoryActionLedger::new()))
            .build()
            .unwrap(),
    );
    let state = BackgroundState::new().with_agent("ops", runner).with_worker_config(
        worker_config("w").with_approval_timeout(Some(Duration::from_secs(600))),
    );
    let runs = Arc::clone(state.run_store());
    let (status, submitted) = post_run(
        &state,
        json!({ "target": "ops", "input": "send 50 USD", "sessionId": "pay-1", "userId": "alice" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = submitted["runId"].as_str().unwrap().to_string();

    let probe = Arc::clone(&runs);
    let id = run_id.clone();
    let parked = wait_for(Duration::from_secs(5), move || {
        let runs = Arc::clone(&probe);
        let id = id.clone();
        Box::pin(async move {
            runs.get(&id).await.ok().flatten().filter(|run| run.state == RunState::Waiting)
        })
    })
    .await
    .expect("the run parks instead of finishing");
    let Some(RunCheckpointRef::AwaitingApproval { session_id, fingerprints, expires_at, .. }) =
        parked.checkpoint.clone()
    else {
        panic!("parked without an approval checkpoint: {:?}", parked.checkpoint);
    };
    assert_eq!(session_id, "pay-1");
    assert_eq!(fingerprints.len(), 1);
    assert!(expires_at.is_some(), "the approval timeout sets the wait's end");
    assert_eq!(parked.attempt, 1);
    assert_eq!(executions.load(Ordering::SeqCst), 0, "nothing executes before the decision");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runs.get(&run_id).await.unwrap().unwrap().state, RunState::Waiting, "stays parked");

    let scope = ApprovalScope::new("ops", "alice", "pay-1");
    approvals.decide(&scope, &fingerprints[0], ToolApproval::approve()).await.unwrap();
    assert_eq!(state.wake_session_runs("ops", "alice", "pay-1").await.unwrap(), 1);

    let finished = wait_terminal(&runs, &run_id).await;
    state.shutdown_worker().await;
    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(finished.attempt, 1, "a wake resumes the same attempt");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the approved call executes once: {finished:?}"
    );
    assert_eq!(
        finished.result.as_ref().unwrap()["text"],
        format!("transfer: {}", json!({ "status": "sent" }))
    );
}

/// `POST /runs/{id}/wake` reaches the store.
#[tokio::test]
async fn the_wake_route_wakes_a_run() {
    let state = BackgroundState::new().with_embedded_worker(false);
    let response = background_runs_router_with_state(state)
        .oneshot(
            Request::builder().method("POST").uri("/runs/absent/wake").body(Body::empty()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
