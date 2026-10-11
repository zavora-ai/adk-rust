//! Conformance suites for the durable stores behind safe actions and the durable runtime.
//!
//! Every store backend — the in-memory ones here, the SQL, Redis, MongoDB, Neo4j, and
//! Firestore ones in `adk-session`, and any third-party one — proves its contract by
//! running the same suite:
//!
//! | Store | Harness | Test macro | Cases |
//! |-------|---------|------------|-------|
//! | [`SpendLedger`](crate::SpendLedger) | [`SpendLedgerHarness`](crate::conformance::SpendLedgerHarness) | [`spend_ledger_conformance_tests!`](crate::spend_ledger_conformance_tests) | [`spend_ledger`](crate::conformance::spend_ledger) |
//! | [`ActionLedger`](crate::ActionLedger) | [`ActionLedgerHarness`](crate::conformance::ActionLedgerHarness) | [`action_ledger_conformance_tests!`](crate::action_ledger_conformance_tests) | [`action_ledger`](crate::conformance::action_ledger) |
//! | [`ApprovalStore`](crate::ApprovalStore) | [`ApprovalStoreHarness`](crate::conformance::ApprovalStoreHarness) | [`approval_store_conformance_tests!`](crate::approval_store_conformance_tests) | [`approval_store`](crate::conformance::approval_store) |
//! | [`LeaseStore`](crate::LeaseStore) | [`LeaseStoreHarness`](crate::conformance::LeaseStoreHarness) | [`lease_store_conformance_tests!`](crate::lease_store_conformance_tests) | [`lease_store`](crate::conformance::lease_store) |
//! | [`DurableRunStore`](crate::DurableRunStore) | [`RunStoreHarness`](crate::conformance::RunStoreHarness) | [`run_store_conformance_tests!`](crate::run_store_conformance_tests) | [`run_store`](crate::conformance::run_store) |
//! | [`ScheduleStore`](crate::ScheduleStore) | [`ScheduleStoreHarness`](crate::conformance::ScheduleStoreHarness) | [`schedule_store_conformance_tests!`](crate::schedule_store_conformance_tests) | [`schedule_store`](crate::conformance::schedule_store) |
//! | [`GovernanceStateStore`](crate::GovernanceStateStore) | [`GovernanceStateStoreHarness`](crate::conformance::GovernanceStateStoreHarness) | [`governance_state_store_conformance_tests!`](crate::governance_state_store_conformance_tests) | [`governance_state_store`](crate::conformance::governance_state_store) |
//! | [`AuditLogStore`](crate::audit_log::AuditLogStore) | [`AuditLogStoreHarness`](crate::conformance::AuditLogStoreHarness) | [`audit_log_store_conformance_tests!`](crate::audit_log_store_conformance_tests) | [`audit_log_store`](crate::conformance::audit_log_store) |
//!
//! The harnesses of time-dependent stores receive the [`Clock`](crate::Clock) to read,
//! and the suites drive it as a [`ManualClock`](crate::ManualClock), so lease expiry,
//! delays, and timers are checked without waiting.
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
pub mod audit_log_store;
pub mod governance_state_store;
pub mod lease_store;
pub mod run_store;
pub mod schedule_store;
pub mod spend_ledger;

pub use action_ledger::ActionLedgerHarness;
pub use approval_store::ApprovalStoreHarness;
pub use audit_log_store::AuditLogStoreHarness;
pub use governance_state_store::GovernanceStateStoreHarness;
pub use lease_store::LeaseStoreHarness;
pub use run_store::RunStoreHarness;
pub use schedule_store::ScheduleStoreHarness;
pub use spend_ledger::{SpendLedgerHarness, SpendLedgerSetup};

/// Returns an identifier no other case or run uses. It contains `:` and `/` so a
/// backend that builds keys or paths from identifiers is checked for escaping.
fn unique(prefix: &str) -> String {
    format!("conformance:{prefix}/{}", uuid::Uuid::new_v4())
}

/// Like [`unique`], with non-ASCII characters as well, so a backend that builds keys,
/// paths, or document ids from identifiers is checked for escaping beyond ASCII.
fn unique_unicode(prefix: &str) -> String {
    format!("conformance:{prefix}/ü-日本/{}", uuid::Uuid::new_v4())
}

/// A fixed whole-second instant, so stores that round timestamps read them back unchanged.
fn base_time() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).single().expect("valid instant")
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
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn spend_unbounded_holds_need_an_uncapped_key() {
            $crate::conformance::spend_ledger::unbounded_holds_need_an_uncapped_key(&$harness)
                .await;
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

/// Generates one test per [`lease_store`] case for a [`LeaseStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! lease_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn lease_acquire_renew_release() {
            $crate::conformance::lease_store::acquire_renew_release(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn lease_concurrent_acquire_has_one_winner() {
            $crate::conformance::lease_store::concurrent_acquire_has_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn lease_expiry_takeover_bumps_epoch() {
            $crate::conformance::lease_store::expiry_takeover_bumps_epoch(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn lease_stale_renew_and_release_are_refused() {
            $crate::conformance::lease_store::stale_renew_and_release_are_refused(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn lease_leases_survive_a_restart() {
            $crate::conformance::lease_store::leases_survive_a_restart(&$harness).await;
        }
    };
}

/// Generates one test per [`run_store`] case for a [`RunStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! run_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_enqueue_claim_finish_round_trip() {
            $crate::conformance::run_store::enqueue_claim_finish_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_idempotent_enqueue_under_concurrency() {
            $crate::conformance::run_store::idempotent_enqueue_under_concurrency(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_on_conflict_policies() {
            $crate::conformance::run_store::on_conflict_policies(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_concurrent_claims_have_one_winner() {
            $crate::conformance::run_store::concurrent_claims_have_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_not_before_is_respected() {
            $crate::conformance::run_store::not_before_is_respected(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_expired_lease_recovery() {
            $crate::conformance::run_store::expired_lease_recovery(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_stale_epoch_writes_are_refused() {
            $crate::conformance::run_store::stale_epoch_writes_are_refused(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_retryable_failure_requeues_until_max_attempts() {
            $crate::conformance::run_store::retryable_failure_requeues_until_max_attempts(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_wait_and_wake() {
            $crate::conformance::run_store::wait_and_wake(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_cancel_is_visible_through_heartbeat() {
            $crate::conformance::run_store::cancel_is_visible_through_heartbeat(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_prune_removes_old_finished_runs() {
            $crate::conformance::run_store::prune_removes_old_finished_runs(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn run_runs_survive_a_restart() {
            $crate::conformance::run_store::runs_survive_a_restart(&$harness).await;
        }
    };
}

/// Generates one test per [`schedule_store`] case for a [`ScheduleStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! schedule_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn schedule_upsert_is_idempotent() {
            $crate::conformance::schedule_store::upsert_is_idempotent(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn schedule_due_orders_by_fire_time() {
            $crate::conformance::schedule_store::due_orders_by_fire_time(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn schedule_advance_has_one_winner() {
            $crate::conformance::schedule_store::advance_has_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn schedule_paused_schedules_are_not_due() {
            $crate::conformance::schedule_store::paused_schedules_are_not_due(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn schedule_schedules_survive_a_restart() {
            $crate::conformance::schedule_store::schedules_survive_a_restart(&$harness).await;
        }
    };
}

/// Generates one test per [`governance_state_store`] case for a [`GovernanceStateStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! governance_state_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn governance_set_get_list_round_trip() {
            $crate::conformance::governance_state_store::set_get_list_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn governance_concurrent_set_has_one_winner() {
            $crate::conformance::governance_state_store::concurrent_set_has_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn governance_scopes_are_independent() {
            $crate::conformance::governance_state_store::scopes_are_independent(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn governance_states_survive_a_restart() {
            $crate::conformance::governance_state_store::states_survive_a_restart(&$harness).await;
        }
    };
}

/// Generates one test per [`audit_log_store`] case for an [`AuditLogStoreHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! audit_log_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_append_assigns_contiguous_sequences() {
            $crate::conformance::audit_log_store::append_assigns_contiguous_sequences(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_append_refuses_a_wrong_start() {
            $crate::conformance::audit_log_store::append_refuses_a_wrong_start(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_append_refuses_invalid_batches() {
            $crate::conformance::audit_log_store::append_refuses_invalid_batches(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_concurrent_appends_have_one_winner() {
            $crate::conformance::audit_log_store::concurrent_appends_have_one_winner(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_logs_are_independent() {
            $crate::conformance::audit_log_store::logs_are_independent(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_hash_at_returns_leaves_and_nodes() {
            $crate::conformance::audit_log_store::hash_at_returns_leaves_and_nodes(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_reads_past_the_end_fail() {
            $crate::conformance::audit_log_store::reads_past_the_end_fail(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_checkpoints_never_fork() {
            $crate::conformance::audit_log_store::checkpoints_never_fork(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_prune_keeps_hashes() {
            $crate::conformance::audit_log_store::prune_keeps_hashes(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn audit_log_logs_survive_a_restart() {
            $crate::conformance::audit_log_store::logs_survive_a_restart(&$harness).await;
        }
    };
}
