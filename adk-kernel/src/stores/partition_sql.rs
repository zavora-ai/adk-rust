//! The SQLite and PostgreSQL [`PartitionStore`](crate::PartitionStore)s (spec 3.2 §7.8,
//! §13 Q8).
//!
//! One row per partition in `adk_kernel_partitions`, the record as JSON text beside its id,
//! so a partition reads back exactly as written and later fields need no schema change.
//! One row per crossing in `adk_kernel_crossings`: the columns a filter reads (`from_partition`,
//! `to_partition`, `run_key`, `fingerprint`, `outcome`, and `at` in microseconds since the
//! Unix epoch) beside the record as JSON text. Crossing rows are only ever inserted. A
//! durable run's live grants are rows of `adk_kernel_crossing_grants`, deleted when the run
//! is released.
//!
//! Parameters appear in ascending order in every statement, so SQLite, which numbers `$N`
//! parameters by first appearance, binds them as PostgreSQL does.

use crate::error::{KernelError, Result};
use crate::partition::{CrossingFilter, CrossingRecord, Partition};

/// The migration registry of the partition store, distinct from every other store's.
pub(crate) const PARTITION_REGISTRY_TABLE: &str = "_adk_kernel_partition_migrations";

/// The SQLite schema.
#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel partition and crossing tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_partitions (\
    id TEXT PRIMARY KEY, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_crossings (\
    id TEXT PRIMARY KEY, \
    at BIGINT NOT NULL, \
    run_key TEXT NOT NULL, \
    from_partition TEXT NOT NULL, \
    to_partition TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, \
    outcome TEXT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_run ON adk_kernel_crossings (run_key, fingerprint);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_from ON adk_kernel_crossings (from_partition, at, id);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_to ON adk_kernel_crossings (to_partition, at, id);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_at ON adk_kernel_crossings (at, id);\
CREATE TABLE IF NOT EXISTS adk_kernel_crossing_grants (\
    run_key TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, \
    PRIMARY KEY (run_key, fingerprint)\
);",
)];

/// The PostgreSQL schema; ids take the `C` collation, so they order by the bytes of the
/// UTF-8 text on every server, as the other backends do.
#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel partition and crossing tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_partitions (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_crossings (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    at BIGINT NOT NULL, \
    run_key TEXT NOT NULL, \
    from_partition TEXT NOT NULL, \
    to_partition TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, \
    outcome TEXT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_run ON adk_kernel_crossings (run_key, fingerprint);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_from ON adk_kernel_crossings (from_partition, at, id);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_to ON adk_kernel_crossings (to_partition, at, id);\
CREATE INDEX IF NOT EXISTS adk_kernel_crossings_at ON adk_kernel_crossings (at, id);\
CREATE TABLE IF NOT EXISTS adk_kernel_crossing_grants (\
    run_key TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, \
    PRIMARY KEY (run_key, fingerprint)\
);",
)];

pub(crate) const PUT: &str = "\
INSERT INTO adk_kernel_partitions (id, record) VALUES ($1, $2) \
ON CONFLICT (id) DO UPDATE SET record = excluded.record";

pub(crate) const GET: &str = "SELECT record FROM adk_kernel_partitions WHERE id = $1";

pub(crate) const LIST: &str = "SELECT record FROM adk_kernel_partitions ORDER BY id";

pub(crate) const DELETE: &str = "DELETE FROM adk_kernel_partitions WHERE id = $1";

pub(crate) const RECORD_CROSSING: &str = "\
INSERT INTO adk_kernel_crossings \
    (id, at, run_key, from_partition, to_partition, fingerprint, outcome, record) \
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";

pub(crate) const GRANT: &str = "\
INSERT INTO adk_kernel_crossing_grants (run_key, fingerprint) VALUES ($1, $2) \
ON CONFLICT (run_key, fingerprint) DO NOTHING";

pub(crate) const GRANTS: &str =
    "SELECT fingerprint FROM adk_kernel_crossing_grants WHERE run_key = $1 ORDER BY fingerprint";

pub(crate) const RELEASE_RUN: &str = "DELETE FROM adk_kernel_crossing_grants WHERE run_key = $1";

/// A value bound to a `crossings` statement.
pub(crate) enum Bind {
    Text(String),
    Int(i64),
}

/// The `crossings` statement for `filter` and its parameters, in order.
pub(crate) fn crossings(filter: &CrossingFilter) -> (String, Vec<Bind>) {
    let mut conditions = Vec::new();
    let mut binds = Vec::new();
    let mut text = |column: &str, value: String, binds: &mut Vec<Bind>| {
        binds.push(Bind::Text(value));
        conditions.push(format!("{column} = ${}", binds.len()));
    };
    if let Some(from) = &filter.from {
        text("from_partition", from.as_str().to_string(), &mut binds);
    }
    if let Some(to) = &filter.to {
        text("to_partition", to.as_str().to_string(), &mut binds);
    }
    if let Some(run) = &filter.run {
        text("run_key", run.clone(), &mut binds);
    }
    if let Some(fingerprint) = &filter.fingerprint {
        text("fingerprint", fingerprint.clone(), &mut binds);
    }
    if !filter.outcomes.is_empty() {
        let mut placeholders = Vec::new();
        for outcome in &filter.outcomes {
            binds.push(Bind::Text(outcome.as_str().to_string()));
            placeholders.push(format!("${}", binds.len()));
        }
        conditions.push(format!("outcome IN ({})", placeholders.join(", ")));
    }
    if let Some(since) = filter.since {
        binds.push(Bind::Int(since.timestamp_micros()));
        conditions.push(format!("at >= ${}", binds.len()));
    }
    let mut sql = "SELECT record FROM adk_kernel_crossings".to_string();
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY at, id");
    if let Some(limit) = filter.limit {
        binds.push(Bind::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
        sql.push_str(&format!(" LIMIT ${}", binds.len()));
    }
    (sql, binds)
}

/// Reads a stored partition back.
pub(crate) fn decode_partition(backend: &'static str, record: &str) -> Result<Partition> {
    serde_json::from_str(record).map_err(|error| KernelError::Store {
        backend,
        reason: format!("a stored partition is corrupt: {error}"),
    })
}

/// Reads a stored crossing back.
pub(crate) fn decode_crossing(backend: &'static str, record: &str) -> Result<CrossingRecord> {
    serde_json::from_str(record).map_err(|error| KernelError::Store {
        backend,
        reason: format!("a stored crossing record is corrupt: {error}"),
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store {
        backend,
        reason: format!("could not {action} the partition table: {error}"),
    }
}

/// Implements [`PartitionStore`](crate::PartitionStore) for a store with a `pool` field over
/// the shared statements.
macro_rules! sql_partition_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::partition::PartitionStore for $store {
            async fn put(
                &self,
                partition: $crate::partition::Partition,
            ) -> $crate::error::Result<()> {
                let record = serde_json::to_string(&partition)?;
                sqlx::query($crate::stores::partition_sql::PUT)
                    .bind(partition.id.as_str())
                    .bind(record)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::partition_sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get(
                &self,
                id: &$crate::partition::PartitionId,
            ) -> $crate::error::Result<Option<$crate::partition::Partition>> {
                let row: Option<(String,)> = sqlx::query_as($crate::stores::partition_sql::GET)
                    .bind(id.as_str())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::partition_sql::db_error($backend, "read", &e))?;
                row.map(|(record,)| {
                    $crate::stores::partition_sql::decode_partition($backend, &record)
                })
                .transpose()
            }

            async fn list(&self) -> $crate::error::Result<Vec<$crate::partition::Partition>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::stores::partition_sql::LIST)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::partition_sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(record,)| {
                        $crate::stores::partition_sql::decode_partition($backend, record)
                    })
                    .collect()
            }

            async fn delete(
                &self,
                id: &$crate::partition::PartitionId,
            ) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::partition_sql::DELETE)
                    .bind(id.as_str())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::partition_sql::db_error($backend, "delete from", &e)
                    })?;
                Ok(done.rows_affected() > 0)
            }

            async fn record_crossing(
                &self,
                record: $crate::partition::CrossingRecord,
            ) -> $crate::error::Result<()> {
                let json = serde_json::to_string(&record)?;
                sqlx::query($crate::stores::partition_sql::RECORD_CROSSING)
                    .bind(&record.id)
                    .bind(record.at.timestamp_micros())
                    .bind(&record.run)
                    .bind(record.from.as_str())
                    .bind(record.to.as_str())
                    .bind(&record.fingerprint)
                    .bind(record.outcome.as_str())
                    .bind(json)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::partition_sql::db_error(
                            $backend,
                            "record a crossing in",
                            &e,
                        )
                    })?;
                Ok(())
            }

            async fn crossings(
                &self,
                filter: &$crate::partition::CrossingFilter,
            ) -> $crate::error::Result<Vec<$crate::partition::CrossingRecord>> {
                let (sql, binds) = $crate::stores::partition_sql::crossings(filter);
                let mut query = sqlx::query_as::<_, (String,)>(sqlx::AssertSqlSafe(sql));
                for bind in binds {
                    query = match bind {
                        $crate::stores::partition_sql::Bind::Text(text) => query.bind(text),
                        $crate::stores::partition_sql::Bind::Int(int) => query.bind(int),
                    };
                }
                let rows = query.fetch_all(&self.pool).await.map_err(|e| {
                    $crate::stores::partition_sql::db_error($backend, "list crossings in", &e)
                })?;
                rows.iter()
                    .map(|(record,)| {
                        $crate::stores::partition_sql::decode_crossing($backend, record)
                    })
                    .collect()
            }

            async fn grant(&self, run: &str, fingerprint: &str) -> $crate::error::Result<()> {
                sqlx::query($crate::stores::partition_sql::GRANT)
                    .bind(run)
                    .bind(fingerprint)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::partition_sql::db_error($backend, "record a grant in", &e)
                    })?;
                Ok(())
            }

            async fn grants(&self, run: &str) -> $crate::error::Result<Vec<String>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::stores::partition_sql::GRANTS)
                    .bind(run)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::partition_sql::db_error($backend, "read the grants in", &e)
                    })?;
                Ok(rows.into_iter().map(|(fingerprint,)| fingerprint).collect())
            }

            async fn release_run(&self, run: &str) -> $crate::error::Result<u64> {
                let done = sqlx::query($crate::stores::partition_sql::RELEASE_RUN)
                    .bind(run)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::partition_sql::db_error($backend, "release grants in", &e)
                    })?;
                Ok(done.rows_affected())
            }
        }
    };
}

/// A [`PartitionStore`](crate::PartitionStore) in SQLite (spec 3.2 §7.1, §7.8, feature
/// `sqlite`).
///
/// In the CLI it lives in `harness.db` beside the harness stores, on the same pool: its
/// tables are `adk_kernel_partitions` and `adk_kernel_crossings`, and
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_partition_migrations`,
/// distinct from every other store's. Call `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{Partition, PartitionId, PartitionStore, SqlitePartitionStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqlitePartitionStore::new(pool);
/// store.migrate().await?;
/// store.put(Partition::new("work", "work")).await?;
/// assert!(store.get(&PartitionId::new("work")).await?.is_some());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqlitePartitionStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqlitePartitionStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's tables.
    ///
    /// Idempotent: a step already recorded in `_adk_kernel_partition_migrations` is not run
    /// again.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            PARTITION_REGISTRY_TABLE,
            SQLITE_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

#[cfg(feature = "sqlite")]
sql_partition_store!(SqlitePartitionStore, "sqlite");

/// A [`PartitionStore`](crate::PartitionStore) in PostgreSQL, shared by every replica (spec
/// 3.2 §7.1, §7.8, feature `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its tables are
/// `adk_kernel_partitions` and `adk_kernel_crossings`; [`migrate`](Self::migrate) records
/// its steps in `_adk_kernel_partition_migrations` under an advisory lock, so replicas
/// starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::PostgresPartitionStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresPartitionStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresPartitionStore {
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

    /// Creates or upgrades the store's tables, under a session-level advisory lock keyed by
    /// the registry table, so replicas starting together migrate one at a time.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        migrate_locked(self.pool.clone()).await
    }
}

/// Runs the migrations under the advisory lock. Boxed so the future is checked for `Send`
/// here, with concrete lifetimes, rather than inside callers' `async_trait` methods.
#[cfg(feature = "postgres")]
fn migrate_locked(
    pool: sqlx::PgPool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    let store_error = |action: &str, error: &dyn std::fmt::Display| KernelError::Store {
        backend: "postgres",
        reason: format!("could not {action}: {error}"),
    };
    Box::pin(async move {
        let key = PARTITION_REGISTRY_TABLE
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
            .cast_signed();
        let mut lock = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .map_err(|error| store_error("connect for the migration lock", &error))?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut lock)
            .await
            .map_err(|error| store_error("take the migration lock", &error))?;
        let result = adk_session::migration::pg_runner::run_sql_migrations(
            &pool,
            PARTITION_REGISTRY_TABLE,
            POSTGRES_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "postgres", reason: error.message });
        let _ = lock.close().await;
        result
    })
}

#[cfg(feature = "postgres")]
sql_partition_store!(PostgresPartitionStore, "postgres");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition::CrossingOutcome;

    #[test]
    fn crossings_numbers_parameters_in_order() {
        assert_eq!(
            crossings(&CrossingFilter::default()).0,
            "SELECT record FROM adk_kernel_crossings ORDER BY at, id"
        );
        let filter = CrossingFilter::default()
            .from("home")
            .to("work")
            .run("run-1")
            .fingerprint("sha256:00")
            .outcomes([CrossingOutcome::Approved, CrossingOutcome::RunGrant])
            .since(chrono::DateTime::UNIX_EPOCH)
            .limit(3);
        let (sql, binds) = crossings(&filter);
        assert_eq!(
            sql,
            "SELECT record FROM adk_kernel_crossings WHERE from_partition = $1 AND \
             to_partition = $2 AND run_key = $3 AND fingerprint = $4 AND outcome IN ($5, $6) \
             AND at >= $7 ORDER BY at, id LIMIT $8"
        );
        assert_eq!(binds.len(), 8);
    }
}
