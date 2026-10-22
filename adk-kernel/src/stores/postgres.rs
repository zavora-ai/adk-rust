//! The PostgreSQL [`ProcessStore`](crate::ProcessStore) (spec 3.2 §7.1, §13 Q8, feature
//! `postgres`).

use super::sql::{REGISTRY_TABLE, sql_process_store};
use crate::error::{KernelError, Result};

/// Migrations of [`PostgresProcessStore`], recorded in `_adk_kernel_process_migrations`.
///
/// `pid` takes the `C` collation, so `list` orders by the bytes of the UTF-8 text on every
/// server, as the other backends do, whatever the database's locale.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel process and spend tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_processes (\
    pid TEXT COLLATE \"C\" PRIMARY KEY, \
    parent TEXT, \
    partition_id TEXT NOT NULL, \
    queue TEXT NOT NULL, \
    kind TEXT NOT NULL, \
    wake TEXT, \
    suspended_by TEXT, \
    suspend_requested BOOLEAN NOT NULL DEFAULT FALSE, \
    agenda_item TEXT, \
    labels TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_processes_partition ON adk_kernel_processes (partition_id, pid);\
CREATE INDEX IF NOT EXISTS adk_kernel_processes_queue ON adk_kernel_processes (queue, pid);\
CREATE INDEX IF NOT EXISTS adk_kernel_processes_parent ON adk_kernel_processes (parent, pid);\
CREATE INDEX IF NOT EXISTS adk_kernel_processes_suspend ON adk_kernel_processes (suspend_requested, pid);\
CREATE TABLE IF NOT EXISTS adk_kernel_spend (\
    pid TEXT NOT NULL, \
    partition_id TEXT NOT NULL, \
    day BIGINT NOT NULL, \
    micro_usd BIGINT NOT NULL, \
    PRIMARY KEY (pid, partition_id, day)\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_spend_partition ON adk_kernel_spend (partition_id, day);",
)];

/// A [`ProcessStore`](crate::ProcessStore) in PostgreSQL, shared by every replica (spec 3.2
/// §7.1, feature `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`, which keeps sessions, approvals, runs,
/// modes, rules and plan checkpoints in the same database. Its tables are
/// `adk_kernel_processes` and `adk_kernel_spend`; [`migrate`](Self::migrate) records its
/// steps in
/// `_adk_kernel_process_migrations` under an advisory lock, so replicas starting together
/// migrate one at a time.
///
/// Every change is a single statement, so replicas writing different fields of one row
/// never lose each other's update.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::PostgresProcessStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresProcessStore {
    pool: sqlx::PgPool,
}

impl PostgresProcessStore {
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

    /// Creates or upgrades the store's table.
    ///
    /// The steps run under a session-level advisory lock keyed by the registry table, held
    /// on a connection opened beside the pool, so a one-connection pool still runs the
    /// statements and replicas starting together migrate one at a time.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        migrate_locked(self.pool.clone(), REGISTRY_TABLE, MIGRATIONS).await
    }
}

/// Runs `migrations` recorded in `registry` under an advisory lock keyed by `registry`.
/// Boxed so the future is checked for `Send` here, with concrete lifetimes, rather than
/// inside callers' `async_trait` methods.
pub(super) fn migrate_locked(
    pool: sqlx::PgPool,
    registry: &'static str,
    migrations: &'static [(i64, &'static str, &'static str)],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    let store_error = |action: &str, error: &dyn std::fmt::Display| KernelError::Store {
        backend: "postgres",
        reason: format!("could not {action}: {error}"),
    };
    Box::pin(async move {
        let key = registry
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
            registry,
            migrations,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "postgres", reason: error.message });
        let _ = lock.close().await;
        result
    })
}

sql_process_store!(PostgresProcessStore, "postgres");
