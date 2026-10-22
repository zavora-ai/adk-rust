//! The durable worker's lifecycle hooks and suspend path (spec 3.2 §10 S1).
//!
//! Each hook is checked to run after the store write it reports, in claim, park, and
//! finish order. A run asked to suspend stops at its next model-call boundary, parks
//! `Waiting` on its `Session` checkpoint, and resumes on another worker after a restart
//! without executing its completed non-idempotent call again; a cancellation still finishes
//! it `Cancelled`.

#![cfg(feature = "background")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::run_store::RunCheckpointRef;
use adk_core::{
    ApprovalScope, ApprovalStore, DurableRunStore, GovernanceControl, InMemoryActionLedger,
    InMemoryApprovalStore, InMemoryRunStore, Llm, NewRun, Part, Result, RunRecord, RunState, Tool,
    ToolApproval, ToolContext, ToolEffect,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::{
    DurableWorker, DurableWorkerConfig, ParkReason, WorkerHooks, WorkflowRegistry,
};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

const TTL: Duration = Duration::from_millis(300);
const POLL: Duration = Duration::from_millis(20);

fn worker_config(id: &str) -> DurableWorkerConfig {
    DurableWorkerConfig::default().with_worker_id(id).with_lease_ttl(TTL).with_poll_interval(POLL)
}

/// Polls `probe` every 10 ms until it returns `Some`, for at most five seconds.
async fn eventually<T>(
    what: &str,
    mut probe: impl FnMut() -> futures::future::BoxFuture<'static, Option<T>>,
) -> T {
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(5) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn run_in_state(runs: &Arc<dyn DurableRunStore>, run_id: &str, state: RunState) -> RunRecord {
    let runs = Arc::clone(runs);
    let id = run_id.to_string();
    eventually(&format!("run {run_id} to be {state:?}"), move || {
        let runs = Arc::clone(&runs);
        let id = id.clone();
        Box::pin(async move { runs.get(&id).await.ok().flatten().filter(|run| run.state == state) })
    })
    .await
}

/// One hook call as the hook saw it: the run it was given, and the run as the store held it
/// at that moment.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    hook: &'static str,
    state: RunState,
    stored: RunState,
    reason: Option<ParkReason>,
}

impl Seen {
    fn claim() -> Self {
        Self { hook: "on_claim", state: RunState::Running, stored: RunState::Running, reason: None }
    }

    fn park(reason: ParkReason) -> Self {
        Self {
            hook: "on_park",
            state: RunState::Waiting,
            stored: RunState::Waiting,
            reason: Some(reason),
        }
    }

    fn finish(state: RunState) -> Self {
        Self { hook: "on_finish", state, stored: state, reason: None }
    }
}

/// Records every hook call; freezes `freeze_on_claim` inside the first `on_claim`, as a
/// freeze that lands after the claim and before the runner starts the agent.
struct Recorder {
    runs: Arc<dyn DurableRunStore>,
    seen: Mutex<Vec<Seen>>,
    freeze_on_claim: Mutex<Option<GovernanceControl>>,
}

impl Recorder {
    fn new(runs: &Arc<dyn DurableRunStore>) -> Arc<Self> {
        Arc::new(Self {
            runs: Arc::clone(runs),
            seen: Mutex::default(),
            freeze_on_claim: Mutex::default(),
        })
    }

    async fn record(&self, hook: &'static str, run: &RunRecord, reason: Option<ParkReason>) {
        let stored = self.runs.get(&run.run_id).await.unwrap().unwrap().state;
        self.seen.lock().unwrap().push(Seen { hook, state: run.state, stored, reason });
    }

    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

#[async_trait]
impl WorkerHooks for Recorder {
    async fn on_claim(&self, run: &RunRecord) {
        self.record("on_claim", run, None).await;
        if let Some(control) = self.freeze_on_claim.lock().unwrap().take() {
            control.freeze("incident 7");
        }
    }

    async fn on_park(&self, run: &RunRecord, reason: ParkReason) {
        self.record("on_park", run, Some(reason)).await;
    }

    async fn on_finish(&self, run: &RunRecord) {
        self.record("on_finish", run, None).await;
    }
}

/// A non-idempotent charge that blocks inside the call until the test adds a permit.
struct ChargeTool {
    charges: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
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
        self.release.acquire().await.expect("the semaphore stays open").forget();
        Ok(json!({ "status": "charged" }))
    }
}

/// Stores, the scripted model, and the charge counter shared by every replica, as separate
/// processes over shared backends would share them.
struct Payment {
    runs: Arc<dyn DurableRunStore>,
    sessions: Arc<InMemorySessionService>,
    ledger: Arc<InMemoryActionLedger>,
    model: Arc<ScriptedLlm>,
    charges: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    governance: Option<GovernanceControl>,
}

impl Payment {
    fn new() -> Self {
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            ledger: Arc::new(InMemoryActionLedger::new()),
            model: Arc::new(ScriptedLlm::new(
                "payment-script",
                [
                    ScriptedResponse::function_call("charge_card", json!({ "amountCents": 1999 })),
                    ScriptedResponse::text("charge status: charged"),
                ],
            )),
            charges: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
            governance: None,
        }
    }

    fn governed(mut self, control: &GovernanceControl) -> Self {
        self.governance = Some(control.clone());
        self
    }

    /// One replica's worker: its own runner, agent, and tool over the shared stores.
    fn worker(&self, id: &str, hooks: &Arc<Recorder>) -> DurableWorker {
        let agent = LlmAgentBuilder::new("billing")
            .model(Arc::clone(&self.model) as Arc<dyn Llm>)
            .tool(Arc::new(ChargeTool {
                charges: Arc::clone(&self.charges),
                release: Arc::clone(&self.release),
            }))
            .build()
            .unwrap();
        let mut runner = Runner::builder()
            .app_name("billing")
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&self.sessions) as Arc<dyn SessionService>)
            .action_ledger(Arc::clone(&self.ledger) as Arc<dyn adk_core::ActionLedger>);
        if let Some(control) = &self.governance {
            runner = runner.governance(control.clone());
        }
        let mut worker = DurableWorker::new(Arc::clone(&self.runs))
            .with_agent("billing", Arc::new(runner.build().unwrap()))
            .with_hooks(Arc::clone(hooks) as Arc<dyn WorkerHooks>)
            .with_config(worker_config(id));
        if let Some(control) = &self.governance {
            worker = worker.with_governance(control.clone());
        }
        worker
    }

    async fn enqueue(&self) -> RunRecord {
        let run =
            NewRun::new("default", "billing", "customer-1", "billing", json!("charge my card"))
                .with_session_id("order-7");
        self.runs.enqueue(run).await.unwrap().record().cloned().unwrap()
    }

    async fn charge_started(&self) {
        let charges = Arc::clone(&self.charges);
        eventually("the charge to start", move || {
            let charges = Arc::clone(&charges);
            Box::pin(async move { (charges.load(Ordering::SeqCst) == 1).then_some(()) })
        })
        .await;
    }

    /// The charge responses each model request carried, request by request.
    fn charge_responses_sent(&self) -> Vec<Vec<Value>> {
        self.model
            .requests()
            .iter()
            .map(|request| {
                request
                    .contents
                    .iter()
                    .flat_map(|content| &content.parts)
                    .filter_map(|part| match part {
                        Part::FunctionResponse { function_response, .. }
                            if function_response.name == "charge_card" =>
                        {
                            Some(function_response.response.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }
}

/// The `Session` checkpoint a suspended run is parked on, with its invocation.
fn session_checkpoint(run: &RunRecord) -> (String, String) {
    match &run.checkpoint {
        Some(RunCheckpointRef::Session { session_id, invocation_id: Some(invocation_id) }) => {
            (session_id.clone(), invocation_id.clone())
        }
        other => panic!("expected a Session checkpoint with an invocation, got {other:?}"),
    }
}

/// Gate: the hooks run after the store write each reports — claim, park on a held call,
/// claim again after the wake, finish — and see the run in the state the store holds.
#[tokio::test]
async fn hooks_report_claim_park_and_finish_after_each_store_write() {
    struct TransferTool(Arc<AtomicUsize>);

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
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "status": "sent" }))
        }
    }

    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let executions = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(ScriptedLlm::new(
        "transfer-script",
        [
            ScriptedResponse::function_call("transfer", json!({ "amountCents": 5000 })),
            ScriptedResponse::text("sent"),
        ],
    ));
    let agent = LlmAgentBuilder::new("ops")
        .model(model as Arc<dyn Llm>)
        .tool(Arc::new(TransferTool(Arc::clone(&executions))))
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name("ops")
        .agent(Arc::new(agent))
        .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
        .approval_store(Arc::clone(&approvals) as Arc<dyn ApprovalStore>)
        .action_ledger(Arc::new(InMemoryActionLedger::new()))
        .build()
        .unwrap();
    let hooks = Recorder::new(&runs);
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent("ops", Arc::new(runner))
        .with_hooks(Arc::clone(&hooks) as Arc<dyn WorkerHooks>)
        .with_config(worker_config("w"));
    let run = runs
        .enqueue(
            NewRun::new("default", "ops", "alice", "ops", json!("send 50 USD"))
                .with_session_id("pay-1"),
        )
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();

    let parked = worker.run_next().await.unwrap().expect("the attempt parks the run");
    assert_eq!(parked.state, RunState::Waiting);
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::park(ParkReason::AwaitingApproval)]);
    let Some(RunCheckpointRef::AwaitingApproval { fingerprints, .. }) = parked.checkpoint else {
        panic!("parked without an approval checkpoint: {:?}", parked.checkpoint);
    };

    let scope = ApprovalScope::new("ops", "alice", "pay-1");
    approvals.decide(&scope, &fingerprints[0], ToolApproval::approve()).await.unwrap();
    runs.wake(&run.run_id).await.unwrap();
    let finished = worker.run_next().await.unwrap().expect("the woken run is claimed");
    assert_eq!((finished.state, finished.attempt), (RunState::Completed, 1), "{finished:?}");
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::finish(RunState::Completed)]);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

/// Gate: a run the kill switch refuses after the claim, before the agent starts, is
/// reported handed back; nothing is reported while frozen, and the run finishes once
/// unfrozen.
#[tokio::test]
async fn a_run_refused_at_its_start_is_reported_handed_back() {
    let control = GovernanceControl::new();
    let payment = Payment::new().governed(&control);
    payment.release.add_permits(1);
    let hooks = Recorder::new(&payment.runs);
    *hooks.freeze_on_claim.lock().unwrap() = Some(control.clone());
    let worker = payment.worker("w", &hooks);
    let run = payment.enqueue().await;

    assert!(worker.run_next().await.unwrap().is_none(), "the refused run is handed back");
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::park(ParkReason::HandedBack)]);
    let handed_back = payment.runs.get(&run.run_id).await.unwrap().unwrap();
    assert_eq!((handed_back.state, handed_back.attempt), (RunState::Waiting, 1));
    assert!(worker.run_next().await.unwrap().is_none(), "nothing is claimed while frozen");
    assert_eq!(hooks.take(), Vec::<Seen>::new());

    control.unfreeze();
    let finished = worker.run_next().await.unwrap().expect("claimed once unfrozen");
    assert_eq!((finished.state, finished.attempt), (RunState::Completed, 1), "{finished:?}");
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::finish(RunState::Completed)]);
}

/// Gate: a run asked to suspend while its non-idempotent charge is executing lets the
/// charge finish, makes no further model call, and parks `Waiting` on its `Session`
/// checkpoint with no lease. It stays parked; after the worker stops, a new worker claims
/// nothing until the run is woken, then resumes the session at the same attempt and
/// completes it with the charge executed once.
#[tokio::test]
async fn a_suspended_run_parks_on_its_session_checkpoint_and_resumes_after_a_worker_restart() {
    let payment = Payment::new();
    let hooks_a = Recorder::new(&payment.runs);
    let replica_a = payment.worker("replica-a", &hooks_a).spawn();
    let run = payment.enqueue().await;

    payment.charge_started().await;
    assert!(replica_a.suspend(&run.run_id), "the run is in flight on replica a");
    payment.release.add_permits(1);
    let parked = run_in_state(&payment.runs, &run.run_id, RunState::Waiting).await;

    assert_eq!(
        (parked.attempt, parked.lease_owner.as_deref(), parked.lease_expires_at),
        (1, None, None),
        "parked at the same attempt, holding no lease: {parked:?}"
    );
    let (session_id, invocation_id) = session_checkpoint(&parked);
    assert_eq!(session_id, "order-7");
    assert_eq!(payment.model.requests().len(), 1, "no model call after the suspension");
    assert_eq!(payment.charges.load(Ordering::SeqCst), 1);
    assert_eq!(hooks_a.take(), vec![Seen::claim(), Seen::park(ParkReason::Suspended)]);

    tokio::time::sleep(POLL * 5).await;
    let still = payment.runs.get(&run.run_id).await.unwrap().unwrap();
    assert_eq!((still.state, still.lease_epoch), (RunState::Waiting, parked.lease_epoch));
    replica_a.shutdown().await;

    // The process restarts: a new worker, runner, agent, and tool over the same stores.
    let hooks_b = Recorder::new(&payment.runs);
    let replica_b = payment.worker("replica-b", &hooks_b);
    assert!(replica_b.run_next().await.unwrap().is_none(), "parked until woken");
    assert_eq!(payment.model.requests().len(), 1, "a parked run causes no model call");

    payment.runs.wake(&run.run_id).await.unwrap();
    let finished = replica_b.run_next().await.unwrap().expect("the woken run is claimed");
    assert_eq!(
        (finished.state, finished.attempt, finished.result.as_ref().map(|r| r["text"].clone())),
        (RunState::Completed, 1, Some(json!("charge status: charged"))),
        "{finished:?}"
    );
    assert_eq!(payment.charges.load(Ordering::SeqCst), 1, "the completed charge never re-runs");
    assert_eq!(
        payment.charge_responses_sent(),
        vec![vec![], vec![json!({ "status": "charged" })]],
        "the resumed turn sends the charge's one response"
    );
    assert_eq!(payment.model.remaining(), 0);
    assert_eq!(hooks_b.take(), vec![Seen::claim(), Seen::finish(RunState::Completed)]);
    assert_ne!(
        finished.result.as_ref().map(|r| r["invocationId"].clone()),
        Some(json!(invocation_id)),
        "the resumed attempt continues the session in a new invocation"
    );
    assert!(!replica_b.suspend(&run.run_id), "a finished run is not in flight");
}

/// Gate: a cancellation requested through the run store still finishes a run that was
/// asked to suspend `Cancelled`, and a wake does not revive it.
#[tokio::test]
async fn a_cancel_still_finishes_a_run_asked_to_suspend_cancelled() {
    let payment = Payment::new();
    let hooks = Recorder::new(&payment.runs);
    let worker = payment.worker("w", &hooks).spawn();
    let run = payment.enqueue().await;

    payment.charge_started().await;
    assert!(worker.suspend(&run.run_id));
    payment.runs.request_cancel(&run.run_id).await.unwrap();
    worker.heartbeat_now();
    let cancelled = run_in_state(&payment.runs, &run.run_id, RunState::Cancelled).await;
    assert_eq!(cancelled.lease_owner, None);
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::finish(RunState::Cancelled)]);

    payment.runs.wake(&run.run_id).await.unwrap();
    tokio::time::sleep(POLL * 5).await;
    assert_eq!(payment.runs.get(&run.run_id).await.unwrap().unwrap().state, RunState::Cancelled);
    assert_eq!(payment.model.requests().len(), 1);
    worker.shutdown().await;
}

/// Gate: a run asked to suspend that the kill switch then refuses at its next model call
/// parks `Waiting` instead of failing with `governance.frozen`, and finishes once unfrozen
/// and woken, with its charge executed once.
#[tokio::test]
async fn a_freeze_at_the_boundary_parks_a_run_asked_to_suspend() {
    let control = GovernanceControl::new();
    let payment = Payment::new().governed(&control);
    let hooks = Recorder::new(&payment.runs);
    let worker = payment.worker("w", &hooks);
    let run = payment.enqueue().await;

    let attempt = {
        let worker = worker.clone();
        tokio::spawn(async move { worker.run_next().await })
    };
    payment.charge_started().await;
    assert!(worker.suspend(&run.run_id), "a clone of the worker reaches its attempts");
    control.freeze("incident 9");
    payment.release.add_permits(1);
    let parked = attempt.await.unwrap().unwrap().expect("the attempt parks the run");

    assert_eq!(
        (parked.state, parked.attempt, parked.error.as_deref()),
        (RunState::Waiting, 1, None)
    );
    assert_eq!(session_checkpoint(&parked).0, "order-7");
    assert_eq!(payment.model.requests().len(), 1);
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::park(ParkReason::Suspended)]);

    payment.runs.wake(&run.run_id).await.unwrap();
    assert!(worker.run_next().await.unwrap().is_none(), "nothing is claimed while frozen");
    control.unfreeze();
    let finished = worker.run_next().await.unwrap().expect("claimed once unfrozen");
    assert_eq!((finished.state, finished.attempt), (RunState::Completed, 1), "{finished:?}");
    assert_eq!(payment.charges.load(Ordering::SeqCst), 1);
}

/// Gate: `suspend` reaches only agent attempts in flight on the worker: a workflow run and
/// an unknown run are not suspended, and the workflow finishes as before.
#[tokio::test]
async fn suspend_reaches_only_agent_attempts_in_flight() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let release = Arc::new(Semaphore::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let registry = {
        let release = Arc::clone(&release);
        let started = Arc::clone(&started);
        WorkflowRegistry::new().register("settle", move |_input, _cancel| {
            let release = Arc::clone(&release);
            let started = Arc::clone(&started);
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                release.acquire().await.expect("open").forget();
                Ok(json!({ "settled": true }))
            }
        })
    };
    let hooks = Recorder::new(&runs);
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_workflows(Arc::new(registry))
        .with_hooks(Arc::clone(&hooks) as Arc<dyn WorkerHooks>)
        .with_config(worker_config("w"))
        .spawn();
    let run = runs
        .enqueue(NewRun::new("default", "background", "user", "settle", json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();
    eventually("the workflow to start", {
        let started = Arc::clone(&started);
        move || {
            let started = Arc::clone(&started);
            Box::pin(async move { (started.load(Ordering::SeqCst) == 1).then_some(()) })
        }
    })
    .await;

    assert!(!worker.suspend(&run.run_id), "a workflow run is not suspendable");
    assert!(!worker.suspend("no-such-run"));
    release.add_permits(1);
    let finished = run_in_state(&runs, &run.run_id, RunState::Completed).await;
    assert_eq!(finished.result, Some(json!({ "settled": true })));
    worker.shutdown().await;
    assert_eq!(hooks.take(), vec![Seen::claim(), Seen::finish(RunState::Completed)]);
}
