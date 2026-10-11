//! Conformance cases for [`SpendLedger`] backends.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`reserve_commit_release_accounting`] | Holds and committed spend count against a cap; release frees a hold; commit records the actual amount, even past the cap |
//! | [`settles_each_reservation_once`] | A second commit or release, or one for an unknown id, fails with `spend.unknown_reservation` and changes nothing |
//! | [`limits_apply_per_key`] | Organization, agent, and vendor caps apply together; `spent` sums by scope |
//! | [`periods_roll_over_at_utc_boundaries`] | Day and month windows restart at UTC midnight and on the first of the month |
//! | [`expired_reservations_stop_holding_budget`] | An abandoned hold stops counting after its time to live; a late commit still records spend |
//! | [`concurrent_reservations_never_exceed_a_cap`] | Concurrent reservations through two handles never grant more than the cap |
//! | [`reservations_survive_a_restart`] | Holds and settlements made before a restart count after it |

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use super::unique;
use crate::{
    AdkError, Clock, DEFAULT_RESERVATION_TTL, ErrorCategory, ManualClock, ReservationId,
    SPEND_LIMIT_EXCEEDED_CODE, SpendKey, SpendLedger, SpendLimits, SpendPeriod,
};

/// How the suite configures a ledger it opens.
#[derive(Debug, Clone)]
pub struct SpendLedgerSetup {
    /// The caps every reservation is checked against.
    pub limits: SpendLimits,
    /// How long an unsettled reservation holds budget.
    pub reservation_ttl: Duration,
    /// The clock the ledger reads for period windows and expiry.
    pub clock: Arc<dyn Clock>,
}

/// Opens a [`SpendLedger`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::{SpendLedgerHarness, SpendLedgerSetup};
/// use adk_core::{InMemorySpendLedger, SpendLedger, async_trait};
///
/// struct InMemory;
///
/// #[async_trait]
/// impl SpendLedgerHarness for InMemory {
///     async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
///         Arc::new(
///             InMemorySpendLedger::new(setup.limits)
///                 .with_reservation_ttl(setup.reservation_ttl)
///                 .with_clock(setup.clock),
///         )
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait SpendLedgerHarness: Send + Sync {
    /// Opens a ledger configured with `setup`.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. Panics when the backend
    /// cannot be opened.
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger>;

    /// Whether data written through one handle is visible through the next.
    ///
    /// `false` skips the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fixed mid-month, mid-day instant, so advancing by minutes never crosses a window.
fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).single().expect("valid instant")
}

fn setup(limits: SpendLimits, clock: &ManualClock) -> SpendLedgerSetup {
    SpendLedgerSetup {
        limits,
        reservation_ttl: DEFAULT_RESERVATION_TTL,
        clock: Arc::new(clock.clone()),
    }
}

fn daily_cap(org: &str, max: u64) -> SpendLimits {
    SpendLimits::new().limit(SpendKey::org(org).per(SpendPeriod::Day), max)
}

#[track_caller]
fn assert_limit_exceeded(
    error: &AdkError,
    limit: &SpendKey,
    consumed: u64,
    requested: u64,
    context: &str,
) {
    assert_eq!(error.code, SPEND_LIMIT_EXCEEDED_CODE, "{context}: {error}");
    assert_eq!(error.category, ErrorCategory::Forbidden, "{context}: {error}");
    let metadata = &error.details.metadata;
    assert_eq!(
        metadata.get("limit"),
        Some(&serde_json::to_value(limit).expect("key serializes")),
        "{context}: the error names the cap that refused it"
    );
    assert_eq!(metadata.get("consumed_micro_usd"), Some(&consumed.into()), "{context}");
    assert_eq!(metadata.get("requested_micro_usd"), Some(&requested.into()), "{context}");
}

#[track_caller]
fn assert_unknown_reservation(result: crate::Result<()>, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.code, "spend.unknown_reservation", "{context}: {error}");
    assert_eq!(error.category, ErrorCategory::NotFound, "{context}: {error}");
}

async fn spent(ledger: &dyn SpendLedger, key: &SpendKey, period: SpendPeriod) -> u64 {
    ledger.spent(key, period).await.expect("spent reads the ledger")
}

/// Holds and committed spend count against a cap, a release frees its hold, and a
/// commit records the actual amount even when it exceeds the hold or the cap.
pub async fn reserve_commit_release_accounting<H: SpendLedgerHarness + ?Sized>(harness: &H) {
    let org = unique("org");
    let clock = ManualClock::new(base_time());
    let ledger = harness.open(setup(daily_cap(&org, 100), &clock)).await;
    let cap = SpendKey::org(&org).per(SpendPeriod::Day);
    let key = SpendKey::org(&org).with_agent("agent").with_vendor("vendor-a");

    let held = ledger.reserve(&key, 60).await.expect("60 of 100 is granted");
    let error = ledger.reserve(&key, 41).await.expect_err("60 held + 41 exceeds 100");
    assert_limit_exceeded(&error, &cap, 60, 41, "a hold counts against the cap");
    assert_eq!(error.details.metadata.get("max_micro_usd"), Some(&100u64.into()));

    ledger.release(held).await.expect("release drops the hold");
    let full = ledger.reserve(&key, 100).await.expect("a released hold frees its budget");
    ledger.release(full).await.expect("release");

    let hold = ledger.reserve(&key, 80).await.expect("80 of 100 is granted");
    ledger.commit(hold, 30).await.expect("commit records the actual amount");
    for period in [SpendPeriod::Day, SpendPeriod::Month, SpendPeriod::Lifetime] {
        assert_eq!(spent(&*ledger, &SpendKey::org(&org), period).await, 30, "{period}");
    }
    assert_eq!(
        spent(&*ledger, &SpendKey::org(&org).with_agent("agent"), SpendPeriod::Day).await,
        30
    );
    assert_eq!(
        spent(&*ledger, &SpendKey::org(&org).with_vendor("vendor-b"), SpendPeriod::Day).await,
        0,
        "spend is attributed to the vendor it was reserved for"
    );

    let rest = ledger.reserve(&key, 70).await.expect("only the committed 30 counts");
    let error = ledger.reserve(&key, 1).await.expect_err("30 committed + 70 held is the cap");
    assert_limit_exceeded(&error, &cap, 100, 1, "committed spend and holds add up");
    ledger.release(rest).await.expect("release");

    let small = ledger.reserve(&key, 10).await.expect("10 of the remaining 70");
    ledger.commit(small, 90).await.expect("a commit past the hold and the cap is recorded");
    assert_eq!(spent(&*ledger, &SpendKey::org(&org), SpendPeriod::Day).await, 120);
    let error = ledger.reserve(&key, 1).await.expect_err("the cap is already exceeded");
    assert_limit_exceeded(&error, &cap, 120, 1, "an overrun commit counts");
}

/// A reservation settles exactly once: a second commit or release, or a settlement of
/// an id the ledger never issued, fails with `spend.unknown_reservation` and records
/// nothing.
pub async fn settles_each_reservation_once<H: SpendLedgerHarness + ?Sized>(harness: &H) {
    let org = unique("org");
    let clock = ManualClock::new(base_time());
    let ledger = harness.open(setup(SpendLimits::new(), &clock)).await;
    let key = SpendKey::org(&org).with_vendor("vendor");

    let committed = ledger.reserve(&key, 10).await.expect("reserve");
    ledger.commit(committed.clone(), 10).await.expect("first commit");
    assert_unknown_reservation(ledger.commit(committed.clone(), 10).await, "second commit");
    assert_unknown_reservation(ledger.release(committed).await, "release after commit");

    let released = ledger.reserve(&key, 10).await.expect("reserve");
    ledger.release(released.clone()).await.expect("first release");
    assert_unknown_reservation(ledger.release(released.clone()).await, "second release");
    assert_unknown_reservation(ledger.commit(released, 10).await, "commit after release");

    assert_unknown_reservation(ledger.commit(ReservationId::generate(), 1).await, "unknown id");
    assert_unknown_reservation(ledger.release(ReservationId::generate()).await, "unknown id");

    assert_eq!(
        spent(&*ledger, &SpendKey::org(&org), SpendPeriod::Lifetime).await,
        10,
        "only the first commit is recorded"
    );
}

/// Every cap that covers a reservation applies — organization, agent, and vendor caps
/// together, each over its own window — and `spent` sums committed spend by scope.
pub async fn limits_apply_per_key<H: SpendLedgerHarness + ?Sized>(harness: &H) {
    let org = unique("org");
    let other_org = unique("org");
    let org_cap = SpendKey::org(&org).per(SpendPeriod::Day);
    let agent_cap = SpendKey::org(&org).with_agent("a").per(SpendPeriod::Day);
    let vendor_cap = SpendKey::org(&org).with_vendor("v").per(SpendPeriod::Month);
    let limits = SpendLimits::new()
        .limit(org_cap.clone(), 1_000)
        .limit(agent_cap.clone(), 100)
        .limit(vendor_cap.clone(), 150);
    let clock = ManualClock::new(base_time());
    let ledger = harness.open(setup(limits, &clock)).await;

    let agent_a = SpendKey::org(&org).with_agent("a").with_vendor("w");
    let hold_a = ledger.reserve(&agent_a, 100).await.expect("agent a's full cap");
    let error = ledger.reserve(&agent_a, 1).await.expect_err("agent a is at its cap");
    assert_limit_exceeded(&error, &agent_cap, 100, 1, "the agent cap");

    let agent_b_vendor_v = SpendKey::org(&org).with_agent("b").with_vendor("v");
    let hold_b = ledger.reserve(&agent_b_vendor_v, 150).await.expect("vendor v's full cap");
    let error = ledger.reserve(&agent_b_vendor_v, 1).await.expect_err("vendor v is at its cap");
    assert_limit_exceeded(&error, &vendor_cap, 150, 1, "the vendor cap");

    let agent_c = SpendKey::org(&org).with_agent("c");
    let hold_c = ledger.reserve(&agent_c, 750).await.expect("the rest of the org cap");
    let error = ledger.reserve(&agent_c, 1).await.expect_err("the org is at its cap");
    assert_limit_exceeded(&error, &org_cap, 1_000, 1, "the org cap");

    let unrelated = ledger
        .reserve(&SpendKey::org(&other_org).with_agent("a").with_vendor("v"), 5_000)
        .await
        .expect("no cap covers another organization");
    ledger.release(unrelated).await.expect("release");

    ledger.commit(hold_a, 40).await.expect("commit a");
    ledger.commit(hold_b, 100).await.expect("commit b");
    ledger.commit(hold_c, 700).await.expect("commit c");
    let scoped = [
        (SpendKey::org(&org), SpendPeriod::Day, 840),
        (SpendKey::org(&org).with_agent("a"), SpendPeriod::Day, 40),
        (SpendKey::org(&org).with_agent("c"), SpendPeriod::Day, 700),
        (SpendKey::org(&org).with_vendor("v"), SpendPeriod::Month, 100),
        (SpendKey::org(&org).with_agent("b").with_vendor("v"), SpendPeriod::Lifetime, 100),
        (SpendKey::org(&org).with_vendor("w"), SpendPeriod::Day, 40),
        (SpendKey::org(&org).with_agent("b").with_vendor("w"), SpendPeriod::Day, 0),
        (SpendKey::org(&other_org), SpendPeriod::Lifetime, 0),
    ];
    for (scope, period, expected) in scoped {
        assert_eq!(
            spent(&*ledger, &scope, period).await,
            expected,
            "spent for {scope} per {period}"
        );
    }
}

/// Day windows restart at UTC midnight and month windows on the first of the month;
/// lifetime spend never resets.
pub async fn periods_roll_over_at_utc_boundaries<H: SpendLedgerHarness + ?Sized>(harness: &H) {
    let org = unique("org");
    let day_cap = SpendKey::org(&org).per(SpendPeriod::Day);
    let month_cap = SpendKey::org(&org).per(SpendPeriod::Month);
    let limits = SpendLimits::new().limit(day_cap.clone(), 100).limit(month_cap.clone(), 250);
    let at = |month, day, hour, minute| {
        Utc.with_ymd_and_hms(2026, month, day, hour, minute, 0).single().expect("valid instant")
    };
    let clock = ManualClock::new(at(10, 31, 23, 50));
    let ledger = harness.open(setup(limits, &clock)).await;
    let key = SpendKey::org(&org).with_agent("agent");
    let totals = |day, month, lifetime| {
        [(SpendPeriod::Day, day), (SpendPeriod::Month, month), (SpendPeriod::Lifetime, lifetime)]
    };

    let hold = ledger.reserve(&key, 100).await.expect("first day's cap");
    ledger.commit(hold, 100).await.expect("commit");
    let error = ledger.reserve(&key, 1).await.expect_err("the day is spent");
    assert_limit_exceeded(&error, &day_cap, 100, 1, "before midnight");

    clock.set(at(11, 1, 0, 10));
    for (period, expected) in totals(0, 0, 100) {
        assert_eq!(
            spent(&*ledger, &SpendKey::org(&org), period).await,
            expected,
            "1 Nov, {period}"
        );
    }
    let hold = ledger.reserve(&key, 100).await.expect("a new day and a new month");
    ledger.commit(hold, 100).await.expect("commit");

    clock.set(at(11, 2, 0, 10));
    for (period, expected) in totals(0, 100, 200) {
        assert_eq!(
            spent(&*ledger, &SpendKey::org(&org), period).await,
            expected,
            "2 Nov, {period}"
        );
    }
    let hold = ledger.reserve(&key, 100).await.expect("a new day in the same month");
    ledger.commit(hold, 100).await.expect("commit");

    clock.set(at(11, 3, 0, 10));
    let error = ledger.reserve(&key, 100).await.expect_err("the month cap still counts");
    assert_limit_exceeded(&error, &month_cap, 200, 100, "the month window spans days");
    let hold = ledger.reserve(&key, 50).await.expect("what the month has left");
    ledger.commit(hold, 50).await.expect("commit");
    for (period, expected) in totals(50, 250, 350) {
        assert_eq!(
            spent(&*ledger, &SpendKey::org(&org), period).await,
            expected,
            "3 Nov, {period}"
        );
    }
}

/// A hold that is neither committed nor released stops counting once its time to live
/// passes, and committing it late still records the spend.
pub async fn expired_reservations_stop_holding_budget<H: SpendLedgerHarness + ?Sized>(harness: &H) {
    let org = unique("org");
    let clock = ManualClock::new(base_time());
    let ledger = harness
        .open(SpendLedgerSetup {
            reservation_ttl: Duration::from_secs(60),
            ..setup(daily_cap(&org, 100), &clock)
        })
        .await;
    let key = SpendKey::org(&org);

    let abandoned = ledger.reserve(&key, 100).await.expect("the full cap");
    ledger.reserve(&key, 1).await.expect_err("the live hold counts");

    clock.advance(Duration::from_secs(61));
    let next = ledger.reserve(&key, 100).await.expect("the expired hold no longer counts");
    ledger.release(next).await.expect("release");

    ledger.commit(abandoned, 40).await.expect("a late commit records spend that happened");
    assert_eq!(spent(&*ledger, &key, SpendPeriod::Day).await, 40);
}

/// Concurrent reservations, split across two handles when the backend is durable,
/// never grant more than the cap allows.
pub async fn concurrent_reservations_never_exceed_a_cap<H: SpendLedgerHarness + ?Sized>(
    harness: &H,
) {
    let org = unique("org");
    let clock = ManualClock::new(base_time());
    let first = harness.open(setup(daily_cap(&org, 1_000), &clock)).await;
    let second = if harness.is_durable() {
        harness.open(setup(daily_cap(&org, 1_000), &clock)).await
    } else {
        first.clone()
    };

    let tasks: Vec<_> = (0..40)
        .map(|i| {
            let ledger = if i % 2 == 0 { first.clone() } else { second.clone() };
            let key = SpendKey::org(&org).with_agent(format!("agent-{}", i % 4));
            tokio::spawn(async move { ledger.reserve(&key, 100).await })
        })
        .collect();
    let mut granted = 0;
    for task in tasks {
        match task.await.expect("reservation task completes") {
            Ok(_) => granted += 1,
            Err(error) => assert_eq!(error.code, SPEND_LIMIT_EXCEEDED_CODE, "{error}"),
        }
    }
    assert_eq!(granted, 10, "exactly the cap's worth of 100 micro-USD reservations is granted");
}

/// Holds taken before a restart still count after it, and a reservation made before
/// the restart can be committed or released after it.
pub async fn reservations_survive_a_restart<H: SpendLedgerHarness + ?Sized>(harness: &H) {
    if !harness.is_durable() {
        return;
    }
    let org = unique("org");
    let clock = ManualClock::new(base_time());
    let key = SpendKey::org(&org).with_agent("agent").with_vendor("vendor");
    let (to_release, to_commit) = {
        let before = harness.open(setup(daily_cap(&org, 100), &clock)).await;
        let to_release = before.reserve(&key, 60).await.expect("reserve");
        let to_commit = before.reserve(&key, 30).await.expect("reserve");
        (to_release, to_commit)
    };

    let after = harness.open(setup(daily_cap(&org, 100), &clock)).await;
    let error = after.reserve(&key, 11).await.expect_err("holds from before the restart count");
    assert_limit_exceeded(
        &error,
        &SpendKey::org(&org).per(SpendPeriod::Day),
        90,
        11,
        "after restart",
    );
    after.commit(to_commit, 30).await.expect("commit a reservation made before the restart");
    after.release(to_release).await.expect("release a reservation made before the restart");
    assert_eq!(spent(&*after, &SpendKey::org(&org), SpendPeriod::Day).await, 30);
    after.reserve(&key, 70).await.expect("the released hold no longer counts");
}
