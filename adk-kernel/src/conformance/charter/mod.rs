//! Conformance suites for the charter module's stores (spec 3.5 §7.10, §7.11, §11;
//! features `charter` and `conformance`).
//!
//! | Store | Harness | Test macro | Cases |
//! |-------|---------|------------|-------|
//! | [`CharterStore`](crate::charter::CharterStore) | [`CharterStoreHarness`] | [`charter_store_conformance_tests!`](crate::charter_store_conformance_tests) | [`charter_store`] |
//! | [`KpiStore`](crate::charter::KpiStore) | [`KpiStoreHarness`] | [`kpi_store_conformance_tests!`](crate::kpi_store_conformance_tests) | [`kpi_store`] |
//! | [`AutonomyStore`](crate::charter::AutonomyStore) | [`AutonomyStoreHarness`] | [`autonomy_store_conformance_tests!`](crate::autonomy_store_conformance_tests) | [`autonomy_store`] |
//! | [`SignalStore`](crate::charter::SignalStore) | [`SignalStoreHarness`] | [`signal_store_conformance_tests!`](crate::signal_store_conformance_tests) | [`signal_store`] |
//!
//! A harness opens the backend; each `open` returns a new handle over the same data, as a
//! restarted or second replica would see it. Every case writes under fresh sites, jobs,
//! partitions and sources, so a harness may point every case at one shared database. The
//! macros are used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests)
//! is; see the [conformance module](crate::conformance).
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_kernel::charter::{CharterStore, InMemoryCharterStore};
//! use adk_kernel::conformance::charter::CharterStoreHarness;
//!
//! struct InMemory;
//!
//! #[async_trait::async_trait]
//! impl CharterStoreHarness for InMemory {
//!     async fn open(&self) -> Arc<dyn CharterStore> {
//!         Arc::new(InMemoryCharterStore::new())
//!     }
//!
//!     fn is_durable(&self) -> bool {
//!         false
//!     }
//! }
//!
//! adk_kernel::charter_store_conformance_tests! { harness = InMemory }
//! ```

pub mod autonomy_store;
pub mod charter_store;
pub mod fixtures;
pub mod kpi_store;
pub mod signal_store;

pub use autonomy_store::AutonomyStoreHarness;
pub use charter_store::CharterStoreHarness;
pub use kpi_store::KpiStoreHarness;
pub use signal_store::SignalStoreHarness;

/// Generates one test per [`charter_store`] case for a [`CharterStoreHarness`] (spec 3.5
/// §7.11, §11).
#[macro_export]
macro_rules! charter_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_empty_site() {
            $crate::conformance::charter::charter_store::empty_site(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_commit_reads_back() {
            $crate::conformance::charter::charter_store::commit_reads_back(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_versions_increment() {
            $crate::conformance::charter::charter_store::versions_increment(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_append_refuses_gaps_and_repeats() {
            $crate::conformance::charter::charter_store::append_refuses_gaps_and_repeats(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_commit_refuses_an_invalid_draft() {
            $crate::conformance::charter::charter_store::commit_refuses_an_invalid_draft(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_commit_refuses_an_unsatisfied_chain() {
            $crate::conformance::charter::charter_store::commit_refuses_an_unsatisfied_chain(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_the_charter_in_force_governs_a_change() {
            $crate::conformance::charter::charter_store::the_charter_in_force_governs_a_change(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_concurrent_commits_take_one_version_each() {
            $crate::conformance::charter::charter_store::concurrent_commits_take_one_version_each(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn charter_store_versions_survive_a_restart() {
            $crate::conformance::charter::charter_store::versions_survive_a_restart(&$harness)
                .await;
        }
    };
}

/// Generates one test per [`kpi_store`] case for a [`KpiStoreHarness`] (spec 3.5 §7.11,
/// §11).
#[macro_export]
macro_rules! kpi_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_samples_round_trip_in_order() {
            $crate::conformance::charter::kpi_store::samples_round_trip_in_order(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_one_sample_per_instant() {
            $crate::conformance::charter::kpi_store::one_sample_per_instant(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_refuses_a_value_that_is_not_finite() {
            $crate::conformance::charter::kpi_store::refuses_a_value_that_is_not_finite(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_aggregates_have_no_gaps() {
            $crate::conformance::charter::kpi_store::aggregates_have_no_gaps(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_states_round_trip() {
            $crate::conformance::charter::kpi_store::states_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_concurrent_records_keep_one_per_instant() {
            $crate::conformance::charter::kpi_store::concurrent_records_keep_one_per_instant(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn kpi_store_samples_survive_a_restart() {
            $crate::conformance::charter::kpi_store::samples_survive_a_restart(&$harness).await;
        }
    };
}

/// Generates one test per [`autonomy_store`] case for an [`AutonomyStoreHarness`] (spec 3.5
/// §7.11, §11).
#[macro_export]
macro_rules! autonomy_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_unknown_job() {
            $crate::conformance::charter::autonomy_store::unknown_job(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_apply_registers_then_moves() {
            $crate::conformance::charter::autonomy_store::apply_registers_then_moves(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_apply_refuses_other_revisions() {
            $crate::conformance::charter::autonomy_store::apply_refuses_other_revisions(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_apply_refuses_a_mismatched_change() {
            $crate::conformance::charter::autonomy_store::apply_refuses_a_mismatched_change(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_decisions_once_and_in_order() {
            $crate::conformance::charter::autonomy_store::decisions_once_and_in_order(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_concurrent_applies_move_once() {
            $crate::conformance::charter::autonomy_store::concurrent_applies_move_once(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn autonomy_store_records_survive_a_restart() {
            $crate::conformance::charter::autonomy_store::records_survive_a_restart(&$harness)
                .await;
        }
    };
}

/// Generates one test per [`signal_store`] case for a [`SignalStoreHarness`] (spec 3.5
/// §7.10, §11).
#[macro_export]
macro_rules! signal_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_first_opens_then_counts() {
            $crate::conformance::charter::signal_store::first_opens_then_counts(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_keys_are_apart() {
            $crate::conformance::charter::signal_store::keys_are_apart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_close_then_reopen() {
            $crate::conformance::charter::signal_store::close_then_reopen(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_list_filters_and_orders() {
            $crate::conformance::charter::signal_store::list_filters_and_orders(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_hits_count_per_source_and_window() {
            $crate::conformance::charter::signal_store::hits_count_per_source_and_window(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_concurrent_records_count_every_signal() {
            $crate::conformance::charter::signal_store::concurrent_records_count_every_signal(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn signal_store_records_survive_a_restart() {
            $crate::conformance::charter::signal_store::records_survive_a_restart(&$harness).await;
        }
    };
}
