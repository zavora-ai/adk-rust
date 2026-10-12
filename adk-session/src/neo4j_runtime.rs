//! Neo4j-backed [`LeaseStore`](adk_core::LeaseStore), [`DurableRunStore`](adk_core::DurableRunStore), [`ScheduleStore`](adk_core::ScheduleStore), and
//! [`GovernanceStateStore`](adk_core::GovernanceStateStore).
//!
//! Every store follows the lock-node pattern of the [Neo4j durable
//! stores](crate::neo4j_stores): an auto-commit `MERGE` creates the node a change
//! serializes on, the transaction's first statement writes that node to take its
//! exclusive lock, and only then does the transaction read the state it decides on and
//! write the result. Reads after the lock see every transaction that committed before
//! it, so no decision acts on a stale read.
//!
//! # Data model
//!
//! | Store | Label | Key (unique) | Properties |
//! |-------|-------|--------------|------------|
//! | [`Neo4jLeaseStore`] | `AdkLease` | `resource` | `owner`, `epoch`, `expires_us`, `released` |
//! | [`Neo4jRunStore`] | `AdkRun` | `run_id`; `(queue, idempotency_key)` | `record` (the [`RunRecord`](adk_core::run_store::RunRecord) as JSON, the source of truth), plus `queue`, `seq`, `state`, `ready_us`, `finished_us`, `concurrency_key`, `idempotency_key`, `app_name`, `user_id`, `session_id`, `created_us`, `revision` for queries |
//! | [`Neo4jRunStore`] | `AdkRunQueue` | `queue` | `seq`: the enqueue counter; the lock every enqueue and claim in the queue takes |
//! | [`Neo4jScheduleStore`] | `AdkSchedule` | `id` | `schedule` (the [`Schedule`](adk_core::schedule::Schedule) as JSON), `order_key`, `paused`, `due_us`, `revision` |
//! | [`Neo4jGovernanceStateStore`] | `AdkGovernanceState` | `key` ([`GovernanceScope::key`](adk_core::governance_state::GovernanceScope::key)) | `state` (the [`GovernanceState`](adk_core::governance_state::GovernanceState) as JSON), `order_key`, `epoch` |
//!
//! Times are integer microseconds since the epoch. `ready_us` is when a run next
//! becomes claimable (`i64::MAX` while it waits for a signal), and `finished_us` is set
//! only on a finished run. `order_key` is the hex of the identifier, so `ORDER BY` follows
//! byte order whatever the database's string collation. A lock node created by `MERGE`
//! that was never written holds no `epoch`, `record`, `schedule`, or `state`, and every
//! read treats it as absent.
//!
//! # Atomicity
//!
//! | Operation | Lock taken first | Then |
//! |-----------|------------------|------|
//! | `acquire`, `renew`, `release` | The `AdkLease` node | Reads owner, epoch, expiry, and release flag; writes the grant, the new expiry, or the release |
//! | `enqueue` | The queue's `AdkRunQueue` node | Reads the idempotency key and the unfinished holders of the concurrency key; for `Interrupt`, locks and cancels each holder; creates the run at the next `seq` |
//! | `claim` | The queue's `AdkRunQueue` node | Reads unfinished runs with `ready_us <= now` in `seq` order, locks the chosen run, re-reads it, checks that no older unfinished run holds its concurrency key, and writes the claim at the next epoch |
//! | `heartbeat`, `set_checkpoint`, `wait`, `finish`, `wake`, `request_cancel` | The `AdkRun` node | Reads the record, compares the epoch (worker writes), and writes the transition |
//! | `prune` | — | Auto-commit `DETACH DELETE` of finished runs in batches of 500; a finished run never changes |
//! | `upsert`, `set_paused`, `advance` | The `AdkSchedule` node | Reads the schedule and its revision; writes on a change or a matching revision |
//! | governance `set` | The `AdkGovernanceState` node | Reads the epoch; writes when it equals the expected one |
//!
//! Locks are always taken queue node first, then run nodes, so two operations cannot
//! wait on each other in a cycle. A transaction that still fails with a transient error,
//! such as a detected deadlock, is retried up to 30 times with a short randomized pause
//! before the error is returned. Call `migrate` once per database to create the
//! constraints and indexes.

use crate::neo4j_stores::{Inline, detached};
use std::future::Future;
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
use adk_core::schedule::{Schedule, ScheduleStore, schedule_not_found_error};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use neo4rs::{Graph, Query, Row, Txn, query};

use crate::durable_common::{
    GOVERNANCE_STORAGE, LEASE_STORAGE, RUN_STORAGE, SCHEDULE_STORAGE, deadline, hex,
    same_definition, state_name, storage_error, stored_now,
};
use crate::durable_transitions::{
    ClaimStep, apply_cancel, apply_checkpoint, apply_claim, apply_exhaust, apply_finish,
    apply_heartbeat, apply_wait, apply_wake, check_fence, claim_step, ready_micros,
};
use crate::neo4j_stores::{Failure, column, connect, graph_rows, rows, run_schema};

/// Attempts at a transaction that fails with a transient error before it is returned.
const RETRY_ATTEMPTS: u32 = 30;

/// Candidate runs a claim reads per page.
const CLAIM_PAGE: i64 = 64;

/// Finished runs `prune` deletes per statement.
const PRUNE_BATCH: i64 = 500;

fn lease_error(message: String) -> AdkError {
    storage_error(LEASE_STORAGE, message)
}

fn run_error(message: String) -> AdkError {
    storage_error(RUN_STORAGE, message)
}

fn schedule_error(message: String) -> AdkError {
    storage_error(SCHEDULE_STORAGE, message)
}

fn governance_error(message: String) -> AdkError {
    storage_error(GOVERNANCE_STORAGE, message)
}

/// Small randomized pause before retry `attempt`, growing with the attempt number.
fn retry_backoff(attempt: u32) -> Duration {
    let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 10;
    Duration::from_millis(u64::from(attempt.min(20)) * 5 + jitter)
}

/// Runs `attempt` until it succeeds, fails with a non-transient error, or has failed
/// [`RETRY_ATTEMPTS`] times, mapping a database failure with `storage`.
async fn retry<T, F, Fut>(storage: fn(String) -> AdkError, mut attempt: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, Failure>>,
{
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(failure) if failure.is_transient() && tries < RETRY_ATTEMPTS => {
                tracing::debug!(
                    attempt = tries,
                    "retrying neo4j transaction after a transient error"
                );
                tokio::time::sleep(retry_backoff(tries)).await;
                tries += 1;
            }
            result => return result.map_err(|failure| failure.into_adk(storage)),
        }
    }
}

/// Runs `$body` in a transaction on `$graph`, committing on success and rolling back
/// on failure. `$body` refers to the transaction as `$txn`.
macro_rules! transaction {
    ($graph:expr, |$txn:ident| $body:expr) => {{
        let mut $txn = $graph.start_txn().await?;
        let result: std::result::Result<_, Failure> = async { $body }.await;
        match result {
            Ok(value) => {
                $txn.commit().await?;
                Ok(value)
            }
            Err(failure) => {
                let _ = $txn.rollback().await;
                Err(failure)
            }
        }
    }};
}

fn read<T: serde::de::DeserializeOwned>(
    row: &Row,
    name: &str,
    storage: fn(String) -> AdkError,
) -> std::result::Result<T, Failure> {
    column(row, name).map_err(|message| Failure::Store(storage(message)))
}

fn decode<T: serde::de::DeserializeOwned>(
    json: &str,
    what: &str,
    storage: fn(String) -> AdkError,
) -> Result<T> {
    serde_json::from_str(json).map_err(|e| storage(format!("stored {what} is corrupt: {e}")))
}

fn encode<T: serde::Serialize>(value: &T, storage: fn(String) -> AdkError) -> Result<String> {
    serde_json::to_string(value).map_err(|e| storage(format!("serialize failed: {e}")))
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn from_micros(micros: i64, storage: fn(String) -> AdkError) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| storage(format!("time {micros} is out of range")))
}

/// The microsecond cut-off equivalent to "before `at`" for microsecond values.
fn micros_before(at: DateTime<Utc>) -> i64 {
    let micros = at.timestamp_micros();
    if at.timestamp_subsec_nanos().is_multiple_of(1_000) {
        micros
    } else {
        micros.saturating_add(1)
    }
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/// The stored state of one resource's lease.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LeaseSlot {
    owner: String,
    epoch: u64,
    expires_us: i64,
    released: bool,
}

impl LeaseSlot {
    fn live(&self, now_us: i64) -> bool {
        !self.released && self.expires_us > now_us
    }

    fn lease(&self, resource: &str) -> Result<Lease> {
        Ok(Lease {
            resource: resource.to_string(),
            owner: self.owner.clone(),
            epoch: self.epoch,
            expires_at: from_micros(self.expires_us, lease_error)?,
        })
    }
}

/// The epoch `owner` is granted at `now_us`, or `None` while another owner holds a live
/// lease. `slot` is `None` for a resource never leased.
fn grant_epoch(slot: Option<&LeaseSlot>, owner: &str, now_us: i64) -> Option<u64> {
    match slot {
        Some(slot) if slot.live(now_us) && slot.owner != owner => None,
        Some(slot) => Some(slot.epoch + 1),
        None => Some(1),
    }
}

const LEASE_READ: &str = "MATCH (l:AdkLease {resource: $resource}) \
    RETURN l.owner AS owner, l.epoch AS epoch, l.expires_us AS expires_us, \
        coalesce(l.released, false) AS released";

fn lease_slot(row: &Row) -> std::result::Result<Option<LeaseSlot>, Failure> {
    let Some(epoch) = read::<Option<i64>>(row, "epoch", lease_error)? else {
        return Ok(None);
    };
    Ok(Some(LeaseSlot {
        owner: read::<Option<String>>(row, "owner", lease_error)?.unwrap_or_default(),
        epoch: to_u64(epoch),
        expires_us: read::<Option<i64>>(row, "expires_us", lease_error)?.unwrap_or(i64::MIN),
        released: read(row, "released", lease_error)?,
    }))
}

/// Write-locks the lease node inside `txn` and reads it. Returns `None` when the node
/// does not exist, `Some(None)` when it was never leased.
async fn lock_lease(
    txn: &mut Txn,
    resource: &str,
) -> std::result::Result<Option<Option<LeaseSlot>>, Failure> {
    let locked = rows(
        txn,
        query(
            "MATCH (l:AdkLease {resource: $resource}) \
             SET l.lock_seq = coalesce(l.lock_seq, 0) + 1 RETURN count(l) AS locked",
        )
        .param("resource", resource),
    )
    .await?;
    if locked.first().map(|row| read::<i64>(row, "locked", lease_error)).transpose()? != Some(1) {
        return Ok(None);
    }
    let found = rows(txn, query(LEASE_READ).param("resource", resource)).await?;
    match found.first() {
        Some(row) => Ok(Some(lease_slot(row)?)),
        None => Ok(None),
    }
}

/// Neo4j-backed [`LeaseStore`].
///
/// One `AdkLease` node per resource, kept after release so epochs never repeat. Every
/// change write-locks the node before reading it; see the
/// [module documentation](crate::neo4j_runtime).
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::LeaseStore;
/// use adk_session::Neo4jLeaseStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let leases = Neo4jLeaseStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// leases.migrate().await?;
/// let lease = leases.acquire("scheduler", "replica-1", Duration::from_secs(30)).await?;
/// # let _ = lease;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jLeaseStore {
    graph: Graph,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Neo4jLeaseStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jLeaseStore").field("clock", &self.clock).finish_non_exhaustive()
    }
}

impl Neo4jLeaseStore {
    /// Connects to Neo4j. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, lease_error).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Creates the uniqueness constraint on the resource name.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &["CREATE CONSTRAINT adk_lease_resource_unique IF NOT EXISTS \
               FOR (l:AdkLease) REQUIRE l.resource IS UNIQUE"],
            lease_error,
        )
        .await
    }

    async fn try_acquire(
        &self,
        resource: &str,
        owner: &str,
        ttl: Duration,
    ) -> std::result::Result<Option<Lease>, Failure> {
        self.graph
            .run(query("MERGE (:AdkLease {resource: $resource})").param("resource", resource))
            .await?;
        let now = stored_now(self.clock.now());
        let expires_at = deadline(now, ttl, "lease.invalid_ttl")?;
        transaction!(self.graph, |txn| {
            let Some(slot) = lock_lease(&mut txn, resource).await? else {
                return Err(Failure::Store(lease_error(format!(
                    "lease node for '{resource}' disappeared during acquire"
                ))));
            };
            let Some(epoch) = grant_epoch(slot.as_ref(), owner, now.timestamp_micros()) else {
                return Ok(None);
            };
            txn.run(
                query(
                    "MATCH (l:AdkLease {resource: $resource}) SET l.owner = $owner, \
                     l.epoch = $epoch, l.expires_us = $expires_us, l.released = false",
                )
                .param("resource", resource)
                .param("owner", owner)
                .param("epoch", to_i64(epoch))
                .param("expires_us", expires_at.timestamp_micros()),
            )
            .await?;
            Ok(Some(Lease {
                resource: resource.to_string(),
                owner: owner.to_string(),
                epoch,
                expires_at,
            }))
        })
    }

    async fn try_renew(&self, lease: &Lease, ttl: Duration) -> std::result::Result<Lease, Failure> {
        let now = stored_now(self.clock.now());
        let expires_at = deadline(now, ttl, "lease.invalid_ttl")?;
        transaction!(self.graph, |txn| {
            let current = lock_lease(&mut txn, &lease.resource).await?.flatten();
            match current {
                Some(slot)
                    if !slot.released && slot.epoch == lease.epoch && slot.owner == lease.owner =>
                {
                    txn.run(
                        query(
                            "MATCH (l:AdkLease {resource: $resource}) \
                             SET l.expires_us = $expires_us",
                        )
                        .param("resource", lease.resource.as_str())
                        .param("expires_us", expires_at.timestamp_micros()),
                    )
                    .await?;
                    Ok(Lease { expires_at, ..lease.clone() })
                }
                _ => Err(Failure::Store(lease_stale_epoch_error(&lease.resource, lease.epoch))),
            }
        })
    }

    async fn try_release(&self, lease: &Lease) -> std::result::Result<(), Failure> {
        transaction!(self.graph, |txn| {
            let current = lock_lease(&mut txn, &lease.resource).await?.flatten();
            if current.is_some_and(|slot| slot.epoch == lease.epoch && slot.owner == lease.owner) {
                txn.run(
                    query("MATCH (l:AdkLease {resource: $resource}) SET l.released = true")
                        .param("resource", lease.resource.as_str()),
                )
                .await?;
            }
            Ok(())
        })
    }
}

#[async_trait]
impl LeaseStore for Inline<Neo4jLeaseStore> {
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>> {
        deadline(self.clock.now(), ttl, "lease.invalid_ttl")?;
        retry(lease_error, || self.try_acquire(resource, owner, ttl)).await
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
        retry(lease_error, || self.try_renew(lease, ttl)).await
    }

    async fn release(&self, lease: &Lease) -> Result<()> {
        retry(lease_error, || self.try_release(lease)).await
    }

    async fn current(&self, resource: &str) -> Result<Option<Lease>> {
        let found = graph_rows(&self.graph, query(LEASE_READ).param("resource", resource))
            .await
            .map_err(|e| lease_error(format!("lease read failed: {e}")))?;
        let now_us = stored_now(self.clock.now()).timestamp_micros();
        let Some(row) = found.first() else {
            return Ok(None);
        };
        let slot = lease_slot(row).map_err(|failure| failure.into_adk(lease_error))?;
        slot.filter(|slot| slot.live(now_us)).map(|slot| slot.lease(resource)).transpose()
    }
}

#[async_trait]
impl LeaseStore for Neo4jLeaseStore {
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>> {
        let inline = Inline(self.clone());
        let resource = resource.to_owned();
        let owner = owner.to_owned();
        detached(async move { inline.acquire(&resource, &owner, ttl).await }).await
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
        let inline = Inline(self.clone());
        let lease = lease.clone();
        detached(async move { inline.renew(&lease, ttl).await }).await
    }

    async fn release(&self, lease: &Lease) -> Result<()> {
        let inline = Inline(self.clone());
        let lease = lease.clone();
        detached(async move { inline.release(&lease).await }).await
    }

    async fn current(&self, resource: &str) -> Result<Option<Lease>> {
        let inline = Inline(self.clone());
        let resource = resource.to_owned();
        detached(async move { inline.current(&resource).await }).await
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// Sets every property derived from the record; `$run_id` names the node.
const WRITE_RUN: &str = "MATCH (r:AdkRun {run_id: $run_id}) SET r.record = $record, \
    r.state = $state, r.ready_us = $ready_us, r.finished_us = $finished_us, \
    r.revision = $revision";

const CREATE_RUN: &str = "CREATE (r:AdkRun {run_id: $run_id}) SET r.queue = $queue, \
    r.seq = $seq, r.created_us = $created_us, r.app_name = $app_name, r.user_id = $user_id, \
    r.session_id = $session_id, r.concurrency_key = $concurrency_key, \
    r.idempotency_key = $idempotency_key, r.record = $record, r.state = $state, \
    r.ready_us = $ready_us, r.finished_us = $finished_us, r.revision = $revision";

/// Binds the record-derived properties of [`WRITE_RUN`] and [`CREATE_RUN`].
fn run_query(statement: &str, record: &RunRecord) -> Result<Query> {
    Ok(query(statement)
        .param("run_id", record.run_id.as_str())
        .param("record", encode(record, run_error)?)
        .param("state", state_name(record.state))
        .param("ready_us", ready_micros(record))
        .param(
            "finished_us",
            record.state.is_terminal().then(|| record.updated_at.timestamp_micros()),
        )
        .param("revision", to_i64(record.revision)))
}

fn record_from(row: &Row) -> std::result::Result<RunRecord, Failure> {
    let json: String = read(row, "record", run_error)?;
    Ok(decode(&json, "run record", run_error)?)
}

/// Write-locks a run node inside `txn` and reads its record; `None` for an unknown run.
async fn lock_run(txn: &mut Txn, run_id: &str) -> std::result::Result<Option<RunRecord>, Failure> {
    rows(
        txn,
        query(
            "MATCH (r:AdkRun {run_id: $run_id}) \
             SET r.lock_seq = coalesce(r.lock_seq, 0) + 1 RETURN count(r) AS locked",
        )
        .param("run_id", run_id),
    )
    .await?;
    let found = rows(
        txn,
        query("MATCH (r:AdkRun {run_id: $run_id}) RETURN r.record AS record")
            .param("run_id", run_id),
    )
    .await?;
    found.first().map(record_from).transpose()
}

/// Write-locks a queue's lock node, created beforehand by an auto-commit `MERGE`, inside
/// `txn` and returns its enqueue counter.
async fn lock_queue(txn: &mut Txn, queue: &str) -> std::result::Result<i64, Failure> {
    txn.run(
        query(
            "MATCH (q:AdkRunQueue {queue: $queue}) \
             SET q.lock_seq = coalesce(q.lock_seq, 0) + 1",
        )
        .param("queue", queue),
    )
    .await?;
    let found = rows(
        txn,
        query("MATCH (q:AdkRunQueue {queue: $queue}) RETURN coalesce(q.seq, 0) AS seq")
            .param("queue", queue),
    )
    .await?;
    Ok(found.first().map(|row| read(row, "seq", run_error)).transpose()?.unwrap_or(0))
}

/// Unfinished runs holding `key` in `queue` enqueued before `before_seq`, oldest first.
async fn key_holders(
    txn: &mut Txn,
    queue: &str,
    key: &str,
    before_seq: i64,
) -> std::result::Result<Vec<String>, Failure> {
    let found = rows(
        txn,
        query(
            "MATCH (r:AdkRun {queue: $queue, concurrency_key: $key}) \
             WHERE r.finished_us IS NULL AND r.seq < $before \
             RETURN r.run_id AS run_id ORDER BY r.seq",
        )
        .param("queue", queue)
        .param("key", key)
        .param("before", before_seq),
    )
    .await?;
    found.iter().map(|row| read(row, "run_id", run_error)).collect()
}

/// A run transition: returns the caller's value and whether the record changed.
type RunChange<'a, T> =
    dyn Fn(&mut RunRecord, DateTime<Utc>) -> Result<(T, bool)> + Send + Sync + 'a;

/// Neo4j-backed [`DurableRunStore`].
///
/// Enqueues and claims in one queue serialize on the queue's `AdkRunQueue` node; worker
/// writes lock the run's `AdkRun` node and compare its epoch. See the
/// [module documentation](crate::neo4j_runtime).
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::{DurableRunStore, NewRun};
/// use adk_session::Neo4jRunStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let runs = Neo4jRunStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// runs.migrate().await?;
/// runs.enqueue(NewRun::new("default", "app", "user", "agent", serde_json::json!({}))).await?;
/// let claimed = runs.claim("default", "worker-1", Duration::from_secs(30)).await?;
/// # let _ = claimed;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jRunStore {
    graph: Graph,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Neo4jRunStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jRunStore").field("clock", &self.clock).finish_non_exhaustive()
    }
}

impl Neo4jRunStore {
    /// Connects to Neo4j. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, run_error).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides delays, waits, and lease expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Creates the constraints and indexes the store relies on.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &[
                "CREATE CONSTRAINT adk_run_id_unique IF NOT EXISTS \
                 FOR (r:AdkRun) REQUIRE r.run_id IS UNIQUE",
                "CREATE CONSTRAINT adk_run_idempotency_unique IF NOT EXISTS \
                 FOR (r:AdkRun) REQUIRE (r.queue, r.idempotency_key) IS UNIQUE",
                "CREATE CONSTRAINT adk_run_queue_unique IF NOT EXISTS \
                 FOR (q:AdkRunQueue) REQUIRE q.queue IS UNIQUE",
                "CREATE INDEX adk_run_queue_ready IF NOT EXISTS FOR (r:AdkRun) ON (r.queue, r.ready_us)",
                "CREATE INDEX adk_run_queue_key IF NOT EXISTS \
                 FOR (r:AdkRun) ON (r.queue, r.concurrency_key)",
                "CREATE INDEX adk_run_finished IF NOT EXISTS FOR (r:AdkRun) ON (r.finished_us)",
            ],
            run_error,
        )
        .await
    }

    async fn try_enqueue(&self, run: &NewRun) -> std::result::Result<Enqueued, Failure> {
        self.graph
            .run(query("MERGE (:AdkRunQueue {queue: $queue})").param("queue", run.queue.as_str()))
            .await?;
        let now = stored_now(self.clock.now());
        transaction!(self.graph, |txn| {
            let seq = lock_queue(&mut txn, &run.queue).await? + 1;
            if let Some(key) = &run.idempotency_key {
                let found = rows(
                    &mut txn,
                    query(
                        "MATCH (r:AdkRun {queue: $queue, idempotency_key: $key}) \
                         RETURN r.record AS record",
                    )
                    .param("queue", run.queue.as_str())
                    .param("key", key.as_str()),
                )
                .await?;
                if let Some(row) = found.first() {
                    return Ok(Enqueued::Existing(record_from(row)?));
                }
            }
            if let Some(key) = &run.concurrency_key {
                let holders = key_holders(&mut txn, &run.queue, key, seq).await?;
                match run.on_conflict {
                    OnConflict::Reject => {
                        if let Some(blocking_run_id) = holders.into_iter().next() {
                            return Ok(Enqueued::Rejected { blocking_run_id });
                        }
                    }
                    OnConflict::Interrupt => {
                        for run_id in holders {
                            if let Some(mut holder) = lock_run(&mut txn, &run_id).await?
                                && apply_cancel(&mut holder, now)
                            {
                                txn.run(run_query(WRITE_RUN, &holder)?).await?;
                            }
                        }
                    }
                    _ => {}
                }
            }
            let record = RunRecord::queued(uuid::Uuid::new_v4().to_string(), run.clone(), now);
            txn.run(
                run_query(CREATE_RUN, &record)?
                    .param("queue", record.queue.as_str())
                    .param("seq", seq)
                    .param("created_us", now.timestamp_micros())
                    .param("app_name", record.app_name.as_str())
                    .param("user_id", record.user_id.as_str())
                    .param("session_id", record.session_id.clone())
                    .param("concurrency_key", record.concurrency_key.clone())
                    .param("idempotency_key", record.idempotency_key.clone()),
            )
            .await?;
            txn.run(
                query("MATCH (q:AdkRunQueue {queue: $queue}) SET q.seq = $seq")
                    .param("queue", run.queue.as_str())
                    .param("seq", seq),
            )
            .await?;
            Ok(Enqueued::Created(record))
        })
    }

    async fn try_claim(
        &self,
        queue: &str,
        worker: &str,
        ttl: Duration,
    ) -> std::result::Result<Option<ClaimedRun>, Failure> {
        self.graph.run(query("MERGE (:AdkRunQueue {queue: $queue})").param("queue", queue)).await?;
        let now = stored_now(self.clock.now());
        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
        transaction!(self.graph, |txn| {
            lock_queue(&mut txn, queue).await?;
            let mut after = i64::MIN;
            loop {
                let page = rows(
                    &mut txn,
                    query(
                        "MATCH (r:AdkRun {queue: $queue}) \
                         WHERE r.ready_us <= $now AND r.finished_us IS NULL AND r.seq > $after \
                         RETURN r.run_id AS run_id, r.seq AS seq, r.record AS record \
                         ORDER BY r.seq LIMIT $limit",
                    )
                    .param("queue", queue)
                    .param("now", now.timestamp_micros())
                    .param("after", after)
                    .param("limit", CLAIM_PAGE),
                )
                .await?;
                for row in &page {
                    let seq: i64 = read(row, "seq", run_error)?;
                    after = seq;
                    let seen = record_from(row)?;
                    if claim_step(&seen, now) == ClaimStep::NotReady {
                        continue;
                    }
                    let Some(mut record) = lock_run(&mut txn, &seen.run_id).await? else {
                        continue;
                    };
                    match claim_step(&record, now) {
                        ClaimStep::NotReady => {}
                        ClaimStep::Exhaust => {
                            apply_exhaust(&mut record, now);
                            txn.run(run_query(WRITE_RUN, &record)?).await?;
                        }
                        ClaimStep::Claim { started } => {
                            if record.state == adk_core::run_store::RunState::Queued
                                && let Some(key) = record.concurrency_key.clone()
                                && !key_holders(&mut txn, queue, &key, seq).await?.is_empty()
                            {
                                continue;
                            }
                            let epoch = apply_claim(&mut record, started, worker, now, expires_at);
                            txn.run(run_query(WRITE_RUN, &record)?).await?;
                            return Ok(Some(ClaimedRun { record, epoch }));
                        }
                    }
                }
                if i64::try_from(page.len()).unwrap_or(i64::MAX) < CLAIM_PAGE {
                    return Ok(None);
                }
            }
        })
    }

    /// Locks a run, applies `change` to it, and writes it when `change` reports a change.
    async fn try_update<T>(
        &self,
        run_id: &str,
        change: &RunChange<'_, T>,
    ) -> std::result::Result<T, Failure>
    where
        T: Send,
    {
        let now = stored_now(self.clock.now());
        transaction!(self.graph, |txn| {
            let Some(mut record) = lock_run(&mut txn, run_id).await? else {
                return Err(Failure::Store(run_not_found_error(run_id)));
            };
            let (value, changed) = change(&mut record, now)?;
            if changed {
                txn.run(run_query(WRITE_RUN, &record)?).await?;
            }
            Ok(value)
        })
    }

    async fn update<T: Send>(
        &self,
        run_id: &str,
        change: impl Fn(&mut RunRecord, DateTime<Utc>) -> Result<(T, bool)> + Send + Sync,
    ) -> Result<T> {
        retry(run_error, || self.try_update(run_id, &change)).await
    }
}

#[async_trait]
impl DurableRunStore for Inline<Neo4jRunStore> {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        run.validate()?;
        retry(run_error, || self.try_enqueue(&run)).await
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        deadline(self.clock.now(), ttl, "run_store.invalid_ttl")?;
        retry(run_error, || self.try_claim(queue, worker, ttl)).await
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        deadline(self.clock.now(), ttl, "run_store.invalid_ttl")?;
        self.update(run_id, |record, now| {
            check_fence(record, epoch)?;
            apply_heartbeat(record, now, deadline(now, ttl, "run_store.invalid_ttl")?);
            Ok((record.cancel_requested, true))
        })
        .await
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        self.update(run_id, |record, now| {
            check_fence(record, epoch)?;
            apply_checkpoint(record, now, checkpoint.clone());
            Ok(((), true))
        })
        .await
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        self.update(run_id, |record, now| {
            check_fence(record, epoch)?;
            apply_wait(record, now, until);
            Ok(((), true))
        })
        .await
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        self.update(run_id, |record, now| Ok(((), apply_wake(record, now)))).await
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        self.update(run_id, |record, now| {
            check_fence(record, epoch)?;
            apply_finish(record, now, outcome.clone());
            Ok((record.clone(), true))
        })
        .await
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        self.update(run_id, |record, now| Ok(((), apply_cancel(record, now)))).await
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        let found = graph_rows(
            &self.graph,
            query("MATCH (r:AdkRun {run_id: $run_id}) RETURN r.record AS record")
                .param("run_id", run_id),
        )
        .await
        .map_err(|e| run_error(format!("run read failed: {e}")))?;
        found.first().map(record_from).transpose().map_err(|failure| failure.into_adk(run_error))
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        let mut conditions = vec!["r.record IS NOT NULL".to_string()];
        let fields = [
            ("queue", &filter.queue),
            ("app_name", &filter.app_name),
            ("user_id", &filter.user_id),
            ("session_id", &filter.session_id),
            ("concurrency_key", &filter.concurrency_key),
        ];
        for (name, value) in &fields {
            if value.is_some() {
                conditions.push(format!("r.{name} = ${name}"));
            }
        }
        if !filter.states.is_empty() {
            conditions.push("r.state IN $states".to_string());
        }
        let limit = if filter.limit.is_some() { " LIMIT $limit" } else { "" };
        let statement = format!(
            "MATCH (r:AdkRun) WHERE {} RETURN r.record AS record \
             ORDER BY r.created_us, r.seq, r.run_id{limit}",
            conditions.join(" AND ")
        );
        let mut statement = query(&statement);
        for (name, value) in fields {
            if let Some(value) = value {
                statement = statement.param(name, value.as_str());
            }
        }
        if !filter.states.is_empty() {
            let states: Vec<String> =
                filter.states.iter().map(|state| state_name(*state).to_string()).collect();
            statement = statement.param("states", states);
        }
        if let Some(limit) = filter.limit {
            statement = statement.param("limit", i64::try_from(limit).unwrap_or(i64::MAX));
        }
        let found = graph_rows(&self.graph, statement)
            .await
            .map_err(|e| run_error(format!("run list failed: {e}")))?;
        found
            .iter()
            .map(|row| record_from(row).map_err(|failure| failure.into_adk(run_error)))
            .collect()
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        let cutoff = micros_before(finished_before);
        let mut total = 0u64;
        loop {
            let found = graph_rows(
                &self.graph,
                query(
                    "MATCH (r:AdkRun) WHERE r.finished_us < $cutoff \
                     WITH r LIMIT $batch DETACH DELETE r RETURN count(*) AS deleted",
                )
                .param("cutoff", cutoff)
                .param("batch", PRUNE_BATCH),
            )
            .await
            .map_err(|e| run_error(format!("run prune failed: {e}")))?;
            let deleted: i64 = match found.first() {
                Some(row) => column(row, "deleted").map_err(run_error)?,
                None => 0,
            };
            total += to_u64(deleted);
            if deleted < PRUNE_BATCH {
                return Ok(total);
            }
        }
    }
}

#[async_trait]
impl DurableRunStore for Neo4jRunStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        let inline = Inline(self.clone());
        detached(async move { inline.enqueue(run).await }).await
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        let inline = Inline(self.clone());
        let queue = queue.to_owned();
        let worker = worker.to_owned();
        detached(async move { inline.claim(&queue, &worker, ttl).await }).await
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.heartbeat(&run_id, epoch, ttl).await }).await
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.set_checkpoint(&run_id, epoch, checkpoint).await }).await
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.wait(&run_id, epoch, until).await }).await
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.wake(&run_id).await }).await
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.finish(&run_id, epoch, outcome).await }).await
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.request_cancel(&run_id).await }).await
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        let inline = Inline(self.clone());
        let run_id = run_id.to_owned();
        detached(async move { inline.get(&run_id).await }).await
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        let inline = Inline(self.clone());
        detached(async move { inline.list(filter).await }).await
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        let inline = Inline(self.clone());
        detached(async move { inline.prune(finished_before).await }).await
    }
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

/// When an unpaused schedule is due, in microseconds; `None` when it is never due.
fn due_micros(schedule: &Schedule) -> Option<i64> {
    if schedule.paused { None } else { schedule.next_fire_at.map(|at| at.timestamp_micros()) }
}

const WRITE_SCHEDULE: &str = "MATCH (s:AdkSchedule {id: $id}) SET s.schedule = $schedule, \
    s.order_key = $order_key, s.paused = $paused, s.due_us = $due_us, s.revision = $revision";

fn schedule_query(schedule: &Schedule) -> Result<Query> {
    Ok(query(WRITE_SCHEDULE)
        .param("id", schedule.id.as_str())
        .param("schedule", encode(schedule, schedule_error)?)
        .param("order_key", hex(&schedule.id))
        .param("paused", schedule.paused)
        .param("due_us", due_micros(schedule))
        .param("revision", to_i64(schedule.revision)))
}

fn schedule_from(row: &Row) -> std::result::Result<Option<Schedule>, Failure> {
    read::<Option<String>>(row, "schedule", schedule_error)?
        .map(|json| decode(&json, "schedule", schedule_error).map_err(Failure::Store))
        .transpose()
}

/// Write-locks a schedule node inside `txn` and reads the schedule. An upsert creates the
/// node beforehand with an auto-commit `MERGE`, so the lock is never a lock upgrade.
async fn lock_schedule(txn: &mut Txn, id: &str) -> std::result::Result<Option<Schedule>, Failure> {
    txn.run(
        query("MATCH (s:AdkSchedule {id: $id}) SET s.lock_seq = coalesce(s.lock_seq, 0) + 1")
            .param("id", id),
    )
    .await?;
    let found = rows(
        txn,
        query("MATCH (s:AdkSchedule {id: $id}) RETURN s.schedule AS schedule").param("id", id),
    )
    .await?;
    Ok(match found.first() {
        Some(row) => schedule_from(row)?,
        None => None,
    })
}

/// Neo4j-backed [`ScheduleStore`].
///
/// One `AdkSchedule` node per schedule; `upsert`, `set_paused`, and `advance`
/// write-lock it before comparing. See the [module documentation](crate::neo4j_runtime).
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::ScheduleStore;
/// use adk_session::Neo4jScheduleStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let schedules = Neo4jScheduleStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// schedules.migrate().await?;
/// let due = schedules.due(chrono::Utc::now(), 100).await?;
/// # let _ = due;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jScheduleStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jScheduleStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jScheduleStore").finish_non_exhaustive()
    }
}

impl Neo4jScheduleStore {
    /// Connects to Neo4j. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, schedule_error).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Creates the uniqueness constraint and the due-time index.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &[
                "CREATE CONSTRAINT adk_schedule_id_unique IF NOT EXISTS \
                 FOR (s:AdkSchedule) REQUIRE s.id IS UNIQUE",
                "CREATE INDEX adk_schedule_due IF NOT EXISTS FOR (s:AdkSchedule) ON (s.due_us)",
            ],
            schedule_error,
        )
        .await
    }

    async fn try_upsert(&self, incoming: &Schedule) -> std::result::Result<Schedule, Failure> {
        self.graph
            .run(query("MERGE (:AdkSchedule {id: $id})").param("id", incoming.id.as_str()))
            .await?;
        transaction!(self.graph, |txn| {
            let stored = lock_schedule(&mut txn, &incoming.id).await?;
            if let Some(stored) = &stored
                && same_definition(stored, incoming)
            {
                return Ok(stored.clone());
            }
            let schedule = Schedule {
                revision: stored.map_or(1, |stored| stored.revision + 1),
                ..incoming.clone()
            };
            txn.run(schedule_query(&schedule)?).await?;
            Ok(schedule)
        })
    }

    /// Locks a schedule and applies `change`, writing it when the change returns
    /// `Some`; returns `change`'s verdict or `schedule.not_found`.
    async fn try_change<T>(
        &self,
        id: &str,
        change: &(dyn Fn(&Schedule) -> (T, Option<Schedule>) + Send + Sync),
    ) -> std::result::Result<T, Failure>
    where
        T: Send,
    {
        transaction!(self.graph, |txn| {
            let Some(stored) = lock_schedule(&mut txn, id).await? else {
                return Err(Failure::Store(schedule_not_found_error(id)));
            };
            let (value, changed) = change(&stored);
            if let Some(changed) = changed {
                txn.run(schedule_query(&changed)?).await?;
            }
            Ok(value)
        })
    }

    async fn try_delete(&self, id: &str) -> std::result::Result<bool, Failure> {
        transaction!(self.graph, |txn| {
            if lock_schedule(&mut txn, id).await?.is_none() {
                return Ok(false);
            }
            txn.run(query("MATCH (s:AdkSchedule {id: $id}) DETACH DELETE s").param("id", id))
                .await?;
            Ok(true)
        })
    }

    async fn read_schedules(&self, statement: Query) -> Result<Vec<Schedule>> {
        let found = graph_rows(&self.graph, statement)
            .await
            .map_err(|e| schedule_error(format!("schedule read failed: {e}")))?;
        let mut schedules = Vec::new();
        for row in &found {
            if let Some(schedule) =
                schedule_from(row).map_err(|failure| failure.into_adk(schedule_error))?
            {
                schedules.push(schedule);
            }
        }
        Ok(schedules)
    }
}

#[async_trait]
impl ScheduleStore for Inline<Neo4jScheduleStore> {
    async fn upsert(&self, schedule: Schedule) -> Result<Schedule> {
        schedule.validate()?;
        retry(schedule_error, || self.try_upsert(&schedule)).await
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        let found = self
            .read_schedules(
                query("MATCH (s:AdkSchedule {id: $id}) RETURN s.schedule AS schedule")
                    .param("id", id),
            )
            .await?;
        Ok(found.into_iter().next())
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        self.read_schedules(query(
            "MATCH (s:AdkSchedule) WHERE s.schedule IS NOT NULL \
             RETURN s.schedule AS schedule ORDER BY s.order_key",
        ))
        .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        if retry(schedule_error, || self.try_delete(id)).await? {
            Ok(())
        } else {
            Err(schedule_not_found_error(id))
        }
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
        let change = |stored: &Schedule| {
            if stored.paused == paused {
                return (stored.clone(), None);
            }
            let changed = Schedule { paused, revision: stored.revision + 1, ..stored.clone() };
            (changed.clone(), Some(changed))
        };
        retry(schedule_error, || self.try_change(id, &change)).await
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
        self.read_schedules(
            query(
                "MATCH (s:AdkSchedule) WHERE s.due_us <= $now AND s.schedule IS NOT NULL \
                 RETURN s.schedule AS schedule ORDER BY s.due_us, s.order_key LIMIT $limit",
            )
            .param("now", now.timestamp_micros())
            .param("limit", i64::try_from(limit).unwrap_or(i64::MAX)),
        )
        .await
    }

    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let change = |stored: &Schedule| {
            if stored.revision != expected_revision {
                return (false, None);
            }
            let changed =
                Schedule { next_fire_at, revision: stored.revision + 1, ..stored.clone() };
            (true, Some(changed))
        };
        retry(schedule_error, || self.try_change(id, &change)).await
    }
}

#[async_trait]
impl ScheduleStore for Neo4jScheduleStore {
    async fn upsert(&self, schedule: Schedule) -> Result<Schedule> {
        let inline = Inline(self.clone());
        detached(async move { inline.upsert(schedule).await }).await
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        let inline = Inline(self.clone());
        let id = id.to_owned();
        detached(async move { inline.get(&id).await }).await
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        let inline = Inline(self.clone());
        detached(async move { inline.list().await }).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let inline = Inline(self.clone());
        let id = id.to_owned();
        detached(async move { inline.delete(&id).await }).await
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
        let inline = Inline(self.clone());
        let id = id.to_owned();
        detached(async move { inline.set_paused(&id, paused).await }).await
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
        let inline = Inline(self.clone());
        detached(async move { inline.due(now, limit).await }).await
    }

    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let inline = Inline(self.clone());
        let id = id.to_owned();
        detached(async move { inline.advance(&id, expected_revision, next_fire_at).await }).await
    }
}

// ---------------------------------------------------------------------------
// Governance state
// ---------------------------------------------------------------------------

fn governance_from(row: &Row) -> std::result::Result<Option<GovernanceState>, Failure> {
    read::<Option<String>>(row, "state", governance_error)?
        .map(|json| decode(&json, "governance state", governance_error).map_err(Failure::Store))
        .transpose()
}

/// Neo4j-backed [`GovernanceStateStore`].
///
/// One `AdkGovernanceState` node per scope; `set` write-locks it before comparing the
/// epoch. See the [module documentation](crate::neo4j_runtime).
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{GovernanceScope, GovernanceStateStore};
/// use adk_session::Neo4jGovernanceStateStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store =
///     Neo4jGovernanceStateStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// store.migrate().await?;
/// let frozen = store.set(GovernanceScope::Org, 0, true, Some("incident".into()), None).await?;
/// assert_eq!(frozen.epoch, 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jGovernanceStateStore {
    graph: Graph,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Neo4jGovernanceStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jGovernanceStateStore")
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl Neo4jGovernanceStateStore {
    /// Connects to Neo4j. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, governance_error).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps `issued_at`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Creates the uniqueness constraint on the scope key.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &["CREATE CONSTRAINT adk_governance_key_unique IF NOT EXISTS \
               FOR (g:AdkGovernanceState) REQUIRE g.key IS UNIQUE"],
            governance_error,
        )
        .await
    }

    async fn try_set(
        &self,
        scope: &GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: &Option<String>,
        issued_by: &Option<SubjectRef>,
    ) -> std::result::Result<GovernanceState, Failure> {
        let key = scope.key();
        self.graph
            .run(query("MERGE (:AdkGovernanceState {key: $key})").param("key", key.as_str()))
            .await?;
        let now = stored_now(self.clock.now());
        transaction!(self.graph, |txn| {
            txn.run(
                query(
                    "MATCH (g:AdkGovernanceState {key: $key}) \
                     SET g.lock_seq = coalesce(g.lock_seq, 0) + 1",
                )
                .param("key", key.as_str()),
            )
            .await?;
            let found = rows(
                &mut txn,
                query("MATCH (g:AdkGovernanceState {key: $key}) RETURN g.state AS state")
                    .param("key", key.as_str()),
            )
            .await?;
            let current = match found.first() {
                Some(row) => governance_from(row)?.map_or(0, |state| state.epoch),
                None => 0,
            };
            if current != expected_epoch {
                return Err(Failure::Store(governance_conflict_error(
                    scope,
                    expected_epoch,
                    current,
                )));
            }
            let state = GovernanceState {
                scope: scope.clone(),
                frozen,
                reason: reason.clone(),
                issued_by: issued_by.clone(),
                issued_at: now,
                epoch: current + 1,
            };
            txn.run(
                query(
                    "MATCH (g:AdkGovernanceState {key: $key}) \
                     SET g.state = $state, g.order_key = $order_key, g.epoch = $epoch",
                )
                .param("key", key.as_str())
                .param("state", encode(&state, governance_error)?)
                .param("order_key", hex(&key))
                .param("epoch", to_i64(state.epoch)),
            )
            .await?;
            Ok(state)
        })
    }

    async fn read_states(&self, statement: Query) -> Result<Vec<GovernanceState>> {
        let found = graph_rows(&self.graph, statement)
            .await
            .map_err(|e| governance_error(format!("governance read failed: {e}")))?;
        let mut states = Vec::new();
        for row in &found {
            if let Some(state) =
                governance_from(row).map_err(|failure| failure.into_adk(governance_error))?
            {
                states.push(state);
            }
        }
        Ok(states)
    }
}

#[async_trait]
impl GovernanceStateStore for Inline<Neo4jGovernanceStateStore> {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        let found = self
            .read_states(
                query("MATCH (g:AdkGovernanceState {key: $key}) RETURN g.state AS state")
                    .param("key", scope.key()),
            )
            .await?;
        Ok(found.into_iter().next())
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        retry(governance_error, || {
            self.try_set(&scope, expected_epoch, frozen, &reason, &issued_by)
        })
        .await
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        self.read_states(query(
            "MATCH (g:AdkGovernanceState) WHERE g.state IS NOT NULL \
             RETURN g.state AS state ORDER BY g.order_key",
        ))
        .await
    }
}

#[async_trait]
impl GovernanceStateStore for Neo4jGovernanceStateStore {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        let inline = Inline(self.clone());
        let scope = scope.clone();
        detached(async move { inline.get(&scope).await }).await
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        let inline = Inline(self.clone());
        detached(async move { inline.set(scope, expected_epoch, frozen, reason, issued_by).await })
            .await
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        let inline = Inline(self.clone());
        detached(async move { inline.list().await }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::run_store::RunState;
    use chrono::TimeZone;

    fn slot(owner: &str, epoch: u64, expires_us: i64, released: bool) -> LeaseSlot {
        LeaseSlot { owner: owner.into(), epoch, expires_us, released }
    }

    #[test]
    fn grants_follow_the_in_memory_lease_rules() {
        assert_eq!(grant_epoch(None, "w1", 0), Some(1), "a resource never leased starts at 1");
        let held = slot("w1", 3, 100, false);
        assert_eq!(grant_epoch(Some(&held), "w2", 50), None, "another owner is refused");
        assert_eq!(grant_epoch(Some(&held), "w1", 50), Some(4), "the owner is granted again");
        assert_eq!(grant_epoch(Some(&held), "w2", 100), Some(4), "an expired lease is taken");
        let released = slot("w1", 3, 100, true);
        assert_eq!(grant_epoch(Some(&released), "w2", 50), Some(4), "a released lease is free");
    }

    #[test]
    fn prune_cutoffs_keep_strict_before() {
        let at = Utc.timestamp_opt(1_800_000_000, 0).single().unwrap();
        assert_eq!(micros_before(at), at.timestamp_micros());
        let later = at + chrono::Duration::nanoseconds(500);
        assert_eq!(micros_before(later), at.timestamp_micros() + 1);
    }

    #[test]
    fn paused_and_finished_schedules_are_never_due() {
        let at = Utc.timestamp_opt(1_800_000_000, 0).single().unwrap();
        let schedule = Schedule::new(
            "s",
            adk_core::schedule::ScheduleSpec::At(at),
            NewRun::new("q", "a", "u", "t", serde_json::Value::Null),
            adk_core::schedule::MissedFirePolicy::FireOnce,
        );
        assert_eq!(due_micros(&schedule), None);
        let next = schedule.clone().with_next_fire_at(at);
        assert_eq!(due_micros(&next), Some(at.timestamp_micros()));
        assert_eq!(due_micros(&next.with_paused(true)), None);
    }

    #[test]
    fn run_properties_mark_finished_runs() {
        let now = Utc.timestamp_opt(1_800_000_000, 0).single().unwrap();
        let mut record =
            RunRecord::queued("r", NewRun::new("q", "a", "u", "t", serde_json::Value::Null), now);
        assert!(run_query(WRITE_RUN, &record).is_ok());
        record.state = RunState::Completed;
        assert!(record.state.is_terminal());
        assert_eq!(state_name(record.state), "completed");
    }
}
