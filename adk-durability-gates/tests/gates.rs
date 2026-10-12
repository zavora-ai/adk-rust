//! The Phase 2 durability gates, each run on every durable backend with separate OS
//! processes that share nothing but the backend.
//!
//! | Gate | Pass condition |
//! |------|----------------|
//! | `crash_mid_payment` | a worker SIGKILLed inside a non-idempotent charge; another process claims the run only after the lease expired, charges nothing, and completes it |
//! | `one_firing_per_schedule` | three scheduler processes; every cron, interval, and one-shot occurrence produces exactly one run |
//! | `stalled_worker` | a worker paused with SIGSTOP past its lease resumes with SIGCONT while another process owns the run; every write it attempts is refused with `run_store.stale_epoch`, and nothing is charged twice |
//! | `kill_switch` | a freeze written by one process stops new runs, model calls, and tool calls on three replica processes within three poll intervals; no schedule fires while frozen; a replica that cannot read the governance store past its staleness limit refuses new runs |
//!
//! SQLite runs by default, one directory per gate. The server gates are `#[ignore]`d and
//! read the variables the nightly `durable-stores` job sets:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//! REDIS_URL=redis://localhost:6379 \
//! MONGODB_URL='mongodb://localhost:27017/?directConnection=true' \
//! NEO4J_URI=bolt://localhost:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=conformance-pass \
//! FIRESTORE_EMULATOR_HOST=localhost:8080 \
//!   cargo nextest run -p adk-durability-gates --features all-backends --run-ignored all
//! ```
//!
//! Gates on one server share it, so they run one at a time per backend: nextest through
//! the `durability-gates-*` test groups in `.config/nextest.toml`, `cargo test` through a
//! lock per backend.

#![cfg(all(
    unix,
    any(
        feature = "sqlite",
        feature = "postgres",
        feature = "redis",
        feature = "mongodb",
        feature = "neo4j",
        feature = "firestore"
    )
))]

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use adk_core::run_store::RUN_STALE_EPOCH_CODE;
use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec, firing_idempotency_key};
use adk_core::{GovernanceControl, NewRun, Part, RunCheckpointRef, RunFilter, RunRecord, RunState};
use adk_durability_gates::{
    APP, Backend, LOOP_INPUT, Stores, USER, WriteRecord, charges, now_ms, read_lines,
};
use adk_session::GetRequest;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

const GATE_PROCESS: &str = env!("CARGO_BIN_EXE_adk-durability-gate");
/// The run lease of the payment workers.
const LEASE: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

/// A gate process, SIGKILLed when dropped so a failed gate leaves nothing running.
struct Process {
    name: String,
    child: Child,
}

impl Process {
    fn start(backend: &Backend, name: &str, role: &str, env: &[(&str, String)]) -> Self {
        let mut command = Command::new(GATE_PROCESS);
        backend.apply(&mut command);
        command.env("ADK_GATE_ROLE", role).stdout(Stdio::null()).stderr(Stdio::inherit());
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command.spawn().unwrap_or_else(|error| panic!("start {name}: {error}"));
        Self { name: name.to_string(), child }
    }

    fn pid(&self) -> String {
        self.child.id().to_string()
    }

    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([format!("-{signal}"), self.pid()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "SIG{signal} {}", self.name);
    }

    fn assert_alive(&mut self) {
        if let Some(status) = self.child.try_wait().expect("poll process") {
            panic!("{} exited early: {status:?}", self.name);
        }
    }

    fn kill(&mut self) {
        self.child.kill().expect("SIGKILL");
        self.child.wait().expect("reaped");
    }

    async fn exit(&mut self, timeout: Duration) -> ExitStatus {
        let started = tokio::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("poll process") {
                return status;
            }
            assert!(started.elapsed() < timeout, "{} did not exit within {timeout:?}", self.name);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Polls `check` until it returns a value, failing the gate after `timeout`.
async fn eventually<T, F, Fut>(what: &str, timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(started.elapsed() < timeout, "timed out after {timeout:?} waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn touch(path: &Path) {
    std::fs::write(path, b"").expect("signal file written");
}

async fn run(stores: &Stores, run_id: &str) -> RunRecord {
    stores.runs.get(run_id).await.expect("read run").expect("run exists")
}

async fn terminal(stores: &Stores, run_id: &str, timeout: Duration) -> RunRecord {
    eventually(&format!("run {run_id} to finish"), timeout, || async {
        let record = run(stores, run_id).await;
        record.state.is_terminal().then_some(record)
    })
    .await
}

async fn enqueue(stores: &Stores, run: NewRun) -> RunRecord {
    stores.runs.enqueue(run).await.expect("enqueue").record().cloned().expect("created")
}

// ---------------------------------------------------------------------------
// Payment gates
// ---------------------------------------------------------------------------

struct PaymentProcess<'a> {
    name: &'a str,
    after_charge: String,
    hold: Option<&'a Path>,
    writes: &'a Path,
    /// Parks the first heartbeat: `(reached, resume)`.
    stall: Option<(&'a Path, &'a Path)>,
}

struct PaymentGate {
    backend: Backend,
    stores: Stores,
    queue: String,
    session_id: String,
    run: RunRecord,
    charge_log: PathBuf,
}

impl PaymentGate {
    async fn new(backend: Backend, dir: &Path) -> Self {
        let stores = backend.open().await;
        let queue = unique("payments");
        let session_id = unique("order");
        let run = enqueue(
            &stores,
            NewRun::new(&queue, APP, USER, "billing", json!("charge my card"))
                .with_session_id(&session_id),
        )
        .await;
        Self { backend, stores, queue, session_id, run, charge_log: dir.join("charges.log") }
    }

    fn worker(&self, spec: PaymentProcess<'_>) -> Process {
        let mut env = vec![
            ("ADK_GATE_QUEUE", self.queue.clone()),
            ("ADK_GATE_RUN", self.run.run_id.clone()),
            ("ADK_GATE_WORKER", spec.name.to_string()),
            ("ADK_GATE_CHARGES", self.charge_log.display().to_string()),
            ("ADK_GATE_AFTER_CHARGE", spec.after_charge),
            ("ADK_GATE_WRITES", spec.writes.display().to_string()),
            ("ADK_GATE_LEASE_MS", LEASE.as_millis().to_string()),
        ];
        if let Some(hold) = spec.hold {
            env.push(("ADK_GATE_HOLD", hold.display().to_string()));
        }
        if let Some((reached, resume)) = spec.stall {
            env.push(("ADK_GATE_STALL_REACHED", reached.display().to_string()));
            env.push(("ADK_GATE_STALL_RESUME", resume.display().to_string()));
        }
        Process::start(&self.backend, spec.name, "payment-worker", &env)
    }

    async fn wait_for_charge(&self, process: &mut Process) {
        eventually("the first process to charge", Duration::from_secs(60), || {
            process.assert_alive();
            std::future::ready((charges(&self.charge_log) > 0).then_some(()))
        })
        .await;
    }

    /// The responses to the charge call in the session, across every process.
    async fn charge_responses(&self) -> Vec<Value> {
        let session = self
            .stores
            .sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: self.session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .expect("session");
        session
            .events()
            .all()
            .iter()
            .filter_map(|event| event.llm_response.content.clone())
            .flat_map(|content| content.parts)
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. }
                    if function_response.name == "charge_card" =>
                {
                    Some(function_response.response)
                }
                _ => None,
            })
            .collect()
    }

    /// The run finished on its second attempt with exactly one charge, whose outcome the
    /// recovering process reported as unknown rather than charging again.
    async fn assert_charged_once(&self) {
        let finished = run(&self.stores, &self.run.run_id).await;
        assert_eq!(finished.state, RunState::Completed, "{finished:?}");
        assert_eq!(finished.attempt, 2, "recovered at the next attempt");
        assert_eq!(
            finished.result.as_ref().expect("result")["text"],
            "charge status: outcome_unknown"
        );
        assert_eq!(charges(&self.charge_log), 1, "exactly one charge across every process");
        let responses = self.charge_responses().await;
        assert_eq!(responses.len(), 1, "the stopped call has one response: {responses:?}");
        assert!(adk_core::is_outcome_unknown(&responses[0]), "{responses:?}");
    }
}

/// Gate: a worker SIGKILLed during a non-idempotent charge; another process takes the
/// run only after the lease expired; exactly one charge; the run finishes.
async fn crash_mid_payment(backend: Backend, dir: PathBuf) {
    let gate = PaymentGate::new(backend, &dir).await;
    let mut a = gate.worker(PaymentProcess {
        name: "process-a",
        after_charge: "hang".into(),
        hold: None,
        writes: &dir.join("writes-a.log"),
        stall: None,
    });
    gate.wait_for_charge(&mut a).await;
    a.kill();

    let stopped = run(&gate.stores, &gate.run.run_id).await;
    assert_eq!(stopped.state, RunState::Running, "the killed process wrote no outcome");
    assert!(matches!(stopped.checkpoint, Some(RunCheckpointRef::Session { .. })), "{stopped:?}");
    let expires = stopped.lease_expires_at.expect("a running run has a lease");

    let b_writes = dir.join("writes-b.log");
    let mut b = gate.worker(PaymentProcess {
        name: "process-b",
        after_charge: "return".into(),
        hold: None,
        writes: &b_writes,
        stall: None,
    });
    let status = b.exit(Duration::from_secs(90)).await;
    assert!(status.success(), "process b failed: {status:?}");

    gate.assert_charged_once().await;
    let claim = WriteRecord::read_all(&b_writes)
        .into_iter()
        .find(|write| write.op == "claim")
        .expect("process b claimed the run");
    assert_eq!(claim.epoch, stopped.lease_epoch + 1, "the recovery claim fences the old epoch");
    // The store reads its clock inside the call, after the recorded start.
    assert!(
        claim.at_ms >= expires.timestamp_millis() - 250,
        "process b claimed at {} before the lease expired at {expires}",
        claim.at_ms
    );
}

/// Gate: a worker paused past its lease resumes while another process owns the run and
/// tries to write; the write is refused by epoch and nothing is charged twice.
///
/// Process a charges, and the call never returns. The pause lands at a's first
/// heartbeat: the test SIGSTOPs it there, waits out the lease, and lets process b
/// recover the run and hold it. a then resumes with SIGCONT and sends that heartbeat
/// with the epoch of the lease it lost.
async fn stalled_worker(backend: Backend, dir: PathBuf) {
    let gate = PaymentGate::new(backend, &dir).await;
    let reached = dir.join("heartbeat-reached");
    let resume = dir.join("heartbeat-resume");
    let release_b = dir.join("release-b");
    let a_writes = dir.join("writes-a.log");
    let b_writes = dir.join("writes-b.log");

    let mut a = gate.worker(PaymentProcess {
        name: "process-a",
        after_charge: "hang".into(),
        hold: None,
        writes: &a_writes,
        stall: Some((&reached, &resume)),
    });
    gate.wait_for_charge(&mut a).await;
    eventually("process a to reach its heartbeat", Duration::from_secs(60), || {
        a.assert_alive();
        std::future::ready(reached.exists().then_some(()))
    })
    .await;
    a.signal("STOP");
    // Seen by the parked heartbeat only once the process runs again.
    touch(&resume);

    let stalled = run(&gate.stores, &gate.run.run_id).await;
    assert_eq!(stalled.state, RunState::Running);
    assert_eq!(stalled.lease_owner.as_deref(), Some("process-a"));
    let expires = stalled.lease_expires_at.expect("a running run has a lease");
    let stalled_epoch = stalled.lease_epoch;
    let past_lease = (expires.timestamp_millis() - now_ms() + 200).max(0);
    tokio::time::sleep(Duration::from_millis(past_lease as u64)).await;

    // Process b recovers the run and holds it: its answer waits for `release_b`.
    let mut b = gate.worker(PaymentProcess {
        name: "process-b",
        after_charge: "return".into(),
        hold: Some(&release_b),
        writes: &b_writes,
        stall: None,
    });
    eventually("process b to recover the run", Duration::from_secs(60), || {
        b.assert_alive();
        let b_writes = &b_writes;
        async move {
            WriteRecord::read_all(b_writes)
                .iter()
                .any(|write| write.op == "set_checkpoint" && write.outcome == "ok")
                .then_some(())
        }
    })
    .await;
    let recovered = run(&gate.stores, &gate.run.run_id).await;
    assert_eq!(
        (recovered.lease_owner.as_deref(), recovered.lease_epoch),
        (Some("process-b"), stalled_epoch + 1),
        "{recovered:?}"
    );

    let before_resume = WriteRecord::read_all(&a_writes).len();
    a.signal("CONT");
    eventually("the resumed process to write", Duration::from_secs(30), || {
        std::future::ready((WriteRecord::read_all(&a_writes).len() > before_resume).then_some(()))
    })
    .await;
    // Let the resumed process do whatever else it would before b finishes.
    tokio::time::sleep(LEASE).await;
    let stale_writes = WriteRecord::read_all(&a_writes)[before_resume..].to_vec();
    assert!(
        stale_writes.iter().all(|write| write.outcome != "ok"),
        "a write from the stalled process succeeded after recovery: {stale_writes:?}"
    );
    assert!(
        stale_writes
            .iter()
            .any(|write| write.epoch == stalled_epoch && write.outcome == RUN_STALE_EPOCH_CODE),
        "the stalled process's write was not refused by epoch: {stale_writes:?}"
    );
    let owner = run(&gate.stores, &gate.run.run_id).await;
    assert_eq!(
        (owner.state, owner.lease_owner.as_deref(), owner.lease_epoch),
        (RunState::Running, Some("process-b"), stalled_epoch + 1),
        "the refused write left the run with b"
    );
    assert_eq!(charges(&gate.charge_log), 1, "the resumed process charged nothing more");

    touch(&release_b);
    let status = b.exit(Duration::from_secs(60)).await;
    assert!(status.success(), "process b failed: {status:?}");
    let status = a.exit(Duration::from_secs(30)).await;
    assert!(status.success(), "process a failed: {status:?}");

    gate.assert_charged_once().await;
    let b_finish = WriteRecord::read_all(&b_writes)
        .into_iter()
        .rfind(|write| write.op == "finish")
        .expect("process b finished the run");
    assert_eq!((b_finish.epoch, b_finish.outcome.as_str()), (stalled_epoch + 1, "ok"));
    let a_after = WriteRecord::read_all(&a_writes)[before_resume..].to_vec();
    assert!(
        a_after.iter().all(|write| write.outcome != "ok"),
        "the stalled process wrote after b finished: {a_after:?}"
    );
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

const SCHEDULE_PREFIX: &str = "gate-";

/// Deletes the schedules an earlier, interrupted gate left on a shared server.
async fn delete_gate_schedules(stores: &Stores) {
    for schedule in stores.schedules.list().await.expect("list schedules") {
        if schedule.id.starts_with(SCHEDULE_PREFIX) {
            stores.schedules.delete(&schedule.id).await.expect("delete schedule");
        }
    }
}

fn whole_ms(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(at.timestamp_millis()).expect("in range")
}

/// The schedule's occurrences from its first fire time through `until`.
fn occurrences(
    spec: &ScheduleSpec,
    first: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let mut times = vec![first];
    while let Some(next) = spec.next_after(*times.last().expect("one")).expect("plannable")
        && next <= until
    {
        times.push(next);
    }
    times
}

/// Gate: three scheduler processes; every cron, interval, and one-shot occurrence
/// produces exactly one run, and the processes together report creating each run once.
async fn one_firing_per_schedule(backend: Backend, dir: PathBuf) {
    const INTERVAL: Duration = Duration::from_millis(250);
    const RACE: Duration = Duration::from_millis(4000);
    const MIN_INTERVAL_FIRINGS: usize = 12;
    const MIN_CRON_FIRINGS: usize = 3;
    let stores = backend.open().await;
    delete_gate_schedules(&stores).await;

    let start = whole_ms(Utc::now()) + chrono::Duration::milliseconds(500);
    let template =
        |id: &str| NewRun::new(format!("fired/{id}"), APP, USER, "probe", json!({ "id": id }));
    let mut schedules = Vec::new();
    let every = unique("gate-every");
    schedules.push(
        Schedule::new(
            &every,
            ScheduleSpec::Every(INTERVAL),
            template(&every),
            MissedFirePolicy::CatchUp(1000),
        )
        .with_next_fire_at(start),
    );
    let cron = unique("gate-cron");
    let cron_spec = ScheduleSpec::Cron { expr: "* * * * * *".into(), timezone: "UTC".into() };
    let first_second = cron_spec.next_after(start).expect("cron").expect("next second");
    schedules.push(
        Schedule::new(&cron, cron_spec, template(&cron), MissedFirePolicy::CatchUp(1000))
            .with_next_fire_at(first_second),
    );
    let one_shots: Vec<String> = (0..5).map(|_| unique("gate-at")).collect();
    for (k, id) in one_shots.iter().enumerate() {
        let at = start + chrono::Duration::milliseconds(300 * k as i64);
        schedules.push(
            Schedule::new(id, ScheduleSpec::At(at), template(id), MissedFirePolicy::FireOnce)
                .with_next_fire_at(at),
        );
    }
    for schedule in &schedules {
        stores.schedules.upsert(schedule.clone()).await.expect("upsert schedule");
    }

    let stop = dir.join("stop");
    let mut processes: Vec<Process> = (0..3)
        .map(|i| {
            Process::start(
                &backend,
                &format!("scheduler-{i}"),
                "scheduler",
                &[
                    ("ADK_GATE_STOP", stop.display().to_string()),
                    ("ADK_GATE_TICK_MS", "10".to_string()),
                    (
                        "ADK_GATE_REPORT",
                        dir.join(format!("scheduler-{i}.report")).display().to_string(),
                    ),
                ],
            )
        })
        .collect();
    let every_queue = format!("fired/{every}");
    eventually("the first firing", Duration::from_secs(60), || async {
        let fired = stores.runs.list(RunFilter::default().queue(&every_queue)).await.expect("list");
        (!fired.is_empty()).then_some(())
    })
    .await;
    // The race runs for at least RACE and until the recurring schedules fired enough
    // contended occurrences; the Firestore emulator's lock waits make it slower there.
    let cron_queue = format!("fired/{cron}");
    tokio::time::sleep(RACE).await;
    eventually("enough contended firings", Duration::from_secs(120), || async {
        let every = stores.runs.list(RunFilter::default().queue(&every_queue)).await.unwrap();
        let cron = stores.runs.list(RunFilter::default().queue(&cron_queue)).await.unwrap();
        (every.len() >= MIN_INTERVAL_FIRINGS && cron.len() >= MIN_CRON_FIRINGS).then_some(())
    })
    .await;
    for process in &mut processes {
        process.assert_alive();
    }
    touch(&stop);
    for process in &mut processes {
        let status = process.exit(Duration::from_secs(30)).await;
        assert!(status.success(), "{} failed: {status:?}", process.name);
    }
    let stopped_at = Utc::now();

    let mut total = 0;
    for schedule in &schedules {
        let stored = stores.schedules.get(&schedule.id).await.expect("read").expect("schedule");
        let first = schedule.next_fire_at.expect("first fire time");
        let grid: Vec<DateTime<Utc>> = occurrences(&schedule.spec, first, stopped_at);
        let fired = stores
            .runs
            .list(RunFilter::default().queue(format!("fired/{}", schedule.id)))
            .await
            .expect("list runs");
        let keys: Vec<String> =
            fired.iter().map(|run| run.idempotency_key.clone().expect("keyed")).collect();
        let distinct: BTreeSet<&String> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{}: one run per occurrence: {keys:?}", schedule.id);

        let on_grid: BTreeSet<String> =
            grid.iter().map(|at| firing_idempotency_key(&schedule.id, *at)).collect();
        for key in &distinct {
            assert!(
                on_grid.contains(*key),
                "{}: run for a time that is no occurrence: {key}",
                schedule.id
            );
        }
        // Every occurrence before the cursor fired; one past it may have fired from a
        // replica that lost the advance race, and is then present once.
        let cursor = stored.next_fire_at;
        let due: Vec<String> = grid
            .iter()
            .filter(|at| cursor.is_none_or(|cursor| **at < cursor))
            .map(|at| firing_idempotency_key(&schedule.id, *at))
            .collect();
        for key in &due {
            assert!(distinct.contains(key), "{}: occurrence never fired: {key}", schedule.id);
        }
        if matches!(schedule.spec, ScheduleSpec::At(_)) {
            assert_eq!(cursor, None, "{}: a one-shot schedule is spent", schedule.id);
            assert_eq!(keys.len(), 1, "{}", schedule.id);
        }
        total += keys.len();
    }
    let reports: Vec<HashMap<String, usize>> = (0..3)
        .map(|i| {
            let line = read_lines(&dir.join(format!("scheduler-{i}.report")))
                .pop()
                .unwrap_or_else(|| panic!("scheduler-{i} wrote no report"));
            line.split(' ')
                .map(|field| {
                    let (key, value) = field.split_once('=').expect("key=value");
                    (key.to_string(), value.parse().expect("count"))
                })
                .collect()
        })
        .collect();
    // Every process raced for firings. Tick counts vary by backend: the Firestore
    // emulator holds contended transactions on lock waits of up to seconds.
    for (i, report) in reports.iter().enumerate() {
        let fired = report["enqueued"] + report["existing"] + report["lost"];
        assert!(fired >= 1, "scheduler-{i} took no part in firing: {report:?}");
    }
    let created: usize = reports.iter().map(|report| report["enqueued"]).sum();
    assert_eq!(created, total, "the processes created each run once: {reports:?}");
    delete_gate_schedules(&stores).await;
}

// ---------------------------------------------------------------------------
// Kill switch
// ---------------------------------------------------------------------------

/// How often each replica reads the governance store.
const POLL: Duration = Duration::from_millis(100);
/// How long a replica runs without a successful read before it refuses work.
const STALENESS: Duration = Duration::from_millis(1000);
/// How long a freeze may take to reach every replica: three poll intervals.
const FREEZE_BOUND_MS: i64 = 3 * POLL.as_millis() as i64;
/// How late a call may start after its replica recorded the freeze: the status probe's
/// period plus scheduling slack.
const CALL_SLACK_MS: i64 = 50;

fn statuses(dir: &Path, replica: usize) -> Vec<(String, i64)> {
    read_lines(&dir.join(format!("status-{replica}.log")))
        .iter()
        .map(|line| {
            let (state, at) = line.split_once(' ').expect("state and time");
            (state.to_string(), at.parse().expect("time"))
        })
        .collect()
}

/// When `replica` first reported `state` at or after `since`.
fn reported(dir: &Path, replica: usize, state: &str, since: i64) -> Option<i64> {
    statuses(dir, replica).into_iter().find(|(s, at)| s == state && *at >= since).map(|(_, at)| at)
}

fn calls(dir: &Path, replica: usize) -> Vec<(String, i64)> {
    read_lines(&dir.join(format!("calls-{replica}.log")))
        .iter()
        .map(|line| {
            let (kind, at) = line.split_once(' ').expect("kind and time");
            (kind.to_string(), at.parse().expect("time"))
        })
        .collect()
}

/// Gate: a freeze written by one process stops new runs, model calls, and tool calls on
/// every replica within the bound; no schedule fires while frozen; a replica that cannot
/// read the governance store past its staleness limit refuses new runs.
async fn kill_switch(backend: Backend, dir: PathBuf) {
    const REPLICAS: usize = 3;
    let stores = backend.open().await;
    delete_gate_schedules(&stores).await;
    let tenant = unique("tenant");
    let queue = unique("replicas");
    let control = GovernanceControl::mirrored(
        stores.governance.clone(),
        adk_core::governance::GovernanceMirrorConfig::default()
            .with_scopes([adk_core::governance_state::GovernanceScope::Tenant(tenant.clone())]),
    );
    control.refresh().await.expect("read the governance store");

    let replicas: Vec<Process> = (0..REPLICAS)
        .map(|i| {
            Process::start(
                &backend,
                &format!("replica-{i}"),
                "replica",
                &[
                    ("ADK_GATE_INDEX", i.to_string()),
                    ("ADK_GATE_DIR", dir.display().to_string()),
                    ("ADK_GATE_QUEUE", queue.clone()),
                    ("ADK_GATE_TENANT", tenant.clone()),
                    ("ADK_GATE_POLL_MS", POLL.as_millis().to_string()),
                    ("ADK_GATE_STALENESS_MS", STALENESS.as_millis().to_string()),
                ],
            )
        })
        .collect();
    for i in 0..REPLICAS {
        eventually(&format!("replica-{i} to read the store"), Duration::from_secs(60), || {
            std::future::ready(reported(&dir, i, "ok", 0))
        })
        .await;
    }

    // One looping agent run per replica, each calling the model and a tool continuously.
    let mut looping = Vec::new();
    for i in 0..REPLICAS {
        let record = enqueue(
            &stores,
            NewRun::new(format!("{queue}/{i}"), APP, USER, "looper", json!(LOOP_INPUT)),
        )
        .await;
        looping.push(record.run_id);
    }
    for i in 0..REPLICAS {
        eventually(&format!("replica-{i} to loop"), Duration::from_secs(60), || {
            let tools = calls(&dir, i).iter().filter(|(kind, _)| kind == "tool").count();
            std::future::ready((tools >= 3).then_some(()))
        })
        .await;
    }

    // 1. The freeze reaches every replica within the bound.
    control.freeze_replicated("durability gate", None).await.expect("freeze written");
    let written = now_ms();
    let mut frozen_at = Vec::new();
    for i in 0..REPLICAS {
        let at = eventually(&format!("replica-{i} to freeze"), Duration::from_secs(10), || {
            std::future::ready(reported(&dir, i, "frozen", 0))
        })
        .await;
        assert!(
            at - written <= FREEZE_BOUND_MS,
            "replica-{i} froze {}ms after the write, past the {FREEZE_BOUND_MS}ms bound",
            at - written
        );
        frozen_at.push(at);
    }

    // 2. While frozen: no model or tool call starts, no run is claimed, no schedule fires.
    let fired_queue = format!("{queue}/fired");
    let schedule_id = unique("gate-frozen");
    let due = whole_ms(Utc::now());
    let stored = stores
        .schedules
        .upsert(
            Schedule::new(
                &schedule_id,
                ScheduleSpec::Every(Duration::from_secs(3600)),
                NewRun::new(&fired_queue, APP, USER, "probe", json!({})),
                MissedFirePolicy::FireOnce,
            )
            .with_next_fire_at(due),
        )
        .await
        .expect("upsert schedule");
    let mut quick = Vec::new();
    for i in 0..REPLICAS {
        let record = enqueue(
            &stores,
            NewRun::new(format!("{queue}/{i}"), APP, USER, "looper", json!("quick")),
        )
        .await;
        quick.push(record.run_id);
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for (i, frozen) in frozen_at.iter().enumerate() {
        let late: Vec<_> =
            calls(&dir, i).into_iter().filter(|(_, at)| *at > frozen + CALL_SLACK_MS).collect();
        assert!(late.is_empty(), "replica-{i} called after it froze at {frozen}: {late:?}");
    }
    for run_id in &looping {
        let record = run(&stores, run_id).await;
        assert_ne!(record.state, RunState::Running, "a looping run still runs: {record:?}");
    }
    for run_id in &quick {
        let record = run(&stores, run_id).await;
        assert_eq!((record.state, record.attempt), (RunState::Queued, 0), "claimed while frozen");
    }
    assert!(
        stores.runs.list(RunFilter::default().queue(&fired_queue)).await.unwrap().is_empty(),
        "a schedule fired while frozen"
    );
    let untouched = stores.schedules.get(&schedule_id).await.unwrap().expect("schedule");
    assert_eq!(
        (untouched.next_fire_at, untouched.revision),
        (stored.next_fire_at, stored.revision),
        "a frozen scheduler advanced the schedule"
    );

    // 3. Unfrozen: the queued runs complete, and the schedule fires exactly once.
    for run_id in &looping {
        let _ = stores.runs.request_cancel(run_id).await;
    }
    control.unfreeze_replicated(None).await.expect("unfreeze written");
    let unfrozen = now_ms();
    for i in 0..REPLICAS {
        eventually(&format!("replica-{i} to unfreeze"), Duration::from_secs(10), || {
            std::future::ready(reported(&dir, i, "ok", unfrozen - FREEZE_BOUND_MS))
        })
        .await;
    }
    for run_id in &quick {
        let record = terminal(&stores, run_id, Duration::from_secs(60)).await;
        assert_eq!(record.state, RunState::Completed, "{record:?}");
    }
    eventually("the schedule to fire after the freeze", Duration::from_secs(30), || async {
        let fired = stores.runs.list(RunFilter::default().queue(&fired_queue)).await.unwrap();
        (!fired.is_empty()).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let fired = stores.runs.list(RunFilter::default().queue(&fired_queue)).await.unwrap();
    assert_eq!(fired.len(), 1, "the missed occurrence fired once: {fired:?}");

    // 4. A replica partitioned from the governance store refuses new runs once its last
    //    read is older than the staleness limit; the others keep working.
    let partitioned = REPLICAS - 1;
    touch(&dir.join(format!("partition-{partitioned}")));
    let cut = now_ms();
    let stale_at =
        eventually("the partitioned replica to go stale", Duration::from_secs(30), || {
            std::future::ready(reported(&dir, partitioned, "stale", cut))
        })
        .await;
    let staleness = STALENESS.as_millis() as i64;
    let poll = POLL.as_millis() as i64;
    assert!(
        stale_at - cut >= staleness - poll - 100 && stale_at - cut <= staleness + 200,
        "went stale {}ms after the partition; the limit is {staleness}ms",
        stale_at - cut
    );
    let refused = enqueue(
        &stores,
        NewRun::new(format!("{queue}/{partitioned}"), APP, USER, "looper", json!("quick")),
    )
    .await;
    let served =
        enqueue(&stores, NewRun::new(format!("{queue}/0"), APP, USER, "looper", json!("quick")))
            .await;
    let record = terminal(&stores, &served.run_id, Duration::from_secs(60)).await;
    assert_eq!(record.state, RunState::Completed, "a reachable replica keeps working");
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let record = run(&stores, &refused.run_id).await;
    assert_eq!((record.state, record.attempt), (RunState::Queued, 0), "a stale replica claimed");
    for i in 0..partitioned {
        let first_ok = reported(&dir, i, "ok", 0).expect("ready");
        let stale = reported(&dir, i, "stale", first_ok);
        assert_eq!(stale, None, "replica-{i} went stale without a partition");
    }

    std::fs::remove_file(dir.join(format!("partition-{partitioned}"))).expect("heal");
    let healed = now_ms();
    eventually("the healed replica to read the store", Duration::from_secs(10), || {
        std::future::ready(reported(&dir, partitioned, "ok", healed))
    })
    .await;
    let record = terminal(&stores, &refused.run_id, Duration::from_secs(60)).await;
    assert_eq!(record.state, RunState::Completed, "{record:?}");

    drop(replicas);
    delete_gate_schedules(&stores).await;
}

// ---------------------------------------------------------------------------
// The backend x gate matrix
// ---------------------------------------------------------------------------

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;

    async fn on_sqlite<F, Fut>(gate: F)
    where
        F: FnOnce(Backend, PathBuf) -> Fut,
        Fut: Future<Output = ()>,
    {
        let dir = tempfile::tempdir().expect("temp dir");
        let stores = dir.path().join("stores");
        std::fs::create_dir(&stores).expect("store dir");
        gate(Backend::sqlite(&stores), dir.path().to_path_buf()).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn crash_mid_payment() {
        on_sqlite(super::crash_mid_payment).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_firing_per_schedule() {
        on_sqlite(super::one_firing_per_schedule).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stalled_worker() {
        on_sqlite(super::stalled_worker).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kill_switch() {
        on_sqlite(super::kill_switch).await;
    }
}

/// Runs `gate` against the server for `kind`, one gate at a time per server.
#[cfg(any(
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
))]
async fn on_server<F, Fut>(
    lock: &tokio::sync::Mutex<()>,
    kind: adk_durability_gates::BackendKind,
    gate: F,
) where
    F: FnOnce(Backend, PathBuf) -> Fut,
    Fut: Future<Output = ()>,
{
    let _serial = lock.lock().await;
    let dir = tempfile::tempdir().expect("temp dir");
    gate(Backend::from_env(kind), dir.path().to_path_buf()).await;
}

macro_rules! server_gates {
    ($module:ident, $feature:literal, $kind:expr, $needs:literal) => {
        #[cfg(feature = $feature)]
        mod $module {
            use super::*;

            static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = $needs]
            async fn crash_mid_payment() {
                on_server(&SERIAL, $kind, super::crash_mid_payment).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = $needs]
            async fn one_firing_per_schedule() {
                on_server(&SERIAL, $kind, super::one_firing_per_schedule).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = $needs]
            async fn stalled_worker() {
                on_server(&SERIAL, $kind, super::stalled_worker).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = $needs]
            async fn kill_switch() {
                on_server(&SERIAL, $kind, super::kill_switch).await;
            }
        }
    };
}

server_gates!(
    postgres,
    "postgres",
    adk_durability_gates::BackendKind::Postgres,
    "requires PostgreSQL at DATABASE_URL"
);
server_gates!(
    redis,
    "redis",
    adk_durability_gates::BackendKind::Redis,
    "requires Redis at REDIS_URL"
);
server_gates!(
    mongodb,
    "mongodb",
    adk_durability_gates::BackendKind::Mongodb,
    "requires a MongoDB replica set at MONGODB_URL"
);
server_gates!(
    neo4j,
    "neo4j",
    adk_durability_gates::BackendKind::Neo4j,
    "requires Neo4j at NEO4J_URI"
);
server_gates!(
    firestore,
    "firestore",
    adk_durability_gates::BackendKind::Firestore,
    "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"
);
