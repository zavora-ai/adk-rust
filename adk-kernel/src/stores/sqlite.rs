//! The SQLite [`ProcessStore`](crate::ProcessStore) (spec 3.2 §7.1, §13 Q8, feature `sqlite`).

use super::sql::{REGISTRY_TABLE, sql_process_store};
use crate::error::{KernelError, Result};

/// Migrations of [`SqliteProcessStore`], recorded in `_adk_kernel_process_migrations`.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel process and spend tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_processes (\
    pid TEXT PRIMARY KEY, \
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

/// A [`ProcessStore`](crate::ProcessStore) in SQLite (spec 3.2 §7.1, feature `sqlite`).
///
/// In the CLI it lives in `harness.db` beside the harness stores, on the same pool: its
/// tables are `adk_kernel_processes` and `adk_kernel_spend`, and
/// [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_process_migrations`, distinct from every other store's tables. Call
/// `migrate` once before use.
///
/// Every change is a single statement, so writers sharing the pool, or another process on
/// the same file, queue on SQLite's write lock for the pool's busy timeout instead of
/// failing; `harness.db` is opened with a 30-second one. `pid` order is SQLite's binary
/// collation, the byte order of the UTF-8 text.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{ProcessKind, ProcessMeta, ProcessStore, QueueClass, SqliteProcessStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteProcessStore::new(pool);
/// store.migrate().await?;
/// store.put(ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal)).await?;
/// assert_eq!(store.get("run-1").await?.unwrap().kind, ProcessKind::Goal);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteProcessStore {
    pool: sqlx::SqlitePool,
}

impl SqliteProcessStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's table.
    ///
    /// Idempotent: a step already recorded in `_adk_kernel_process_migrations` is not run
    /// again.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            REGISTRY_TABLE,
            MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

sql_process_store!(SqliteProcessStore, "sqlite");
