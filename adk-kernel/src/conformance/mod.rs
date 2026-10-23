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
//! | [`AgendaStore`](crate::AgendaStore) | [`AgendaStoreHarness`] | [`agenda_store_conformance_tests!`](crate::agenda_store_conformance_tests) | [`agenda_store`] |
//! | [`DriverStore`](crate::drivers::DriverStore) (feature `drivers`) | `DriverStoreHarness` | `driver_store_conformance_tests!` | `driver_store` |
//! | `proc::LedgerStore` (feature `proc`) | `LedgerStoreHarness` | `ledger_store_conformance_tests!` | `ledger_store` |
//!
//! In-memory and SQLite run on the PR tier; PostgreSQL and Redis run in the CI `postgres`
//! job, `#[ignore]`d elsewhere. The partition suite arrives with its
//! stores.
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

pub mod agenda_store;
#[cfg(feature = "drivers")]
pub mod driver_store;
#[cfg(feature = "proc")]
pub mod ledger_store;
pub mod process_store;

pub use agenda_store::AgendaStoreHarness;
#[cfg(feature = "drivers")]
pub use driver_store::DriverStoreHarness;
#[cfg(feature = "proc")]
pub use ledger_store::LedgerStoreHarness;
pub use process_store::ProcessStoreHarness;

/// Generates one test per [`agenda_store`] case for an [`AgendaStoreHarness`] (spec 3.2
/// §7.5, §11).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[macro_export]
macro_rules! agenda_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_put_get_round_trip() {
            $crate::conformance::agenda_store::put_get_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_put_replaces() {
            $crate::conformance::agenda_store::put_replaces(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_list_filters_and_orders() {
            $crate::conformance::agenda_store::list_filters_and_orders(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_transition_compares_the_state() {
            $crate::conformance::agenda_store::transition_compares_the_state(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_concurrent_transitions_move_once() {
            $crate::conformance::agenda_store::concurrent_transitions_move_once(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_delete() {
            $crate::conformance::agenda_store::delete(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_records_survive_a_restart() {
            $crate::conformance::agenda_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agenda_store_concurrent_writers() {
            $crate::conformance::agenda_store::concurrent_writers(&$harness).await;
        }
    };
}

/// Generates one test per `ledger_store` case for a `LedgerStoreHarness` (spec 3.2 §7.10,
/// §11; feature `proc`).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[cfg(feature = "proc")]
#[macro_export]
macro_rules! ledger_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_append_round_trips() {
            $crate::conformance::ledger_store::append_round_trips(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_list_filters_and_pages() {
            $crate::conformance::ledger_store::list_filters_and_pages(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_records_survive_a_restart() {
            $crate::conformance::ledger_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_concurrent_appends() {
            $crate::conformance::ledger_store::concurrent_appends(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_sustained_appends() {
            $crate::conformance::ledger_store::sustained_appends(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_observations_replace_and_order() {
            $crate::conformance::ledger_store::observations_replace_and_order(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn ledger_store_pass_states_round_trip() {
            $crate::conformance::ledger_store::pass_states_round_trip(&$harness).await;
        }
    };
}

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
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_suspend_request_round_trips() {
            $crate::conformance::process_store::suspend_request_round_trips(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn process_store_spend_sums_by_day_and_scope() {
            $crate::conformance::process_store::spend_sums_by_day_and_scope(&$harness).await;
        }
    };
}

/// Generates one test per `driver_store` case for a `DriverStoreHarness` (feature
/// `drivers`).
///
/// See the [module documentation](crate::conformance) for the form; the harness implements
/// `DriverStoreHarness` instead.
#[cfg(feature = "drivers")]
#[macro_export]
macro_rules! driver_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_put_get_round_trip() {
            $crate::conformance::driver_store::put_get_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_put_replaces() {
            $crate::conformance::driver_store::put_replaces(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_list_orders_by_id() {
            $crate::conformance::driver_store::list_orders_by_id(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_unknown_id() {
            $crate::conformance::driver_store::unknown_id(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_delete() {
            $crate::conformance::driver_store::delete(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_records_survive_a_restart() {
            $crate::conformance::driver_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_concurrent_writers() {
            $crate::conformance::driver_store::concurrent_writers(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_park_round_trip_per_kind() {
            $crate::conformance::driver_store::park_round_trip_per_kind(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_parks_are_keyed_by_scope_and_call() {
            $crate::conformance::driver_store::parks_are_keyed_by_scope_and_call(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_list_parks_filters_by_driver() {
            $crate::conformance::driver_store::list_parks_filters_by_driver(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn driver_store_delete_park() {
            $crate::conformance::driver_store::delete_park(&$harness).await;
        }
    };
}
