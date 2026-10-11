//! `ModelCallScope` applies the run budget and the spend ledger to model calls made
//! outside an agent's own model loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{
    AdkError, BudgetTracker, Content, ErrorCategory, InMemorySpendLedger, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, MeteredLlm, ModelCallScope, Result, RunBudget, SpendKey,
    SpendLedger, SpendLimits, SpendPeriod, UsageMetadata, generate_metered,
};
use async_trait::async_trait;
use futures::StreamExt;

/// Answers with fixed usage, or fails, counting how often it is called.
struct FakeModel {
    calls: Arc<AtomicUsize>,
    cost: Option<f64>,
    fail: bool,
    bound: Option<u64>,
}

impl FakeModel {
    fn priced(cost: f64) -> Self {
        Self { calls: Arc::default(), cost: Some(cost), fail: false, bound: Some(20_000) }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Llm for FakeModel {
    fn name(&self) -> &str {
        "grok-4.6"
    }

    fn max_cost_micro_usd(&self, _request: &LlmRequest) -> Option<u64> {
        self.bound
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(AdkError::model("provider unavailable"));
        }
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("ok")),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 70,
                candidates_token_count: 30,
                total_token_count: 100,
                cost: self.cost,
                ..Default::default()
            }),
            provider: Some("xai".to_string()),
            model: Some("grok-4.6".to_string()),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

fn request() -> LlmRequest {
    LlmRequest::new("grok-4.6", vec![Content::new("user").with_text("hi")])
}

async fn drain(stream: LlmResponseStream) -> Vec<LlmResponse> {
    stream.map(|item| item.unwrap()).collect().await
}

#[tokio::test]
async fn a_call_counts_its_usage_and_commits_its_cost() {
    let model = FakeModel::priced(0.0042);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0)));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "acme")
        .with_agent("summarizer");

    drain(scope.generate(&model, request(), false).await.unwrap()).await;

    let usage = tracker.usage();
    assert_eq!((usage.model_calls, usage.total_tokens, usage.cost_micro_usd), (1, 100, 4_200));
    let key = SpendKey::org("acme").with_agent("summarizer").with_vendor("xai");
    assert_eq!(ledger.spent(&key, SpendPeriod::Lifetime).await.unwrap(), 4_200);
}

#[tokio::test]
async fn a_call_over_the_budget_is_refused_before_it_is_sent_and_its_hold_released() {
    let model = FakeModel::priced(0.01);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(0)));
    let ledger =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(SpendKey::org("acme"), 50_000)));
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "acme");

    let error = scope.generate(&model, request(), false).await.err().expect("refused");

    assert_eq!(error.category, ErrorCategory::ResourceExhausted);
    assert_eq!(error.code, "budget.model_calls");
    assert_eq!(model.calls(), 0);
    // The released hold no longer counts, so a full-cap reservation still fits.
    let hold = ledger.reserve(&SpendKey::org("acme"), 50_000).await;
    assert!(hold.is_ok(), "the refused call's hold was released: {hold:?}");
}

#[tokio::test]
async fn a_call_over_the_spend_cap_is_refused_before_it_is_sent() {
    let model = FakeModel::priced(0.01);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(SpendKey::org("acme"), 1)));
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "acme");

    let error = scope.generate(&model, request(), false).await.err().expect("refused");

    assert_eq!(error.code, adk_core::SPEND_LIMIT_EXCEEDED_CODE);
    assert_eq!(model.calls(), 0);
    assert_eq!(tracker.usage().model_calls, 0, "the budget is not charged for a refused call");
}

#[tokio::test]
async fn a_failed_call_releases_its_hold() {
    let model = FakeModel { calls: Arc::default(), cost: None, fail: true, bound: Some(20_000) };
    let ledger =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(SpendKey::org("acme"), 50_000)));
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "acme");

    assert!(scope.generate(&model, request(), false).await.is_err());

    assert!(ledger.reserve(&SpendKey::org("acme"), 50_000).await.is_ok());
    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap(), 0);
}

#[tokio::test]
async fn an_unpriced_response_commits_the_estimate_and_stops_a_cost_capped_budget() {
    let model = FakeModel { calls: Arc::default(), cost: None, fail: false, bound: None };
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0)));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "acme")
        .with_spend_estimate(adk_core::LlmSpendEstimate::per_call(7_000));

    drain(scope.generate(&model, request(), false).await.unwrap()).await;

    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap(), 7_000);
    assert_eq!(tracker.check().unwrap_err().code(), "budget.cost_unknown");
}

#[tokio::test]
async fn generate_metered_uses_the_current_scope_and_is_unmetered_outside_one() {
    let model = FakeModel::priced(0.001);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));

    drain(generate_metered(&model, request(), false).await.unwrap()).await;
    assert_eq!(tracker.usage().model_calls, 0);

    let scope = ModelCallScope::new().with_budget(tracker.clone());
    scope
        .within(async { drain(generate_metered(&model, request(), false).await.unwrap()).await })
        .await;
    assert_eq!(tracker.usage().model_calls, 1);
    assert_eq!(model.calls(), 2);
}

#[tokio::test]
async fn metered_llm_prefers_its_own_scope() {
    let own = Arc::new(BudgetTracker::new(RunBudget::new()));
    let current = Arc::new(BudgetTracker::new(RunBudget::new()));
    let model = MeteredLlm::new(Arc::new(FakeModel::priced(0.001)))
        .with_scope(ModelCallScope::new().with_budget(own.clone()));

    ModelCallScope::new()
        .with_budget(current.clone())
        .within(async { drain(model.generate_content(request(), false).await.unwrap()).await })
        .await;

    assert_eq!(own.usage().model_calls, 1);
    assert_eq!(current.usage().model_calls, 0);
    assert_eq!(model.name(), "grok-4.6");
}

#[tokio::test]
async fn the_hold_is_the_call_bound_so_a_cap_is_never_overshot() {
    // Each call costs 0.012 USD and is bounded at 0.015 USD. Reserving a 0.01 USD estimate
    // would admit the second call (12_000 + 10_000 ≤ 25_000) and commit 24_000; reserving
    // the bound refuses it (12_000 + 15_000 > 25_000).
    let model =
        FakeModel { calls: Arc::default(), cost: Some(0.012), fail: false, bound: Some(15_000) };
    let cap = 25_000;
    let ledger =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(SpendKey::org("acme"), cap)));
    let scope = ModelCallScope::new()
        .with_spend_ledger(ledger.clone(), "acme")
        .with_spend_estimate(adk_core::LlmSpendEstimate::per_call(10_000));

    drain(scope.generate(&model, request(), false).await.unwrap()).await;
    let second = scope.generate(&model, request(), false).await;

    // 12_000 committed + a 15_000 hold exceeds the cap, so the second call is refused.
    assert_eq!(second.err().map(|e| e.code), Some(adk_core::SPEND_LIMIT_EXCEEDED_CODE));
    assert_eq!(model.calls(), 1);
    let spent = ledger.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap();
    assert_eq!(spent, 12_000);
    assert!(spent <= cap);
}

#[tokio::test]
async fn an_unbounded_call_runs_only_where_no_cap_applies() {
    let unpriced = || FakeModel { calls: Arc::default(), cost: None, fail: false, bound: None };
    let capped = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("acme"), 1_000_000_000),
    ));
    let model = unpriced();
    let refused = ModelCallScope::new()
        .with_spend_ledger(capped, "acme")
        .generate(&model, request(), false)
        .await;
    assert_eq!(refused.err().map(|e| e.code), Some(adk_core::SPEND_LIMIT_EXCEEDED_CODE));
    assert_eq!(model.calls(), 0);

    let uncapped =
        Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(SpendKey::org("other-org"), 1)));
    let model = unpriced();
    let scope = ModelCallScope::new()
        .with_spend_ledger(uncapped.clone(), "acme")
        .with_spend_estimate(adk_core::LlmSpendEstimate::per_call(3_000));
    drain(scope.generate(&model, request(), false).await.unwrap()).await;
    assert_eq!(uncapped.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap(), 3_000);
}
