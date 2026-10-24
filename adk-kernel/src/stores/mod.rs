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
//! Phase 2 adds the [`AgendaStore`](crate::AgendaStore) on the same four backends
//! ([`InMemoryAgendaStore`], `SqliteAgendaStore`, `PostgresAgendaStore`, `RedisAgendaStore`,
//! suite `agenda_store_conformance_tests!`).
//!
//! The driver store (`drivers::DriverStore`, feature `drivers`) runs on
//! the same four backends: `InMemoryDriverStore`, `SqliteDriverStore`,
//! `PostgresDriverStore` and `RedisDriverStore` (spec 3.2 §7.7).
//!
//! The ledger store (`proc::LedgerStore`, feature `proc`) runs on the same four backends:
//! `InMemoryLedgerStore`, `SqliteLedgerStore`, `PostgresLedgerStore` and `RedisLedgerStore`
//! (spec 3.2 §7.10, suite `ledger_store_conformance_tests!`).
//!
//! The [`PartitionStore`](crate::PartitionStore) — the partition table and the crossing
//! ledger (§7.8) — runs on in-memory, SQLite, PostgreSQL and Redis
//! ([`InMemoryPartitionStore`], `SqlitePartitionStore`, `PostgresPartitionStore`,
//! `RedisPartitionStore`, suite `partition_store_conformance_tests!`).
//!
//! Phase 4 adds these stores on MongoDB, Firestore and Neo4j (§13 Q8), each **Alpha** until
//! its live conformance suite passes in CI:
//!
//! | Store | MongoDB (`mongodb`) | Firestore (`firestore`) | Neo4j (`neo4j`) |
//! |---|---|---|---|
//! | [`ProcessStore`] | `MongoProcessStore` | `FirestoreProcessStore` | `Neo4jProcessStore` |
//! | [`AgendaStore`](crate::AgendaStore) | `MongoAgendaStore` | `FirestoreAgendaStore` | `Neo4jAgendaStore` |
//! | `drivers::DriverStore` (with `drivers`) | `MongoDriverStore` | `FirestoreDriverStore` | `Neo4jDriverStore` |
//! | `proc::LedgerStore` (with `proc`) | `MongoLedgerStore` | `FirestoreLedgerStore` | `Neo4jLedgerStore` |
//! | [`PartitionStore`](crate::PartitionStore) | `MongoPartitionStore` | `FirestorePartitionStore` | `Neo4jPartitionStore` |
//!
//! There is no file backend and no Vertex AI backend (§13 Q8): no durable store on `main` is file-backed, and
//! Vertex AI holds sessions only.

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

#[cfg(feature = "firestore")]
mod agenda_firestore;
mod agenda_memory;
#[cfg(feature = "mongodb")]
mod agenda_mongodb;
#[cfg(feature = "neo4j")]
mod agenda_neo4j;
#[cfg(feature = "redis")]
mod agenda_redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) mod agenda_sql;
#[cfg(all(feature = "drivers", feature = "firestore"))]
mod driver_firestore;
#[cfg(feature = "drivers")]
mod driver_memory;
#[cfg(all(feature = "drivers", feature = "mongodb"))]
mod driver_mongodb;
#[cfg(all(feature = "drivers", feature = "neo4j"))]
mod driver_neo4j;
#[cfg(all(feature = "drivers", feature = "postgres"))]
mod driver_postgres;
#[cfg(all(feature = "drivers", feature = "redis"))]
mod driver_redis;
#[cfg(all(feature = "drivers", any(feature = "sqlite", feature = "postgres")))]
mod driver_sql;
#[cfg(all(feature = "drivers", feature = "sqlite"))]
mod driver_sqlite;
#[cfg(feature = "firestore")]
mod firestore;
#[cfg(all(feature = "proc", feature = "firestore"))]
mod ledger_firestore;
#[cfg(feature = "proc")]
mod ledger_memory;
#[cfg(all(feature = "proc", feature = "mongodb"))]
mod ledger_mongodb;
#[cfg(all(feature = "proc", feature = "neo4j"))]
mod ledger_neo4j;
#[cfg(all(feature = "proc", feature = "redis"))]
mod ledger_redis;
#[cfg(all(feature = "proc", any(feature = "sqlite", feature = "postgres")))]
pub(crate) mod ledger_sql;
mod memory;
#[cfg(feature = "mongodb")]
mod mongodb;
#[cfg(feature = "neo4j")]
mod neo4j;
#[cfg(feature = "firestore")]
mod partition_firestore;
mod partition_memory;
#[cfg(feature = "mongodb")]
mod partition_mongodb;
#[cfg(feature = "neo4j")]
mod partition_neo4j;
#[cfg(feature = "redis")]
mod partition_redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) mod partition_sql;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "redis")]
mod redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;
#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(feature = "firestore")]
pub use agenda_firestore::FirestoreAgendaStore;
pub use agenda_memory::InMemoryAgendaStore;
#[cfg(feature = "mongodb")]
pub use agenda_mongodb::MongoAgendaStore;
#[cfg(feature = "neo4j")]
pub use agenda_neo4j::Neo4jAgendaStore;
#[cfg(feature = "redis")]
pub use agenda_redis::RedisAgendaStore;
#[cfg(feature = "postgres")]
pub use agenda_sql::PostgresAgendaStore;
#[cfg(feature = "sqlite")]
pub use agenda_sql::SqliteAgendaStore;
#[cfg(all(feature = "drivers", feature = "firestore"))]
pub use driver_firestore::FirestoreDriverStore;
#[cfg(feature = "drivers")]
pub use driver_memory::InMemoryDriverStore;
#[cfg(all(feature = "drivers", feature = "mongodb"))]
pub use driver_mongodb::MongoDriverStore;
#[cfg(all(feature = "drivers", feature = "neo4j"))]
pub use driver_neo4j::Neo4jDriverStore;
#[cfg(all(feature = "drivers", feature = "postgres"))]
pub use driver_postgres::PostgresDriverStore;
#[cfg(all(feature = "drivers", feature = "redis"))]
pub use driver_redis::RedisDriverStore;
#[cfg(all(feature = "drivers", feature = "sqlite"))]
pub use driver_sqlite::SqliteDriverStore;
#[cfg(feature = "firestore")]
pub use firestore::FirestoreProcessStore;
#[cfg(all(feature = "proc", feature = "firestore"))]
pub use ledger_firestore::FirestoreLedgerStore;
#[cfg(feature = "proc")]
pub use ledger_memory::InMemoryLedgerStore;
#[cfg(all(feature = "proc", feature = "mongodb"))]
pub use ledger_mongodb::MongoLedgerStore;
#[cfg(all(feature = "proc", feature = "neo4j"))]
pub use ledger_neo4j::Neo4jLedgerStore;
#[cfg(all(feature = "proc", feature = "redis"))]
pub use ledger_redis::RedisLedgerStore;
#[cfg(all(feature = "proc", feature = "postgres"))]
pub use ledger_sql::PostgresLedgerStore;
#[cfg(all(feature = "proc", feature = "sqlite"))]
pub use ledger_sql::SqliteLedgerStore;
pub use memory::InMemoryProcessStore;
#[cfg(feature = "mongodb")]
pub use mongodb::MongoProcessStore;
#[cfg(feature = "neo4j")]
pub use neo4j::Neo4jProcessStore;
#[cfg(feature = "firestore")]
pub use partition_firestore::FirestorePartitionStore;
pub use partition_memory::InMemoryPartitionStore;
#[cfg(feature = "mongodb")]
pub use partition_mongodb::MongoPartitionStore;
#[cfg(feature = "neo4j")]
pub use partition_neo4j::Neo4jPartitionStore;
#[cfg(feature = "redis")]
pub use partition_redis::RedisPartitionStore;
#[cfg(feature = "postgres")]
pub use partition_sql::PostgresPartitionStore;
#[cfg(feature = "sqlite")]
pub use partition_sql::SqlitePartitionStore;
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

/// The stored form of a UTC day in the document and graph backends: its number of days from
/// the common era, as the SQL backends store it, so a range compares as integers.
#[cfg(any(feature = "mongodb", feature = "firestore", feature = "neo4j"))]
pub(crate) fn day_number(day: NaiveDate) -> i64 {
    i64::from(chrono::Datelike::num_days_from_ce(&day))
}

/// A spend delta as the document and graph backends store it, a signed 64-bit integer.
#[cfg(any(feature = "mongodb", feature = "firestore", feature = "neo4j"))]
pub(crate) fn stored_micro_usd(backend: &'static str, delta: SpendDelta) -> Result<i64> {
    i64::try_from(delta.micro_usd).map_err(|_| crate::KernelError::Store {
        backend,
        reason: format!(
            "a spend delta of {} micro-USD exceeds a signed 64-bit integer",
            delta.micro_usd
        ),
    })
}

/// A summed spend total as returned.
#[cfg(any(feature = "mongodb", feature = "firestore", feature = "neo4j"))]
pub(crate) fn stored_total(backend: &'static str, total: i64) -> Result<u64> {
    u64::try_from(total).map_err(|_| crate::KernelError::Store {
        backend,
        reason: format!("the stored spend total {total} is negative"),
    })
}
