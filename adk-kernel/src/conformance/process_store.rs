//! Conformance cases for [`ProcessStore`] backends (spec 3.2 §11, §13 Q8).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`put_get_round_trip`] | A row reads back exactly as put, every field and every [`Wake`](crate::Wake), [`ProcessKind`](crate::ProcessKind) and [`Suspension`](crate::Suspension) variant included; ids with `:`, `/` and non-ASCII characters survive |
//! | [`put_replaces`] | A second put with the same `pid` replaces the row |
//! | [`list_filters_and_orders`] | `list` applies partition, queue, parent and limit, orders by `pid`, and ignores `states` |
//! | [`set_wake_and_queue`] | `set_wake` sets and clears the wake set, `set_queue` the queue, leaving every other field |
//! | [`unknown_pid`] | `get` of an unknown `pid` is `None`; `set_wake` and `set_queue` fail with `kernel.not_found` |
//! | [`delete`] | `delete` removes the row and answers `true`, then `false` |
//! | [`records_survive_a_restart`] | Rows read back through a new handle (durable backends only) |
//! | [`concurrent_writers`] | Concurrent puts and updates from two handles over distinct pids lose no row |
//! | [`sustained_writes`] | A sustained stream of puts, updates and deletes stays consistent and readable (SQLite's WAL included) |

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::KernelError;
use crate::stores::ProcessStore;

/// Opens a [`ProcessStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::ProcessStoreHarness;
/// use adk_kernel::{InMemoryProcessStore, ProcessStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl ProcessStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ProcessStore> {
///         Arc::new(InMemoryProcessStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait ProcessStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ProcessStore>;

    /// Whether data written through one handle is visible through the next. `false` skips
    /// the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

fn stub(case: &'static str) -> ! {
    panic!("{}", KernelError::Unimplemented { item: case })
}

/// A row reads back exactly as put.
pub async fn put_get_round_trip(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::put_get_round_trip")
}

/// A second put with the same `pid` replaces the row.
pub async fn put_replaces(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::put_replaces")
}

/// `list` applies partition, queue, parent and limit and orders by `pid`.
pub async fn list_filters_and_orders(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::list_filters_and_orders")
}

/// `set_wake` and `set_queue` change only their field.
pub async fn set_wake_and_queue(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::set_wake_and_queue")
}

/// An unknown `pid` reads as `None` and fails updates with `kernel.not_found`.
pub async fn unknown_pid(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::unknown_pid")
}

/// `delete` removes the row once.
pub async fn delete(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::delete")
}

/// Rows survive a restart.
pub async fn records_survive_a_restart(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::records_survive_a_restart")
}

/// Concurrent writers over distinct pids lose no row.
pub async fn concurrent_writers(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::concurrent_writers")
}

/// A sustained stream of writes stays consistent.
pub async fn sustained_writes(_harness: &dyn ProcessStoreHarness) {
    stub("conformance::process_store::sustained_writes")
}
