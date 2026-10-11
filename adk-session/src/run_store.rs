//! SQL-backed [`DurableRunStore`](adk_core::DurableRunStore) implementations.
//!
//! One row per run in `adk_runs`. `seq` is assigned by the database and breaks ties in
//! enqueue order, so runs are ordered by `(created_at_us, seq)`.
//!
//! | Guarantee | Primitive |
//! |-----------|-----------|
//! | Idempotent enqueue | Unique index on `(queue, idempotency_key)`; `INSERT ... ON CONFLICT DO NOTHING`, then the existing run is read |
//! | Double texting | The concurrency-key holders are read and `Reject` / `Interrupt` applied in the enqueue transaction, which PostgreSQL serializes per `(queue, concurrency_key)` with `pg_advisory_xact_lock` and SQLite with `BEGIN IMMEDIATE` |
//! | Single claim winner | PostgreSQL: `SELECT ... ORDER BY created_at_us, seq LIMIT 1 FOR UPDATE OF r SKIP LOCKED`, then `UPDATE ... WHERE seq = $1`, in one transaction; SQLite: the same statements under `BEGIN IMMEDIATE` |
//! | Fenced worker writes | `UPDATE ... WHERE run_id = $r AND lease_epoch = $e AND state = 'running'`; no row changed is `run_store.stale_epoch` (or `run_store.not_found` when the run does not exist) |
//! | `wake`, `request_cancel` | One `UPDATE` whose `CASE` arms act on the state the row has when the write lands |
//! | `prune` | `DELETE ... WHERE seq IN (SELECT ... LIMIT 500)`, repeated until a batch comes back short |
//!
//! | Index | Serves |
//! |-------|--------|
//! | `adk_runs_idempotency` (unique) | Idempotent enqueue |
//! | `adk_runs_claim`, partial on unfinished states | `claim` in enqueue order |
//! | `adk_runs_concurrency`, partial on unfinished states | Concurrency-key holders in `enqueue` and `claim` |
//! | `adk_runs_finished`, partial on finished states | `prune` |
//! | `adk_runs_owner` | `list` by application, user, and session |

use std::sync::Arc;
use std::time::Duration;

use adk_core::run_store::{run_not_found_error, run_stale_epoch_error};
use adk_core::{
    ClaimedRun, Clock, DurableRunStore, Enqueued, NewRun, OnConflict, Result, RunCheckpointRef,
    RunFilter, RunOutcome, RunRecord, RunState, SystemClock,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::durable_sql::{
    deadline, from_json, from_micros, micros, now, storage_error, to_i64, to_json, to_u32, to_u64,
};

const CODE: &str = "session.run_store.storage";

const REGISTRY_TABLE: &str = "_adk_run_migrations";

/// Rows deleted per `prune` statement, so one call never holds a long lock.
const PRUNE_BATCH: i64 = 500;

/// Indexes shared by both dialects; `seq` is the only column whose definition differs.
macro_rules! run_indexes {
    () => {
        "\
CREATE UNIQUE INDEX IF NOT EXISTS adk_runs_idempotency ON adk_runs (queue, idempotency_key);\
CREATE INDEX IF NOT EXISTS adk_runs_claim ON adk_runs (queue, created_at_us, seq) \
    WHERE state IN ('queued', 'running', 'waiting');\
CREATE INDEX IF NOT EXISTS adk_runs_concurrency ON adk_runs (queue, concurrency_key, created_at_us, seq) \
    WHERE state IN ('queued', 'running', 'waiting');\
CREATE INDEX IF NOT EXISTS adk_runs_finished ON adk_runs (updated_at_us) \
    WHERE state IN ('completed', 'failed', 'cancelled');\
CREATE INDEX IF NOT EXISTS adk_runs_owner ON adk_runs (app_name, user_id, session_id);"
    };
}

macro_rules! run_table {
    ($seq:literal) => {
        concat!(
            "CREATE TABLE IF NOT EXISTS adk_runs (",
            $seq,
            ", \
    run_id TEXT NOT NULL UNIQUE, \
    queue TEXT NOT NULL, \
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT, \
    target TEXT NOT NULL, \
    input TEXT NOT NULL, \
    idempotency_key TEXT, \
    concurrency_key TEXT, \
    on_conflict TEXT NOT NULL, \
    not_before_us BIGINT, \
    max_attempts BIGINT NOT NULL, \
    principal TEXT, \
    state TEXT NOT NULL, \
    attempt BIGINT NOT NULL, \
    lease_owner TEXT, \
    lease_epoch BIGINT NOT NULL, \
    lease_expires_at_us BIGINT, \
    heartbeat_at_us BIGINT, \
    checkpoint TEXT, \
    cancel_requested BOOLEAN NOT NULL, \
    wake_pending BOOLEAN NOT NULL, \
    result TEXT, \
    error TEXT, \
    created_at_us BIGINT NOT NULL, \
    updated_at_us BIGINT NOT NULL, \
    revision BIGINT NOT NULL\
);",
            run_indexes!()
        )
    };
}

macro_rules! run_columns {
    () => {
        "run_id, queue, app_name, user_id, session_id, target, input, idempotency_key, \
         concurrency_key, on_conflict, not_before_us, max_attempts, principal, state, attempt, \
         lease_owner, lease_epoch, lease_expires_at_us, heartbeat_at_us, checkpoint, \
         cancel_requested, wake_pending, result, error, created_at_us, updated_at_us, revision"
    };
}

// Parameters appear in ascending order in every statement, so SQLite, which numbers
// `$N` parameters by first appearance, binds them as PostgreSQL does.
const INSERT: &str = concat!(
    "INSERT INTO adk_runs (",
    run_columns!(),
    ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, \
     $19, $20, $21, $22, $23, $24, $25, $26, $27) \
     ON CONFLICT (queue, idempotency_key) DO NOTHING"
);

const GET: &str = concat!("SELECT ", run_columns!(), " FROM adk_runs WHERE run_id = $1");

const BY_KEY: &str =
    concat!("SELECT ", run_columns!(), " FROM adk_runs WHERE queue = $1 AND idempotency_key = $2");

const EXISTS: &str = "SELECT 1 FROM adk_runs WHERE run_id = $1";

const OLDEST_HOLDER: &str = "\
SELECT run_id FROM adk_runs \
WHERE queue = $1 AND concurrency_key = $2 AND state IN ('queued', 'running', 'waiting') \
ORDER BY created_at_us, seq LIMIT 1";

/// Cancels queued and waiting holders and flags running ones. One statement, so a holder
/// that a worker parks or claims concurrently is handled in the state it lands in.
const INTERRUPT: &str = "\
UPDATE adk_runs SET \
    cancel_requested = CASE WHEN state = 'running' THEN TRUE ELSE cancel_requested END, \
    lease_owner = CASE WHEN state = 'running' THEN lease_owner ELSE NULL END, \
    lease_expires_at_us = CASE WHEN state = 'running' THEN lease_expires_at_us ELSE NULL END, \
    state = CASE WHEN state = 'running' THEN state ELSE 'cancelled' END, \
    updated_at_us = $1, revision = revision + 1 \
WHERE queue = $2 AND concurrency_key = $3 AND state IN ('queued', 'running', 'waiting')";

/// Claimable runs in queue order: a queued run that is due and has no earlier unfinished
/// run with its concurrency key, a waiting run whose wait ended, or a running run whose
/// lease expired.
macro_rules! claim_candidate {
    ($lock:literal) => {
        concat!(
            "SELECT r.seq, r.state, r.attempt, r.max_attempts FROM adk_runs r \
             WHERE r.queue = $1 AND r.state IN ('queued', 'running', 'waiting') AND ( \
                 (r.state = 'queued' \
                     AND (r.not_before_us IS NULL OR r.not_before_us <= $2) \
                     AND (r.concurrency_key IS NULL OR NOT EXISTS ( \
                         SELECT 1 FROM adk_runs h \
                         WHERE h.queue = r.queue AND h.concurrency_key = r.concurrency_key \
                             AND h.state IN ('queued', 'running', 'waiting') \
                             AND (h.created_at_us < r.created_at_us \
                                 OR (h.created_at_us = r.created_at_us AND h.seq < r.seq))))) \
                 OR (r.state = 'waiting' AND r.not_before_us IS NOT NULL AND r.not_before_us <= $2) \
                 OR (r.state = 'running' \
                     AND (r.lease_expires_at_us IS NULL OR r.lease_expires_at_us <= $2))) \
             ORDER BY r.created_at_us, r.seq LIMIT 1",
            $lock
        )
    };
}

const CLAIM: &str = concat!(
    "UPDATE adk_runs SET \
         attempt = attempt + CASE WHEN state = 'waiting' THEN 0 ELSE 1 END, \
         state = 'running', lease_epoch = lease_epoch + 1, lease_owner = $1, \
         lease_expires_at_us = $2, heartbeat_at_us = $3, not_before_us = NULL, \
         updated_at_us = $3, revision = revision + 1 \
     WHERE seq = $4 RETURNING ",
    run_columns!()
);

const EXHAUST: &str = "\
UPDATE adk_runs SET state = 'failed', error = $1, lease_owner = NULL, \
    lease_expires_at_us = NULL, updated_at_us = $2, revision = revision + 1 \
WHERE seq = $3";

const HEARTBEAT: &str = "\
UPDATE adk_runs SET lease_expires_at_us = $1, heartbeat_at_us = $2, updated_at_us = $2, \
    revision = revision + 1 \
WHERE run_id = $3 AND lease_epoch = $4 AND state = 'running' \
RETURNING cancel_requested";

const SET_CHECKPOINT: &str = "\
UPDATE adk_runs SET checkpoint = $1, updated_at_us = $2, revision = revision + 1 \
WHERE run_id = $3 AND lease_epoch = $4 AND state = 'running'";

/// A wake that arrived while the run was running parks it as already woken.
const WAIT: &str = "\
UPDATE adk_runs SET state = 'waiting', \
    not_before_us = CASE WHEN wake_pending THEN $1 ELSE $2 END, \
    wake_pending = FALSE, lease_owner = NULL, lease_expires_at_us = NULL, \
    updated_at_us = $1, revision = revision + 1 \
WHERE run_id = $3 AND lease_epoch = $4 AND state = 'running'";

const WAKE: &str = "\
UPDATE adk_runs SET \
    not_before_us = CASE WHEN state = 'waiting' THEN $1 ELSE not_before_us END, \
    wake_pending = CASE WHEN state = 'running' THEN TRUE ELSE wake_pending END, \
    updated_at_us = $1, revision = revision + 1 \
WHERE run_id = $2 AND state IN ('waiting', 'running')";

/// A retryable failure requeues while attempts remain and no cancellation was requested.
const FINISH: &str = concat!(
    "UPDATE adk_runs SET \
         state = CASE WHEN $1 AND attempt < max_attempts AND NOT cancel_requested \
             THEN 'queued' ELSE $2 END, \
         result = COALESCE($3, result), error = COALESCE($4, error), \
         wake_pending = FALSE, lease_owner = NULL, lease_expires_at_us = NULL, \
         updated_at_us = $5, revision = revision + 1 \
     WHERE run_id = $6 AND lease_epoch = $7 AND state = 'running' RETURNING ",
    run_columns!()
);

const REQUEST_CANCEL: &str = "\
UPDATE adk_runs SET \
    cancel_requested = CASE WHEN state = 'running' THEN TRUE ELSE cancel_requested END, \
    lease_owner = CASE WHEN state = 'running' THEN lease_owner ELSE NULL END, \
    lease_expires_at_us = CASE WHEN state = 'running' THEN lease_expires_at_us ELSE NULL END, \
    state = CASE WHEN state = 'running' THEN state ELSE 'cancelled' END, \
    updated_at_us = $1, revision = revision + 1 \
WHERE run_id = $2 AND state IN ('queued', 'running', 'waiting')";

const PRUNE: &str = "\
DELETE FROM adk_runs WHERE seq IN (\
    SELECT seq FROM adk_runs \
    WHERE state IN ('completed', 'failed', 'cancelled') AND updated_at_us < $1 \
    LIMIT $2)";

fn failed(e: sqlx::Error) -> adk_core::AdkError {
    storage_error(CODE, format!("run store query failed: {e}"))
}

/// The stored name of a serde unit variant, such as `"queued"` for [`RunState::Queued`].
fn variant_name<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        Ok(other) => Err(storage_error(CODE, format!("{other} is not a unit variant"))),
        Err(e) => Err(storage_error(CODE, format!("serialize failed: {e}"))),
    }
}

fn from_variant_name<T: DeserializeOwned>(column: &str, name: String) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(name))
        .map_err(|e| storage_error(CODE, format!("column '{column}' is corrupt: {e}")))
}

fn optional_json<T: DeserializeOwned>(column: &str, json: Option<String>) -> Result<Option<T>> {
    json.map(|json| from_json(CODE, column, &json)).transpose()
}

fn optional_time(column: &str, us: Option<i64>) -> Result<Option<DateTime<Utc>>> {
    us.map(|us| from_micros(CODE, column, us)).transpose()
}

/// A new run's columns that need encoding: JSON text, variant names, and microseconds.
struct RunRow {
    input: String,
    on_conflict: String,
    not_before_us: Option<i64>,
    principal: Option<String>,
    state: String,
    created_at_us: i64,
}

impl RunRow {
    fn new(record: &RunRecord) -> Result<Self> {
        Ok(Self {
            input: to_json(CODE, &record.input)?,
            on_conflict: variant_name(&record.on_conflict)?,
            not_before_us: record.not_before.map(|at| micros(at).1),
            principal: record.principal.as_ref().map(|p| to_json(CODE, p)).transpose()?,
            state: variant_name(&record.state)?,
            created_at_us: micros(record.created_at).1,
        })
    }
}

/// Decodes a row selected with [`run_columns!`].
macro_rules! decode_run {
    ($row:expr) => {{
        use sqlx::Row;
        let row = $row;
        let column = |name: &str, e: sqlx::Error| {
            storage_error(CODE, format!("run store column '{name}' unreadable: {e}"))
        };
        macro_rules! get {
            ($name:literal) => {
                row.try_get($name).map_err(|e| column($name, e))?
            };
        }
        let input: String = get!("input");
        let on_conflict: String = get!("on_conflict");
        let state: String = get!("state");
        let max_attempts: i64 = get!("max_attempts");
        let attempt: i64 = get!("attempt");
        let lease_epoch: i64 = get!("lease_epoch");
        let revision: i64 = get!("revision");
        let created_at_us: i64 = get!("created_at_us");
        let updated_at_us: i64 = get!("updated_at_us");
        let record: Result<RunRecord> = Ok(RunRecord {
            run_id: get!("run_id"),
            queue: get!("queue"),
            app_name: get!("app_name"),
            user_id: get!("user_id"),
            session_id: get!("session_id"),
            target: get!("target"),
            input: from_json(CODE, "input", &input)?,
            idempotency_key: get!("idempotency_key"),
            concurrency_key: get!("concurrency_key"),
            on_conflict: from_variant_name::<OnConflict>("on_conflict", on_conflict)?,
            not_before: optional_time("not_before_us", get!("not_before_us"))?,
            max_attempts: to_u32(CODE, "max_attempts", max_attempts)?,
            principal: optional_json("principal", get!("principal"))?,
            state: from_variant_name::<RunState>("state", state)?,
            attempt: to_u32(CODE, "attempt", attempt)?,
            lease_owner: get!("lease_owner"),
            lease_epoch: to_u64(CODE, "lease_epoch", lease_epoch)?,
            lease_expires_at: optional_time("lease_expires_at_us", get!("lease_expires_at_us"))?,
            heartbeat_at: optional_time("heartbeat_at_us", get!("heartbeat_at_us"))?,
            checkpoint: optional_json::<RunCheckpointRef>("checkpoint", get!("checkpoint"))?,
            cancel_requested: get!("cancel_requested"),
            wake_pending: get!("wake_pending"),
            result: optional_json("result", get!("result"))?,
            error: get!("error"),
            created_at: from_micros(CODE, "created_at_us", created_at_us)?,
            updated_at: from_micros(CODE, "updated_at_us", updated_at_us)?,
            revision: to_u64(CODE, "revision", revision)?,
        });
        record
    }};
}

macro_rules! sql_run_store {
    (
        $(#[$doc:meta])*
        $name:ident, $pool:ty, $db:ty,
        begin = $begin:literal,
        lock = $lock:literal,
        serialize_key = $serialize_key:expr,
        $migrate:expr
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: $pool,
            clock: Arc<dyn Clock>,
        }

        impl $name {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.run_store.storage` when the database cannot be reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    storage_error(CODE, format!("run store connection failed: {e}"))
                })?;
                Ok(Self::from_pool(pool))
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool, clock: Arc::new(SystemClock) }
            }

            /// Sets the clock that decides delays, waits, and lease expiry.
            pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
                self.clock = clock;
                self
            }

            /// Creates or upgrades the run table and its indexes.
            ///
            /// # Errors
            ///
            /// Returns an error when a migration statement fails.
            pub async fn migrate(&self) -> Result<()> {
                let migrate = $migrate;
                migrate(&self.pool).await
            }

            async fn by_key(&self, queue: &str, key: &str) -> Result<Option<RunRecord>> {
                let row = sqlx::query(BY_KEY)
                    .bind(queue)
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(failed)?;
                row.map(|row| decode_run!(&row)).transpose()
            }

            /// The error for a fenced write that changed no row.
            async fn refused(&self, run_id: &str, epoch: u64) -> adk_core::AdkError {
                match sqlx::query_scalar::<_, i32>(EXISTS)
                    .bind(run_id)
                    .fetch_optional(&self.pool)
                    .await
                {
                    Ok(Some(_)) => run_stale_epoch_error(run_id, epoch),
                    Ok(None) => run_not_found_error(run_id),
                    Err(e) => failed(e),
                }
            }

            /// Applies the double-texting policy inside the enqueue transaction and
            /// returns the blocking run when `Reject` refuses the new run.
            async fn apply_conflict_policy(
                tx: &mut sqlx::Transaction<'static, $db>,
                run: &NewRun,
                now_us: i64,
            ) -> Result<Option<String>> {
                let Some(key) = &run.concurrency_key else { return Ok(None) };
                let serialize_key = $serialize_key;
                serialize_key(&mut **tx, &run.queue, key).await?;
                match run.on_conflict {
                    OnConflict::Reject => sqlx::query_scalar(OLDEST_HOLDER)
                        .bind(&run.queue)
                        .bind(key)
                        .fetch_optional(&mut **tx)
                        .await
                        .map_err(failed),
                    OnConflict::Interrupt => {
                        sqlx::query(INTERRUPT)
                            .bind(now_us)
                            .bind(&run.queue)
                            .bind(key)
                            .execute(&mut **tx)
                            .await
                            .map_err(failed)?;
                        Ok(None)
                    }
                    OnConflict::Enqueue => Ok(None),
                    // `OnConflict` is non-exhaustive; a policy this store does not know
                    // is refused rather than silently treated as another.
                    other => Err(adk_core::run_store::invalid_run_error(&format!(
                        "on_conflict policy {other:?} is not supported by this run store"
                    ))),
                }
            }
        }

        #[async_trait::async_trait]
        impl DurableRunStore for $name {
            async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
                run.validate()?;
                // A concurrent enqueue of the same key wins the unique index; the next
                // pass reads its run. A run pruned in between makes a further pass.
                for _ in 0..3 {
                    if let Some(key) = &run.idempotency_key
                        && let Some(existing) = self.by_key(&run.queue, key).await?
                    {
                        return Ok(Enqueued::Existing(existing));
                    }
                    let (now, now_us) = now(&*self.clock);
                    let mut tx = self.pool.begin_with($begin).await.map_err(failed)?;
                    if let Some(blocking_run_id) =
                        Self::apply_conflict_policy(&mut tx, &run, now_us).await?
                    {
                        tx.rollback().await.map_err(failed)?;
                        return Ok(Enqueued::Rejected { blocking_run_id });
                    }
                    let record =
                        RunRecord::queued(uuid::Uuid::new_v4().to_string(), run.clone(), now);
                    let row = RunRow::new(&record)?;
                    let inserted = sqlx::query(INSERT)
                        .bind(&record.run_id)
                        .bind(&record.queue)
                        .bind(&record.app_name)
                        .bind(&record.user_id)
                        .bind(record.session_id.as_deref())
                        .bind(&record.target)
                        .bind(&row.input)
                        .bind(record.idempotency_key.as_deref())
                        .bind(record.concurrency_key.as_deref())
                        .bind(&row.on_conflict)
                        .bind(row.not_before_us)
                        .bind(i64::from(record.max_attempts))
                        .bind(row.principal.as_deref())
                        .bind(&row.state)
                        .bind(0_i64)
                        .bind(None::<String>)
                        .bind(0_i64)
                        .bind(None::<i64>)
                        .bind(None::<i64>)
                        .bind(None::<String>)
                        .bind(false)
                        .bind(false)
                        .bind(None::<String>)
                        .bind(None::<String>)
                        .bind(row.created_at_us)
                        .bind(row.created_at_us)
                        .bind(1_i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(failed)?;
                    if inserted.rows_affected() == 1 {
                        tx.commit().await.map_err(failed)?;
                        let mut record = record;
                        record.not_before = record.not_before.map(|at| micros(at).0);
                        return Ok(Enqueued::Created(record));
                    }
                    tx.rollback().await.map_err(failed)?;
                }
                Err(storage_error(
                    CODE,
                    "the run holding this idempotency key was pruned while enqueueing three \
                     times; retry the enqueue",
                ))
            }

            async fn claim(
                &self,
                queue: &str,
                worker: &str,
                ttl: Duration,
            ) -> Result<Option<ClaimedRun>> {
                let (now, now_us) = now(&*self.clock);
                let (_, expires_us) = deadline(now, ttl, "run_store.invalid_ttl")?;
                let mut tx = self.pool.begin_with($begin).await.map_err(failed)?;
                loop {
                    let candidate: Option<(i64, String, i64, i64)> =
                        sqlx::query_as(claim_candidate!($lock))
                            .bind(queue)
                            .bind(now_us)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(failed)?;
                    let Some((seq, state, attempt, max_attempts)) = candidate else {
                        tx.commit().await.map_err(failed)?;
                        return Ok(None);
                    };
                    if state == "running" && attempt >= max_attempts {
                        sqlx::query(EXHAUST)
                            .bind(format!(
                                "attempts exhausted: the lease of attempt {attempt} of \
                                 {max_attempts} expired"
                            ))
                            .bind(now_us)
                            .bind(seq)
                            .execute(&mut *tx)
                            .await
                            .map_err(failed)?;
                        continue;
                    }
                    let row = sqlx::query(CLAIM)
                        .bind(worker)
                        .bind(expires_us)
                        .bind(now_us)
                        .bind(seq)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(failed)?;
                    let record = decode_run!(&row)?;
                    tx.commit().await.map_err(failed)?;
                    return Ok(Some(ClaimedRun { epoch: record.lease_epoch, record }));
                }
            }

            async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
                let (now, now_us) = now(&*self.clock);
                let (_, expires_us) = deadline(now, ttl, "run_store.invalid_ttl")?;
                let Ok(stored_epoch) = to_i64(CODE, epoch) else {
                    return Err(self.refused(run_id, epoch).await);
                };
                let cancel_requested: Option<bool> = sqlx::query_scalar(HEARTBEAT)
                    .bind(expires_us)
                    .bind(now_us)
                    .bind(run_id)
                    .bind(stored_epoch)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(failed)?;
                match cancel_requested {
                    Some(cancel_requested) => Ok(cancel_requested),
                    None => Err(self.refused(run_id, epoch).await),
                }
            }

            async fn set_checkpoint(
                &self,
                run_id: &str,
                epoch: u64,
                checkpoint: RunCheckpointRef,
            ) -> Result<()> {
                let (_, now_us) = now(&*self.clock);
                let Ok(stored_epoch) = to_i64(CODE, epoch) else {
                    return Err(self.refused(run_id, epoch).await);
                };
                let updated = sqlx::query(SET_CHECKPOINT)
                    .bind(to_json(CODE, &checkpoint)?)
                    .bind(now_us)
                    .bind(run_id)
                    .bind(stored_epoch)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                if updated.rows_affected() == 0 {
                    return Err(self.refused(run_id, epoch).await);
                }
                Ok(())
            }

            async fn wait(
                &self,
                run_id: &str,
                epoch: u64,
                until: Option<DateTime<Utc>>,
            ) -> Result<()> {
                let (_, now_us) = now(&*self.clock);
                let Ok(stored_epoch) = to_i64(CODE, epoch) else {
                    return Err(self.refused(run_id, epoch).await);
                };
                let updated = sqlx::query(WAIT)
                    .bind(now_us)
                    .bind(until.map(|at| micros(at).1))
                    .bind(run_id)
                    .bind(stored_epoch)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                if updated.rows_affected() == 0 {
                    return Err(self.refused(run_id, epoch).await);
                }
                Ok(())
            }

            async fn wake(&self, run_id: &str) -> Result<()> {
                let (_, now_us) = now(&*self.clock);
                let updated = sqlx::query(WAKE)
                    .bind(now_us)
                    .bind(run_id)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                if updated.rows_affected() == 0 && self.get(run_id).await?.is_none() {
                    return Err(run_not_found_error(run_id));
                }
                Ok(())
            }

            async fn finish(
                &self,
                run_id: &str,
                epoch: u64,
                outcome: RunOutcome,
            ) -> Result<RunRecord> {
                let (_, now_us) = now(&*self.clock);
                let Ok(stored_epoch) = to_i64(CODE, epoch) else {
                    return Err(self.refused(run_id, epoch).await);
                };
                let (retryable, state, result, error) = match outcome {
                    RunOutcome::Completed(result) => {
                        (false, RunState::Completed, Some(to_json(CODE, &result)?), None)
                    }
                    RunOutcome::Failed { error, retryable } => {
                        (retryable, RunState::Failed, None, Some(error))
                    }
                    RunOutcome::Cancelled => (false, RunState::Cancelled, None, None),
                };
                let row = sqlx::query(FINISH)
                    .bind(retryable)
                    .bind(variant_name(&state)?)
                    .bind(result)
                    .bind(error)
                    .bind(now_us)
                    .bind(run_id)
                    .bind(stored_epoch)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(failed)?;
                match row {
                    Some(row) => decode_run!(&row),
                    None => Err(self.refused(run_id, epoch).await),
                }
            }

            async fn request_cancel(&self, run_id: &str) -> Result<()> {
                let (_, now_us) = now(&*self.clock);
                let updated = sqlx::query(REQUEST_CANCEL)
                    .bind(now_us)
                    .bind(run_id)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                if updated.rows_affected() == 0 && self.get(run_id).await?.is_none() {
                    return Err(run_not_found_error(run_id));
                }
                Ok(())
            }

            async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
                let row =
                    sqlx::query(GET).bind(run_id).fetch_optional(&self.pool).await.map_err(failed)?;
                row.map(|row| decode_run!(&row)).transpose()
            }

            async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
                let mut query = sqlx::QueryBuilder::<$db>::new(concat!(
                    "SELECT ",
                    run_columns!(),
                    " FROM adk_runs WHERE TRUE"
                ));
                let fields = [
                    (" AND queue = ", &filter.queue),
                    (" AND app_name = ", &filter.app_name),
                    (" AND user_id = ", &filter.user_id),
                    (" AND session_id = ", &filter.session_id),
                    (" AND concurrency_key = ", &filter.concurrency_key),
                ];
                for (clause, value) in fields {
                    if let Some(value) = value {
                        query.push(clause).push_bind(value.clone());
                    }
                }
                if !filter.states.is_empty() {
                    query.push(" AND state IN (");
                    let mut states = query.separated(", ");
                    for state in &filter.states {
                        states.push_bind(variant_name(state)?);
                    }
                    query.push(")");
                }
                query.push(" ORDER BY created_at_us, seq");
                if let Some(limit) = filter.limit {
                    query.push(" LIMIT ").push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
                }
                let rows = query.build().fetch_all(&self.pool).await.map_err(failed)?;
                rows.iter().map(|row| decode_run!(row)).collect()
            }

            async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
                let cutoff = micros(finished_before).1;
                let mut pruned = 0;
                loop {
                    let deleted = sqlx::query(PRUNE)
                        .bind(cutoff)
                        .bind(PRUNE_BATCH)
                        .execute(&self.pool)
                        .await
                        .map_err(failed)?
                        .rows_affected();
                    pruned += deleted;
                    if deleted < PRUNE_BATCH.cast_unsigned() {
                        return Ok(pruned);
                    }
                }
            }
        }
    };
}

#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] =
    &[(1, "create run table", run_table!("seq INTEGER PRIMARY KEY AUTOINCREMENT"))];

#[cfg(feature = "sqlite")]
sql_run_store! {
    /// SQLite-backed [`DurableRunStore`].
    ///
    /// `enqueue` and `claim` run under `BEGIN IMMEDIATE`, which takes SQLite's write lock
    /// before the first read, so concurrent claimers in any number of processes sharing
    /// the database file never claim one run twice. Worker writes are single fenced
    /// `UPDATE` statements.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use adk_core::{DurableRunStore, NewRun, RunOutcome};
    /// use adk_session::SqliteRunStore;
    /// use serde_json::json;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = SqliteRunStore::new("sqlite://runtime.db?mode=rwc").await?;
    /// store.migrate().await?;
    /// store.enqueue(NewRun::new("default", "app", "user", "agent", json!({"text": "hi"}))).await?;
    /// if let Some(claimed) = store.claim("default", "worker-1", Duration::from_secs(30)).await? {
    ///     store.finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!("ok"))).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    SqliteRunStore, sqlx::SqlitePool, sqlx::Sqlite,
    begin = "BEGIN IMMEDIATE",
    lock = "",
    // `BEGIN IMMEDIATE` already serializes every enqueue.
    serialize_key = |_: &mut sqlx::SqliteConnection, _: &str, _: &str| async { Ok::<_, adk_core::AdkError>(()) },
    |pool: &sqlx::SqlitePool| {
        let pool = pool.clone();
        async move {
            crate::migration::sqlite_runner::run_sql_migrations(
                &pool,
                REGISTRY_TABLE,
                SQLITE_MIGRATIONS,
                || async { Ok(false) },
            )
            .await
        }
    }
}

#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] =
    &[(1, "create run table", run_table!("seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY"))];

#[cfg(feature = "postgres")]
sql_run_store! {
    /// PostgreSQL-backed [`DurableRunStore`]. [`migrate`](Self::migrate) takes an
    /// advisory lock, so concurrent processes can call it at startup.
    ///
    /// `claim` selects the oldest claimable run with `FOR UPDATE SKIP LOCKED` and updates
    /// it in the same transaction, so concurrent workers in any number of processes each
    /// take a different run without waiting on one another. Enqueues that share a
    /// `(queue, concurrency_key)` are serialized with `pg_advisory_xact_lock`, so
    /// `Reject` and `Interrupt` see every earlier holder.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use adk_core::{DurableRunStore, NewRun, OnConflict};
    /// use adk_session::PostgresRunStore;
    /// use serde_json::json;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = PostgresRunStore::new("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// let run = NewRun::new("default", "app", "user", "agent", json!({"text": "hi"}))
    ///     .with_idempotency_key("webhook:evt_123")
    ///     .with_concurrency_key("thread:42", OnConflict::Interrupt);
    /// store.enqueue(run).await?;
    /// let claimed = store.claim("default", "worker-1", Duration::from_secs(30)).await?;
    /// # let _ = claimed;
    /// # Ok(())
    /// # }
    /// ```
    PostgresRunStore, sqlx::PgPool, sqlx::Postgres,
    begin = "BEGIN",
    lock = " FOR UPDATE OF r SKIP LOCKED",
    serialize_key = pg_serialize_key,
    |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, POSTGRES_MIGRATIONS)
                .await
        }
    }
}

/// Holds a transaction-scoped advisory lock on `(queue, key)` until the enqueue commits.
#[cfg(feature = "postgres")]
fn pg_serialize_key<'c>(
    conn: &'c mut sqlx::PgConnection,
    queue: &str,
    key: &str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'c>> {
    // Two pairs whose names hash alike share a lock, which only serializes their enqueues.
    let name = format!("adk_runs\u{1f}{queue}\u{1f}{key}");
    Box::pin(async move {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(name)
            .execute(conn)
            .await
            .map_err(failed)?;
        Ok(())
    })
}
