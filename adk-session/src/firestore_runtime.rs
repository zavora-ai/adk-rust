//! Firestore-backed [`LeaseStore`](adk_core::LeaseStore), [`DurableRunStore`](adk_core::DurableRunStore), [`ScheduleStore`](adk_core::ScheduleStore), and
//! [`GovernanceStateStore`](adk_core::GovernanceStateStore).
//!
//! Every read-modify-write runs in one Firestore read-write transaction. Firestore
//! transactions are serializable: a transaction that read a document another
//! transaction changed before it committed is aborted, and the `firestore` crate retries
//! it with exponential backoff for up to two minutes before the operation fails with the
//! store's `session.*.storage` error.
//!
//! # Data model
//!
//! | Collection (document ID) | Contents |
//! |--------------------------|----------|
//! | `adk_leases` (SHA-256 of the resource) | The resource, the [`Lease`](adk_core::Lease) as JSON, and a `released` flag |
//! | `adk_runs` (run ID, a UUID) | The [`RunRecord`](adk_core::RunRecord) as JSON, the queue, the enqueue sequence, the state, and the derived keys below |
//! | `adk_run_queues` (SHA-256 of the queue) | The queue's enqueue sequence counter, written by every enqueue and claim in the queue |
//! | `adk_run_keys` (SHA-256 of queue and idempotency key) | The queue, the key, and the run it names |
//! | `adk_run_key_holders` (SHA-256 of queue and concurrency key) | The unfinished runs holding the key, in enqueue order |
//! | `adk_schedules` (hex of the schedule ID) | The [`Schedule`](adk_core::Schedule) as JSON and `due_us` |
//! | `adk_governance` (SHA-256 of the scope key) | The scope key and the [`GovernanceState`](adk_core::GovernanceState) as JSON |
//!
//! Records are stored as JSON text, so timestamps and run inputs read back exactly. Every
//! document also stores the full identifier it was addressed by, and every read checks
//! it, so two identifiers that share a digest never read each other's record. Schedule
//! documents are named by the hex of the ID, which orders like the ID itself, so `due`
//! breaks fire-time ties by ID; a schedule ID longer than 700 bytes is refused.
//!
//! Each query is a range over one field, so Firestore's automatic single-field indexes
//! serve all of them and no composite index is needed:
//!
//! | Field | Present on | Value | Query |
//! |-------|------------|-------|-------|
//! | `claim_key` | Unfinished runs | `{queue digest}:{ready time}:{sequence}` | `claim`: the queue's runs whose ready time has passed, oldest ready first, then by enqueue order |
//! | `finished_us` | Finished runs | When the run finished, in microseconds | `prune` |
//! | `queue` | Every run | The queue | `list` with a queue filter |
//! | `due_us` | Unpaused schedules with a next fire time | The next fire time, in microseconds | `due` |
//!
//! Times and sequences in the keys are fixed-width decimal text that orders like the
//! signed integer, so text order is time order.
//!
//! # Atomicity
//!
//! | Operation | Transaction reads | Transaction writes |
//! |-----------|-------------------|--------------------|
//! | `acquire`, `renew`, `release` | The lease document | The lease document |
//! | `enqueue` | The queue document, the idempotency document, the key's holders document, each holder `Interrupt` cancels | The queue document (next sequence), the run, the idempotency document, the holders document, cancelled holders |
//! | `claim` (one transaction per candidate) | The candidate run; for a queued keyed run, its key's holders document | The claimed run; a run failed for exhausted attempts and its key's holders |
//! | `heartbeat`, `set_checkpoint`, `wait`, `finish`, `wake`, `request_cancel` | The run | The run, when it changes |
//! | `prune` | Each finished run in a batch | Deletes the run and its idempotency document |
//! | Schedule `upsert`, `set_paused`, `advance`, `delete` | The schedule | The schedule, when it changes |
//! | Governance `set` | The scope's document | The scope's document, when the epoch matches |
//!
//! No transaction runs a query. A claim finds candidates with a plain query on
//! `claim_key`, then re-reads and claims each one in its own transaction, so two claims
//! never take the same run at the same epoch. Concurrency keys keep their unfinished
//! holders in one document, read by `get` inside the transactions that change it: an
//! enqueue appends to it, a run leaves it in the transaction that finishes or cancels
//! it, and only its first holder is claimable while queued. A worker write reads the
//! run's epoch in the transaction that writes it, so a stale epoch is refused with
//! `run_store.stale_epoch`.

use std::sync::Arc;
use std::time::Duration;

use adk_core::governance_state::{
    GovernanceScope, GovernanceState, GovernanceStateStore, governance_conflict_error,
};
use adk_core::lease::{Lease, LeaseStore, lease_stale_epoch_error};
use adk_core::principal::SubjectRef;
use adk_core::run_store::{
    ClaimedRun, DurableRunStore, Enqueued, NewRun, OnConflict, RunCheckpointRef, RunFilter,
    RunOutcome, RunRecord, run_not_found_error,
};
use adk_core::schedule::{
    Schedule, ScheduleStore, invalid_schedule_error, schedule_not_found_error,
};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firestore::errors::{BackoffError, FirestoreError};
use firestore::{FirestoreDb, FirestoreQueryDirection, FirestoreWritePrecondition};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::durable_common::{
    GOVERNANCE_STORAGE, LEASE_STORAGE, RUN_STORAGE, SCHEDULE_STORAGE, deadline, hex,
    same_definition, state_name, storage_error, stored_now,
};
use crate::durable_transitions::{
    ClaimStep, apply_cancel, apply_checkpoint, apply_claim, apply_exhaust, apply_finish,
    apply_heartbeat, apply_wait, apply_wake, check_fence, claim_step, ready_micros,
};
use crate::firestore_stores::{
    TxFuture, TxResult, connect, document_id, from_json, retry_or_fail, to_json, transaction_error,
    transaction_options,
};

const LEASES: &str = "adk_leases";
const RUNS: &str = "adk_runs";
const RUN_QUEUES: &str = "adk_run_queues";
const RUN_KEYS: &str = "adk_run_keys";
const RUN_KEY_HOLDERS: &str = "adk_run_key_holders";
const SCHEDULES: &str = "adk_schedules";
const GOVERNANCE: &str = "adk_governance";

/// Candidates a claim reads per query page.
const CLAIM_PAGE: u32 = 64;
/// Finished runs a prune deletes per transaction.
const PRUNE_BATCH: u32 = 100;
/// Firestore document IDs are at most 1,500 bytes; a schedule's is the hex of its ID.
const MAX_SCHEDULE_ID_BYTES: usize = 700;

/// Maps a Firestore failure inside a transaction: retried when Firestore says so.
fn tx_fail(code: &'static str) -> impl Fn(FirestoreError) -> BackoffError<AdkError> {
    move |error| {
        retry_or_fail(error, |e| storage_error(code, format!("firestore request failed: {e}")))
    }
}

/// Fails a transaction with a contract error, rolling it back.
fn refuse<T>(error: AdkError) -> TxResult<T> {
    Err(BackoffError::permanent(error))
}

fn json_text<T: Serialize>(value: &T, code: &'static str) -> TxResult<String> {
    to_json(value, |m| storage_error(code, m)).map_err(BackoffError::permanent)
}

fn parse_json<T: serde::de::DeserializeOwned>(
    text: &str,
    what: &str,
    code: &'static str,
) -> Result<T> {
    from_json(text, what, |m| storage_error(code, m))
}

/// Fixed-width text that orders like the signed integer.
fn sortable(value: i64) -> String {
    format!("{:020}", (value as u64) ^ (1 << 63))
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseDoc {
    resource: String,
    /// JSON of the [`Lease`].
    lease: String,
    released: bool,
}

fn lease_id(resource: &str) -> String {
    document_id(&json!(["lease", resource]))
}

async fn read_lease(db: &FirestoreDb, resource: &str) -> TxResult<Option<(Lease, bool)>> {
    let stored: Option<LeaseDoc> = db
        .fluent()
        .select()
        .by_id_in(LEASES)
        .obj::<LeaseDoc>()
        .one(lease_id(resource))
        .await
        .map_err(tx_fail(LEASE_STORAGE))?;
    match stored.filter(|doc| doc.resource == resource) {
        Some(doc) => {
            let lease =
                parse_json(&doc.lease, "lease", LEASE_STORAGE).map_err(BackoffError::permanent)?;
            Ok(Some((lease, doc.released)))
        }
        None => Ok(None),
    }
}

fn write_lease(
    db: &FirestoreDb,
    transaction: &mut firestore::FirestoreTransaction<'_>,
    lease: &Lease,
    released: bool,
) -> TxResult<()> {
    let doc = LeaseDoc {
        resource: lease.resource.clone(),
        lease: json_text(lease, LEASE_STORAGE)?,
        released,
    };
    db.fluent()
        .update()
        .in_col(LEASES)
        .document_id(lease_id(&lease.resource))
        .object(&doc)
        .add_to_transaction(transaction)
        .map_err(tx_fail(LEASE_STORAGE))?;
    Ok(())
}

/// Firestore-backed [`LeaseStore`].
///
/// Each resource is one `adk_leases` document; every change reads and writes it in one
/// transaction. See the [module documentation](self) for the data model.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::LeaseStore;
/// use adk_session::FirestoreLeaseStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = FirestoreLeaseStore::new("my-gcp-project").await?;
/// store.migrate().await?;
/// let lease = store.acquire("scheduler", "replica-1", Duration::from_secs(30)).await?;
/// # let _ = lease;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreLeaseStore {
    db: FirestoreDb,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FirestoreLeaseStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreLeaseStore").field("clock", &self.clock).finish_non_exhaustive()
    }
}

impl FirestoreLeaseStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        let db = connect(project_id).await.map_err(|e| storage_error(LEASE_STORAGE, e))?;
        Ok(Self::from_db(db))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Prepares the store's storage.
    ///
    /// Firestore creates collections on first write and the store reads leases by
    /// document ID, so there is nothing to create; the method exists so every backend
    /// is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl LeaseStore for FirestoreLeaseStore {
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>> {
        let now = stored_now(self.clock.now());
        let expires_at = deadline(now, ttl, "lease.invalid_ttl")?;
        let request = Arc::new((resource.to_string(), owner.to_string()));
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<Lease>> {
                    let request = request.clone();
                    Box::pin(async move {
                        let (resource, owner) = &*request;
                        let previous = match read_lease(&db, resource).await? {
                            Some((lease, released)) => {
                                let live = !released && lease.expires_at > now;
                                if live && lease.owner != *owner {
                                    return Ok(None);
                                }
                                lease.epoch
                            }
                            None => 0,
                        };
                        let lease = Lease {
                            resource: resource.clone(),
                            owner: owner.clone(),
                            epoch: previous + 1,
                            expires_at,
                        };
                        write_lease(&db, transaction, &lease, false)?;
                        Ok(Some(lease))
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(LEASE_STORAGE, m)))
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
        let expires_at = deadline(stored_now(self.clock.now()), ttl, "lease.invalid_ttl")?;
        let held = Arc::new(lease.clone());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Lease> {
                    let held = held.clone();
                    Box::pin(async move {
                        match read_lease(&db, &held.resource).await? {
                            Some((stored, false))
                                if stored.epoch == held.epoch && stored.owner == held.owner =>
                            {
                                let renewed = Lease { expires_at, ..stored };
                                write_lease(&db, transaction, &renewed, false)?;
                                Ok(renewed)
                            }
                            _ => refuse(lease_stale_epoch_error(&held.resource, held.epoch)),
                        }
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(LEASE_STORAGE, m)))
    }

    async fn release(&self, lease: &Lease) -> Result<()> {
        let held = Arc::new(lease.clone());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, ()> {
                    let held = held.clone();
                    Box::pin(async move {
                        if let Some((stored, false)) = read_lease(&db, &held.resource).await?
                            && stored.epoch == held.epoch
                            && stored.owner == held.owner
                        {
                            write_lease(&db, transaction, &stored, true)?;
                        }
                        Ok(())
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(LEASE_STORAGE, m)))
    }

    async fn current(&self, resource: &str) -> Result<Option<Lease>> {
        let now = self.clock.now();
        let stored = read_lease(&self.db, resource).await.map_err(|e| match e {
            BackoffError::Permanent(error) | BackoffError::Transient { err: error, .. } => error,
        })?;
        Ok(stored
            .filter(|(lease, released)| !released && lease.expires_at > now)
            .map(|(lease, _)| lease))
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// The per-queue document every enqueue and claim reads and writes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct QueueDoc {
    queue: String,
    #[serde(default)]
    seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunDoc {
    run_id: String,
    queue: String,
    seq: i64,
    state: String,
    /// JSON of the [`RunRecord`]: the durable truth; the other fields derive from it.
    record: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_us: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunKeyDoc {
    queue: String,
    key: String,
    run_id: String,
}

fn queue_id(queue: &str) -> String {
    document_id(&json!(["queue", queue]))
}

fn run_key_id(queue: &str, key: &str) -> String {
    document_id(&json!(["idempotency", queue, key]))
}

/// Run IDs are hyphenated UUIDs; anything else names no run and is never a document path.
fn is_run_id(run_id: &str) -> bool {
    uuid::Uuid::parse_str(run_id).is_ok_and(|id| id.hyphenated().to_string() == run_id)
}

fn run_doc(record: &RunRecord, seq: i64) -> TxResult<RunDoc> {
    let unfinished = !record.state.is_terminal();
    Ok(RunDoc {
        run_id: record.run_id.clone(),
        queue: record.queue.clone(),
        seq,
        state: state_name(record.state).to_string(),
        record: json_text(record, RUN_STORAGE)?,
        claim_key: unfinished.then(|| {
            format!(
                "{}:{}:{}",
                queue_id(&record.queue),
                sortable(ready_micros(record)),
                sortable(seq)
            )
        }),
        finished_us: (!unfinished).then(|| record.updated_at.timestamp_micros()),
    })
}

fn decode_run(doc: &RunDoc) -> Result<RunRecord> {
    let record: RunRecord = parse_json(&doc.record, "run record", RUN_STORAGE)?;
    if record.run_id != doc.run_id {
        return Err(storage_error(
            RUN_STORAGE,
            format!("run document '{}' holds run '{}'", doc.run_id, record.run_id),
        ));
    }
    Ok(record)
}

fn write_run(
    db: &FirestoreDb,
    transaction: &mut firestore::FirestoreTransaction<'_>,
    record: &RunRecord,
    seq: i64,
    precondition: Option<FirestoreWritePrecondition>,
) -> TxResult<()> {
    let doc = run_doc(record, seq)?;
    let builder = db.fluent().update().in_col(RUNS);
    let builder = match precondition {
        Some(precondition) => builder.precondition(precondition),
        None => builder,
    };
    builder
        .document_id(&record.run_id)
        .object(&doc)
        .add_to_transaction(transaction)
        .map_err(tx_fail(RUN_STORAGE))?;
    Ok(())
}

async fn read_run(db: &FirestoreDb, run_id: &str) -> TxResult<Option<(RunRecord, i64)>> {
    if !is_run_id(run_id) {
        return Ok(None);
    }
    let stored: Option<RunDoc> = db
        .fluent()
        .select()
        .by_id_in(RUNS)
        .obj::<RunDoc>()
        .one(run_id)
        .await
        .map_err(tx_fail(RUN_STORAGE))?;
    match stored.filter(|doc| doc.run_id == run_id) {
        Some(doc) => Ok(Some((decode_run(&doc).map_err(BackoffError::permanent)?, doc.seq))),
        None => Ok(None),
    }
}

async fn read_queue(db: &FirestoreDb, queue: &str) -> TxResult<QueueDoc> {
    let stored: Option<QueueDoc> = db
        .fluent()
        .select()
        .by_id_in(RUN_QUEUES)
        .obj::<QueueDoc>()
        .one(queue_id(queue))
        .await
        .map_err(tx_fail(RUN_STORAGE))?;
    Ok(stored
        .filter(|doc| doc.queue == queue)
        .unwrap_or_else(|| QueueDoc { queue: queue.to_string(), ..QueueDoc::default() }))
}

fn write_queue(
    db: &FirestoreDb,
    transaction: &mut firestore::FirestoreTransaction<'_>,
    queue: &QueueDoc,
) -> TxResult<()> {
    db.fluent()
        .update()
        .in_col(RUN_QUEUES)
        .document_id(queue_id(&queue.queue))
        .object(queue)
        .add_to_transaction(transaction)
        .map_err(tx_fail(RUN_STORAGE))?;
    Ok(())
}

/// The unfinished runs holding one concurrency key, in enqueue order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KeyHoldersDoc {
    queue: String,
    key: String,
    #[serde(default)]
    holders: Vec<String>,
}

fn key_holders_id(queue: &str, key: &str) -> String {
    document_id(&json!(["holders", queue, key]))
}

async fn read_holders(db: &FirestoreDb, queue: &str, key: &str) -> TxResult<KeyHoldersDoc> {
    let stored: Option<KeyHoldersDoc> = db
        .fluent()
        .select()
        .by_id_in(RUN_KEY_HOLDERS)
        .obj::<KeyHoldersDoc>()
        .one(key_holders_id(queue, key))
        .await
        .map_err(tx_fail(RUN_STORAGE))?;
    Ok(stored.filter(|doc| doc.queue == queue && doc.key == key).unwrap_or_else(|| KeyHoldersDoc {
        queue: queue.to_string(),
        key: key.to_string(),
        holders: Vec::new(),
    }))
}

fn write_holders(
    db: &FirestoreDb,
    transaction: &mut firestore::FirestoreTransaction<'_>,
    doc: &KeyHoldersDoc,
) -> TxResult<()> {
    db.fluent()
        .update()
        .in_col(RUN_KEY_HOLDERS)
        .document_id(key_holders_id(&doc.queue, &doc.key))
        .object(doc)
        .add_to_transaction(transaction)
        .map_err(tx_fail(RUN_STORAGE))?;
    Ok(())
}

/// Removes a run that just finished from its concurrency key's holders.
async fn leave_key(
    db: &FirestoreDb,
    transaction: &mut firestore::FirestoreTransaction<'_>,
    record: &RunRecord,
) -> TxResult<()> {
    if let Some(key) = record.concurrency_key.as_deref().filter(|_| record.state.is_terminal()) {
        let mut doc = read_holders(db, &record.queue, key).await?;
        doc.holders.retain(|id| *id != record.run_id);
        write_holders(db, transaction, &doc)?;
    }
    Ok(())
}

/// Changes one run in a transaction; returns what `op` returns.
type RunOp<T> = dyn Fn(&mut RunRecord, DateTime<Utc>) -> Result<(T, bool)> + Send + Sync;

/// Firestore-backed [`DurableRunStore`].
///
/// See the [module documentation](self) for the data model and how each operation stays
/// atomic.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::DurableRunStore;
/// use adk_core::run_store::{NewRun, RunOutcome};
/// use adk_session::FirestoreRunStore;
/// use serde_json::json;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = FirestoreRunStore::new("my-gcp-project").await?;
/// store.migrate().await?;
/// store.enqueue(NewRun::new("default", "app", "user", "agent", json!({}))).await?;
/// if let Some(claimed) = store.claim("default", "worker-1", Duration::from_secs(30)).await? {
///     store.finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!("ok"))).await?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreRunStore {
    db: FirestoreDb,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FirestoreRunStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreRunStore").field("clock", &self.clock).finish_non_exhaustive()
    }
}

impl FirestoreRunStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        let db = connect(project_id).await.map_err(|e| storage_error(RUN_STORAGE, e))?;
        Ok(Self::from_db(db))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides delays, waits, and lease expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Prepares the store's storage.
    ///
    /// Firestore creates collections on first write, and every query the store makes is
    /// a range over one field that the automatic single-field indexes serve, so there is
    /// nothing to create; the method exists so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    /// Claims one candidate in a transaction over the run and, for a queued keyed run,
    /// its key's holders; `None` when the candidate is no longer claimable, and an
    /// exhausted recovery fails the run.
    async fn claim_one(
        &self,
        run_id: &str,
        worker: &str,
        ttl: Duration,
    ) -> Result<Option<ClaimedRun>> {
        let clock = self.clock.clone();
        let request = Arc::new((run_id.to_string(), worker.to_string()));
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<ClaimedRun>> {
                    let (request, clock) = (request.clone(), clock.clone());
                    Box::pin(async move {
                        let (run_id, worker) = &*request;
                        let now = stored_now(clock.now());
                        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")
                            .map_err(BackoffError::permanent)?;
                        let Some((mut record, seq)) = read_run(&db, run_id).await? else {
                            return Ok(None);
                        };
                        let started = match claim_step(&record, now) {
                            ClaimStep::NotReady => return Ok(None),
                            ClaimStep::Exhaust => {
                                apply_exhaust(&mut record, now);
                                leave_key(&db, transaction, &record).await?;
                                write_run(&db, transaction, &record, seq, None)?;
                                return Ok(None);
                            }
                            ClaimStep::Claim { started } => started,
                        };
                        if record.state == adk_core::run_store::RunState::Queued
                            && let Some(key) = &record.concurrency_key
                            && read_holders(&db, &record.queue, key).await?.holders.first()
                                != Some(&record.run_id)
                        {
                            return Ok(None);
                        }
                        let epoch = apply_claim(&mut record, started, worker, now, expires_at);
                        write_run(&db, transaction, &record, seq, None)?;
                        Ok(Some(ClaimedRun { record, epoch }))
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(RUN_STORAGE, m)))
    }

    async fn mutate<T: Send + 'static>(&self, run_id: &str, op: Arc<RunOp<T>>) -> Result<T> {
        let clock = self.clock.clone();
        let run_id = Arc::new(run_id.to_string());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, T> {
                    let (op, run_id, clock) = (op.clone(), run_id.clone(), clock.clone());
                    Box::pin(async move {
                        let Some((mut record, seq)) = read_run(&db, &run_id).await? else {
                            return refuse(run_not_found_error(&run_id));
                        };
                        let now = stored_now(clock.now());
                        let (value, changed) = match op(&mut record, now) {
                            Ok(done) => done,
                            Err(error) => return refuse(error),
                        };
                        if changed {
                            leave_key(&db, transaction, &record).await?;
                            write_run(
                                &db,
                                transaction,
                                &record,
                                seq,
                                Some(FirestoreWritePrecondition::Exists(true)),
                            )?;
                        }
                        Ok(value)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(RUN_STORAGE, m)))
    }
}

#[async_trait]
impl DurableRunStore for FirestoreRunStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        run.validate()?;
        let clock = self.clock.clone();
        let request = Arc::new((run, uuid::Uuid::new_v4().to_string()));
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Enqueued> {
                    let (request, clock) = (request.clone(), clock.clone());
                    Box::pin(async move {
                        let (run, run_id) = &*request;
                        let now = stored_now(clock.now());
                        let mut queue = read_queue(&db, &run.queue).await?;
                        if let Some(key) = &run.idempotency_key {
                            let stored: Option<RunKeyDoc> = db
                                .fluent()
                                .select()
                                .by_id_in(RUN_KEYS)
                                .obj::<RunKeyDoc>()
                                .one(run_key_id(&run.queue, key))
                                .await
                                .map_err(tx_fail(RUN_STORAGE))?;
                            if let Some(stored) =
                                stored.filter(|doc| doc.queue == run.queue && doc.key == *key)
                                && let Some((existing, _)) = read_run(&db, &stored.run_id).await?
                            {
                                return Ok(Enqueued::Existing(existing));
                            }
                        }
                        let mut holders = match &run.concurrency_key {
                            Some(key) => Some(read_holders(&db, &run.queue, key).await?),
                            None => None,
                        };
                        if let Some(doc) = &mut holders {
                            match run.on_conflict {
                                OnConflict::Reject => {
                                    if let Some(blocking_run_id) = doc.holders.first() {
                                        return Ok(Enqueued::Rejected {
                                            blocking_run_id: blocking_run_id.clone(),
                                        });
                                    }
                                }
                                OnConflict::Interrupt => {
                                    for holder_id in doc.holders.clone() {
                                        let Some((mut holder, seq)) =
                                            read_run(&db, &holder_id).await?
                                        else {
                                            doc.holders.retain(|id| *id != holder_id);
                                            continue;
                                        };
                                        if apply_cancel(&mut holder, now) {
                                            write_run(&db, transaction, &holder, seq, None)?;
                                        }
                                        if holder.state.is_terminal() {
                                            doc.holders.retain(|id| *id != holder_id);
                                        }
                                    }
                                }
                                _ => {}
                            }
                            doc.holders.push(run_id.clone());
                            write_holders(&db, transaction, doc)?;
                        }
                        queue.seq += 1;
                        write_queue(&db, transaction, &queue)?;
                        let record = RunRecord::queued(run_id.clone(), run.clone(), now);
                        write_run(
                            &db,
                            transaction,
                            &record,
                            queue.seq,
                            Some(FirestoreWritePrecondition::Exists(false)),
                        )?;
                        if let Some(key) = &run.idempotency_key {
                            let doc = RunKeyDoc {
                                queue: run.queue.clone(),
                                key: key.clone(),
                                run_id: run_id.clone(),
                            };
                            db.fluent()
                                .update()
                                .in_col(RUN_KEYS)
                                .document_id(run_key_id(&run.queue, key))
                                .object(&doc)
                                .add_to_transaction(transaction)
                                .map_err(tx_fail(RUN_STORAGE))?;
                        }
                        Ok(Enqueued::Created(record))
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(RUN_STORAGE, m)))
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        let now = stored_now(self.clock.now());
        deadline(now, ttl, "run_store.invalid_ttl")?;
        let prefix = queue_id(queue);
        let end = format!("{prefix}:{};", sortable(now.timestamp_micros()));
        let mut after: Option<String> = None;
        loop {
            // Candidates are read outside any transaction, so a claim never holds a query
            // range; each candidate is then re-read and claimed in its own transaction.
            let lower = after.clone().unwrap_or_else(|| format!("{prefix}:"));
            let first_page = after.is_none();
            let docs: Vec<RunDoc> = self
                .db
                .fluent()
                .select()
                .from(RUNS)
                .filter(|q| {
                    q.for_all([
                        if first_page {
                            q.field("claim_key").greater_than_or_equal(lower.as_str())
                        } else {
                            q.field("claim_key").greater_than(lower.as_str())
                        },
                        q.field("claim_key").less_than(end.as_str()),
                    ])
                })
                .order_by([("claim_key", FirestoreQueryDirection::Ascending)])
                .limit(CLAIM_PAGE)
                .obj::<RunDoc>()
                .query()
                .await
                .map_err(|e| storage_error(RUN_STORAGE, format!("run store query failed: {e}")))?;
            let page_len = docs.len();
            for doc in docs {
                after = doc.claim_key.clone();
                if doc.queue != queue {
                    continue;
                }
                if let Some(claimed) = self.claim_one(&doc.run_id, worker, ttl).await? {
                    return Ok(Some(claimed));
                }
            }
            if page_len < CLAIM_PAGE as usize {
                return Ok(None);
            }
        }
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        deadline(stored_now(self.clock.now()), ttl, "run_store.invalid_ttl")?;
        self.mutate(
            run_id,
            Arc::new(move |record: &mut RunRecord, now| {
                check_fence(record, epoch)?;
                let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
                apply_heartbeat(record, now, expires_at);
                Ok((record.cancel_requested, true))
            }),
        )
        .await
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        self.mutate(
            run_id,
            Arc::new(move |record: &mut RunRecord, now| {
                check_fence(record, epoch)?;
                apply_checkpoint(record, now, checkpoint.clone());
                Ok(((), true))
            }),
        )
        .await
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        self.mutate(
            run_id,
            Arc::new(move |record: &mut RunRecord, now| {
                check_fence(record, epoch)?;
                apply_wait(record, now, until);
                Ok(((), true))
            }),
        )
        .await
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        self.mutate(
            run_id,
            Arc::new(|record: &mut RunRecord, now| Ok(((), apply_wake(record, now)))),
        )
        .await
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        self.mutate(
            run_id,
            Arc::new(move |record: &mut RunRecord, now| {
                check_fence(record, epoch)?;
                apply_finish(record, now, outcome.clone());
                Ok((record.clone(), true))
            }),
        )
        .await
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        self.mutate(
            run_id,
            Arc::new(|record: &mut RunRecord, now| Ok(((), apply_cancel(record, now)))),
        )
        .await
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        let stored = read_run(&self.db, run_id).await.map_err(|e| match e {
            BackoffError::Permanent(error) | BackoffError::Transient { err: error, .. } => error,
        })?;
        Ok(stored.map(|(record, _)| record))
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        let select = self.db.fluent().select().from(RUNS);
        let docs: Vec<RunDoc> = match &filter.queue {
            Some(queue) => {
                select
                    .filter(|q| q.for_all([q.field("queue").eq(queue.as_str())]))
                    .obj::<RunDoc>()
                    .query()
                    .await
            }
            None => select.obj::<RunDoc>().query().await,
        }
        .map_err(|e| storage_error(RUN_STORAGE, format!("run store query failed: {e}")))?;
        let mut matching = Vec::new();
        for doc in docs {
            let record = decode_run(&doc)?;
            if filter.matches(&record) {
                matching.push((doc.seq, record));
            }
        }
        matching.sort_by(|a, b| {
            (a.1.created_at, a.0, &a.1.run_id).cmp(&(b.1.created_at, b.0, &b.1.run_id))
        });
        let limit = filter.limit.unwrap_or(usize::MAX);
        Ok(matching.into_iter().take(limit).map(|(_, record)| record).collect())
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        let cutoff = finished_before.timestamp_micros();
        let mut pruned = 0u64;
        loop {
            let docs: Vec<RunDoc> = self
                .db
                .fluent()
                .select()
                .from(RUNS)
                .filter(|q| q.for_all([q.field("finished_us").less_than(cutoff)]))
                .limit(PRUNE_BATCH)
                .obj::<RunDoc>()
                .query()
                .await
                .map_err(|e| storage_error(RUN_STORAGE, format!("run store query failed: {e}")))?;
            if docs.is_empty() {
                return Ok(pruned);
            }
            let run_ids = Arc::new(docs.into_iter().map(|doc| doc.run_id).collect::<Vec<_>>());
            let deleted = self
                .db
                .run_transaction_with_options(
                    move |db, transaction| -> TxFuture<'_, u64> {
                        let run_ids = run_ids.clone();
                        Box::pin(async move {
                            let mut deleted = 0;
                            for run_id in run_ids.iter() {
                                let Some((record, _)) = read_run(&db, run_id).await? else {
                                    continue;
                                };
                                if !record.state.is_terminal()
                                    || record.updated_at.timestamp_micros() >= cutoff
                                {
                                    continue;
                                }
                                if let Some(key) = &record.idempotency_key {
                                    let key_id = run_key_id(&record.queue, key);
                                    let stored: Option<RunKeyDoc> = db
                                        .fluent()
                                        .select()
                                        .by_id_in(RUN_KEYS)
                                        .obj::<RunKeyDoc>()
                                        .one(&key_id)
                                        .await
                                        .map_err(tx_fail(RUN_STORAGE))?;
                                    if stored.is_some_and(|doc| doc.run_id == record.run_id) {
                                        db.fluent()
                                            .delete()
                                            .from(RUN_KEYS)
                                            .document_id(&key_id)
                                            .add_to_transaction(transaction)
                                            .map_err(tx_fail(RUN_STORAGE))?;
                                    }
                                }
                                db.fluent()
                                    .delete()
                                    .from(RUNS)
                                    .document_id(run_id)
                                    .add_to_transaction(transaction)
                                    .map_err(tx_fail(RUN_STORAGE))?;
                                deleted += 1;
                            }
                            Ok(deleted)
                        })
                    },
                    transaction_options(),
                )
                .await
                .map_err(|e| transaction_error(e, |m| storage_error(RUN_STORAGE, m)))?;
            if deleted == 0 {
                return Ok(pruned);
            }
            pruned += deleted;
        }
    }
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScheduleDoc {
    id: String,
    /// JSON of the [`Schedule`], revision included.
    schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    due_us: Option<i64>,
}

/// The document ID of a schedule, or `None` when the ID is too long to address.
fn schedule_doc_id(id: &str) -> Option<String> {
    (id.len() <= MAX_SCHEDULE_ID_BYTES).then(|| hex(id))
}

fn schedule_doc(schedule: &Schedule) -> TxResult<ScheduleDoc> {
    Ok(ScheduleDoc {
        id: schedule.id.clone(),
        schedule: json_text(schedule, SCHEDULE_STORAGE)?,
        due_us: schedule.next_fire_at.filter(|_| !schedule.paused).map(|at| at.timestamp_micros()),
    })
}

fn decode_schedule(doc: &ScheduleDoc) -> Result<Schedule> {
    parse_json(&doc.schedule, "schedule", SCHEDULE_STORAGE)
}

async fn read_schedule(db: &FirestoreDb, id: &str) -> TxResult<Option<Schedule>> {
    let Some(doc_id) = schedule_doc_id(id) else {
        return Ok(None);
    };
    let stored: Option<ScheduleDoc> = db
        .fluent()
        .select()
        .by_id_in(SCHEDULES)
        .obj::<ScheduleDoc>()
        .one(doc_id)
        .await
        .map_err(tx_fail(SCHEDULE_STORAGE))?;
    stored
        .filter(|doc| doc.id == id)
        .map(|doc| decode_schedule(&doc))
        .transpose()
        .map_err(BackoffError::permanent)
}

fn write_schedule(
    db: &FirestoreDb,
    transaction: &mut firestore::FirestoreTransaction<'_>,
    schedule: &Schedule,
) -> TxResult<()> {
    let doc = schedule_doc(schedule)?;
    db.fluent()
        .update()
        .in_col(SCHEDULES)
        .document_id(hex(&schedule.id))
        .object(&doc)
        .add_to_transaction(transaction)
        .map_err(tx_fail(SCHEDULE_STORAGE))?;
    Ok(())
}

/// Firestore-backed [`ScheduleStore`].
///
/// Each schedule is one `adk_schedules` document named by the hex of its ID; every change
/// reads and writes it in one transaction, so `advance` is a compare-and-swap on the
/// revision. See the [module documentation](self) for the data model.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::ScheduleStore;
/// use adk_core::run_store::NewRun;
/// use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec};
/// use adk_session::FirestoreScheduleStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = FirestoreScheduleStore::new("my-gcp-project").await?;
/// store.migrate().await?;
/// let schedule = Schedule::new(
///     "heartbeat",
///     ScheduleSpec::Every(Duration::from_secs(60)),
///     NewRun::new("default", "ops", "system", "probe", serde_json::json!({})),
///     MissedFirePolicy::Skip,
/// )
/// .with_next_fire_at(chrono::Utc::now());
/// store.upsert(schedule).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreScheduleStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreScheduleStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreScheduleStore").finish_non_exhaustive()
    }
}

impl FirestoreScheduleStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        let db = connect(project_id).await.map_err(|e| storage_error(SCHEDULE_STORAGE, e))?;
        Ok(Self::from_db(db))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Prepares the store's storage.
    ///
    /// Firestore creates collections on first write and serves `due` from the automatic
    /// single-field index on `due_us`, so there is nothing to create; the method exists
    /// so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    /// Changes one schedule in a transaction; `op` returns the value and the schedule to
    /// write, if any.
    async fn change<T: Send + 'static>(
        &self,
        id: &str,
        op: Arc<dyn Fn(Schedule) -> (T, Option<Schedule>) + Send + Sync>,
    ) -> Result<T> {
        let id = Arc::new(id.to_string());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, T> {
                    let (op, id) = (op.clone(), id.clone());
                    Box::pin(async move {
                        let Some(stored) = read_schedule(&db, &id).await? else {
                            return refuse(schedule_not_found_error(&id));
                        };
                        let (value, changed) = op(stored);
                        if let Some(changed) = changed {
                            write_schedule(&db, transaction, &changed)?;
                        }
                        Ok(value)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(SCHEDULE_STORAGE, m)))
    }
}

#[async_trait]
impl ScheduleStore for FirestoreScheduleStore {
    async fn upsert(&self, schedule: Schedule) -> Result<Schedule> {
        schedule.validate()?;
        if schedule_doc_id(&schedule.id).is_none() {
            return Err(invalid_schedule_error(&format!(
                "a Firestore schedule id is at most {MAX_SCHEDULE_ID_BYTES} bytes"
            )));
        }
        let incoming = Arc::new(schedule);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Schedule> {
                    let incoming = incoming.clone();
                    Box::pin(async move {
                        let mut schedule = (*incoming).clone();
                        match read_schedule(&db, &schedule.id).await? {
                            Some(stored) if same_definition(&stored, &schedule) => {
                                return Ok(stored);
                            }
                            Some(stored) => schedule.revision = stored.revision + 1,
                            None => schedule.revision = 1,
                        }
                        write_schedule(&db, transaction, &schedule)?;
                        Ok(schedule)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(SCHEDULE_STORAGE, m)))
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        read_schedule(&self.db, id).await.map_err(|e| match e {
            BackoffError::Permanent(error) | BackoffError::Transient { err: error, .. } => error,
        })
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        let docs: Vec<ScheduleDoc> =
            self.db.fluent().select().from(SCHEDULES).obj::<ScheduleDoc>().query().await.map_err(
                |e| storage_error(SCHEDULE_STORAGE, format!("schedule query failed: {e}")),
            )?;
        let mut schedules = docs.iter().map(decode_schedule).collect::<Result<Vec<_>>>()?;
        schedules.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(schedules)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let id = Arc::new(id.to_string());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, ()> {
                    let id = id.clone();
                    Box::pin(async move {
                        if read_schedule(&db, &id).await?.is_none() {
                            return refuse(schedule_not_found_error(&id));
                        }
                        db.fluent()
                            .delete()
                            .from(SCHEDULES)
                            .document_id(hex(&id))
                            .add_to_transaction(transaction)
                            .map_err(tx_fail(SCHEDULE_STORAGE))?;
                        Ok(())
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(SCHEDULE_STORAGE, m)))
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
        self.change(
            id,
            Arc::new(move |mut schedule: Schedule| {
                if schedule.paused == paused {
                    return (schedule, None);
                }
                schedule.paused = paused;
                schedule.revision += 1;
                (schedule.clone(), Some(schedule))
            }),
        )
        .await
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now_us = now.timestamp_micros();
        let docs: Vec<ScheduleDoc> = self
            .db
            .fluent()
            .select()
            .from(SCHEDULES)
            .filter(|q| q.for_all([q.field("due_us").less_than_or_equal(now_us)]))
            // Firestore breaks ties by document name, the hex of the ID, which orders
            // like the ID.
            .order_by([("due_us", FirestoreQueryDirection::Ascending)])
            .limit(u32::try_from(limit).unwrap_or(u32::MAX))
            .obj::<ScheduleDoc>()
            .query()
            .await
            .map_err(|e| storage_error(SCHEDULE_STORAGE, format!("schedule query failed: {e}")))?;
        let mut due = docs.iter().map(decode_schedule).collect::<Result<Vec<_>>>()?;
        due.retain(|schedule| {
            !schedule.paused && schedule.next_fire_at.is_some_and(|at| at <= now)
        });
        due.sort_by(|a, b| (a.next_fire_at, &a.id).cmp(&(b.next_fire_at, &b.id)));
        Ok(due)
    }

    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        self.change(
            id,
            Arc::new(move |mut schedule: Schedule| {
                if schedule.revision != expected_revision {
                    return (false, None);
                }
                schedule.next_fire_at = next_fire_at;
                schedule.revision += 1;
                (true, Some(schedule))
            }),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Governance state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GovernanceDoc {
    key: String,
    /// JSON of the [`GovernanceState`].
    state: String,
}

fn governance_id(key: &str) -> String {
    document_id(&json!(["governance", key]))
}

fn decode_governance(doc: &GovernanceDoc) -> Result<GovernanceState> {
    parse_json(&doc.state, "governance state", GOVERNANCE_STORAGE)
}

async fn read_governance(db: &FirestoreDb, key: &str) -> TxResult<Option<GovernanceState>> {
    let stored: Option<GovernanceDoc> = db
        .fluent()
        .select()
        .by_id_in(GOVERNANCE)
        .obj::<GovernanceDoc>()
        .one(governance_id(key))
        .await
        .map_err(tx_fail(GOVERNANCE_STORAGE))?;
    stored
        .filter(|doc| doc.key == key)
        .map(|doc| decode_governance(&doc))
        .transpose()
        .map_err(BackoffError::permanent)
}

/// Firestore-backed [`GovernanceStateStore`].
///
/// Each scope is one `adk_governance` document; `set` reads its epoch and writes the new
/// state in one transaction, so two operators acting at once never overwrite each
/// other. See the [module documentation](self) for the data model.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::GovernanceStateStore;
/// use adk_core::governance_state::GovernanceScope;
/// use adk_session::FirestoreGovernanceStateStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = FirestoreGovernanceStateStore::new("my-gcp-project").await?;
/// store.migrate().await?;
/// let frozen = store.set(GovernanceScope::Org, 0, true, Some("incident".into()), None).await?;
/// assert_eq!(frozen.epoch, 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreGovernanceStateStore {
    db: FirestoreDb,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FirestoreGovernanceStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreGovernanceStateStore")
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl FirestoreGovernanceStateStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        let db = connect(project_id).await.map_err(|e| storage_error(GOVERNANCE_STORAGE, e))?;
        Ok(Self::from_db(db))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps `issued_at`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Prepares the store's storage.
    ///
    /// Firestore creates collections on first write and the store reads scopes by
    /// document ID, so there is nothing to create; the method exists so every backend is
    /// set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl GovernanceStateStore for FirestoreGovernanceStateStore {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        read_governance(&self.db, &scope.key()).await.map_err(|e| match e {
            BackoffError::Permanent(error) | BackoffError::Transient { err: error, .. } => error,
        })
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        let clock = self.clock.clone();
        let request = Arc::new((scope, reason, issued_by));
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, GovernanceState> {
                    let (request, clock) = (request.clone(), clock.clone());
                    Box::pin(async move {
                        let (scope, reason, issued_by) = &*request;
                        let key = scope.key();
                        let current =
                            read_governance(&db, &key).await?.map_or(0, |state| state.epoch);
                        if current != expected_epoch {
                            return refuse(governance_conflict_error(
                                scope,
                                expected_epoch,
                                current,
                            ));
                        }
                        let state = GovernanceState {
                            scope: scope.clone(),
                            frozen,
                            reason: reason.clone(),
                            issued_by: issued_by.clone(),
                            issued_at: stored_now(clock.now()),
                            epoch: current + 1,
                        };
                        let doc = GovernanceDoc {
                            key: key.clone(),
                            state: json_text(&state, GOVERNANCE_STORAGE)?,
                        };
                        db.fluent()
                            .update()
                            .in_col(GOVERNANCE)
                            .document_id(governance_id(&key))
                            .object(&doc)
                            .add_to_transaction(transaction)
                            .map_err(tx_fail(GOVERNANCE_STORAGE))?;
                        Ok(state)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, |m| storage_error(GOVERNANCE_STORAGE, m)))
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        let docs: Vec<GovernanceDoc> = self
            .db
            .fluent()
            .select()
            .from(GOVERNANCE)
            .obj::<GovernanceDoc>()
            .query()
            .await
            .map_err(|e| {
                storage_error(GOVERNANCE_STORAGE, format!("governance query failed: {e}"))
            })?;
        let mut states = docs.iter().map(decode_governance).collect::<Result<Vec<_>>>()?;
        states.sort_by_key(|state| state.scope.key());
        Ok(states)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sortable_text_orders_like_the_integer() {
        let values = [i64::MIN, -5_000_000, -1, 0, 1, 1_800_000_000_000_000, i64::MAX];
        let keys: Vec<_> = values.iter().map(|value| sortable(*value)).collect();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]), "{keys:?}");
        assert!(keys.iter().all(|key| key.len() == 20));
    }

    #[test]
    fn claim_keys_stay_inside_their_queue_and_ready_range() {
        let prefix = queue_id("conformance:q/ü");
        let key = format!("{prefix}:{}:{}", sortable(10), sortable(3));
        let end = format!("{prefix}:{};", sortable(10));
        assert!(key.as_str() >= format!("{prefix}:").as_str() && key < end);
        let later = format!("{prefix}:{}:{}", sortable(11), sortable(1));
        assert!(later > end, "a run ready later is outside the range");
        assert_ne!(queue_id("a"), queue_id("b"));
    }

    #[test]
    fn derived_fields_follow_the_state() {
        let run = NewRun::new("q", "a", "u", "t", json!({}))
            .with_concurrency_key("k", OnConflict::Enqueue);
        let mut record = RunRecord::queued(uuid::Uuid::new_v4().to_string(), run, Utc::now());
        let doc = run_doc(&record, 7).unwrap();
        assert!(doc.claim_key.is_some() && doc.finished_us.is_none());
        assert_eq!(doc.state, "queued");
        assert!(apply_cancel(&mut record, Utc::now()));
        let doc = run_doc(&record, 7).unwrap();
        assert!(doc.claim_key.is_none() && doc.finished_us.is_some());
        assert_eq!(decode_run(&doc).unwrap(), record);
    }

    #[test]
    fn only_hyphenated_uuids_name_runs() {
        assert!(is_run_id(&uuid::Uuid::new_v4().to_string()));
        assert!(!is_run_id("conformance:missing/ü"));
        assert!(!is_run_id(&uuid::Uuid::new_v4().simple().to_string()));
    }

    #[test]
    fn schedule_ids_keep_their_order_and_length_limit() {
        assert!(schedule_doc_id("a").unwrap() < schedule_doc_id("b").unwrap());
        assert!(schedule_doc_id(&"x".repeat(MAX_SCHEDULE_ID_BYTES + 1)).is_none());
    }
}
