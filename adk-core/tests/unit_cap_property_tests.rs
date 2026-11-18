//! Per-unit run caps (`RunBudget::max_units`) on calls billed in units.
//!
//! The scenario is spec 3.7 S7: a run capped at 20 images makes 30 `sam-3.1` segmentation
//! calls; 20 succeed, the 21st is refused before any request, and the ledger shows 20
//! commits of 2,500 µUSD. The properties check that no sequence of holds, commits and
//! failures takes a unit past its cap.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, BudgetLimit, BudgetTracker, ErrorCategory, InMemorySpendLedger, MeteredUsage,
    ModelCallScope, ReservationId, Result, RunBudget, SpendKey, SpendLedger, SpendPeriod, UnitRate,
    UsageCall, UsageUnit, UsageVector,
};
use async_trait::async_trait;
use proptest::prelude::*;

/// An in-memory ledger that records every commit.
#[derive(Debug, Default)]
struct RecordingLedger {
    inner: InMemorySpendLedger,
    commits: Mutex<Vec<u64>>,
}

impl RecordingLedger {
    fn commits(&self) -> Vec<u64> {
        self.commits.lock().unwrap().clone()
    }
}

#[async_trait]
impl SpendLedger for RecordingLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        self.inner.reserve(key, amount_micro_usd).await
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        self.commits.lock().unwrap().push(actual_micro_usd);
        self.inner.commit(id, actual_micro_usd).await
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        self.inner.release(id).await
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        self.inner.spent(key, period).await
    }
}

fn images(count: u64) -> UsageVector {
    UsageVector::new().with(UsageUnit::Images, count)
}

#[tokio::test]
async fn a_run_capped_at_twenty_images_refuses_the_twenty_first_segmentation_before_sending() {
    let rate = UnitRate::new(UsageUnit::Images, 2_500_000, 1_000);
    let ledger = Arc::new(RecordingLedger::default());
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_units(UsageUnit::Images, 20)));
    let scope = ModelCallScope::new()
        .with_spend_ledger(ledger.clone(), "inspector")
        .with_budget(Arc::clone(&tracker));
    let sent = AtomicUsize::new(0);

    let mut outcomes = Vec::new();
    for _ in 0..30 {
        let call = UsageCall::new("meta", "sam-3.1")
            .with_max_cost_micro_usd(rate.cost_micro_usd(1))
            .with_units(images(1));
        let outcome = scope
            .meter(call, async {
                sent.fetch_add(1, Ordering::SeqCst);
                Ok(((), MeteredUsage::new(images(1)).with_cost_micro_usd(rate.cost_micro_usd(1))))
            })
            .await;
        outcomes.push(outcome);
    }

    assert_eq!(sent.load(Ordering::SeqCst), 20, "no request after the cap");
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 20);
    let refused: &AdkError = outcomes[20].as_ref().unwrap_err();
    assert_eq!(refused.category, ErrorCategory::ResourceExhausted);
    assert_eq!(refused.code, "budget.units");
    assert_eq!(refused.details.metadata["unit"], "images");
    assert_eq!(refused.details.metadata["max"], 20);
    assert!(refused.message.contains("cap of 20 images"), "{}", refused.message);
    assert_eq!(ledger.commits(), vec![2_500; 20]);
    assert_eq!(ledger.spent(&SpendKey::org("inspector"), SpendPeriod::Day).await.unwrap(), 50_000);
    assert_eq!(tracker.units(), images(20));
    assert_eq!(tracker.usage().cost_micro_usd, 50_000);
}

#[tokio::test]
async fn local_calls_record_their_units_at_zero_cost() {
    let ledger = Arc::new(RecordingLedger::default());
    let tracker = Arc::new(BudgetTracker::new(
        RunBudget::new().max_units(UsageUnit::Images, 3).max_cost_micro_usd(1_000),
    ));
    let scope = ModelCallScope::new()
        .with_spend_ledger(ledger.clone(), "desk")
        .with_budget(Arc::clone(&tracker));
    for _ in 0..3 {
        let call = UsageCall::new("local", "rf-detr-nano")
            .with_max_cost_micro_usd(0)
            .with_units(images(1));
        scope
            .meter(call, async { Ok(((), MeteredUsage::new(images(1)).with_cost_micro_usd(0))) })
            .await
            .unwrap();
    }
    let call =
        UsageCall::new("local", "rf-detr-nano").with_max_cost_micro_usd(0).with_units(images(1));
    let refused = scope.begin_usage(&call).await.unwrap_err();
    assert_eq!(refused.code, "budget.units");
    assert_eq!(ledger.commits(), vec![0, 0, 0]);
    assert_eq!((tracker.units(), tracker.usage().cost_micro_usd), (images(3), 0));
}

#[tokio::test]
async fn a_failed_or_abandoned_call_returns_the_units_it_held() {
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_units(UsageUnit::Frames, 50)));
    let scope = ModelCallScope::new().with_budget(Arc::clone(&tracker));
    let frames = UsageVector::new().with(UsageUnit::Frames, 50);
    let call = UsageCall::new("meta", "sam-3.1").with_max_cost_micro_usd(10_000).with_units(frames);

    let failed: Result<()> =
        scope.meter(call.clone(), async { Err(AdkError::model("upstream timeout")) }).await;
    assert!(failed.is_err());
    // Dropped while the call is in flight: the hold goes with it.
    let abandoned = scope.begin_usage(&call).await.unwrap();
    assert_eq!(tracker.remaining().max_units[&UsageUnit::Frames], 0);
    drop(abandoned);
    assert_eq!(tracker.remaining().max_units[&UsageUnit::Frames], 50);
    assert!(scope.begin_usage(&call).await.is_ok());
    assert_eq!(tracker.units(), UsageVector::new());
}

#[derive(Debug, Clone)]
enum Step {
    /// Declare `held` images, then commit `used` of them, or fail.
    Call { held: u64, used: u64, fails: bool },
    /// Hold `held` images and leave the hold open.
    Open { held: u64 },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (1u64..6, 0u64..6, any::<bool>()).prop_map(|(held, used, fails)| Step::Call {
            held,
            used: used.min(held),
            fails
        }),
        (1u64..4).prop_map(|held| Step::Open { held }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever calls declare, commit, fail or leave open, used plus held units never pass
    /// the cap of any tracker in the chain, and a refused hold changes nothing.
    #[test]
    fn units_never_pass_their_cap(
        run_cap in 0u64..30,
        team_cap in 0u64..30,
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        let run = Arc::new(BudgetTracker::new(RunBudget::new().max_units(UsageUnit::Images, run_cap)));
        let team = Arc::new(BudgetTracker::with_parent(
            RunBudget::new().max_units(UsageUnit::Images, team_cap),
            Arc::clone(&run),
        ));
        let scope = ModelCallScope::new().with_budget(Arc::clone(&team));
        let cap = run_cap.min(team_cap);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let mut open = Vec::new();
        let mut used = 0u64;
        let mut held_open = 0u64;
        for step in steps {
            let held = match step {
                Step::Call { held, .. } | Step::Open { held } => held,
            };
            let call = UsageCall::new("meta", "sam-3.1").with_max_cost_micro_usd(0).with_units(images(held));
            let before = team.remaining().max_units[&UsageUnit::Images];
            match runtime.block_on(scope.begin_usage(&call)) {
                Ok(hold) => {
                    prop_assert!(used + held_open + held <= cap);
                    match step {
                        Step::Call { used: spent, fails: false, .. } => {
                            runtime.block_on(hold.commit(MeteredUsage::new(images(spent)).with_cost_micro_usd(0)));
                            used += spent;
                        }
                        Step::Call { fails: true, .. } => runtime.block_on(hold.release()),
                        Step::Open { .. } => {
                            held_open += held;
                            open.push(hold);
                        }
                    }
                }
                Err(refused) => {
                    prop_assert_eq!(refused.code, "budget.units");
                    prop_assert!(used + held_open + held > cap);
                    prop_assert_eq!(team.remaining().max_units[&UsageUnit::Images], before);
                }
            }
            // A tracker's remaining room is its own cap narrowed by its ancestors'.
            for (tracker, chain_cap) in [(&run, run_cap), (&team, cap)] {
                let used_here = tracker.units().get(UsageUnit::Images);
                let left = tracker.remaining().max_units[&UsageUnit::Images];
                prop_assert_eq!(used_here, used);
                prop_assert!(used_here + held_open <= chain_cap);
                prop_assert_eq!(left, chain_cap - used - held_open);
            }
        }
        drop(open);
        prop_assert_eq!(team.remaining().max_units[&UsageUnit::Images], cap - used);
        prop_assert_eq!(run.remaining().max_units[&UsageUnit::Images], run_cap - used);
    }

    /// A per-request minimum bills every request with usage at least the minimum, and never
    /// less than the rate without it.
    #[test]
    fn a_minimum_is_a_floor_per_request(amount in 0u64..10_000, minimum in 0u64..200) {
        let plain = UnitRate::new(UsageUnit::Frames, 200_000, 1_000);
        let floored = plain.with_minimum_per_request(minimum);
        let cost = floored.cost_micro_usd(amount);
        if amount == 0 {
            prop_assert_eq!(cost, 0);
        } else {
            prop_assert!(cost >= plain.cost_micro_usd(amount));
            prop_assert!(cost >= plain.cost_micro_usd(minimum));
            prop_assert_eq!(cost, plain.cost_micro_usd(amount.max(minimum)));
        }
    }
}

#[test]
fn a_budget_limit_names_each_unit() {
    let ids: Vec<&str> =
        UsageUnit::ALL.into_iter().map(|unit| BudgetLimit::Units(unit).as_str()).collect();
    assert_eq!(
        ids,
        [
            "units.inputTokens",
            "units.outputTokens",
            "units.images",
            "units.audioMillis",
            "units.videoMillis",
            "units.characters",
            "units.pages",
            "units.requests",
            "units.frames",
        ]
    );
    for unit in UsageUnit::ALL {
        assert_eq!(BudgetLimit::Units(unit).as_str(), format!("units.{unit}"));
    }
}
