//! MongoDB-backed [`LeaseStore`](adk_core::LeaseStore), [`DurableRunStore`](adk_core::DurableRunStore), [`ScheduleStore`](adk_core::ScheduleStore), and
//! [`GovernanceStateStore`](adk_core::GovernanceStateStore).
//!
//! Every store works on a standalone `mongod` as well as on a replica set or sharded
//! cluster. Run enqueues use a multi-document transaction where the server supports one
//! (see [Enqueue paths](#enqueue-paths)); every other guarantee rests on a
//! single-document atomic write — a filtered `findOneAndUpdate`, a revision-filtered
//! replace, or a unique index.
//!
//! # Collections
//!
//! | Collection | `_id` | Contents |
//! |------------|-------|----------|
//! | `adk_leases` | Resource name | `owner`, `epoch`, `expires_us`, `released` |
//! | `adk_runs` | Run ID (UUID v4) | `record` (the [`RunRecord`](adk_core::run_store::RunRecord) as JSON, the source of truth) plus the queried fields `queue`, `seq`, `state`, `ready_us`, `finished_us`, `idempotency_key`, `concurrency_key`, `app_name`, `user_id`, `session_id`, `revision` |
//! | `adk_run_counters` | `seq` | The counter that orders runs by enqueue |
//! | `adk_run_keys` | Hex of the queue and the concurrency key | `holders`: unfinished runs holding the key, in admission order; `pending_cancel` while an `Interrupt` is applied |
//! | `adk_schedules` | Schedule ID | `schedule` (JSON), `paused`, `due_us`, `revision` |
//! | `adk_governance` | [`GovernanceScope::key`](adk_core::governance_state::GovernanceScope::key) | `state` (JSON), `epoch` |
//!
//! Times are stored as integer microseconds since the epoch; stores truncate their
//! clock to microseconds, so records read back exactly.
//!
//! # Atomicity
//!
//! | Operation | Mechanism |
//! |-----------|-----------|
//! | Lease `acquire` | `findOneAndUpdate` with `upsert`, filtered on `released`, expiry, or the same owner; a duplicate-key error on the upsert means another owner holds the lease |
//! | Lease `renew`, `release` | `findOneAndUpdate` / `updateOne` filtered on `epoch` and `owner` |
//! | Idempotent `enqueue` | Unique partial index on `(queue, idempotency_key)`; a duplicate-key error returns the existing run |
//! | `claim`, fenced writes, `wake`, `request_cancel` | Read the record, apply the transition, and replace it filtered on its `revision`; a lost compare-and-swap re-reads |
//! | Concurrency keys | The key's `adk_run_keys` document: only its first unfinished holder is claimable while `Queued`; `Reject` admits a run only while `holders` is empty; `Interrupt` swaps `holders` by exact match and parks the cancelled runs in `pending_cancel`, which every reader applies before deciding |
//! | Schedule `upsert`, `advance`, `set_paused` | Replace filtered on `revision`; an insert for revision 1 |
//! | Governance `set` | `insertOne` for epoch 0, `updateOne` filtered on `epoch` otherwise |
//!
//! # Enqueue paths
//!
//! | Server | Enqueue | Guarantee |
//! |--------|---------|-----------|
//! | Replica set (one node is enough) or sharded cluster | One multi-document transaction: idempotency check, key holders' policy, insert, and admission to the key commit together | No window: a caller never receives a run that is later withdrawn, and a key never admits a second run under `Reject` |
//! | Standalone `mongod` (fallback) | Single-document steps: insert, then admission to the key | Weaker; see below |
//!
//! [`MongoRunStore::supports_transactions`] picks the path from the server's `hello`
//! reply, or from [`MongoRunStore::with_transactions`].
//!
//! On the standalone fallback a keyed run is inserted before it is admitted to its key,
//! which leaves two windows:
//!
//! - A `Reject` enqueue that loses the admission check deletes the run it inserted. A
//!   concurrent enqueue with the same idempotency key can receive that run as
//!   [`Enqueued::Existing`](adk_core::run_store::Enqueued::Existing) in the moment before it is deleted.
//! - A process that stops between inserting a keyed run and admitting it leaves the
//!   run `Queued` but not yet admitted; a claim admits it at the back of its key once it
//!   is a minute old. An `Interrupt` lost this way does not cancel the earlier holders.

use std::sync::Arc;
use std::time::Duration;

use adk_core::governance_state::{
    GovernanceScope, GovernanceState, GovernanceStateStore, governance_conflict_error,
};
use adk_core::lease::{Lease, LeaseStore, lease_stale_epoch_error};
use adk_core::principal::SubjectRef;
use adk_core::run_store::{
    ClaimedRun, DurableRunStore, Enqueued, NewRun, OnConflict, RunCheckpointRef, RunFilter,
    RunOutcome, RunRecord, RunState, run_not_found_error,
};
use adk_core::schedule::{Schedule, ScheduleStore, schedule_not_found_error};
use adk_core::{Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mongodb::bson::{Bson, Document, doc};
use mongodb::error::{TRANSIENT_TRANSACTION_ERROR, UNKNOWN_TRANSACTION_COMMIT_RESULT};
use mongodb::options::{IndexOptions, ReturnDocument};
use mongodb::{ClientSession, Collection, Database, IndexModel};

use crate::durable_common::{
    GOVERNANCE_STORAGE, LEASE_STORAGE, RUN_STORAGE, SCHEDULE_STORAGE, deadline, hex,
    same_definition, state_name, storage_error, stored_now,
};
use crate::durable_transitions::{
    ClaimStep, apply_cancel, apply_checkpoint, apply_claim, apply_exhaust, apply_finish,
    apply_heartbeat, apply_wait, apply_wake, check_fence, claim_step, ready_micros,
};
use crate::mongodb_stores::{connect, is_duplicate_key, read_i64};

const LEASES: &str = "adk_leases";
const RUNS: &str = "adk_runs";
const RUN_COUNTERS: &str = "adk_run_counters";
const RUN_KEYS: &str = "adk_run_keys";
const SCHEDULES: &str = "adk_schedules";
const GOVERNANCE: &str = "adk_governance";

/// Compare-and-swap attempts before an operation reports contention as a failure.
const MAX_CAS_ATTEMPTS: u32 = 64;

/// Candidates a claim reads per query.
const CLAIM_PAGE: i64 = 64;

/// How old a keyed run that was never admitted to its key must be before a claim
/// admits it, so an enqueue still in flight is left to finish.
const ADMIT_GRACE: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

/// The smallest whole microsecond at or after `at`, so `stored < at` is `stored_us < ceil`.
fn ceil_micros(at: DateTime<Utc>) -> i64 {
    let floor = at.timestamp_micros();
    if at.timestamp_subsec_nanos().is_multiple_of(1_000) { floor } else { floor.saturating_add(1) }
}

fn from_micros(value: i64, code: &'static str) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(value)
        .ok_or_else(|| storage_error(code, format!("stored time {value} is out of range")))
}

async fn ping(db: &Database, code: &'static str) -> Result<()> {
    db.run_command(doc! { "ping": 1 })
        .await
        .map(|_| ())
        .map_err(|e| storage_error(code, format!("mongodb ping failed: {e}")))
}

async fn open(
    connection_string: &str,
    database_name: &str,
    code: &'static str,
) -> Result<Database> {
    connect(connection_string, database_name)
        .await
        .map_err(|e| storage_error(code, format!("mongodb connection failed: {e}")))
}

async fn create_index(
    collection: &Collection<Document>,
    keys: Document,
    options: IndexOptions,
    code: &'static str,
) -> Result<()> {
    collection
        .create_index(IndexModel::builder().keys(keys).options(options).build())
        .await
        .map(|_| ())
        .map_err(|e| storage_error(code, format!("mongodb index creation failed: {e}")))
}

fn named(name: &str) -> IndexOptions {
    IndexOptions::builder().name(name.to_string()).build()
}

async fn collect(
    mut cursor: mongodb::Cursor<Document>,
    code: &'static str,
) -> Result<Vec<Document>> {
    let mut documents = Vec::new();
    while cursor.advance().await.map_err(|e| storage_error(code, e))? {
        documents.push(cursor.deserialize_current().map_err(|e| storage_error(code, e))?);
    }
    Ok(documents)
}

fn field_str<'a>(document: &'a Document, field: &str, code: &'static str) -> Result<&'a str> {
    document
        .get_str(field)
        .map_err(|_| storage_error(code, format!("stored field '{field}' is missing or corrupt")))
}

fn field_i64(document: &Document, field: &str, code: &'static str) -> Result<i64> {
    read_i64(document, field)
        .ok_or_else(|| storage_error(code, format!("stored field '{field}' is missing or corrupt")))
}

fn decode<T: serde::de::DeserializeOwned>(
    document: &Document,
    field: &str,
    code: &'static str,
) -> Result<T> {
    serde_json::from_str(field_str(document, field, code)?)
        .map_err(|e| storage_error(code, format!("stored field '{field}' is corrupt: {e}")))
}

fn encode<T: serde::Serialize>(value: &T, code: &'static str) -> Result<String> {
    serde_json::to_string(value).map_err(|e| storage_error(code, format!("serialize failed: {e}")))
}

fn to_i64(value: u64, code: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| storage_error(code, format!("{value} does not fit in i64")))
}

fn optional(value: Option<&str>) -> Bson {
    value.map_or(Bson::Null, |value| Bson::String(value.to_string()))
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/// MongoDB-backed [`LeaseStore`].
///
/// One `adk_leases` document per resource, kept after release so epochs never repeat.
/// Expiry is compared inside each filtered write, never left to a TTL index.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::LeaseStore;
/// use adk_session::MongoLeaseStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = MongoLeaseStore::new("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// let lease = store.acquire("scheduler", "worker-1", Duration::from_secs(30)).await?;
/// # let _ = lease;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoLeaseStore {
    db: Database,
    clock: Arc<dyn Clock>,
}

impl MongoLeaseStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(open(connection_string, database_name, LEASE_STORAGE).await?))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Checks that the server is reachable. Leases need no index beyond `_id`.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the server cannot be reached.
    pub async fn migrate(&self) -> Result<()> {
        ping(&self.db, LEASE_STORAGE).await
    }

    fn collection(&self) -> Collection<Document> {
        self.db.collection(LEASES)
    }
}

fn lease_from(resource: &str, document: &Document) -> Result<Lease> {
    let epoch = field_i64(document, "epoch", LEASE_STORAGE)?;
    Ok(Lease {
        resource: resource.to_string(),
        owner: field_str(document, "owner", LEASE_STORAGE)?.to_string(),
        epoch: u64::try_from(epoch).map_err(|_| {
            storage_error(LEASE_STORAGE, format!("stored epoch {epoch} is negative"))
        })?,
        expires_at: from_micros(field_i64(document, "expires_us", LEASE_STORAGE)?, LEASE_STORAGE)?,
    })
}

#[async_trait]
impl LeaseStore for MongoLeaseStore {
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>> {
        let now = stored_now(self.clock.now());
        let expires_at = deadline(now, ttl, "lease.invalid_ttl")?;
        let result = self
            .collection()
            .find_one_and_update(
                doc! {
                    "_id": resource,
                    "$or": [
                        { "released": true },
                        { "expires_us": { "$lte": micros(now) } },
                        { "owner": owner },
                    ],
                },
                doc! {
                    "$inc": { "epoch": 1_i64 },
                    "$set": { "owner": owner, "expires_us": micros(expires_at), "released": false },
                },
            )
            .upsert(true)
            .return_document(ReturnDocument::After)
            .await;
        match result {
            Ok(Some(document)) => lease_from(resource, &document).map(Some),
            Ok(None) => Ok(None),
            // The resource exists and another owner holds it, so the upsert's insert collided.
            Err(error) if is_duplicate_key(&error) => Ok(None),
            Err(error) => {
                Err(storage_error(LEASE_STORAGE, format!("lease acquire failed: {error}")))
            }
        }
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
        let expires_at = deadline(stored_now(self.clock.now()), ttl, "lease.invalid_ttl")?;
        let renewed = self
            .collection()
            .find_one_and_update(
                doc! {
                    "_id": &lease.resource,
                    "epoch": to_i64(lease.epoch, LEASE_STORAGE)?,
                    "owner": &lease.owner,
                    "released": false,
                },
                doc! { "$set": { "expires_us": micros(expires_at) } },
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease renew failed: {e}")))?;
        match renewed {
            Some(document) => lease_from(&lease.resource, &document),
            None => Err(lease_stale_epoch_error(&lease.resource, lease.epoch)),
        }
    }

    async fn release(&self, lease: &Lease) -> Result<()> {
        let Ok(epoch) = i64::try_from(lease.epoch) else {
            return Ok(());
        };
        self.collection()
            .update_one(
                doc! { "_id": &lease.resource, "epoch": epoch, "owner": &lease.owner },
                doc! { "$set": { "released": true } },
            )
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease release failed: {e}")))?;
        Ok(())
    }

    async fn current(&self, resource: &str) -> Result<Option<Lease>> {
        let now = stored_now(self.clock.now());
        let Some(document) = self
            .collection()
            .find_one(doc! { "_id": resource })
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease read failed: {e}")))?
        else {
            return Ok(None);
        };
        if document.get_bool("released").unwrap_or(false) {
            return Ok(None);
        }
        let lease = lease_from(resource, &document)?;
        Ok((lease.expires_at > now).then_some(lease))
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// MongoDB-backed [`DurableRunStore`].
///
/// See the [module documentation](self) for the collections and how each guarantee is
/// kept without transactions. Claims return the oldest claimable run by enqueue order.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::{DurableRunStore, NewRun};
/// use adk_session::MongoRunStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = MongoRunStore::new("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// store.enqueue(NewRun::new("default", "app", "user", "agent", serde_json::json!({}))).await?;
/// let claimed = store.claim("default", "worker-1", Duration::from_secs(30)).await?;
/// # let _ = claimed;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoRunStore {
    db: Database,
    clock: Arc<dyn Clock>,
    /// Set by `with_transactions`; otherwise detected from the server's `hello` reply.
    transactions: Option<bool>,
    detected: Arc<tokio::sync::OnceCell<bool>>,
}

/// Why a transactional enqueue attempt stopped.
enum TxFailure {
    Mongo(mongodb::error::Error),
    Store(adk_core::AdkError),
    /// A document changed under the attempt; the transaction is retried.
    Retry,
}

impl From<mongodb::error::Error> for TxFailure {
    fn from(error: mongodb::error::Error) -> Self {
        Self::Mongo(error)
    }
}

impl From<adk_core::AdkError> for TxFailure {
    fn from(error: adk_core::AdkError) -> Self {
        Self::Store(error)
    }
}

fn run_document(record: &RunRecord, seq: i64) -> Result<Document> {
    let terminal = record.state.is_terminal();
    let mut document = doc! {
        "_id": &record.run_id,
        "record": encode(record, RUN_STORAGE)?,
        "queue": &record.queue,
        "seq": seq,
        "state": state_name(record.state),
        "ready_us": if terminal { Bson::Null } else { Bson::Int64(ready_micros(record)) },
        "finished_us": if terminal { Bson::Int64(micros(record.updated_at)) } else { Bson::Null },
        "concurrency_key": optional(record.concurrency_key.as_deref()),
        "app_name": &record.app_name,
        "user_id": &record.user_id,
        "session_id": optional(record.session_id.as_deref()),
        "revision": to_i64(record.revision, RUN_STORAGE)?,
    };
    // Absent rather than null, so the partial unique index skips runs without a key.
    if let Some(key) = &record.idempotency_key {
        document.insert("idempotency_key", key.as_str());
    }
    Ok(document)
}

fn parse_run(document: &Document) -> Result<(RunRecord, i64)> {
    Ok((decode(document, "record", RUN_STORAGE)?, field_i64(document, "seq", RUN_STORAGE)?))
}

/// The `_id` of a concurrency key's gate document.
fn gate_id(queue: &str, key: &str) -> String {
    format!("{}:{}", hex(queue), hex(key))
}

fn string_array(document: &Document, field: &str) -> Option<Vec<String>> {
    document.get_array(field).ok().map(|values| {
        values.iter().filter_map(|value| value.as_str().map(str::to_string)).collect()
    })
}

fn storage(error: impl std::fmt::Display) -> adk_core::AdkError {
    storage_error(RUN_STORAGE, error)
}

impl MongoRunStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(open(connection_string, database_name, RUN_STORAGE).await?))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self {
            db,
            clock: Arc::new(SystemClock),
            transactions: None,
            detected: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Sets the clock that decides delays, waits, and lease expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Forces the enqueue path: `true` runs every enqueue in a multi-document
    /// transaction (needs a replica set or a sharded cluster), `false` uses the
    /// single-document fallback. Without it the store asks the server with `hello`.
    pub fn with_transactions(mut self, enabled: bool) -> Self {
        self.transactions = Some(enabled);
        self
    }

    /// Whether enqueues run in a multi-document transaction: the
    /// [`with_transactions`](Self::with_transactions) setting, or detected once from
    /// `hello` — a replica set member (`setName`) or a `mongos` router supports
    /// transactions, a standalone server does not.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_session::MongoRunStore;
    ///
    /// # async fn demo() -> adk_core::Result<()> {
    /// let store = MongoRunStore::new("mongodb://localhost:27017/?replicaSet=rs0", "adk").await?;
    /// assert!(store.supports_transactions().await?);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when the server cannot be asked.
    pub async fn supports_transactions(&self) -> Result<bool> {
        if let Some(enabled) = self.transactions {
            return Ok(enabled);
        }
        self.detected
            .get_or_try_init(|| async {
                let hello = self
                    .db
                    .run_command(doc! { "hello": 1 })
                    .await
                    .map_err(|e| storage(format!("mongodb hello failed: {e}")))?;
                Ok(hello.contains_key("setName") || hello.get_str("msg") == Ok("isdbgrid"))
            })
            .await
            .copied()
    }

    /// Creates the unique idempotency index and the indexes claims, listings, and
    /// pruning read through, and detects whether the server supports transactions.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when an index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        let runs = self.runs();
        create_index(
            &runs,
            doc! { "queue": 1, "idempotency_key": 1 },
            IndexOptions::builder()
                .name("adk_runs_idempotency".to_string())
                .unique(true)
                .partial_filter_expression(doc! { "idempotency_key": { "$type": "string" } })
                .build(),
            RUN_STORAGE,
        )
        .await?;
        create_index(
            &runs,
            doc! { "queue": 1, "seq": 1 },
            named("adk_runs_queue_seq"),
            RUN_STORAGE,
        )
        .await?;
        create_index(
            &runs,
            doc! { "queue": 1, "ready_us": 1 },
            named("adk_runs_queue_ready"),
            RUN_STORAGE,
        )
        .await?;
        create_index(&runs, doc! { "finished_us": 1 }, named("adk_runs_finished"), RUN_STORAGE)
            .await?;
        self.supports_transactions().await.map(drop)
    }

    fn runs(&self) -> Collection<Document> {
        self.db.collection(RUNS)
    }

    fn gates(&self) -> Collection<Document> {
        self.db.collection(RUN_KEYS)
    }

    fn now(&self) -> DateTime<Utc> {
        stored_now(self.clock.now())
    }

    async fn next_seq(&self) -> Result<i64> {
        for _ in 0..2 {
            let result = self
                .db
                .collection::<Document>(RUN_COUNTERS)
                .find_one_and_update(doc! { "_id": "seq" }, doc! { "$inc": { "seq": 1_i64 } })
                .upsert(true)
                .return_document(ReturnDocument::After)
                .await;
            match result {
                Ok(Some(document)) => return field_i64(&document, "seq", RUN_STORAGE),
                Ok(None) => {}
                // Two first enqueues raced to create the counter; the retry increments it.
                Err(error) if is_duplicate_key(&error) => {}
                Err(error) => return Err(storage(format!("run counter update failed: {error}"))),
            }
        }
        Err(storage("run counter could not be created"))
    }

    async fn read(&self, run_id: &str) -> Result<Option<(RunRecord, i64)>> {
        self.runs()
            .find_one(doc! { "_id": run_id })
            .await
            .map_err(|e| storage(format!("run read failed: {e}")))?
            .as_ref()
            .map(parse_run)
            .transpose()
    }

    /// Replaces the run when its stored revision is still `expected`.
    async fn swap(&self, record: &RunRecord, seq: i64, expected: u64) -> Result<bool> {
        let replaced = self
            .runs()
            .replace_one(
                doc! { "_id": &record.run_id, "revision": to_i64(expected, RUN_STORAGE)? },
                run_document(record, seq)?,
            )
            .await
            .map_err(|e| storage(format!("run write failed: {e}")))?;
        Ok(replaced.matched_count == 1)
    }

    /// Reads the run, lets `change` decide, and writes the result under a revision
    /// compare-and-swap, re-reading when another write landed first. `change` returns
    /// whether it modified the record.
    async fn modify(
        &self,
        run_id: &str,
        mut change: impl FnMut(&mut RunRecord) -> Result<bool> + Send,
    ) -> Result<RunRecord> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some((mut record, seq)) = self.read(run_id).await? else {
                return Err(run_not_found_error(run_id));
            };
            let expected = record.revision;
            if !change(&mut record)? {
                return Ok(record);
            }
            if self.swap(&record, seq, expected).await? {
                if record.state.is_terminal() {
                    self.leave_key(&record).await?;
                }
                return Ok(record);
            }
        }
        Err(storage(format!(
            "run '{run_id}' lost {MAX_CAS_ATTEMPTS} compare-and-swap attempts to concurrent writes"
        )))
    }

    /// Removes a finished run from its concurrency key's holders.
    async fn leave_key(&self, record: &RunRecord) -> Result<()> {
        if let Some(key) = &record.concurrency_key {
            self.gates()
                .update_one(
                    doc! { "_id": gate_id(&record.queue, key) },
                    doc! { "$pull": { "holders": &record.run_id } },
                )
                .await
                .map_err(|e| storage(format!("concurrency key update failed: {e}")))?;
        }
        Ok(())
    }

    /// Cancels one run as part of an `Interrupt`; a run already gone is skipped.
    async fn cancel_holder(&self, run_id: &str, now: DateTime<Utc>) -> Result<()> {
        match self.modify(run_id, |record| Ok(apply_cancel(record, now))).await {
            Err(error) if error.is_not_found() => Ok(()),
            other => other.map(|_| ()),
        }
    }

    /// Applies any pending `Interrupt`, drops finished runs from the head of the key's
    /// holders, and returns the stored holders.
    async fn settle_key(&self, queue: &str, key: &str) -> Result<Vec<String>> {
        let id = gate_id(queue, key);
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(gate) = self
                .gates()
                .find_one(doc! { "_id": &id })
                .await
                .map_err(|e| storage(format!("concurrency key read failed: {e}")))?
            else {
                return Ok(Vec::new());
            };
            if let Some(pending) = string_array(&gate, "pending_cancel") {
                let now = self.now();
                for run_id in &pending {
                    self.cancel_holder(run_id, now).await?;
                }
                self.gates()
                    .update_one(
                        doc! { "_id": &id, "pending_cancel": &pending },
                        doc! { "$unset": { "pending_cancel": "" } },
                    )
                    .await
                    .map_err(|e| storage(format!("concurrency key update failed: {e}")))?;
                continue;
            }
            let mut holders = string_array(&gate, "holders").unwrap_or_default();
            while let Some(head) = holders.first().cloned() {
                let finished = match self.read(&head).await? {
                    Some((record, _)) => record.state.is_terminal(),
                    // Pruned, so it finished.
                    None => true,
                };
                if !finished {
                    break;
                }
                self.gates()
                    .update_one(doc! { "_id": &id }, doc! { "$pull": { "holders": &head } })
                    .await
                    .map_err(|e| storage(format!("concurrency key update failed: {e}")))?;
                holders.remove(0);
            }
            return Ok(holders);
        }
        Err(storage(format!(
            "concurrency key '{key}' kept changing under {MAX_CAS_ATTEMPTS} reads"
        )))
    }

    /// Appends `run_id` to its key's holders unless it is there already.
    async fn admit(&self, queue: &str, key: &str, run_id: &str) -> Result<()> {
        match self
            .gates()
            .update_one(
                doc! { "_id": gate_id(queue, key), "holders": { "$ne": run_id } },
                doc! { "$push": { "holders": run_id } },
            )
            .upsert(true)
            .await
        {
            Ok(_) => Ok(()),
            // The gate exists and already lists the run.
            Err(error) if is_duplicate_key(&error) => Ok(()),
            Err(error) => Err(storage(format!("concurrency key update failed: {error}"))),
        }
    }

    /// Admits `run_id` only while the key has no unfinished holder; returns whether it did.
    async fn admit_if_free(&self, queue: &str, key: &str, run_id: &str) -> Result<bool> {
        let result = self
            .gates()
            .update_one(
                doc! {
                    "_id": gate_id(queue, key),
                    "holders": { "$size": 0 },
                    "pending_cancel": { "$exists": false },
                },
                doc! { "$push": { "holders": run_id } },
            )
            .upsert(true)
            .await;
        match result {
            Ok(result) => Ok(result.matched_count == 1 || result.upserted_id.is_some()),
            Err(error) if is_duplicate_key(&error) => Ok(false),
            Err(error) => Err(storage(format!("concurrency key update failed: {error}"))),
        }
    }

    /// Admits `run_id` behind the key's current holders and cancels them.
    async fn admit_interrupting(&self, queue: &str, key: &str, run_id: &str) -> Result<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let holders = self.settle_key(queue, key).await?;
            if holders.is_empty() {
                if self.admit_if_free(queue, key, run_id).await? {
                    return Ok(());
                }
                continue;
            }
            let mut next = holders.clone();
            next.push(run_id.to_string());
            let id = gate_id(queue, key);
            let swapped = self
                .gates()
                .update_one(
                    doc! {
                        "_id": &id,
                        "holders": &holders,
                        "pending_cancel": { "$exists": false },
                    },
                    doc! { "$set": { "holders": &next, "pending_cancel": &holders } },
                )
                .await
                .map_err(|e| storage(format!("concurrency key update failed: {e}")))?;
            if swapped.matched_count == 1 {
                let now = self.now();
                for holder in &holders {
                    self.cancel_holder(holder, now).await?;
                }
                self.gates()
                    .update_one(
                        doc! { "_id": &id, "pending_cancel": &holders },
                        doc! { "$unset": { "pending_cancel": "" } },
                    )
                    .await
                    .map_err(|e| storage(format!("concurrency key update failed: {e}")))?;
                return Ok(());
            }
        }
        Err(storage(format!(
            "concurrency key '{key}' kept changing under {MAX_CAS_ATTEMPTS} swaps"
        )))
    }

    /// Whether a queued keyed run is first among its key's unfinished holders.
    async fn heads_its_key(
        &self,
        record: &RunRecord,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let holders = self.settle_key(&record.queue, key).await?;
        if holders.first() == Some(&record.run_id) {
            return Ok(true);
        }
        if !holders.contains(&record.run_id) && record.created_at + ADMIT_GRACE <= now {
            // Its enqueue stopped before admitting it.
            self.admit(&record.queue, key, &record.run_id).await?;
            let holders = self.settle_key(&record.queue, key).await?;
            return Ok(holders.first() == Some(&record.run_id));
        }
        Ok(false)
    }

    async fn find_existing(&self, queue: &str, key: &str) -> Result<Option<RunRecord>> {
        self.runs()
            .find_one(doc! { "queue": queue, "idempotency_key": key })
            .await
            .map_err(|e| storage(format!("run read failed: {e}")))?
            .as_ref()
            .map(|document| parse_run(document).map(|(record, _)| record))
            .transpose()
    }

    /// Enqueues in one multi-document transaction: the idempotency check, the key
    /// holders' policy, the insert, and the run's admission to its key commit together,
    /// so no caller sees a run that is later withdrawn and a key never admits a second
    /// run under `Reject`.
    async fn enqueue_in_transaction(&self, run: NewRun, now: DateTime<Utc>) -> Result<Enqueued> {
        let record = RunRecord::queued(uuid::Uuid::new_v4().to_string(), run, now);
        // Ordering only: taken outside the transaction so enqueues do not all conflict on
        // the counter document.
        let seq = self.next_seq().await?;
        let mut session = self
            .db
            .client()
            .start_session()
            .await
            .map_err(|e| storage(format!("mongodb session failed: {e}")))?;
        for attempt in 0..MAX_CAS_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(crate::mongodb_stores::claim_backoff(attempt)).await;
            }
            session
                .start_transaction()
                .await
                .map_err(|e| storage(format!("mongodb transaction failed: {e}")))?;
            let (enqueued, writes) = match self.enqueue_body(&mut session, &record, seq, now).await
            {
                Ok(outcome) => outcome,
                Err(failure) => {
                    let _ = session.abort_transaction().await;
                    match failure {
                        TxFailure::Retry => continue,
                        TxFailure::Store(error) => return Err(error),
                        // A concurrent enqueue of the same key or holders won; the next
                        // attempt reads its committed result.
                        TxFailure::Mongo(error)
                            if is_duplicate_key(&error)
                                || error.contains_label(TRANSIENT_TRANSACTION_ERROR) =>
                        {
                            continue;
                        }
                        TxFailure::Mongo(error) => {
                            return Err(storage(format!("run enqueue failed: {error}")));
                        }
                    }
                }
            };
            if !writes {
                let _ = session.abort_transaction().await;
                return Ok(enqueued);
            }
            let mut unknown = 0;
            loop {
                match session.commit_transaction().await {
                    Ok(()) => return Ok(enqueued),
                    Err(error)
                        if unknown < 3
                            && error.contains_label(UNKNOWN_TRANSACTION_COMMIT_RESULT) =>
                    {
                        unknown += 1;
                    }
                    Err(error) if error.contains_label(TRANSIENT_TRANSACTION_ERROR) => break,
                    Err(error) => {
                        return Err(storage(format!("run enqueue commit failed: {error}")));
                    }
                }
            }
        }
        Err(storage(format!(
            "enqueue into queue '{}' lost {MAX_CAS_ATTEMPTS} transactions to concurrent writes",
            record.queue
        )))
    }

    /// One transactional enqueue attempt; returns the outcome and whether it wrote.
    async fn enqueue_body(
        &self,
        session: &mut ClientSession,
        record: &RunRecord,
        seq: i64,
        now: DateTime<Utc>,
    ) -> std::result::Result<(Enqueued, bool), TxFailure> {
        if let Some(key) = &record.idempotency_key
            && let Some(existing) = self
                .runs()
                .find_one(doc! { "queue": &record.queue, "idempotency_key": key })
                .session(&mut *session)
                .await?
        {
            return Ok((Enqueued::Existing(parse_run(&existing)?.0), false));
        }
        let mut holders: Vec<String> = Vec::new();
        if let Some(key) = &record.concurrency_key {
            let gate = self
                .gates()
                .find_one(doc! { "_id": gate_id(&record.queue, key) })
                .session(&mut *session)
                .await?;
            let listed = gate.as_ref().and_then(|gate| string_array(gate, "holders"));
            // Runs a fallback-path `Interrupt` left to cancel are cancelled here too.
            let pending = gate.as_ref().and_then(|gate| string_array(gate, "pending_cancel"));
            let interrupt = record.on_conflict == OnConflict::Interrupt;
            for run_id in listed.unwrap_or_default() {
                let Some(document) =
                    self.runs().find_one(doc! { "_id": &run_id }).session(&mut *session).await?
                else {
                    continue;
                };
                let (mut holder, holder_seq) = parse_run(&document)?;
                if holder.state.is_terminal() {
                    continue;
                }
                if record.on_conflict == OnConflict::Reject {
                    return Ok((Enqueued::Rejected { blocking_run_id: run_id }, false));
                }
                let cancel = interrupt || pending.as_ref().is_some_and(|ids| ids.contains(&run_id));
                if cancel {
                    let expected = holder.revision;
                    if apply_cancel(&mut holder, now) {
                        let replaced = self
                            .runs()
                            .replace_one(
                                doc! { "_id": &run_id, "revision": to_i64(expected, RUN_STORAGE)? },
                                run_document(&holder, holder_seq)?,
                            )
                            .session(&mut *session)
                            .await?;
                        if replaced.matched_count == 0 {
                            return Err(TxFailure::Retry);
                        }
                    }
                    if holder.state.is_terminal() {
                        continue;
                    }
                }
                holders.push(run_id);
            }
        }
        self.runs().insert_one(run_document(record, seq)?).session(&mut *session).await?;
        if let Some(key) = &record.concurrency_key {
            holders.push(record.run_id.clone());
            self.gates()
                .update_one(
                    doc! { "_id": gate_id(&record.queue, key) },
                    doc! { "$set": { "holders": holders }, "$unset": { "pending_cancel": "" } },
                )
                .upsert(true)
                .session(&mut *session)
                .await?;
        }
        Ok((Enqueued::Created(record.clone()), true))
    }

    /// Tries to claim one candidate; returns the claim, or `None` to move on.
    async fn try_claim(
        &self,
        mut record: RunRecord,
        mut seq: i64,
        worker: &str,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Option<ClaimedRun>> {
        // One re-read covers a write that landed between the query and the swap.
        for _ in 0..2 {
            let expected = record.revision;
            match claim_step(&record, now) {
                ClaimStep::NotReady => return Ok(None),
                ClaimStep::Exhaust => {
                    apply_exhaust(&mut record, now);
                    if self.swap(&record, seq, expected).await? {
                        self.leave_key(&record).await?;
                        return Ok(None);
                    }
                }
                ClaimStep::Claim { started } => {
                    if record.state == RunState::Queued
                        && let Some(key) = record.concurrency_key.clone()
                        && !self.heads_its_key(&record, &key, now).await?
                    {
                        return Ok(None);
                    }
                    let epoch = apply_claim(&mut record, started, worker, now, expires_at);
                    if self.swap(&record, seq, expected).await? {
                        return Ok(Some(ClaimedRun { record, epoch }));
                    }
                }
            }
            match self.read(&record.run_id).await? {
                Some((fresh, fresh_seq)) => (record, seq) = (fresh, fresh_seq),
                None => return Ok(None),
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl DurableRunStore for MongoRunStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        run.validate()?;
        let now = self.now();
        if self.supports_transactions().await? {
            return self.enqueue_in_transaction(run, now).await;
        }
        if let Some(key) = &run.idempotency_key
            && let Some(existing) = self.find_existing(&run.queue, key).await?
        {
            return Ok(Enqueued::Existing(existing));
        }
        if let (Some(key), OnConflict::Reject) = (&run.concurrency_key, run.on_conflict)
            && let Some(blocking_run_id) = self.settle_key(&run.queue, key).await?.first()
        {
            return Ok(Enqueued::Rejected { blocking_run_id: blocking_run_id.clone() });
        }

        let run_id = uuid::Uuid::new_v4().to_string();
        let seq = self.next_seq().await?;
        let record = RunRecord::queued(run_id.clone(), run, now);
        match self.runs().insert_one(run_document(&record, seq)?).await {
            Ok(_) => {}
            Err(error) if is_duplicate_key(&error) => {
                let key = record.idempotency_key.as_deref().unwrap_or_default();
                return match self.find_existing(&record.queue, key).await? {
                    Some(existing) => Ok(Enqueued::Existing(existing)),
                    None => Err(storage(format!(
                        "run with idempotency key '{key}' was pruned during enqueue; retry"
                    ))),
                };
            }
            Err(error) => return Err(storage(format!("run insert failed: {error}"))),
        }

        let Some(key) = record.concurrency_key.clone() else {
            return Ok(Enqueued::Created(record));
        };
        match record.on_conflict {
            OnConflict::Reject => {
                for _ in 0..8 {
                    if self.admit_if_free(&record.queue, &key, &run_id).await? {
                        return Ok(Enqueued::Created(record));
                    }
                    if let Some(blocking_run_id) =
                        self.settle_key(&record.queue, &key).await?.first().cloned()
                    {
                        self.runs()
                            .delete_one(doc! { "_id": &run_id })
                            .await
                            .map_err(|e| storage(format!("run delete failed: {e}")))?;
                        return Ok(Enqueued::Rejected { blocking_run_id });
                    }
                }
                Err(storage(format!("concurrency key '{key}' kept changing during enqueue")))
            }
            OnConflict::Interrupt => {
                self.admit_interrupting(&record.queue, &key, &run_id).await?;
                Ok(Enqueued::Created(record))
            }
            _ => {
                self.admit(&record.queue, &key, &run_id).await?;
                Ok(Enqueued::Created(record))
            }
        }
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        let now = self.now();
        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
        let mut after = i64::MIN;
        loop {
            let page = collect(
                self.runs()
                    .find(doc! {
                        "queue": queue,
                        "ready_us": { "$lte": micros(now) },
                        "seq": { "$gt": after },
                    })
                    .sort(doc! { "seq": 1 })
                    .limit(CLAIM_PAGE)
                    .await
                    .map_err(|e| storage(format!("run query failed: {e}")))?,
                RUN_STORAGE,
            )
            .await?;
            if page.is_empty() {
                return Ok(None);
            }
            for document in &page {
                let (record, seq) = parse_run(document)?;
                after = seq;
                if let Some(claimed) = self.try_claim(record, seq, worker, now, expires_at).await? {
                    return Ok(Some(claimed));
                }
            }
        }
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        let now = self.now();
        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
        let record = self
            .modify(run_id, |record| {
                check_fence(record, epoch)?;
                apply_heartbeat(record, now, expires_at);
                Ok(true)
            })
            .await?;
        Ok(record.cancel_requested)
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        let now = self.now();
        self.modify(run_id, |record| {
            check_fence(record, epoch)?;
            apply_checkpoint(record, now, checkpoint.clone());
            Ok(true)
        })
        .await
        .map(|_| ())
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        let now = self.now();
        self.modify(run_id, |record| {
            check_fence(record, epoch)?;
            apply_wait(record, now, until);
            Ok(true)
        })
        .await
        .map(|_| ())
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        let now = self.now();
        self.modify(run_id, |record| Ok(apply_wake(record, now))).await.map(|_| ())
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        let now = self.now();
        self.modify(run_id, |record| {
            check_fence(record, epoch)?;
            apply_finish(record, now, outcome.clone());
            Ok(true)
        })
        .await
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        let now = self.now();
        self.modify(run_id, |record| Ok(apply_cancel(record, now))).await.map(|_| ())
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        Ok(self.read(run_id).await?.map(|(record, _)| record))
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        let mut query = Document::new();
        if let Some(queue) = &filter.queue {
            query.insert("queue", queue.as_str());
        }
        if !filter.states.is_empty() {
            let states: Vec<&str> = filter.states.iter().map(|state| state_name(*state)).collect();
            query.insert("state", doc! { "$in": states });
        }
        for (field, value) in [
            ("app_name", &filter.app_name),
            ("user_id", &filter.user_id),
            ("session_id", &filter.session_id),
            ("concurrency_key", &filter.concurrency_key),
        ] {
            if let Some(value) = value {
                query.insert(field, value.as_str());
            }
        }
        let limit = match filter.limit {
            Some(0) => return Ok(Vec::new()),
            Some(limit) => i64::try_from(limit).unwrap_or(i64::MAX),
            // A zero limit is unlimited.
            None => 0,
        };
        let runs = self.runs();
        let documents = collect(
            runs.find(query)
                .sort(doc! { "seq": 1 })
                .limit(limit)
                .await
                .map_err(|e| storage(format!("run query failed: {e}")))?,
            RUN_STORAGE,
        )
        .await?;
        documents.iter().map(|document| parse_run(document).map(|(record, _)| record)).collect()
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        let terminal = [RunState::Completed, RunState::Failed, RunState::Cancelled].map(state_name);
        let deleted = self
            .runs()
            .delete_many(doc! {
                "state": { "$in": terminal.to_vec() },
                "finished_us": { "$lt": ceil_micros(finished_before) },
            })
            .await
            .map_err(|e| storage(format!("run prune failed: {e}")))?;
        Ok(deleted.deleted_count)
    }
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

/// MongoDB-backed [`ScheduleStore`].
///
/// One `adk_schedules` document per schedule; `due` reads an index on `due_us`, the next
/// fire time of an unpaused schedule.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::ScheduleStore;
/// use adk_session::MongoScheduleStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = MongoScheduleStore::new("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// let due = store.due(chrono::Utc::now(), 100).await?;
/// # let _ = due;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoScheduleStore {
    db: Database,
}

fn schedule_document(schedule: &Schedule) -> Result<Document> {
    let due = match (schedule.paused, schedule.next_fire_at) {
        (false, Some(at)) => Bson::Int64(micros(at)),
        _ => Bson::Null,
    };
    Ok(doc! {
        "_id": &schedule.id,
        "schedule": encode(schedule, SCHEDULE_STORAGE)?,
        "paused": schedule.paused,
        "due_us": due,
        "revision": to_i64(schedule.revision, SCHEDULE_STORAGE)?,
    })
}

impl MongoScheduleStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(open(connection_string, database_name, SCHEDULE_STORAGE).await?))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Creates the index `due` reads through.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when the index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        create_index(
            &self.collection(),
            doc! { "due_us": 1, "_id": 1 },
            named("adk_schedules_due"),
            SCHEDULE_STORAGE,
        )
        .await
    }

    fn collection(&self) -> Collection<Document> {
        self.db.collection(SCHEDULES)
    }

    async fn read(&self, id: &str) -> Result<Option<Schedule>> {
        self.collection()
            .find_one(doc! { "_id": id })
            .await
            .map_err(|e| storage_error(SCHEDULE_STORAGE, format!("schedule read failed: {e}")))?
            .as_ref()
            .map(|document| decode(document, "schedule", SCHEDULE_STORAGE))
            .transpose()
    }

    /// Replaces the schedule when its stored revision is still `expected`.
    async fn swap(&self, schedule: &Schedule, expected: u64) -> Result<bool> {
        let replaced = self
            .collection()
            .replace_one(
                doc! { "_id": &schedule.id, "revision": to_i64(expected, SCHEDULE_STORAGE)? },
                schedule_document(schedule)?,
            )
            .await
            .map_err(|e| storage_error(SCHEDULE_STORAGE, format!("schedule write failed: {e}")))?;
        Ok(replaced.matched_count == 1)
    }

    fn contended(id: &str) -> adk_core::AdkError {
        storage_error(
            SCHEDULE_STORAGE,
            format!("schedule '{id}' lost {MAX_CAS_ATTEMPTS} compare-and-swap attempts"),
        )
    }
}

#[async_trait]
impl ScheduleStore for MongoScheduleStore {
    async fn upsert(&self, mut schedule: Schedule) -> Result<Schedule> {
        schedule.validate()?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            match self.read(&schedule.id).await? {
                Some(stored) => {
                    if same_definition(&stored, &schedule) {
                        return Ok(stored);
                    }
                    schedule.revision = stored.revision + 1;
                    if self.swap(&schedule, stored.revision).await? {
                        return Ok(schedule);
                    }
                }
                None => {
                    schedule.revision = 1;
                    match self.collection().insert_one(schedule_document(&schedule)?).await {
                        Ok(_) => return Ok(schedule),
                        Err(error) if is_duplicate_key(&error) => {}
                        Err(error) => {
                            return Err(storage_error(
                                SCHEDULE_STORAGE,
                                format!("schedule insert failed: {error}"),
                            ));
                        }
                    }
                }
            }
        }
        Err(Self::contended(&schedule.id))
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        self.read(id).await
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        let documents = collect(
            self.collection().find(doc! {}).sort(doc! { "_id": 1 }).await.map_err(|e| {
                storage_error(SCHEDULE_STORAGE, format!("schedule query failed: {e}"))
            })?,
            SCHEDULE_STORAGE,
        )
        .await?;
        documents.iter().map(|document| decode(document, "schedule", SCHEDULE_STORAGE)).collect()
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let deleted =
            self.collection().delete_one(doc! { "_id": id }).await.map_err(|e| {
                storage_error(SCHEDULE_STORAGE, format!("schedule delete failed: {e}"))
            })?;
        if deleted.deleted_count == 0 {
            return Err(schedule_not_found_error(id));
        }
        Ok(())
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(stored) = self.read(id).await? else {
                return Err(schedule_not_found_error(id));
            };
            if stored.paused == paused {
                return Ok(stored);
            }
            let changed = Schedule { paused, revision: stored.revision + 1, ..stored.clone() };
            if self.swap(&changed, stored.revision).await? {
                return Ok(changed);
            }
        }
        Err(Self::contended(id))
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let documents = collect(
            self.collection()
                .find(doc! { "due_us": { "$lte": micros(now) } })
                .sort(doc! { "due_us": 1, "_id": 1 })
                .limit(i64::try_from(limit).unwrap_or(i64::MAX))
                .await
                .map_err(|e| {
                    storage_error(SCHEDULE_STORAGE, format!("schedule query failed: {e}"))
                })?,
            SCHEDULE_STORAGE,
        )
        .await?;
        documents.iter().map(|document| decode(document, "schedule", SCHEDULE_STORAGE)).collect()
    }

    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let Some(stored) = self.read(id).await? else {
            return Err(schedule_not_found_error(id));
        };
        if stored.revision != expected_revision {
            return Ok(false);
        }
        let advanced = Schedule { next_fire_at, revision: expected_revision + 1, ..stored };
        if self.swap(&advanced, expected_revision).await? {
            return Ok(true);
        }
        match self.read(id).await? {
            Some(_) => Ok(false),
            None => Err(schedule_not_found_error(id)),
        }
    }
}

// ---------------------------------------------------------------------------
// Governance state
// ---------------------------------------------------------------------------

/// MongoDB-backed [`GovernanceStateStore`].
///
/// One `adk_governance` document per scope, keyed by [`GovernanceScope::key`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{GovernanceScope, GovernanceStateStore};
/// use adk_session::MongoGovernanceStateStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = MongoGovernanceStateStore::new("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// store.set(GovernanceScope::Org, 0, true, Some("incident 42".into()), None).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoGovernanceStateStore {
    db: Database,
    clock: Arc<dyn Clock>,
}

impl MongoGovernanceStateStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(open(connection_string, database_name, GOVERNANCE_STORAGE).await?))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps `issued_at`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Checks that the server is reachable. States need no index beyond `_id`.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the server cannot be reached.
    pub async fn migrate(&self) -> Result<()> {
        ping(&self.db, GOVERNANCE_STORAGE).await
    }

    fn collection(&self) -> Collection<Document> {
        self.db.collection(GOVERNANCE)
    }

    async fn current_epoch(&self, scope: &GovernanceScope) -> Result<u64> {
        Ok(self.get(scope).await?.map_or(0, |state| state.epoch))
    }
}

fn governance_failure(error: mongodb::error::Error) -> adk_core::AdkError {
    storage_error(GOVERNANCE_STORAGE, format!("governance write failed: {error}"))
}

#[async_trait]
impl GovernanceStateStore for MongoGovernanceStateStore {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        self.collection()
            .find_one(doc! { "_id": scope.key() })
            .await
            .map_err(|e| storage_error(GOVERNANCE_STORAGE, format!("governance read failed: {e}")))?
            .as_ref()
            .map(|document| decode(document, "state", GOVERNANCE_STORAGE))
            .transpose()
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        let key = scope.key();
        let state = GovernanceState {
            scope,
            frozen,
            reason,
            issued_by,
            issued_at: stored_now(self.clock.now()),
            epoch: expected_epoch + 1,
        };
        let json = encode(&state, GOVERNANCE_STORAGE)?;
        let epoch = to_i64(state.epoch, GOVERNANCE_STORAGE)?;
        let landed = if expected_epoch == 0 {
            match self
                .collection()
                .insert_one(doc! { "_id": &key, "state": &json, "epoch": epoch })
                .await
            {
                Ok(_) => true,
                Err(error) if is_duplicate_key(&error) => false,
                Err(error) => return Err(governance_failure(error)),
            }
        } else {
            self.collection()
                .update_one(
                    doc! { "_id": &key, "epoch": to_i64(expected_epoch, GOVERNANCE_STORAGE)? },
                    doc! { "$set": { "state": &json, "epoch": epoch } },
                )
                .await
                .map_err(governance_failure)?
                .matched_count
                == 1
        };
        if landed {
            return Ok(state);
        }
        let current = self.current_epoch(&state.scope).await?;
        Err(governance_conflict_error(&state.scope, expected_epoch, current))
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        let documents = collect(
            self.collection().find(doc! {}).sort(doc! { "_id": 1 }).await.map_err(|e| {
                storage_error(GOVERNANCE_STORAGE, format!("governance query failed: {e}"))
            })?,
            GOVERNANCE_STORAGE,
        )
        .await?;
        documents.iter().map(|document| decode(document, "state", GOVERNANCE_STORAGE)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn prune_cutoffs_round_up_to_whole_microseconds() {
        let whole = Utc.timestamp_opt(1_800_000_000, 5_000).single().unwrap();
        assert_eq!(ceil_micros(whole), whole.timestamp_micros());
        let partial = Utc.timestamp_opt(1_800_000_000, 5_500).single().unwrap();
        assert_eq!(ceil_micros(partial), whole.timestamp_micros() + 1);
    }

    #[test]
    fn gate_ids_cannot_collide_across_queue_and_key() {
        assert_ne!(gate_id("a:b", "c"), gate_id("a", "b:c"));
        assert_eq!(gate_id("q", "k"), "71:6b");
    }

    #[test]
    fn run_documents_round_trip_and_index_their_state() {
        let mut record = RunRecord::queued(
            "r1",
            NewRun::new("q/ü", "app", "user", "agent", serde_json::json!({}))
                .with_idempotency_key("k"),
            Utc.timestamp_opt(1_800_000_000, 0).single().unwrap(),
        );
        let queued = run_document(&record, 7).unwrap();
        assert_eq!(queued.get_str("state").unwrap(), "queued");
        assert_eq!(queued.get_i64("ready_us").unwrap(), i64::MIN);
        assert_eq!(queued.get("finished_us"), Some(&Bson::Null));
        assert_eq!(parse_run(&queued).unwrap(), (record.clone(), 7));

        record.state = RunState::Cancelled;
        record.idempotency_key = None;
        let finished = run_document(&record, 7).unwrap();
        assert_eq!(finished.get("ready_us"), Some(&Bson::Null));
        assert_eq!(finished.get_i64("finished_us").unwrap(), micros(record.updated_at));
        assert!(!finished.contains_key("idempotency_key"));
    }

    #[test]
    fn paused_schedules_have_no_due_time() {
        let at = Utc.timestamp_opt(1_800_000_000, 0).single().unwrap();
        let schedule = Schedule::new(
            "s",
            adk_core::schedule::ScheduleSpec::At(at),
            NewRun::new("q", "a", "u", "t", serde_json::Value::Null),
            adk_core::schedule::MissedFirePolicy::FireOnce,
        )
        .with_next_fire_at(at);
        assert_eq!(schedule_document(&schedule).unwrap().get_i64("due_us").unwrap(), micros(at));
        let paused = schedule_document(&schedule.with_paused(true)).unwrap();
        assert_eq!(paused.get("due_us"), Some(&Bson::Null));
    }
}
