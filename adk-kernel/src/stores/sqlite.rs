//! The SQLite [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `sqlite`).

use async_trait::async_trait;
use chrono::NaiveDate;

use super::{ProcessFilter, ProcessStore, SpendScope};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// A [`ProcessStore`] in SQLite (spec 3.2 §7.1, feature `sqlite`).
///
/// In the CLI it lives in `harness.db` beside the harness stores, on the same pool: its
/// table is `adk_kernel_processes` and [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_process_migrations`, distinct from every other store's tables. Call
/// `migrate` once before use.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = adk_kernel::SqliteProcessStore::new(pool);
/// store.migrate().await?;
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
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::migrate" })
    }
}

#[async_trait]
impl ProcessStore for SqliteProcessStore {
    async fn put(&self, _meta: ProcessMeta) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::put" })
    }

    async fn get(&self, _pid: &str) -> Result<Option<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::get" })
    }

    async fn list(&self, _filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::list" })
    }

    async fn set_wake(&self, _pid: &str, _wake: Option<WakeSet>) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::set_wake" })
    }

    async fn set_queue(&self, _pid: &str, _queue: QueueClass) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::set_queue" })
    }

    async fn delete(&self, _pid: &str) -> Result<bool> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::delete" })
    }

    async fn set_suspend_requested(&self, _pid: &str, _requested: bool) -> Result<()> {
        Err(KernelError::Unimplemented {
            item: "stores::SqliteProcessStore::set_suspend_requested",
        })
    }

    async fn add_spend(
        &self,
        _pid: &str,
        _partition: &PartitionId,
        _day: NaiveDate,
        _delta: SpendDelta,
    ) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::add_spend" })
    }

    async fn spend(
        &self,
        _scope: &SpendScope,
        _from: NaiveDate,
        _to_inclusive: NaiveDate,
    ) -> Result<u64> {
        Err(KernelError::Unimplemented { item: "stores::SqliteProcessStore::spend" })
    }
}
