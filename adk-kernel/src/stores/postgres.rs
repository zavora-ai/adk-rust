//! The PostgreSQL [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `postgres`).

use async_trait::async_trait;
use chrono::NaiveDate;

use super::{ProcessFilter, ProcessStore, SpendScope};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// A [`ProcessStore`] in PostgreSQL, shared by every replica (spec 3.2 §7.1, feature
/// `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`, which keeps sessions, approvals, runs,
/// modes, rules and plan checkpoints in the same database. Its table is
/// `adk_kernel_processes`; [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_process_migrations` under an advisory lock, so replicas starting together
/// migrate one at a time.
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
    pub async fn connect(_database_url: &str) -> Result<Self> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::connect" })
    }

    /// Creates or upgrades the store's table.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::migrate" })
    }
}

#[async_trait]
impl ProcessStore for PostgresProcessStore {
    async fn put(&self, _meta: ProcessMeta) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::put" })
    }

    async fn get(&self, _pid: &str) -> Result<Option<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::get" })
    }

    async fn list(&self, _filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::list" })
    }

    async fn set_wake(&self, _pid: &str, _wake: Option<WakeSet>) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::set_wake" })
    }

    async fn set_queue(&self, _pid: &str, _queue: QueueClass) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::set_queue" })
    }

    async fn delete(&self, _pid: &str) -> Result<bool> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::delete" })
    }

    async fn set_suspend_requested(&self, _pid: &str, _requested: bool) -> Result<()> {
        Err(KernelError::Unimplemented {
            item: "stores::PostgresProcessStore::set_suspend_requested",
        })
    }

    async fn add_spend(
        &self,
        _pid: &str,
        _partition: &PartitionId,
        _day: NaiveDate,
        _delta: SpendDelta,
    ) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::add_spend" })
    }

    async fn spend(
        &self,
        _scope: &SpendScope,
        _from: NaiveDate,
        _to_inclusive: NaiveDate,
    ) -> Result<u64> {
        Err(KernelError::Unimplemented { item: "stores::PostgresProcessStore::spend" })
    }
}
