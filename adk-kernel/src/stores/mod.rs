//! The kernel-owned stores and their backends (spec 3.2 §7.1, §7.3, §13 Q8).
//!
//! Phase 1 has one kernel-owned store, the [`ProcessStore`] side table, with the kernel's
//! daily spend totals per process (the phase-1 source of `ProcessView::spend_today` and
//! `spend_7d`). It runs on every
//! backend that holds the run, schedule and governance stores, with one conformance suite
//! (`process_store_conformance_tests!`, feature `conformance`):
//!
//! | Backend | Type | Feature | Opened from |
//! |---|---|---|---|
//! | In-memory | [`InMemoryProcessStore`] | — | — |
//! | SQLite | `SqliteProcessStore` | `sqlite` (default) | the pool of `harness.db`, with its own migration table |
//! | PostgreSQL | `PostgresProcessStore` | `postgres` | the URL of `adk-rust serve --store` |
//! | Redis | `RedisProcessStore` | `redis` | the URL of `adk-rust serve --store` |
//!
//! There is no file backend (§13 Q8). MongoDB, Firestore and Neo4j backends are phase 4,
//! and the agenda, driver, partition and ledger stores arrive with their phases.

use async_trait::async_trait;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use adk_core::RunState;

use crate::bus::SpendDelta;
use crate::error::Result;
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

mod agenda_memory;
mod memory;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "redis")]
mod redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;
#[cfg(feature = "sqlite")]
mod sqlite;

pub use agenda_memory::InMemoryAgendaStore;
pub use memory::InMemoryProcessStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresProcessStore;
#[cfg(feature = "redis")]
pub use redis::RedisProcessStore;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteProcessStore;

/// The kernel's side table of process metadata, keyed by run id (spec 3.2 §7.3, §6 D-2).
///
/// `RunRecord` has no metadata field, so the kernel keeps what the run store does not here
/// instead of changing the run store's schema across six backends. Rows of runs the run
/// store has pruned are deleted with [`delete`](Self::delete).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{InMemoryProcessStore, ProcessKind, ProcessMeta, ProcessStore, QueueClass};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryProcessStore::new();
/// store.put(ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal)).await?;
/// store.set_queue("run-1", QueueClass::Interactive).await?;
/// assert_eq!(store.get("run-1").await?.unwrap().queue, QueueClass::Interactive);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait ProcessStore: Send + Sync {
    /// Inserts `meta`, or replaces the row with the same `pid`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put(&self, meta: ProcessMeta) -> Result<()>;

    /// The row of `pid`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>>;

    /// The rows `filter` selects by partition, queue, parent and pending suspension request,
    /// ordered by `pid`, at most
    /// `filter.limit`. `filter.states` is a run-store field and is applied by
    /// [`ProcessTable::ps`](crate::ProcessTable::ps), not here.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>>;

    /// Sets or clears the wake set of `pid`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`](crate::KernelError::NotFound) when no row has
    /// `pid`, or [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()>;

    /// Sets the queue class of `pid`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`](crate::KernelError::NotFound) when no row has
    /// `pid`, or [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()>;

    /// Deletes the row of `pid`; `false` when there was none.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn delete(&self, pid: &str) -> Result<bool>;

    /// Sets or clears the pending suspension request of `pid`
    /// ([`ProcessMeta::suspend_requested`]).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`](crate::KernelError::NotFound) when no row has
    /// `pid`, or [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> Result<()>;

    /// Adds `delta` to the spend of `pid` in `partition` on the UTC day `day`. Additions
    /// from concurrent writers all count.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn add_spend(
        &self,
        pid: &str,
        partition: &PartitionId,
        day: NaiveDate,
        delta: SpendDelta,
    ) -> Result<()>;

    /// The spend `scope` recorded from `from` through `to_inclusive`, in micro-USD; 0 when
    /// none was.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn spend(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<u64>;
}

/// Whose spend [`ProcessStore::spend`] sums (spec 3.2 §7.3, §7.10).
///
/// # Example
///
/// ```rust
/// use adk_kernel::stores::SpendScope;
///
/// let scope = SpendScope::Process("run-1".into());
/// assert_eq!(serde_json::to_value(&scope).unwrap()["process"], "run-1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SpendScope {
    /// One process.
    Process(String),
    /// Every process of a partition.
    Partition(PartitionId),
}

/// Which processes `ps` lists (spec 3.2 §7.3, §8.1 `adk-rust ps [--partition P] [--state S]`).
///
/// An empty field matches everything.
///
/// # Example
///
/// ```rust
/// use adk_core::RunState;
/// use adk_kernel::{ProcessFilter, ProcessKind, ProcessMeta, QueueClass};
///
/// let filter = ProcessFilter::default().partition("default").states([RunState::Waiting]).limit(20);
/// let meta = ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal);
/// assert!(filter.matches(&meta));
/// assert!(!filter.clone().queue(QueueClass::Interactive).matches(&meta));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProcessFilter {
    /// Only processes in this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only processes whose run is in one of these states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<RunState>,
    /// Only processes in this queue class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueClass>,
    /// Only processes this run caused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Only processes whose [`ProcessMeta::suspend_requested`] is this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_requested: Option<bool>,
    /// At most this many processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl ProcessFilter {
    /// Only processes in `partition`.
    #[must_use]
    pub fn partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    /// Only processes whose run is in one of `states`.
    #[must_use]
    pub fn states(mut self, states: impl IntoIterator<Item = RunState>) -> Self {
        self.states = states.into_iter().collect();
        self
    }

    /// Only processes in `queue`.
    #[must_use]
    pub fn queue(mut self, queue: QueueClass) -> Self {
        self.queue = Some(queue);
        self
    }

    /// Only processes `parent` caused.
    #[must_use]
    pub fn parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Only processes whose pending suspension request is `requested`.
    #[must_use]
    pub fn suspend_requested(mut self, requested: bool) -> Self {
        self.suspend_requested = Some(requested);
        self
    }

    /// At most `limit` processes.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether `meta` passes the side-table fields: partition, queue, parent and the
    /// pending suspension request. `states` and `limit` are not checked here.
    pub fn matches(&self, meta: &ProcessMeta) -> bool {
        self.partition.as_ref().is_none_or(|partition| *partition == meta.partition)
            && self.queue.as_ref().is_none_or(|queue| *queue == meta.queue)
            && self.parent.as_ref().is_none_or(|parent| meta.parent.as_ref() == Some(parent))
            && self.suspend_requested.is_none_or(|requested| requested == meta.suspend_requested)
    }
}
