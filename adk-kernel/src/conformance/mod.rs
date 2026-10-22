//! Conformance suites for kernel store backends (spec 3.2 §11, §13 Q8; feature
//! `conformance`).
//!
//! `adk-core` cannot depend on `adk-kernel`, so the kernel's suites live here, in the form
//! of `adk_core::conformance`: a harness trait that opens the backend, one public function
//! per case, and a macro that generates one `#[tokio::test]` per case.
//!
//! | Store | Harness | Test macro | Cases |
//! |-------|---------|------------|-------|
//! | [`ProcessStore`](crate::ProcessStore) | [`ProcessStoreHarness`] | [`process_store_conformance_tests!`](crate::process_store_conformance_tests) | [`process_store`] |
//!
//! In-memory and SQLite run on the PR tier; PostgreSQL and Redis run in the CI `postgres`
//! job, `#[ignore]`d elsewhere. The agenda, driver, partition and ledger suites arrive with
//! their stores.
//!
//! A harness opens the backend; each call to `open` returns a new handle over the same
//! data, as a restarted or second process would see it. Each case writes under fresh
//! process ids, so a harness may point every case at one shared database. The macros need
//! `tokio` with the `macros` and `rt-multi-thread` features in the calling crate, and
//! apply the attributes written before `harness =` to every generated test.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_kernel::conformance::ProcessStoreHarness;
//! use adk_kernel::{InMemoryProcessStore, ProcessStore};
//!
//! struct InMemory;
//!
//! #[async_trait::async_trait]
//! impl ProcessStoreHarness for InMemory {
//!     async fn open(&self) -> Arc<dyn ProcessStore> {
//!         Arc::new(InMemoryProcessStore::new())
//!     }
//!
//!     fn is_durable(&self) -> bool {
//!         false
//!     }
//! }
//!
//! adk_kernel::process_store_conformance_tests! { harness = InMemory }
//! ```

pub mod process_store;

pub use process_store::ProcessStoreHarness;

/// Generates one test per [`process_store`] case for a [`ProcessStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! process_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_put_get_round_trip() {
            $crate::conformance::process_store::put_get_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_put_replaces() {
            $crate::conformance::process_store::put_replaces(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_list_filters_and_orders() {
            $crate::conformance::process_store::list_filters_and_orders(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_set_wake_and_queue() {
            $crate::conformance::process_store::set_wake_and_queue(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_unknown_pid() {
            $crate::conformance::process_store::unknown_pid(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_delete() {
            $crate::conformance::process_store::delete(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_records_survive_a_restart() {
            $crate::conformance::process_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_concurrent_writers() {
            $crate::conformance::process_store::concurrent_writers(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_sustained_writes() {
            $crate::conformance::process_store::sustained_writes(&$harness).await;
        }
    };
}
