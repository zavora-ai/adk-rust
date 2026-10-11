//! Conformance suites for the durable stores behind safe actions.
//!
//! Every [`SpendLedger`](crate::SpendLedger), [`ActionLedger`](crate::ActionLedger), and
//! [`ApprovalStore`](crate::ApprovalStore) backend — the in-memory ones here, the SQL,
//! Redis, MongoDB, Neo4j, and Firestore ones in `adk-session`, and any third-party
//! one — proves its contract by running the same suite:
//!
//! | Store | Harness | Test macro | Cases |
//! |-------|---------|------------|-------|
//! | [`SpendLedger`](crate::SpendLedger) | [`SpendLedgerHarness`](crate::conformance::SpendLedgerHarness) | [`spend_ledger_conformance_tests!`](crate::spend_ledger_conformance_tests) | [`spend_ledger`](crate::conformance::spend_ledger) |
//! | [`ActionLedger`](crate::ActionLedger) | [`ActionLedgerHarness`](crate::conformance::ActionLedgerHarness) | [`action_ledger_conformance_tests!`](crate::action_ledger_conformance_tests) | [`action_ledger`](crate::conformance::action_ledger) |
//! | [`ApprovalStore`](crate::ApprovalStore) | [`ApprovalStoreHarness`](crate::conformance::ApprovalStoreHarness) | [`approval_store_conformance_tests!`](crate::approval_store_conformance_tests) | [`approval_store`](crate::conformance::approval_store) |
//!
//! A harness opens the backend. Each call to `open` returns a new handle over the same
//! data, as a process restart or a second process would, so the suite can check that
//! records survive a restart and that two handles never overspend or double-execute.
//! Each case writes under fresh identifiers, so a harness may point every case at one
//! shared database.
//!
//! The macros expand to one `#[tokio::test]` per case on a multi-threaded runtime, so
//! the calling crate needs `tokio` with the `macros` and `rt-multi-thread` features.
//! Attributes written before `harness =` are applied to every generated test, which is
//! how a live-database suite is marked `#[ignore]`.
//!
//! Enable the `conformance` feature of `adk-core` in `[dev-dependencies]`.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_core::conformance::{SpendLedgerHarness, SpendLedgerSetup};
//! use adk_core::{SpendLedger, async_trait};
//!
//! struct MyHarness { url: String }
//!
//! #[async_trait]
//! impl SpendLedgerHarness for MyHarness {
//!     async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
//!         Arc::new(
//!             MyLedger::connect(&self.url).await.unwrap()
//!                 .with_limits(setup.limits)
//!                 .with_reservation_ttl(setup.reservation_ttl)
//!                 .with_clock(setup.clock),
//!         )
//!     }
//! }
//!
//! adk_core::spend_ledger_conformance_tests! {
//!     #[ignore = "requires a server at MY_URL"]
//!     harness = MyHarness { url: std::env::var("MY_URL").unwrap() },
//! }
//! ```

pub mod action_ledger;
pub mod approval_store;
pub mod spend_ledger;

pub use action_ledger::ActionLedgerHarness;
pub use approval_store::ApprovalStoreHarness;
pub use spend_ledger::{SpendLedgerHarness, SpendLedgerSetup};

/// Returns an identifier no other case or run uses. It contains `:` and `/` so a
/// backend that builds keys or paths from identifiers is checked for escaping.
fn unique(prefix: &str) -> String {
    format!("conformance:{prefix}/{}", uuid::Uuid::new_v4())
}

/// Generates one test per [`spend_ledger`] case for a [`SpendLedgerHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! spend_ledger_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_reserve_commit_release_accounting() {
            $crate::conformance::spend_ledger::reserve_commit_release_accounting(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_settles_each_reservation_once() {
            $crate::conformance::spend_ledger::settles_each_reservation_once(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_limits_apply_per_key() {
            $crate::conformance::spend_ledger::limits_apply_per_key(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_periods_roll_over_at_utc_boundaries() {
            $crate::conformance::spend_ledger::periods_roll_over_at_utc_boundaries(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_expired_reservations_stop_holding_budget() {
            $crate::conformance::spend_ledger::expired_reservations_stop_holding_budget(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_concurrent_reservations_never_exceed_a_cap() {
            $crate::conformance::spend_ledger::concurrent_reservations_never_exceed_a_cap(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_reservations_survive_a_restart() {
            $crate::conformance::spend_ledger::reservations_survive_a_restart(&$harness).await;
        }
    };
}

/// Generates one test per [`action_ledger`] case for an [`ActionLedgerHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! action_ledger_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn action_begin_complete_round_trip() {
            $crate::conformance::action_ledger::begin_complete_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn action_recorded_call_is_answered_without_executing() {
            $crate::conformance::action_ledger::recorded_call_is_answered_without_executing(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn action_incomplete_call_is_outcome_unknown_after_restart() {
            $crate::conformance::action_ledger::incomplete_call_is_outcome_unknown_after_restart(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn action_concurrent_begin_has_one_winner() {
            $crate::conformance::action_ledger::concurrent_begin_has_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn action_outcomes_are_write_once() {
            $crate::conformance::action_ledger::outcomes_are_write_once(&$harness).await;
        }
    };
}

/// Generates one test per [`approval_store`] case for an [`ApprovalStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! approval_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_pending_requests_are_kept_once_oldest_first() {
            $crate::conformance::approval_store::pending_requests_are_kept_once_oldest_first(
                &$harness,
            )
            .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_decision_is_consumed_once() {
            $crate::conformance::approval_store::decision_is_consumed_once(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_later_decision_replaces_earlier() {
            $crate::conformance::approval_store::later_decision_replaces_earlier(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_expired_decisions_are_discarded() {
            $crate::conformance::approval_store::expired_decisions_are_discarded(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_decisions_stay_in_their_scope() {
            $crate::conformance::approval_store::decisions_stay_in_their_scope(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_concurrent_take_has_one_winner() {
            $crate::conformance::approval_store::concurrent_take_has_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn approval_records_survive_a_restart() {
            $crate::conformance::approval_store::records_survive_a_restart(&$harness).await;
        }
    };
}
