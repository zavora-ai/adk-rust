//! The phase-1 kernel scenarios of spec 3.2 §4 and §11, end to end on SQLite: K2 (sleep is
//! free), K3 (it wakes on what matters), K9 (one switch stops everything) and the chaos
//! test (a kernel killed mid-run resumes from its checkpoint on restart).
//!
//! Every scenario runs a `Kernel` with its real `DurableWorker`s and `DurableScheduler`,
//! a `FileWatchTrigger`, a served `WebhookIngress` and a `QueueIngress`, over a
//! `ScriptedLlm` and SQLite stores in one database file. K2 runs on a `ManualClock`, so
//! its hour takes about a second.

#![cfg(feature = "sqlite")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::AuditActor;
use adk_core::schedule::MissedFirePolicy;
use adk_core::{
    ActionLedger, Clock, DurableRunStore, GovernanceControl, Llm, ManualClock, NewRun, Part,
    RunCheckpointRef, RunFilter, RunRecord, RunState, Schedule, ScheduleSpec, ScheduleStore,
    SubjectRef, SystemClock, Tool, ToolContext, ToolEffect,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    ExpiryAction, Kernel, KernelBuilder, KernelConfig, KernelEvent, PartitionId, ProcessFilter,
    ProcessKind, ProcessSpec, ProcessStore, ProcessTable, QueueClass, SqliteProcessStore,
    Suspension, Wake, WakeSet,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{
    GetRequest, SessionService, SqliteActionLedger, SqliteGovernanceStateStore, SqliteRunStore,
    SqliteScheduleStore, SqliteSessionService,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

const APP: &str = "adk-rust";
const TARGET: &str = "adk-rust";

/// The kernel's stores over one pool on the database file.
struct Stores {
    pool: SqlitePool,
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    sessions: Arc<SqliteSessionService>,
    ledger: Arc<SqliteActionLedger>,
    schedules: Arc<SqliteScheduleStore>,
    governance: Arc<SqliteGovernanceStateStore>,
}

/// Opens a new pool over `path`, as a kernel process opens its store, and migrates every
/// store on it. `clock` decides the run store's leases and `not_before`.
async fn open(path: &Path, clock: Arc<dyn Clock>) -> Stores {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("valid URL")
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .expect("SQLite opens");
    let runs = SqliteRunStore::from_pool(pool.clone()).with_clock(clock);
    runs.migrate().await.expect("run schema migrates");
    let processes = SqliteProcessStore::new(pool.clone());
    processes.migrate().await.expect("process schema migrates");
    let sessions = SqliteSessionService::from_pool(pool.clone());
    sessions.migrate().await.expect("session schema migrates");
    let ledger = SqliteActionLedger::from_pool(pool.clone());
    ledger.migrate().await.expect("ledger schema migrates");
    let schedules = SqliteScheduleStore::from_pool(pool.clone());
    schedules.migrate().await.expect("schedule schema migrates");
    let governance = SqliteGovernanceStateStore::from_pool(pool.clone());
    governance.migrate().await.expect("governance schema migrates");
    Stores {
        pool,
        runs: Arc::new(runs),
        processes: Arc::new(processes),
        sessions: Arc::new(sessions),
        ledger: Arc::new(ledger),
        schedules: Arc::new(schedules),
        governance: Arc::new(governance),
    }
}

/// A runner of `agent` over the stores, governed by `control`.
fn runner(stores: &Stores, control: &GovernanceControl, agent: LlmAgentBuilder) -> Arc<Runner> {
    Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent.build().expect("the agent builds")))
            .session_service(Arc::clone(&stores.sessions) as Arc<dyn SessionService>)
            .action_ledger(Arc::clone(&stores.ledger) as Arc<dyn ActionLedger>)
            .governance(control.clone())
            .build()
            .expect("the runner builds"),
    )
}

/// The configuration of a kernel replica named `name`: workers with at most `slots` runs
/// each, a 50 ms scheduler tick on `clock`, and no control plane.
fn config(
    name: &str,
    control: &GovernanceControl,
    clock: Arc<dyn Clock>,
    slots: usize,
) -> KernelConfig {
    let worker = DurableWorkerConfig::default()
        .with_worker_id(name)
        .with_lease_ttl(Duration::from_secs(1))
        .with_max_concurrent_runs(slots);
    KernelConfig::default()
        .with_app_name(APP)
        .with_worker(worker)
        .with_scheduler_tick(Duration::from_millis(50))
        .with_governance(control.clone())
        .with_clock(clock)
        .with_control(ControlPlaneConfig::default().without_socket())
}

/// A kernel replica named `name` over the stores, configured by [`config`].
fn kernel(
    name: &str,
    stores: &Stores,
    control: &GovernanceControl,
    clock: Arc<dyn Clock>,
    slots: usize,
) -> KernelBuilder {
    Kernel::builder()
        .config(config(name, control, clock, slots))
        .run_store(Arc::clone(&stores.runs))
        .process_store(Arc::clone(&stores.processes))
        .session_service(Arc::clone(&stores.sessions) as Arc<dyn SessionService>)
        .schedule_store(Arc::clone(&stores.schedules) as Arc<dyn ScheduleStore>)
}

fn operator() -> AuditActor {
    AuditActor::subject(SubjectRef::human("operator"))
}

fn run_on(queue: QueueClass, target: &str, text: &str) -> NewRun {
    NewRun::new(queue.as_str(), APP, "alice", target, json!({ "text": text }))
}

/// Polls `probe` every 10 ms until it returns `Some`, for at most `limit`.
async fn eventually<T, F, Fut>(what: &str, limit: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= limit {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out after {limit:?} waiting for {what}");
}

async fn in_state(runs: &Arc<dyn DurableRunStore>, pid: &str, state: RunState) -> RunRecord {
    eventually(&format!("{pid} to be {state:?}"), Duration::from_secs(10), || async {
        runs.get(pid).await.ok().flatten().filter(|run| run.state == state)
    })
    .await
}

/// Every run on the kernel's queues.
async fn all_runs(runs: &Arc<dyn DurableRunStore>) -> Vec<RunRecord> {
    let mut all = Vec::new();
    for queue in QueueClass::defaults() {
        all.extend(runs.list(RunFilter::default().queue(queue.as_str())).await.unwrap());
    }
    all
}

/// The `kernel.wake` runs whose key starts with `prefix`.
async fn wake_runs(runs: &Arc<dyn DurableRunStore>, prefix: &str) -> Vec<RunRecord> {
    runs.list(RunFilter::default().queue(QueueClass::MAINTENANCE))
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.idempotency_key.as_deref().is_some_and(|key| key.starts_with(prefix)))
        .collect()
}

/// K2: ten processes asleep on ten kinds of wake set make no model call, spend nothing and
/// hold no lease over a clock-controlled hour; `ps` lists all ten `Waiting` with their wake
/// sets. Past the hour, the time wake, the expiry and the schedule fire on the same clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k2_sleep_is_free_for_an_hour() {
    let db = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "# lockfile\n").unwrap();
    // A fixed weekday morning: the kernel fires the daily 02:00 cron sleeper itself, and
    // the test's two hours never reach it.
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-21T10:00:00Z").unwrap().to_utc();
    let clock = ManualClock::new(start);
    let stores = open(&db.path().join("kernel.db"), Arc::new(clock.clone())).await;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        ["Woke at nine.", "The expiry ran me.", "The report is ready."].map(ScriptedResponse::text),
    ));
    let control = GovernanceControl::new();
    let agent = LlmAgentBuilder::new("agent").model(Arc::clone(&model) as Arc<dyn Llm>);
    let kernel = kernel("k2", &stores, &control, Arc::new(clock.clone()), 16)
        .agent(TARGET, runner(&stores, &control, agent))
        .build()
        .unwrap();
    let report_at = start + chrono::Duration::minutes(90);
    let report = Schedule::new(
        "report",
        ScheduleSpec::At(report_at),
        run_on(QueueClass::Background, TARGET, "write the report"),
        MissedFirePolicy::FireOnce,
    )
    .with_next_fire_at(report_at);
    stores.schedules.upsert(report).await.unwrap();
    kernel.start().await.unwrap();

    let two_hours = start + chrono::Duration::hours(2);
    let sets = [
        WakeSet::any_of([Wake::Webhook { hook: "github".into() }]),
        WakeSet::any_of([Wake::Queue { source: "orders".into() }]),
        WakeSet::any_of([Wake::FileChange {
            root: repo.path().to_path_buf(),
            glob: "Cargo.lock".into(),
            debounce: Duration::from_millis(100),
        }]),
        WakeSet::any_of([Wake::Cron {
            expr: "0 0 2 * * *".into(),
            missed: MissedFirePolicy::Skip,
        }]),
        WakeSet::any_of([Wake::Approval { fingerprint: "sha256:held-call".into() }]),
        WakeSet::any_of([Wake::RunFinished { pid: "run-elsewhere".into() }]),
        WakeSet::any_of([Wake::BudgetReplenished { partition: PartitionId::new("default") }]),
        WakeSet::any_of([Wake::Manual]),
        WakeSet::any_of([Wake::At(two_hours)]),
        WakeSet::any_of([Wake::Webhook { hook: "linear".into() }])
            .with_expiry(two_hours, ExpiryAction::Run),
    ];
    let mut pids = Vec::new();
    for (n, set) in sets.iter().enumerate() {
        let process = kernel
            .processes()
            .spawn(
                run_on(QueueClass::Background, TARGET, &format!("sleeper {n}")),
                ProcessSpec::new(ProcessKind::Goal).with_wake(set.clone()),
            )
            .await
            .unwrap();
        pids.push(process.meta.pid);
    }
    for pid in &pids {
        in_state(&stores.runs, pid, RunState::Waiting).await;
    }

    // An hour in one-minute steps, each long enough for the maintenance tick to sweep.
    for minute in 1..=60 {
        clock.advance(Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(25)).await;
        if minute % 10 == 0 {
            let leased: Vec<_> = all_runs(&stores.runs)
                .await
                .into_iter()
                .filter(|run| run.lease_owner.is_some())
                .map(|run| run.run_id)
                .collect();
            assert_eq!(leased, Vec::<String>::new(), "no lease at minute {minute}");
        }
    }

    assert_eq!(model.requests().len(), 0, "no model call in the hour (K2)");
    let runs = all_runs(&stores.runs).await;
    assert_eq!(runs.len(), 10, "nothing but the sleepers ran: {runs:?}");
    assert!(runs.iter().all(|run| run.lease_owner.is_none() && run.state == RunState::Waiting));
    let waiting =
        kernel.processes().ps(ProcessFilter::default().states([RunState::Waiting])).await.unwrap();
    assert_eq!(waiting.len(), 10);
    for (pid, set) in pids.iter().zip(&sets) {
        let process = waiting.iter().find(|process| &process.meta.pid == pid).expect("listed");
        assert_eq!(process.meta.wake.as_ref(), Some(set), "ps shows {pid}'s wake set");
        assert_eq!((process.view.spend_today, process.view.model_calls), (0, 0));
        assert!(process.view.age >= Duration::from_secs(59 * 60), "{:?}", process.view.age);
    }

    // Past two hours on the same clock: the time wake and the expiry wake their sleepers,
    // and the schedule due at 90 minutes fires once.
    clock.advance(Duration::from_secs(61 * 60));
    in_state(&stores.runs, &pids[8], RunState::Completed).await;
    in_state(&stores.runs, &pids[9], RunState::Completed).await;
    let scheduled = eventually("the scheduled run", Duration::from_secs(10), || async {
        all_runs(&stores.runs).await.into_iter().find(|run| {
            run.idempotency_key.as_deref().is_some_and(|key| key.starts_with("schedule:report:"))
                && run.state == RunState::Completed
        })
    })
    .await;
    assert_eq!(model.requests().len(), 3);
    let process = kernel.processes().get(&scheduled.run_id).await.unwrap().unwrap();
    assert_eq!(process.meta.kind, ProcessKind::Scheduled);
    for pid in &pids[..8] {
        let run = stores.runs.get(pid).await.unwrap().unwrap();
        assert_eq!((run.state, run.lease_owner), (RunState::Waiting, None), "{pid}");
    }
    kernel.stop().await.unwrap();
}

/// K3: a saved `Cargo.lock` wakes its sleeper within two seconds, a signed webhook fires
/// a trigger, and a queue message wakes another sleeper. Each wake is one `kernel.wake`
/// run whose record holds the event and its outcome; a redelivered webhook or queue
/// message wakes nothing.
#[cfg(feature = "webhooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k3_it_wakes_on_what_matters_and_a_duplicate_wakes_nothing() {
    use adk_core::queue_source::InMemoryQueueSource;
    use adk_core::webhook::{WebhookSecret, WebhookSigner, WebhookVerifier};
    use adk_kernel::{
        KERNEL_WAKE_TARGET, Trigger, WakeOutcome, keys, queue_wake_ingress, webhook_wake_hook,
    };
    use adk_server::webhooks::{WebhookIngress, webhook_ingress_router};

    let db = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "# lockfile\n").unwrap();
    let stores = open(&db.path().join("kernel.db"), Arc::new(SystemClock)).await;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        ["The lockfile changed two crates.", "Triaged.", "Order handled."]
            .map(ScriptedResponse::text),
    ));
    let control = GovernanceControl::new();
    let agent = LlmAgentBuilder::new("agent").model(Arc::clone(&model) as Arc<dyn Llm>);
    let orders = Arc::new(InMemoryQueueSource::new("orders"));
    let triage = Trigger::new(
        "triage",
        Wake::Webhook { hook: "github".into() },
        run_on(QueueClass::Background, TARGET, "triage the delivery").with_session_id("triage"),
    );
    let kernel = kernel("k3", &stores, &control, Arc::new(SystemClock), 4)
        .agent(TARGET, runner(&stores, &control, agent))
        .trigger(triage)
        .queue_ingress(queue_wake_ingress(orders.clone(), Arc::clone(&stores.runs), APP))
        .build()
        .unwrap();
    let secret = WebhookSecret::from_bytes(b"a shared signing key of 32 bytes".to_vec()).unwrap();
    let ingress = WebhookIngress::new(Arc::clone(&stores.runs)).with_hook(webhook_wake_hook(
        "github",
        WebhookVerifier::new(secret.clone()),
        APP,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, webhook_ingress_router(ingress)).await.unwrap();
    });
    kernel.start().await.unwrap();

    let sleep_on = |text: &str, wake: Wake| {
        let spawn = kernel.processes().spawn(
            run_on(QueueClass::Background, TARGET, text),
            ProcessSpec::new(ProcessKind::Goal).with_wake(WakeSet::any_of([wake])),
        );
        async move { spawn.await.unwrap().meta.pid }
    };
    let audit = sleep_on(
        "audit the repo",
        Wake::FileChange {
            root: repo.path().to_path_buf(),
            glob: "Cargo.lock".into(),
            debounce: Duration::from_millis(100),
        },
    )
    .await;
    let order = sleep_on("handle the order", Wake::Queue { source: "orders".into() }).await;
    in_state(&stores.runs, &audit, RunState::Waiting).await;
    in_state(&stores.runs, &order, RunState::Waiting).await;
    // The file watcher subscribes once the spawn signals it.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let saved = tokio::time::Instant::now();
    std::fs::write(repo.path().join("Cargo.lock"), "# lockfile\n# two crates bumped\n").unwrap();
    in_state(&stores.runs, &audit, RunState::Completed).await;
    assert!(saved.elapsed() < Duration::from_secs(2), "woke in {:?}", saved.elapsed());

    let signer = WebhookSigner::new(secret);
    let client = reqwest::Client::new();
    let deliver = |id: &'static str| {
        let body = serde_json::to_vec(&json!({"action": "opened", "delivery": id})).unwrap();
        let signed = signer.sign_now(id, &body);
        let mut request = client.post(format!("http://{address}/hooks/github")).body(body);
        for (name, value) in signed.header_pairs() {
            request = request.header(name, value);
        }
        async move {
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (status, response.json::<Value>().await.unwrap()["duplicate"].clone())
        }
    };
    let delivered = tokio::time::Instant::now();
    assert_eq!(deliver("msg_1").await, (202, json!(false)));
    let triggered = eventually("the trigger's run", Duration::from_secs(5), || async {
        all_runs(&stores.runs).await.into_iter().find(|run| {
            run.idempotency_key.as_deref() == Some(&keys::trigger("hook:github:msg_1", "triage"))
        })
    })
    .await;
    in_state(&stores.runs, &triggered.run_id, RunState::Completed).await;
    assert!(delivered.elapsed() < Duration::from_secs(2), "ran in {:?}", delivered.elapsed());

    let published = tokio::time::Instant::now();
    orders.publish("m-1", serde_json::to_vec(&json!({"order": 42})).unwrap());
    in_state(&stores.runs, &order, RunState::Completed).await;
    assert!(published.elapsed() < Duration::from_secs(2), "woke in {:?}", published.elapsed());

    assert_eq!(deliver("msg_1").await, (200, json!(true)), "a redelivery is a duplicate");
    orders.publish("m-1", serde_json::to_vec(&json!({"order": 42})).unwrap());
    eventually("the redelivered message to be acknowledged", Duration::from_secs(5), || async {
        (orders.unacked() == 0 && orders.acked().len() == 2).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(model.requests().len(), 3, "one model turn per wake, none for a duplicate");
    let outcomes = |run: &RunRecord| -> Vec<WakeOutcome> {
        let result = run.result.as_ref().expect("a finished wake run records its outcomes");
        serde_json::from_value(result["outcomes"].clone()).unwrap()
    };
    let hook = wake_runs(&stores.runs, "hook:github:").await;
    assert_eq!(hook.len(), 1, "one wake run per delivery");
    assert_eq!(hook[0].input["key"], "hook:github:msg_1");
    assert_eq!(hook[0].input["payload"]["delivery"], "msg_1");
    assert_eq!(
        outcomes(&hook[0]),
        [WakeOutcome::Spawned { pid: triggered.run_id.clone(), trigger: "triage".into() }]
    );
    let queued = wake_runs(&stores.runs, "queue:orders:").await;
    assert_eq!(queued.len(), 1, "one wake run per message");
    assert_eq!(queued[0].input["payload"]["order"], 42);
    assert_eq!(outcomes(&queued[0]), [WakeOutcome::Woke { pid: order.clone() }]);
    let file = wake_runs(&stores.runs, "file:").await;
    let woke: Vec<_> = file.iter().flat_map(outcomes).collect();
    assert_eq!(woke, [WakeOutcome::Woke { pid: audit.clone() }], "one save, one wake");
    let process = kernel.processes().get(&triggered.run_id).await.unwrap().unwrap();
    assert_eq!(process.meta.kind, ProcessKind::Triggered { source: "triage".into() });
    let finished = wake_runs(&stores.runs, "run:").await;
    assert_eq!(finished.len(), 3, "each finished process publishes one run-finished wake");
    assert!(finished.iter().all(|run| outcomes(run).is_empty()), "no process waits on them");
    let mut processes: Vec<_> = all_runs(&stores.runs)
        .await
        .into_iter()
        .filter(|run| run.target != KERNEL_WAKE_TARGET)
        .map(|run| run.run_id)
        .collect();
    processes.sort();
    let mut expected = vec![audit, order, triggered.run_id];
    expected.sort();
    assert_eq!(processes, expected, "the three wakes ran three processes and nothing else");
    let wakes = wake_runs(&stores.runs, "").await;
    assert_eq!(wakes.len(), hook.len() + queued.len() + file.len() + finished.len());

    kernel.stop().await.unwrap();
    server.abort();
}

/// A numbered step of process `process`'s work, counting its executions.
struct Step {
    process: usize,
    executions: Arc<Mutex<BTreeMap<(usize, u64), usize>>>,
}

#[async_trait]
impl Tool for Step {
    fn name(&self) -> &str {
        "step"
    }

    fn description(&self) -> &str {
        "does one numbered step of the work"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let n = args["n"].as_u64().unwrap_or_default();
        *self.executions.lock().unwrap().entry((self.process, n)).or_default() += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(json!({ "step": n, "status": "done" }))
    }
}

/// K9: two kernel replicas over one SQLite file run six processes, three each. A freeze on
/// one replica parks all six `Waiting` within a second on both; nothing runs while frozen;
/// the unfreeze resumes them to completion, and the action ledger shows every step once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn k9_one_switch_stops_everything_on_both_replicas() {
    const PROCESSES: usize = 6;
    const STEPS: u64 = 30;
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("kernel.db");
    let replicas =
        [open(&path, Arc::new(SystemClock)).await, open(&path, Arc::new(SystemClock)).await];
    let executions = Arc::new(Mutex::new(BTreeMap::new()));
    let models: Vec<Arc<ScriptedLlm>> = (0..PROCESSES)
        .map(|i| {
            let mut script: Vec<ScriptedResponse> = (1..=STEPS)
                .map(|n| ScriptedResponse::function_call("step", json!({ "n": n })))
                .collect();
            script.push(ScriptedResponse::text("All steps done."));
            Arc::new(ScriptedLlm::new(format!("process-{i}"), script))
        })
        .collect();
    let mut kernels = Vec::new();
    for (name, stores) in ["replica-a", "replica-b"].into_iter().zip(&replicas) {
        let control = KernelConfig::mirrored_governance(Arc::clone(&stores.governance) as _);
        let mut builder = kernel(name, stores, &control, Arc::new(SystemClock), 3);
        for (i, model) in models.iter().enumerate() {
            let agent = LlmAgentBuilder::new("agent")
                .model(Arc::clone(model) as Arc<dyn Llm>)
                .tool(Arc::new(Step { process: i, executions: Arc::clone(&executions) }));
            builder = builder.agent(format!("process-{i}"), runner(stores, &control, agent));
        }
        kernels.push(builder.build().unwrap());
    }
    let mut events: Vec<_> = kernels.iter().map(Kernel::events).collect();
    for kernel in &kernels {
        kernel.start().await.unwrap();
    }

    let mut pids = Vec::new();
    for i in 0..PROCESSES {
        let target = format!("process-{i}");
        let run = run_on(QueueClass::Background, &target, "work through the steps")
            .with_session_id(target.clone());
        let process = kernels[i % 2]
            .processes()
            .spawn(run, ProcessSpec::new(ProcessKind::Goal))
            .await
            .unwrap();
        pids.push(process.meta.pid);
    }
    let runs = &replicas[0].runs;
    // Three slots per replica, so each runs three of the six.
    let owners = eventually("six running processes", Duration::from_secs(10), || async {
        let mut owners = BTreeMap::<String, usize>::new();
        for pid in &pids {
            let run = runs.get(pid).await.ok().flatten()?;
            (run.state == RunState::Running).then_some(())?;
            *owners.entry(run.lease_owner?).or_default() += 1;
        }
        let stepped = executions.lock().unwrap().keys().filter(|(_, n)| *n == 2).count();
        (stepped == PROCESSES).then_some(owners)
    })
    .await;
    assert_eq!(
        owners,
        BTreeMap::from([("replica-a:background".into(), 3), ("replica-b:background".into(), 3)])
    );

    let frozen = tokio::time::Instant::now();
    kernels[0].freeze("incident 9", operator()).await.unwrap();
    eventually("six freeze-parked processes", Duration::from_secs(5), || async {
        for pid in &pids {
            let run = runs.get(pid).await.ok().flatten()?;
            (run.state == RunState::Waiting && run.lease_owner.is_none()).then_some(())?;
            let meta = replicas[1].processes.get(pid).await.ok().flatten()?;
            (meta.suspended_by == Some(Suspension::Freeze)).then_some(())?;
        }
        Some(())
    })
    .await;
    assert!(frozen.elapsed() < Duration::from_secs(1), "parked in {:?}", frozen.elapsed());
    for (replica, events) in events.iter_mut().enumerate() {
        let mut parked = 0;
        while let Ok(Some(record)) =
            tokio::time::timeout(Duration::from_millis(100), events.recv()).await
        {
            if let KernelEvent::Process { to: RunState::Waiting, reason: Some(reason), .. } =
                record.event
                && reason == "frozen"
            {
                parked += 1;
            }
        }
        assert_eq!(parked, 3, "replica {replica} parked its own three");
    }
    let still = (
        executions.lock().unwrap().clone(),
        models.iter().map(|m| m.requests().len()).sum::<usize>(),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = (
        executions.lock().unwrap().clone(),
        models.iter().map(|m| m.requests().len()).sum::<usize>(),
    );
    assert_eq!(still, after, "no tool or model call while frozen");

    kernels[0].unfreeze(operator()).await.unwrap();
    for pid in &pids {
        in_state(runs, pid, RunState::Completed).await;
    }
    let executions = executions.lock().unwrap().clone();
    let twice: Vec<_> = executions.iter().filter(|(_, count)| **count != 1).collect();
    assert_eq!(twice, Vec::<(&(usize, u64), &usize)>::new(), "no step executed twice");
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT idempotency_key, args_digest, outcome FROM adk_action_ledger")
            .fetch_all(&replicas[0].pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), executions.len(), "one ledger record per execution");
    let mut per_call = BTreeMap::<(String, String), usize>::new();
    for (key, digest, outcome) in &rows {
        assert!(outcome.is_some(), "{key} recorded its outcome");
        let session = key.split('/').nth(2).unwrap_or_default().to_string();
        *per_call.entry((session, digest.clone())).or_default() += 1;
    }
    assert!(per_call.values().all(|count| *count == 1), "each step once: {per_call:?}");
    for kernel in &kernels {
        kernel.stop().await.unwrap();
    }
}

/// A step that returns only once `control` reports the freeze, so the run itself is the
/// first on its replica to meet the freeze, at its next boundary.
struct FreezeGate {
    process: usize,
    control: GovernanceControl,
    entered: Arc<AtomicUsize>,
    executions: Arc<Mutex<BTreeMap<(usize, u64), usize>>>,
}

#[async_trait]
impl Tool for FreezeGate {
    fn name(&self) -> &str {
        "gate"
    }

    fn description(&self) -> &str {
        "does the first step of the work"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let n = args["n"].as_u64().unwrap_or_default();
        *self.executions.lock().unwrap().entry((self.process, n)).or_default() += 1;
        self.entered.fetch_add(1, Ordering::SeqCst);
        let started = tokio::time::Instant::now();
        while !self.control.is_frozen() {
            if started.elapsed() > Duration::from_secs(10) {
                return Err(adk_core::AdkError::tool("the freeze never reached this replica"));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(json!({ "step": n, "status": "done" }))
    }
}

/// K9's race on the second replica: replica A freezes while replica B's two runs are inside
/// a tool call, and each run meets the freeze through its own check before anything else on
/// B acts on it — one at the next call of its batch (the tool-call boundary, answered `not
/// run`), one at its next model call. Both park `Waiting` under the freeze instead of
/// failing `governance.frozen`, and the unfreeze resumes both, every step run once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k9_a_run_that_meets_the_freeze_itself_parks_on_the_second_replica() {
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("kernel.db");
    let replicas =
        [open(&path, Arc::new(SystemClock)).await, open(&path, Arc::new(SystemClock)).await];
    let controls = replicas
        .each_ref()
        .map(|stores| KernelConfig::mirrored_governance(Arc::clone(&stores.governance) as _));
    let executions = Arc::new(Mutex::new(BTreeMap::new()));
    let entered = Arc::new(AtomicUsize::new(0));
    let scripts = [
        // Meets the freeze at step 2, the second call of its batch.
        vec![
            ScriptedResponse::function_calls([
                ("gate", json!({ "n": 1 })),
                ("step", json!({ "n": 2 })),
            ]),
            ScriptedResponse::function_call("step", json!({ "n": 2 })),
            ScriptedResponse::text("Both steps done."),
        ],
        // Meets the freeze at its next model call.
        vec![
            ScriptedResponse::function_call("gate", json!({ "n": 1 })),
            ScriptedResponse::text("The step is done."),
        ],
    ];
    let models: Vec<Arc<ScriptedLlm>> = scripts
        .into_iter()
        .enumerate()
        .map(|(i, script)| Arc::new(ScriptedLlm::new(format!("process-{i}"), script)))
        .collect();
    // Replica A serves only the interactive queue, so replica B runs both processes.
    let freezer = kernel("replica-a", &replicas[0], &controls[0], Arc::new(SystemClock), 2)
        .config(
            config("replica-a", &controls[0], Arc::new(SystemClock), 2)
                .with_queues([QueueClass::Interactive]),
        )
        .build()
        .unwrap();
    let mut builder = kernel("replica-b", &replicas[1], &controls[1], Arc::new(SystemClock), 2);
    for (i, model) in models.iter().enumerate() {
        let agent = LlmAgentBuilder::new("agent")
            .model(Arc::clone(model) as Arc<dyn Llm>)
            .tool(Arc::new(FreezeGate {
                process: i,
                control: controls[1].clone(),
                entered: Arc::clone(&entered),
                executions: Arc::clone(&executions),
            }))
            .tool(Arc::new(Step { process: i, executions: Arc::clone(&executions) }));
        builder = builder.agent(format!("process-{i}"), runner(&replicas[1], &controls[1], agent));
    }
    let replica_b = builder.build().unwrap();
    let mut events = replica_b.events();
    freezer.start().await.unwrap();
    replica_b.start().await.unwrap();

    let mut pids = Vec::new();
    for i in 0..models.len() {
        let target = format!("process-{i}");
        let run = run_on(QueueClass::Background, &target, "work through the steps")
            .with_session_id(target.clone());
        let process =
            replica_b.processes().spawn(run, ProcessSpec::new(ProcessKind::Goal)).await.unwrap();
        pids.push(process.meta.pid);
    }
    eventually("both runs inside their first step", Duration::from_secs(10), || async {
        (entered.load(Ordering::SeqCst) == pids.len()).then_some(())
    })
    .await;

    freezer.freeze("incident 9", operator()).await.unwrap();
    let runs = &replicas[1].runs;
    eventually("both runs freeze-parked", Duration::from_secs(5), || async {
        for pid in &pids {
            let run = runs.get(pid).await.ok().flatten()?;
            assert_ne!(run.state, RunState::Failed, "{pid} failed: {:?}", run.error);
            (run.state == RunState::Waiting && run.lease_owner.is_none()).then_some(())?;
            let meta = replicas[1].processes.get(pid).await.ok().flatten()?;
            (meta.suspended_by == Some(Suspension::Freeze)).then_some(())?;
        }
        Some(())
    })
    .await;
    assert_eq!(
        executions.lock().unwrap().clone(),
        BTreeMap::from([((0, 1), 1), ((1, 1), 1)]),
        "step 2 of the batch was answered `not run`"
    );
    let calls: Vec<usize> = models.iter().map(|model| model.requests().len()).collect();
    assert_eq!(calls, [1, 1], "no model call met the freeze");
    let session = replicas[1]
        .sessions
        .get(GetRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: "process-0".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let refused = session.events().all().iter().any(|event| {
        event.llm_response.content.iter().flat_map(|content| &content.parts).any(|part| {
            matches!(part, Part::FunctionResponse { function_response, .. }
                if function_response.name == "step"
                    && function_response.response.to_string().contains("not run"))
        })
    });
    assert!(refused, "step 2 was answered `not run` at the tool-call boundary");

    freezer.unfreeze(operator()).await.unwrap();
    for pid in &pids {
        in_state(runs, pid, RunState::Completed).await;
    }
    assert_eq!(
        executions.lock().unwrap().clone(),
        BTreeMap::from([((0, 1), 1), ((0, 2), 1), ((1, 1), 1)]),
        "every step once"
    );
    let mut parked = 0;
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(100), events.recv()).await
    {
        match record.event {
            KernelEvent::Process { to: RunState::Waiting, reason: Some(reason), .. }
                if reason == "frozen" =>
            {
                parked += 1;
            }
            KernelEvent::Process { to: RunState::Failed, .. } => {
                panic!("a process failed: {record:?}")
            }
            _ => {}
        }
    }
    assert_eq!(parked, 2, "replica B parked both under the freeze");
    replica_b.stop().await.unwrap();
    freezer.stop().await.unwrap();
}

const CHAOS_DIR: &str = "ADK_KERNEL_CHAOS_DIR";
const CHAOS_LIFE: &str = "ADK_KERNEL_CHAOS_LIFE";
const CHAOS_PID: &str = "ADK_KERNEL_CHAOS_PID";

/// A step that logs `<n> <life>` as it starts; in the first life, step 2 never returns.
struct LoggedStep {
    log: PathBuf,
    life: String,
}

#[async_trait]
impl Tool for LoggedStep {
    fn name(&self) -> &str {
        "step"
    }

    fn description(&self) -> &str {
        "does one numbered step of the work"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        use std::io::Write;
        let n = args["n"].as_u64().unwrap_or_default();
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|error| adk_core::AdkError::tool(format!("step log: {error}")))?;
        writeln!(log, "{n} {}", self.life)
            .and_then(|()| log.sync_all())
            .map_err(|error| adk_core::AdkError::tool(format!("step log: {error}")))?;
        if self.life == "first" && n == 2 {
            // The parent kills this process here.
            std::future::pending::<()>().await;
        }
        Ok(json!({ "step": n, "status": "done" }))
    }
}

/// The kernel process the chaos test starts and kills: a kernel over the database in
/// `ADK_KERNEL_CHAOS_DIR` that runs until process `ADK_KERNEL_CHAOS_PID` completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the kernel process of chaos_a_killed_kernel_resumes_from_its_checkpoint; does nothing alone"]
async fn chaos_kernel_process() {
    let Ok(dir) = std::env::var(CHAOS_DIR) else { return };
    let dir = PathBuf::from(dir);
    let life = std::env::var(CHAOS_LIFE).unwrap();
    let pid = std::env::var(CHAOS_PID).unwrap();
    let stores = open(&dir.join("kernel.db"), Arc::new(SystemClock)).await;
    let script = if life == "first" {
        vec![
            ScriptedResponse::function_call("step", json!({ "n": 1 })),
            ScriptedResponse::function_call("step", json!({ "n": 2 })),
        ]
    } else {
        vec![
            ScriptedResponse::function_call("step", json!({ "n": 3 })),
            ScriptedResponse::text("All three steps are done."),
        ]
    };
    let model = Arc::new(ScriptedLlm::new(life.clone(), script));
    let control = GovernanceControl::new();
    let agent = LlmAgentBuilder::new("agent")
        .model(model as Arc<dyn Llm>)
        .tool(Arc::new(LoggedStep { log: dir.join("steps.log"), life: life.clone() }));
    let kernel = kernel(&life, &stores, &control, Arc::new(SystemClock), 4)
        .agent(TARGET, runner(&stores, &control, agent))
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    in_state(&stores.runs, &pid, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

/// Starts this test binary as a kernel process in `life`.
fn kernel_process(dir: &Path, life: &str, pid: &str) -> tokio::process::Child {
    let stderr = std::fs::File::create(dir.join(format!("{life}.stderr"))).unwrap();
    tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["chaos_kernel_process", "--exact", "--ignored", "--nocapture"])
        .env(CHAOS_DIR, dir)
        .env(CHAOS_LIFE, life)
        .env(CHAOS_PID, pid)
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

/// The chaos test: a kernel process is killed with `SIGKILL` while a step runs, and a
/// restarted kernel resumes the process from its session checkpoint once the lease lapses.
/// No step runs twice: the completed step is answered from the ledger and the interrupted
/// one `outcome_unknown`, the original prompt is never sent again, and the run completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos_a_killed_kernel_resumes_from_its_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let stores = open(&dir.path().join("kernel.db"), Arc::new(SystemClock)).await;
    let pid = ProcessTable::new(Arc::clone(&stores.runs), Arc::clone(&stores.processes))
        .spawn(
            run_on(QueueClass::Interactive, TARGET, "work through three steps")
                .with_session_id("chaos"),
            ProcessSpec::new(ProcessKind::Goal),
        )
        .await
        .unwrap()
        .meta
        .pid;
    let log = dir.path().join("steps.log");
    let stderr = |life: &str| {
        std::fs::read_to_string(dir.path().join(format!("{life}.stderr"))).unwrap_or_default()
    };

    let mut first = kernel_process(dir.path(), "first", &pid);
    let started = tokio::time::Instant::now();
    while !std::fs::read_to_string(&log).unwrap_or_default().contains("2 first") {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the first kernel never reached step 2: {}",
            stderr("first")
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    first.kill().await.unwrap();
    let killed = stores.runs.get(&pid).await.unwrap().unwrap();
    assert_eq!(killed.state, RunState::Running, "the killed kernel still holds the lease");
    assert_eq!(killed.lease_owner.as_deref(), Some("first:interactive"));

    let mut second = kernel_process(dir.path(), "second", &pid);
    let status = tokio::time::timeout(Duration::from_secs(60), second.wait())
        .await
        .unwrap_or_else(|_| panic!("the restarted kernel did not finish: {}", stderr("second")))
        .unwrap();
    assert!(status.success(), "the restarted kernel failed: {}", stderr("second"));

    let run = stores.runs.get(&pid).await.unwrap().unwrap();
    assert_eq!(run.state, RunState::Completed);
    assert!(
        matches!(&run.checkpoint, Some(RunCheckpointRef::Session { session_id, .. }) if session_id == "chaos"),
        "{:?}",
        run.checkpoint
    );
    let steps = std::fs::read_to_string(&log).unwrap();
    assert_eq!(steps.lines().collect::<Vec<_>>(), ["1 first", "2 first", "3 second"]);
    let session = stores
        .sessions
        .get(GetRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: "chaos".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let events = session.events().all();
    let prompts = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .filter(|content| content.role == "user")
        .flat_map(|content| content.parts.iter())
        .filter(|part| part.text() == Some("work through three steps"))
        .count();
    assert_eq!(prompts, 1, "the resumed run never sends its prompt again");
    let responses: Vec<Value> = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 3, "{responses:?}");
    assert_eq!(responses[0]["status"], "done");
    assert!(adk_core::is_outcome_unknown(&responses[1]), "{}", responses[1]);
    assert_eq!(responses[2]["status"], "done");
    let ledger: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT args_digest, outcome FROM adk_action_ledger")
            .fetch_all(&stores.pool)
            .await
            .unwrap();
    assert_eq!(ledger.len(), 3, "one ledger record per step: {ledger:?}");
}
