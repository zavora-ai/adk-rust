//! The SQLite and PostgreSQL [`LedgerStore`](crate::proc::LedgerStore)s (spec 3.2 §7.10,
//! §13 Q8, feature `proc` with `sqlite` or `postgres`).
//!
//! Three tables, migrated in `_adk_kernel_ledger_migrations`, distinct from every other
//! store's:
//!
//! | Table | Rows |
//! |---|---|
//! | `adk_kernel_ledger` | One per ledger row: `seq` (the store's increasing key), `ts_ms` and `offset_s` (the instant in UTC milliseconds and the local offset in seconds), and the row's columns; `meta` is JSON text |
//! | `adk_kernel_observations` | One per observation, by id: its partition, its day (days from the common era) and its JSON |
//! | `adk_kernel_proc_state` | One per partition: the baseline pass state as JSON |
//!
//! Parameters appear in ascending order in every statement, so SQLite, which numbers `$N`
//! parameters by first appearance, binds them as PostgreSQL does.

use chrono::{DateTime, Datelike as _, FixedOffset, NaiveDate};
use serde_json::{Map, Value};

use crate::error::{KernelError, Result};
use crate::proc::{LedgerEntry, LedgerQuery, ObservationQuery};

/// The migration registry of the ledger store.
pub(crate) const REGISTRY_TABLE: &str = "_adk_kernel_ledger_migrations";

#[cfg(feature = "sqlite")]
pub(crate) const SQLITE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel ledger, observation and pass state tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_ledger (\
    seq INTEGER PRIMARY KEY AUTOINCREMENT, \
    ts_ms BIGINT NOT NULL, \
    offset_s INTEGER NOT NULL, \
    partition_id TEXT NOT NULL, \
    agent TEXT NOT NULL, \
    kind TEXT NOT NULL, \
    effect TEXT, \
    duration_ms BIGINT, \
    subject_hash TEXT, \
    meta TEXT NOT NULL, \
    trace_id TEXT\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_ledger_ts ON adk_kernel_ledger (ts_ms);\
CREATE INDEX IF NOT EXISTS adk_kernel_ledger_partition ON adk_kernel_ledger (partition_id, seq);\
CREATE INDEX IF NOT EXISTS adk_kernel_ledger_kind ON adk_kernel_ledger (kind, seq);\
CREATE TABLE IF NOT EXISTS adk_kernel_observations (\
    id TEXT PRIMARY KEY, \
    partition_id TEXT NOT NULL, \
    as_of BIGINT NOT NULL, \
    body TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_observations_as_of ON adk_kernel_observations (as_of, id);\
CREATE TABLE IF NOT EXISTS adk_kernel_proc_state (\
    partition_id TEXT PRIMARY KEY, \
    body TEXT NOT NULL\
);",
)];

/// `id` and `partition_id` take the `C` collation, so they order by the bytes of the UTF-8
/// text on every server, as the other backends do.
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel ledger, observation and pass state tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_ledger (\
    seq BIGSERIAL PRIMARY KEY, \
    ts_ms BIGINT NOT NULL, \
    offset_s INTEGER NOT NULL, \
    partition_id TEXT NOT NULL, \
    agent TEXT NOT NULL, \
    kind TEXT NOT NULL, \
    effect TEXT, \
    duration_ms BIGINT, \
    subject_hash TEXT, \
    meta TEXT NOT NULL, \
    trace_id TEXT\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_ledger_ts ON adk_kernel_ledger (ts_ms);\
CREATE INDEX IF NOT EXISTS adk_kernel_ledger_partition ON adk_kernel_ledger (partition_id, seq);\
CREATE INDEX IF NOT EXISTS adk_kernel_ledger_kind ON adk_kernel_ledger (kind, seq);\
CREATE TABLE IF NOT EXISTS adk_kernel_observations (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    partition_id TEXT NOT NULL, \
    as_of BIGINT NOT NULL, \
    body TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_observations_as_of ON adk_kernel_observations (as_of, id);\
CREATE TABLE IF NOT EXISTS adk_kernel_proc_state (\
    partition_id TEXT COLLATE \"C\" PRIMARY KEY, \
    body TEXT NOT NULL\
);",
)];

pub(crate) const APPEND: &str = "\
INSERT INTO adk_kernel_ledger \
    (ts_ms, offset_s, partition_id, agent, kind, effect, duration_ms, subject_hash, meta, \
     trace_id) \
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING seq";

const COLUMNS: &str = "seq, ts_ms, offset_s, partition_id, agent, kind, effect, duration_ms, \
                       subject_hash, meta, trace_id";

pub(crate) const PUT_OBSERVATION: &str = "\
INSERT INTO adk_kernel_observations (id, partition_id, as_of, body) VALUES ($1, $2, $3, $4) \
ON CONFLICT (id) DO UPDATE SET \
    partition_id = excluded.partition_id, as_of = excluded.as_of, body = excluded.body";

pub(crate) const PASS_STATE: &str =
    "SELECT body FROM adk_kernel_proc_state WHERE partition_id = $1";

pub(crate) const PASS_STATES: &str = "SELECT body FROM adk_kernel_proc_state ORDER BY partition_id";

pub(crate) const PUT_PASS_STATE: &str = "\
INSERT INTO adk_kernel_proc_state (partition_id, body) VALUES ($1, $2) \
ON CONFLICT (partition_id) DO UPDATE SET body = excluded.body";

/// A stored ledger row, in [`COLUMNS`] order.
pub(crate) type Row = (
    i64,
    i64,
    i32,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    String,
    Option<String>,
);

/// A value bound to a dynamic statement.
pub(crate) enum Bind {
    Text(String),
    Int(i64),
}

/// The `list` statement for `query` and its parameters, in order.
pub(crate) fn list(query: &LedgerQuery) -> (String, Vec<Bind>) {
    let mut conditions = Vec::new();
    let mut binds = Vec::new();
    let bind = |binds: &mut Vec<Bind>, value: Bind| {
        binds.push(value);
        format!("${}", binds.len())
    };
    if let Some(partition) = &query.partition {
        let n = bind(&mut binds, Bind::Text(partition.as_str().to_string()));
        conditions.push(format!("partition_id = {n}"));
    }
    if !query.kinds.is_empty() {
        let names: Vec<String> =
            query.kinds.iter().map(|kind| bind(&mut binds, Bind::Text(kind.clone()))).collect();
        conditions.push(format!("kind IN ({})", names.join(", ")));
    }
    if let Some(agent) = &query.agent {
        let n = bind(&mut binds, Bind::Text(agent.clone()));
        conditions.push(format!("agent = {n}"));
    }
    if let Some(from) = query.from {
        let n = bind(&mut binds, Bind::Int(from.timestamp_millis()));
        conditions.push(format!("ts_ms >= {n}"));
    }
    if let Some(to) = query.to {
        let n = bind(&mut binds, Bind::Int(to.timestamp_millis()));
        conditions.push(format!("ts_ms < {n}"));
    }
    if let Some(after) = query.after {
        let n = bind(&mut binds, Bind::Int(i64::try_from(after).unwrap_or(i64::MAX)));
        conditions.push(format!("seq > {n}"));
    }
    let mut sql = format!("SELECT {COLUMNS} FROM adk_kernel_ledger");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY seq");
    if let Some(limit) = query.limit {
        let n = bind(&mut binds, Bind::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
        sql.push_str(&format!(" LIMIT {n}"));
    }
    (sql, binds)
}

/// The `observations` statement for `query` and its parameters, in order.
pub(crate) fn observations(query: &ObservationQuery) -> (String, Vec<Bind>) {
    let mut conditions = Vec::new();
    let mut binds = Vec::new();
    if let Some(partition) = &query.partition {
        binds.push(Bind::Text(partition.as_str().to_string()));
        conditions.push(format!("partition_id = ${}", binds.len()));
    }
    if let Some(since) = query.since {
        binds.push(Bind::Int(day(since)));
        conditions.push(format!("as_of >= ${}", binds.len()));
    }
    let mut sql = "SELECT body FROM adk_kernel_observations".to_string();
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY as_of DESC, id");
    if let Some(limit) = query.limit {
        binds.push(Bind::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
        sql.push_str(&format!(" LIMIT ${}", binds.len()));
    }
    (sql, binds)
}

/// A day as stored: its number of days from the common era.
pub(crate) fn day(day: NaiveDate) -> i64 {
    i64::from(day.num_days_from_ce())
}

/// The columns of `entry` other than `seq`, encoded for storage.
pub(crate) struct Encoded {
    pub(crate) ts_ms: i64,
    pub(crate) offset_s: i32,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) meta: String,
}

impl Encoded {
    pub(crate) fn new(entry: &LedgerEntry) -> Result<Self> {
        Ok(Self {
            ts_ms: entry.ts.timestamp_millis(),
            offset_s: entry.ts.offset().local_minus_utc(),
            duration_ms: entry.duration_ms.map(|ms| i64::try_from(ms).unwrap_or(i64::MAX)),
            meta: serde_json::to_string(&entry.meta)?,
        })
    }
}

/// Reads a stored row back into the entry that was appended.
pub(crate) fn decode(backend: &'static str, row: Row) -> Result<LedgerEntry> {
    let (
        seq,
        ts_ms,
        offset_s,
        partition,
        agent,
        kind,
        effect,
        duration_ms,
        subject_hash,
        meta,
        trace_id,
    ) = row;
    let corrupt = |column: &str| KernelError::Store {
        backend,
        reason: format!("column `{column}` of ledger row {seq} is corrupt"),
    };
    let offset = FixedOffset::east_opt(offset_s).ok_or_else(|| corrupt("offset_s"))?;
    let ts = DateTime::from_timestamp_millis(ts_ms).ok_or_else(|| corrupt("ts_ms"))?;
    let meta: Map<String, Value> = serde_json::from_str(&meta).map_err(|_| corrupt("meta"))?;
    Ok(LedgerEntry {
        seq: u64::try_from(seq).map_err(|_| corrupt("seq"))?,
        ts: ts.with_timezone(&offset),
        partition: partition.into(),
        agent,
        kind,
        effect,
        duration_ms: duration_ms.map(|ms| u64::try_from(ms).unwrap_or(0)),
        subject_hash,
        meta,
        trace_id,
    })
}

/// Reads a stored JSON body.
pub(crate) fn body<T: serde::de::DeserializeOwned>(
    backend: &'static str,
    table: &str,
    body: &str,
) -> Result<T> {
    serde_json::from_str(body).map_err(|error| KernelError::Store {
        backend,
        reason: format!("a row of `{table}` is corrupt: {error}"),
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action}: {error}") }
}

/// Implements [`LedgerStore`](crate::proc::LedgerStore) for a store with a `pool` field over
/// the shared statements.
macro_rules! sql_ledger_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::proc::LedgerStore for $store {
            async fn append(
                &self,
                mut entry: $crate::proc::LedgerEntry,
            ) -> $crate::error::Result<$crate::proc::LedgerEntry> {
                let row = $crate::stores::ledger_sql::Encoded::new(&entry)?;
                let (seq,): (i64,) = sqlx::query_as($crate::stores::ledger_sql::APPEND)
                    .bind(row.ts_ms)
                    .bind(row.offset_s)
                    .bind(entry.partition.as_str())
                    .bind(&entry.agent)
                    .bind(&entry.kind)
                    .bind(&entry.effect)
                    .bind(row.duration_ms)
                    .bind(&entry.subject_hash)
                    .bind(row.meta)
                    .bind(&entry.trace_id)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::ledger_sql::db_error($backend, "append to the ledger", &e)
                    })?;
                entry.seq = u64::try_from(seq).unwrap_or_default();
                Ok(entry)
            }

            async fn list(
                &self,
                query: &$crate::proc::LedgerQuery,
            ) -> $crate::error::Result<Vec<$crate::proc::LedgerEntry>> {
                let (sql, binds) = $crate::stores::ledger_sql::list(query);
                let mut statement =
                    sqlx::query_as::<_, $crate::stores::ledger_sql::Row>(sqlx::AssertSqlSafe(sql));
                for bind in binds {
                    statement = match bind {
                        $crate::stores::ledger_sql::Bind::Text(text) => statement.bind(text),
                        $crate::stores::ledger_sql::Bind::Int(int) => statement.bind(int),
                    };
                }
                let rows = statement.fetch_all(&self.pool).await.map_err(|e| {
                    $crate::stores::ledger_sql::db_error($backend, "read the ledger", &e)
                })?;
                rows.into_iter()
                    .map(|row| $crate::stores::ledger_sql::decode($backend, row))
                    .collect()
            }

            async fn put_observation(
                &self,
                observation: $crate::proc::Observation,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::stores::ledger_sql::PUT_OBSERVATION)
                    .bind(&observation.id)
                    .bind(observation.partition.as_str())
                    .bind($crate::stores::ledger_sql::day(observation.as_of))
                    .bind(serde_json::to_string(&observation)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::ledger_sql::db_error($backend, "write an observation", &e)
                    })?;
                Ok(())
            }

            async fn observations(
                &self,
                query: &$crate::proc::ObservationQuery,
            ) -> $crate::error::Result<Vec<$crate::proc::Observation>> {
                let (sql, binds) = $crate::stores::ledger_sql::observations(query);
                let mut statement = sqlx::query_as::<_, (String,)>(sqlx::AssertSqlSafe(sql));
                for bind in binds {
                    statement = match bind {
                        $crate::stores::ledger_sql::Bind::Text(text) => statement.bind(text),
                        $crate::stores::ledger_sql::Bind::Int(int) => statement.bind(int),
                    };
                }
                let rows = statement.fetch_all(&self.pool).await.map_err(|e| {
                    $crate::stores::ledger_sql::db_error($backend, "read the observations", &e)
                })?;
                rows.iter()
                    .map(|(body,)| {
                        $crate::stores::ledger_sql::body($backend, "adk_kernel_observations", body)
                    })
                    .collect()
            }

            async fn pass_state(
                &self,
                partition: &$crate::partition::PartitionId,
            ) -> $crate::error::Result<Option<$crate::proc::PassState>> {
                let row: Option<(String,)> = sqlx::query_as($crate::stores::ledger_sql::PASS_STATE)
                    .bind(partition.as_str())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::ledger_sql::db_error($backend, "read the pass state", &e)
                    })?;
                row.map(|(body,)| {
                    $crate::stores::ledger_sql::body($backend, "adk_kernel_proc_state", &body)
                })
                .transpose()
            }

            async fn pass_states(&self) -> $crate::error::Result<Vec<$crate::proc::PassState>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::stores::ledger_sql::PASS_STATES)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::ledger_sql::db_error($backend, "read the pass states", &e)
                    })?;
                rows.iter()
                    .map(|(body,)| {
                        $crate::stores::ledger_sql::body($backend, "adk_kernel_proc_state", body)
                    })
                    .collect()
            }

            async fn put_pass_state(
                &self,
                state: $crate::proc::PassState,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::stores::ledger_sql::PUT_PASS_STATE)
                    .bind(state.partition.as_str())
                    .bind(serde_json::to_string(&state)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::ledger_sql::db_error($backend, "write the pass state", &e)
                    })?;
                Ok(())
            }
        }
    };
}

/// A [`LedgerStore`](crate::proc::LedgerStore) in SQLite (spec 3.2 §7.1, §7.10, features
/// `proc` and `sqlite`).
///
/// In the CLI it lives in `harness.db` beside the harness's and the process table's stores,
/// on the same pool, with its own tables (`adk_kernel_ledger`, `adk_kernel_observations`,
/// `adk_kernel_proc_state`) and migration registry (`_adk_kernel_ledger_migrations`). Call
/// [`migrate`](Self::migrate) once before use. Each append is one statement, so writers on
/// the pool, or another process on the file, queue on SQLite's write lock for the pool's
/// busy timeout.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::{Activity, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerSink, LedgerStore, SqliteLedgerStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = Arc::new(SqliteLedgerStore::new(pool));
/// store.migrate().await?;
/// let ledger = Ledger::new(store.clone(), LedgerKey::from_bytes([1; 32]));
/// ledger.record(Activity::new(LedgerKind::Intent, "harness")).await?;
/// assert_eq!(store.list(&LedgerQuery::default()).await?[0].seq, 1);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteLedgerStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteLedgerStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's tables; a step already recorded is not run again.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            REGISTRY_TABLE,
            SQLITE_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

#[cfg(feature = "sqlite")]
sql_ledger_store!(SqliteLedgerStore, "sqlite");

/// A [`LedgerStore`](crate::proc::LedgerStore) in PostgreSQL, shared by every replica (spec
/// 3.2 §7.1, §7.10, features `proc` and `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. [`migrate`](Self::migrate) records
/// its steps in `_adk_kernel_ledger_migrations` under an advisory lock, so replicas starting
/// together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::proc::PostgresLedgerStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresLedgerStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresLedgerStore {
    /// A store over `pool`, such as the one the session service and the run store use.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool =
            sqlx::PgPool::connect(database_url).await.map_err(|error| KernelError::Store {
                backend: "postgres",
                reason: format!("could not connect: {error}"),
            })?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's tables under a session-level advisory lock keyed by
    /// the registry table.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        super::postgres::migrate_locked(self.pool.clone(), REGISTRY_TABLE, POSTGRES_MIGRATIONS)
            .await
    }
}

#[cfg(feature = "postgres")]
sql_ledger_store!(PostgresLedgerStore, "postgres");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_numbers_parameters_in_order() {
        let query = LedgerQuery::default()
            .partition("work")
            .kind("tool_call")
            .kind("intent")
            .agent("harness")
            .from(DateTime::from_timestamp_millis(1).unwrap())
            .to(DateTime::from_timestamp_millis(2).unwrap())
            .after(9)
            .limit(5);
        assert_eq!(
            list(&query).0,
            format!(
                "SELECT {COLUMNS} FROM adk_kernel_ledger WHERE partition_id = $1 AND kind IN \
                 ($2, $3) AND agent = $4 AND ts_ms >= $5 AND ts_ms < $6 AND seq > $7 ORDER BY \
                 seq LIMIT $8"
            )
        );
        assert_eq!(
            list(&LedgerQuery::default()).0,
            format!("SELECT {COLUMNS} FROM adk_kernel_ledger ORDER BY seq")
        );
        let observed = ObservationQuery::default()
            .partition("work")
            .since(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())
            .limit(2);
        assert_eq!(
            observations(&observed).0,
            "SELECT body FROM adk_kernel_observations WHERE partition_id = $1 AND as_of >= $2 \
             ORDER BY as_of DESC, id LIMIT $3"
        );
    }
}
