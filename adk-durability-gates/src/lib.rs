//! Multi-process durability gates for the durable runtime.
//!
//! The gates in `tests/gates.rs` start the `adk-durability-gate` binary as separate OS
//! processes — workers, schedulers, and replicas — that share nothing but one backend.
//! Each gate is parameterised by a `Backend`, which opens every durable store the
//! runtime needs on that backend:
//!
//! | Store | Used by |
//! |-------|---------|
//! | `adk_core::DurableRunStore` | runs, fenced leases, recovery |
//! | `adk_session::SessionService` | agent session history |
//! | `adk_core::ActionLedger` | non-idempotent tool calls |
//! | `adk_core::ScheduleStore` | cron and timer schedules |
//! | `adk_core::GovernanceStateStore` | the replicated kill switch |
//!
//! SQLite runs on every pull request. The server-backed gates need their server and
//! run in the nightly `durable-stores` job (see `tests/gates.rs`). Without a backend
//! feature the crate is empty.

#![cfg(feature = "gates")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use adk_core::governance_state::{GovernanceScope, GovernanceState};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActionLedger, AdkError, ClaimedRun, Content, DurableRunStore, Enqueued, ErrorCategory,
    ErrorComponent, GovernanceStateStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, NewRun,
    Part, Result, RunCheckpointRef, RunFilter, RunOutcome, RunRecord, ScheduleStore, Tool,
    ToolContext, ToolEffect,
};
use adk_session::SessionService;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// The environment variable naming the backend kind of a gate process.
pub const BACKEND_ENV: &str = "ADK_GATE_BACKEND";
/// The environment variable carrying the backend location of a gate process.
pub const LOCATION_ENV: &str = "ADK_GATE_LOCATION";
/// The app every gate run belongs to.
pub const APP: &str = "durability-gates";
/// The user every gate run belongs to.
pub const USER: &str = "customer-1";
/// The MongoDB database the gates write to.
pub const MONGO_DATABASE: &str = "adk_durability_gates";

/// A durable backend the gates run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// One SQLite file per store in a directory.
    Sqlite,
    /// A PostgreSQL server.
    Postgres,
    /// A Redis server.
    Redis,
    /// A MongoDB replica set.
    Mongodb,
    /// A Neo4j server.
    Neo4j,
    /// The Firestore emulator.
    Firestore,
}

impl BackendKind {
    /// The name used in test names, environment variables, and reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::Redis => "redis",
            Self::Mongodb => "mongodb",
            Self::Neo4j => "neo4j",
            Self::Firestore => "firestore",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Self::Sqlite, Self::Postgres, Self::Redis, Self::Mongodb, Self::Neo4j, Self::Firestore]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// A backend and where it lives: a directory for SQLite, a URL for a server.
#[derive(Debug, Clone)]
pub struct Backend {
    /// The backend kind.
    pub kind: BackendKind,
    /// The SQLite directory, server URL, or Firestore project id.
    pub location: String,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

impl Backend {
    /// SQLite files in `dir`.
    pub fn sqlite(dir: &Path) -> Self {
        Self { kind: BackendKind::Sqlite, location: dir.display().to_string() }
    }

    /// The server for `kind` named by the variables the nightly job sets
    /// (`DATABASE_URL`, `REDIS_URL`, `MONGODB_URL`, `NEO4J_URI`, `FIRESTORE_PROJECT_ID`).
    pub fn from_env(kind: BackendKind) -> Self {
        let location = match kind {
            BackendKind::Sqlite => panic!("SQLite gates use a temporary directory"),
            BackendKind::Postgres => {
                env_or("DATABASE_URL", "postgres://postgres@localhost:5432/postgres")
            }
            BackendKind::Redis => env_or("REDIS_URL", "redis://localhost:6379"),
            BackendKind::Mongodb => {
                env_or("MONGODB_URL", "mongodb://localhost:27017/?directConnection=true")
            }
            BackendKind::Neo4j => env_or("NEO4J_URI", "bolt://localhost:7687"),
            BackendKind::Firestore => env_or("FIRESTORE_PROJECT_ID", "adk-durability-gates"),
        };
        Self { kind, location }
    }

    /// The backend a gate process was started for, read from its environment.
    pub fn from_process_env() -> Self {
        let kind = std::env::var(BACKEND_ENV).expect("ADK_GATE_BACKEND is set");
        let kind = BackendKind::parse(&kind).unwrap_or_else(|| panic!("unknown backend {kind}"));
        Self { kind, location: std::env::var(LOCATION_ENV).expect("ADK_GATE_LOCATION is set") }
    }

    /// Passes this backend to a gate process.
    pub fn apply(&self, command: &mut Command) {
        command.env(BACKEND_ENV, self.kind.name()).env(LOCATION_ENV, &self.location);
    }

    /// Opens every store on this backend with fresh connections, as a new process would.
    ///
    /// # Panics
    ///
    /// Panics when the backend is unreachable or its feature is not compiled in.
    pub async fn open(&self) -> Stores {
        let url = self.location.as_str();
        match self.kind {
            #[cfg(feature = "sqlite")]
            BackendKind::Sqlite => {
                let file = |name: &str| format!("sqlite:{url}/{name}.db?mode=rwc");
                let runs = adk_session::SqliteRunStore::new(&file("runs")).await.expect("runs");
                runs.migrate().await.expect("run schema");
                let sessions = adk_session::SqliteSessionService::new(&file("sessions"))
                    .await
                    .expect("sessions");
                sessions.migrate().await.expect("session schema");
                let ledger =
                    adk_session::SqliteActionLedger::new(&file("ledger")).await.expect("ledger");
                ledger.migrate().await.expect("ledger schema");
                let schedules = adk_session::SqliteScheduleStore::new(&file("schedules"))
                    .await
                    .expect("schedules");
                schedules.migrate().await.expect("schedule schema");
                let governance = adk_session::SqliteGovernanceStateStore::new(&file("governance"))
                    .await
                    .expect("governance");
                governance.migrate().await.expect("governance schema");
                Stores::new(runs, sessions, ledger, schedules, governance)
            }
            #[cfg(feature = "postgres")]
            BackendKind::Postgres => {
                let runs = adk_session::PostgresRunStore::new(url).await.expect("runs");
                runs.migrate().await.expect("run schema");
                let sessions =
                    adk_session::PostgresSessionService::new(url).await.expect("sessions");
                sessions.migrate().await.expect("session schema");
                let ledger = adk_session::PostgresActionLedger::new(url).await.expect("ledger");
                ledger.migrate().await.expect("ledger schema");
                let schedules =
                    adk_session::PostgresScheduleStore::new(url).await.expect("schedules");
                schedules.migrate().await.expect("schedule schema");
                let governance =
                    adk_session::PostgresGovernanceStateStore::new(url).await.expect("governance");
                governance.migrate().await.expect("governance schema");
                Stores::new(runs, sessions, ledger, schedules, governance)
            }
            #[cfg(feature = "redis")]
            BackendKind::Redis => {
                let runs = adk_session::RedisRunStore::new(url).await.expect("runs");
                runs.migrate().await.expect("run scripts");
                let sessions =
                    adk_session::RedisSessionService::new(adk_session::RedisSessionConfig {
                        url: url.to_string(),
                        ttl: None,
                        cluster_nodes: None,
                    })
                    .await
                    .expect("sessions");
                let ledger = adk_session::RedisActionLedger::new(url).await.expect("ledger");
                ledger.migrate().await.expect("ledger scripts");
                let schedules = adk_session::RedisScheduleStore::new(url).await.expect("schedules");
                schedules.migrate().await.expect("schedule scripts");
                let governance =
                    adk_session::RedisGovernanceStateStore::new(url).await.expect("governance");
                governance.migrate().await.expect("governance scripts");
                Stores::new(runs, sessions, ledger, schedules, governance)
            }
            #[cfg(feature = "mongodb")]
            BackendKind::Mongodb => {
                let db = MONGO_DATABASE;
                let runs = adk_session::MongoRunStore::new(url, db).await.expect("runs");
                runs.migrate().await.expect("run indexes");
                let sessions =
                    adk_session::MongoSessionService::new(url, db).await.expect("sessions");
                sessions.migrate().await.expect("session indexes");
                let ledger = adk_session::MongoActionLedger::new(url, db).await.expect("ledger");
                ledger.migrate().await.expect("ledger indexes");
                let schedules =
                    adk_session::MongoScheduleStore::new(url, db).await.expect("schedules");
                schedules.migrate().await.expect("schedule indexes");
                let governance =
                    adk_session::MongoGovernanceStateStore::new(url, db).await.expect("governance");
                governance.migrate().await.expect("governance indexes");
                Stores::new(runs, sessions, ledger, schedules, governance)
            }
            #[cfg(feature = "neo4j")]
            BackendKind::Neo4j => {
                let user = env_or("NEO4J_USER", "neo4j");
                let password = env_or("NEO4J_PASSWORD", "conformance-pass");
                let runs =
                    adk_session::Neo4jRunStore::new(url, &user, &password).await.expect("runs");
                runs.migrate().await.expect("run schema");
                let sessions = adk_session::Neo4jSessionService::new(url, &user, &password)
                    .await
                    .expect("sessions");
                sessions.migrate().await.expect("session schema");
                let ledger = adk_session::Neo4jActionLedger::new(url, &user, &password)
                    .await
                    .expect("ledger");
                ledger.migrate().await.expect("ledger schema");
                let schedules = adk_session::Neo4jScheduleStore::new(url, &user, &password)
                    .await
                    .expect("schedules");
                schedules.migrate().await.expect("schedule schema");
                let governance = adk_session::Neo4jGovernanceStateStore::new(url, &user, &password)
                    .await
                    .expect("governance");
                governance.migrate().await.expect("governance schema");
                Stores::new(runs, sessions, ledger, schedules, governance)
            }
            #[cfg(feature = "firestore")]
            BackendKind::Firestore => {
                let connect = || async {
                    adk_session::firestore_stores::connect_firestore_emulator(url)
                        .await
                        .expect("connect to the Firestore emulator")
                };
                let runs = adk_session::FirestoreRunStore::from_db(connect().await);
                runs.migrate().await.expect("runs");
                let sessions = adk_session::FirestoreSessionService::from_db(connect().await, None);
                let ledger = adk_session::FirestoreActionLedger::from_db(connect().await);
                ledger.migrate().await.expect("ledger");
                let schedules = adk_session::FirestoreScheduleStore::from_db(connect().await);
                schedules.migrate().await.expect("schedules");
                let governance =
                    adk_session::FirestoreGovernanceStateStore::from_db(connect().await);
                governance.migrate().await.expect("governance");
                Stores::new(runs, sessions, ledger, schedules, governance)
            }
            #[allow(unreachable_patterns)] // Reached only when a backend feature is off.
            other => panic!(
                "the {} backend at {url} is not compiled in; enable its feature",
                other.name()
            ),
        }
    }
}

/// Every durable store one process opens on one backend.
#[derive(Clone)]
pub struct Stores {
    /// Runs and their fenced leases.
    pub runs: Arc<dyn DurableRunStore>,
    /// Agent session history.
    pub sessions: Arc<dyn SessionService>,
    /// The non-idempotent action ledger.
    pub ledger: Arc<dyn ActionLedger>,
    /// Cron and timer schedules.
    pub schedules: Arc<dyn ScheduleStore>,
    /// The replicated kill switch state.
    pub governance: Arc<dyn GovernanceStateStore>,
}

impl Stores {
    #[allow(dead_code)] // Unused when every backend feature is off.
    fn new(
        runs: impl DurableRunStore + 'static,
        sessions: impl SessionService + 'static,
        ledger: impl ActionLedger + 'static,
        schedules: impl ScheduleStore + 'static,
        governance: impl GovernanceStateStore + 'static,
    ) -> Self {
        Self {
            runs: Arc::new(runs),
            sessions: Arc::new(sessions),
            ledger: Arc::new(ledger),
            schedules: Arc::new(schedules),
            governance: Arc::new(governance),
        }
    }
}

// ---------------------------------------------------------------------------
// Files shared between the test and its processes
// ---------------------------------------------------------------------------

/// Milliseconds since the Unix epoch; every process on the host shares this clock.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Appends one line and syncs it, so a process killed right after still leaves it.
pub fn append_line(path: &Path, line: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    writeln!(file, "{line}").expect("line written");
    file.sync_all().expect("line synced");
}

/// The complete lines of `path`; none when it does not exist yet.
pub fn read_lines(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    // A line still being written has no newline yet.
    let complete = text.rfind('\n').map_or("", |end| &text[..end]);
    complete.lines().map(str::to_string).collect()
}

/// Waits until `path` exists.
pub async fn wait_for_file(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------
// The payment agent
// ---------------------------------------------------------------------------

/// The function call id of the one charge a payment run makes.
pub const CHARGE_CALL_ID: &str = "call-charge-1";

/// Charges once; once the charge has a response, reports its status.
///
/// With `hold`, the answer after the charge waits until that file exists, so the run
/// holds its lease for as long as the test needs.
pub struct PaymentModel {
    /// A file the post-charge answer waits for.
    pub hold: Option<PathBuf>,
}

#[async_trait]
impl Llm for PaymentModel {
    fn name(&self) -> &str {
        "payment-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let status = req.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "charge_card" =>
            {
                function_response.response.get("status").and_then(Value::as_str).map(str::to_string)
            }
            _ => None,
        });
        let content = match status {
            Some(status) => {
                if let Some(hold) = &self.hold {
                    wait_for_file(hold).await;
                }
                Content::new("model").with_text(format!("charge status: {status}"))
            }
            None => Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "charge_card".to_string(),
                    args: json!({ "amountCents": 1999 }),
                    id: Some(CHARGE_CALL_ID.to_string()),
                    thought_signature: None,
                }],
            },
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// What the charge tool does after it charged.
#[derive(Debug, Clone)]
pub enum AfterCharge {
    /// Returns at once.
    Return,
    /// Never returns; the process is killed inside the call.
    Hang,
    /// Returns once this file exists.
    WaitFor(PathBuf),
}

/// Appends one line to the charge log per charge — the external side effect the gates
/// count — then does what `after` says.
pub struct ChargeCard {
    /// The charge log shared by every process of one gate.
    pub log: PathBuf,
    /// What happens after the charge.
    pub after: AfterCharge,
}

#[async_trait]
impl Tool for ChargeCard {
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
        append_line(&self.log, &format!("charged by {}", std::process::id()));
        match &self.after {
            AfterCharge::Return => {}
            AfterCharge::Hang => std::future::pending::<()>().await,
            AfterCharge::WaitFor(path) => wait_for_file(path).await,
        }
        Ok(json!({ "status": "charged" }))
    }
}

/// The lines of the charge log: one per charge, across every process.
pub fn charges(log: &Path) -> usize {
    read_lines(log).len()
}

// ---------------------------------------------------------------------------
// The looping agent for the kill switch gate
// ---------------------------------------------------------------------------

/// Input that makes [`LoopModel`] call its tool forever.
pub const LOOP_INPUT: &str = "loop";

/// Calls `tick` forever when the run's input is [`LOOP_INPUT`]; otherwise answers at once.
/// Every call is logged with its start time.
pub struct LoopModel {
    /// The call log of this process.
    pub calls: PathBuf,
}

#[async_trait]
impl Llm for LoopModel {
    fn name(&self) -> &str {
        "loop-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        append_line(&self.calls, &format!("model {}", now_ms()));
        let looping = req
            .contents
            .iter()
            .filter(|content| content.role == "user")
            .flat_map(|content| &content.parts)
            .filter_map(Part::text)
            .any(|text| text == LOOP_INPUT);
        let content = if looping {
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "tick".to_string(),
                    args: json!({}),
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                }],
            }
        } else {
            Content::new("model").with_text("done")
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Logs its start time and takes a little while, like a real tool.
pub struct Tick {
    /// The call log of this process.
    pub calls: PathBuf,
}

#[async_trait]
impl Tool for Tick {
    fn name(&self) -> &str {
        "tick"
    }

    fn description(&self) -> &str {
        "does one unit of work"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        append_line(&self.calls, &format!("tool {}", now_ms()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(json!({ "ok": true }))
    }
}

// ---------------------------------------------------------------------------
// Store decorators
// ---------------------------------------------------------------------------

/// One fenced run-store write a process attempted, as recorded by [`RecordingRunStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRecord {
    /// When the call started, in Unix milliseconds.
    pub at_ms: i64,
    /// `claim`, `heartbeat`, `set_checkpoint`, `wait`, or `finish`.
    pub op: String,
    /// The epoch the write carried, or the epoch a claim returned.
    pub epoch: u64,
    /// `ok`, or the error code that refused the write.
    pub outcome: String,
}

impl WriteRecord {
    /// Parses the records a [`RecordingRunStore`] wrote.
    pub fn read_all(path: &Path) -> Vec<Self> {
        read_lines(path)
            .iter()
            .map(|line| {
                let fields: Vec<&str> = line.split(' ').collect();
                let [at_ms, op, epoch, outcome] = fields[..] else {
                    panic!("malformed write record: {line}");
                };
                Self {
                    at_ms: at_ms.parse().expect("time"),
                    op: op.to_string(),
                    epoch: epoch.parse().expect("epoch"),
                    outcome: outcome.to_string(),
                }
            })
            .collect()
    }
}

/// Records every fenced write and successful claim this process makes, so the test can
/// see what a worker tried to write and what the backend answered.
pub struct RecordingRunStore {
    inner: Arc<dyn DurableRunStore>,
    log: PathBuf,
    stall: Option<HeartbeatStall>,
}

/// Where a stalled worker's pause lands: its first heartbeat.
///
/// The heartbeat creates `reached` and waits for `resume` before it is sent, so the
/// test can stop the process there. A paused worker sends no renewal, and once resumed
/// the heartbeat is the first write it makes — with the epoch of the lease it lost.
pub struct HeartbeatStall {
    /// Created when the first heartbeat is about to be sent.
    pub reached: PathBuf,
    /// The heartbeat is sent once this exists.
    pub resume: PathBuf,
    armed: std::sync::atomic::AtomicBool,
}

impl HeartbeatStall {
    /// Stalls the first heartbeat until `resume` exists.
    pub fn new(reached: PathBuf, resume: PathBuf) -> Self {
        Self { reached, resume, armed: std::sync::atomic::AtomicBool::new(true) }
    }
}

impl RecordingRunStore {
    /// Wraps `inner`, recording to `log`.
    pub fn new(inner: Arc<dyn DurableRunStore>, log: PathBuf) -> Self {
        Self { inner, log, stall: None }
    }

    /// Stalls the first heartbeat as `stall` says.
    #[must_use]
    pub fn with_heartbeat_stall(mut self, stall: HeartbeatStall) -> Self {
        self.stall = Some(stall);
        self
    }

    fn record<T>(&self, started_ms: i64, op: &str, epoch: u64, result: &Result<T>) {
        let outcome = match result {
            Ok(_) => "ok".to_string(),
            Err(error) => error.code.to_string(),
        };
        append_line(&self.log, &format!("{started_ms} {op} {epoch} {outcome}"));
    }
}

#[async_trait]
impl DurableRunStore for RecordingRunStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        self.inner.enqueue(run).await
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        let started = now_ms();
        let claimed = self.inner.claim(queue, worker, ttl).await;
        if let Ok(Some(run)) = &claimed {
            self.record(started, "claim", run.epoch, &claimed);
        }
        claimed
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        if let Some(stall) = &self.stall
            && stall.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            std::fs::write(&stall.reached, b"").expect("stall marker written");
            wait_for_file(&stall.resume).await;
        }
        let started = now_ms();
        let result = self.inner.heartbeat(run_id, epoch, ttl).await;
        self.record(started, "heartbeat", epoch, &result);
        result
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        let started = now_ms();
        let result = self.inner.set_checkpoint(run_id, epoch, checkpoint).await;
        self.record(started, "set_checkpoint", epoch, &result);
        result
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        let started = now_ms();
        let result = self.inner.wait(run_id, epoch, until).await;
        self.record(started, "wait", epoch, &result);
        result
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        self.inner.wake(run_id).await
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        let started = now_ms();
        let result = self.inner.finish(run_id, epoch, outcome).await;
        self.record(started, "finish", epoch, &result);
        result
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

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        self.inner.prune(finished_before).await
    }
}

/// A governance store this process cannot read while `partition` exists — a network
/// partition between one process and the backend, which every other process still
/// reaches.
pub struct PartitionedGovernance {
    inner: Arc<dyn GovernanceStateStore>,
    partition: PathBuf,
}

impl PartitionedGovernance {
    /// Wraps `inner`, unreachable while `partition` exists.
    pub fn new(inner: Arc<dyn GovernanceStateStore>, partition: PathBuf) -> Self {
        Self { inner, partition }
    }

    fn reachable(&self) -> Result<()> {
        if self.partition.exists() {
            return Err(AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::Unavailable,
                "gate.partitioned",
                "the governance store is unreachable from this process",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl GovernanceStateStore for PartitionedGovernance {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        self.reachable()?;
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
        self.reachable()?;
        self.inner.set(scope, expected_epoch, frozen, reason, issued_by).await
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        self.reachable()?;
        self.inner.list().await
    }
}
