//! S7 with adk-core's per-unit run caps: a run capped at 20 images makes 30 `sam-3.1` calls.

mod support;

use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget, SpendKey, SpendLedger,
    SpendPeriod, UsageUnit,
};
use adk_vision::{
    BBox, DetectQuery, EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, VisionCx,
    VisionRuntime,
};
use support::{ColourDetector, boxes_image};

#[tokio::test]
async fn a_run_capped_at_twenty_images_refuses_the_twenty_first_before_its_request() {
    let sam = Arc::new(ColourDetector::remote("sam-3.1"));
    let local = Arc::new(ColourDetector::local("rf-detr-n"));
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime = VisionRuntime::builder()
        .detector(local.clone())
        .remote_detector(sam.clone())
        .egress_gate(gate)
        .build()
        .unwrap();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new().max_units(UsageUnit::Images, 20)));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "acme"),
    );
    let page = boxes_image(640, 480, &[("red", BBox::new(100.0, 80.0, 260.0, 200.0))]);
    let query = DetectQuery::Concepts(vec!["red".into()]);
    let to_sam = adk_vision::CallOptions::new().with_route(adk_vision::Route::to("sam-3.1"));

    let mut outcomes = Vec::new();
    for _ in 0..30 {
        outcomes.push(runtime.detect_with(&cx, &page, &query, &to_sam).await);
    }
    let ok = outcomes.iter().filter(|o| o.is_ok()).count();
    let first_refusal = outcomes.into_iter().find_map(Result::err).unwrap();
    let adk: AdkError = first_refusal.into();
    println!("refusal: {} {} {}", adk.code, adk.category, adk.message);
    assert_eq!(ok, 20);
    assert_eq!(sam.calls(), 20, "the 21st call was refused before its request");
    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 50_000);
    assert_eq!(budget.units().get(UsageUnit::Images), 20);
    println!(
        "sam calls {}, ledger {} µUSD, images {}",
        sam.calls(),
        50_000,
        budget.units().get(UsageUnit::Images)
    );
}
