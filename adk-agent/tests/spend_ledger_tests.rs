//! A run whose config carries a spend ledger reserves before each model call and commits
//! the call's reported cost, keyed by app, agent, and vendor.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Content, InMemorySpendLedger, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    Result, RunConfig, SPEND_LIMIT_EXCEEDED_CODE, SessionId, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod, UsageMetadata, UserId,
};
use adk_runner::{LlmSpendEstimate, Runner};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Answers every call with one text chunk reporting `cost`, or fails before streaming.
struct PricedModel {
    name: &'static str,
    cost: Option<f64>,
    provider: Option<&'static str>,
    fail: bool,
    calls: AtomicUsize,
    /// Upper-bound cost the model reports for every request; `None` is unpriced.
    bound: Option<u64>,
}

impl PricedModel {
    fn new(name: &'static str, cost: Option<f64>) -> Arc<Self> {
        Arc::new(Self {
            name,
            cost,
            provider: None,
            fail: false,
            calls: AtomicUsize::new(0),
            bound: None,
        })
    }

    fn bounded(name: &'static str, cost: Option<f64>, bound: u64) -> Arc<Self> {
        Arc::new(Self {
            name,
            cost,
            provider: None,
            fail: false,
            calls: AtomicUsize::new(0),
            bound: Some(bound),
        })
    }

    fn served_by(name: &'static str, cost: Option<f64>, provider: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            cost,
            provider: Some(provider),
            fail: false,
            calls: AtomicUsize::new(0),
            bound: None,
        })
    }

    fn failing(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            cost: None,
            provider: None,
            fail: true,
            calls: AtomicUsize::new(0),
            bound: Some(30_000),
        })
    }
}

#[async_trait]
impl Llm for PricedModel {
    fn name(&self) -> &str {
        self.name
    }

    fn max_cost_micro_usd(&self, _request: &LlmRequest) -> Option<u64> {
        self.bound
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(AdkError::model("provider unavailable"));
        }
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
                cost: self.cost,
                ..Default::default()
            }),
            turn_complete: true,
            provider: self.provider.map(str::to_string),
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Runs `agent_name` on `model` once with `ledger` on the run config.
async fn run_once(
    agent_name: &str,
    model: Arc<PricedModel>,
    ledger: Arc<dyn SpendLedger>,
) -> Vec<Result<adk_core::Event>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "acme".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let agent = LlmAgentBuilder::new(agent_name).model(model as Arc<dyn Llm>).build().unwrap();
    let runner = Runner::builder()
        .app_name("acme")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().spend_ledger(ledger).build())
        .build()
        .unwrap()
        .with_llm_spend_estimate(LlmSpendEstimate::per_call(30_000));
    runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap()
        .collect()
        .await
}

async fn spent(ledger: &InMemorySpendLedger, key: SpendKey) -> u64 {
    ledger.spent(&key, SpendPeriod::Day).await.unwrap()
}

#[tokio::test]
async fn model_spend_is_recorded_per_vendor_and_agent() {
    let ledger = Arc::new(InMemorySpendLedger::default());

    run_once("researcher", PricedModel::new("gemini-3.7-flash", Some(0.0125)), ledger.clone())
        .await;
    run_once("writer", PricedModel::new("claude-sonnet-4-6", Some(0.02)), ledger.clone()).await;

    assert_eq!(spent(&ledger, SpendKey::org("acme").with_vendor("gemini")).await, 12_500);
    assert_eq!(spent(&ledger, SpendKey::org("acme").with_vendor("anthropic")).await, 20_000);
    assert_eq!(
        spent(&ledger, SpendKey::org("acme").with_agent("urn:adk:agent:acme/writer")).await,
        20_000
    );
    assert_eq!(spent(&ledger, SpendKey::org("acme")).await, 32_500);
}

#[tokio::test]
async fn spend_is_committed_under_the_provider_the_response_names() {
    let ledger = Arc::new(InMemorySpendLedger::default());

    // The model id reads as Anthropic; the response reports the provider that billed it.
    run_once(
        "writer",
        PricedModel::served_by("claude-sonnet-4-6", Some(0.02), "bedrock"),
        ledger.clone(),
    )
    .await;

    assert_eq!(spent(&ledger, SpendKey::org("acme").with_vendor("bedrock")).await, 20_000);
    assert_eq!(spent(&ledger, SpendKey::org("acme").with_vendor("anthropic")).await, 0);
    assert_eq!(
        spent(&ledger, SpendKey::org("acme").with_agent("urn:adk:agent:acme/writer")).await,
        20_000
    );
    assert_eq!(spent(&ledger, SpendKey::org("acme")).await, 20_000);
}

#[tokio::test]
async fn a_daily_cap_refuses_the_call_that_would_exceed_it() {
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000),
    ));

    let first = PricedModel::bounded("gemini-3.7-flash", Some(0.03), 30_000);
    let events = run_once("researcher", first.clone(), ledger.clone()).await;
    assert!(events.iter().all(Result::is_ok));

    // 30_000 committed plus a 30_000 hold exceeds 50_000, so the model is never called.
    let second = PricedModel::bounded("gemini-3.7-flash", Some(0.03), 30_000);
    let events = run_once("researcher", second.clone(), ledger.clone()).await;
    let refused = events.into_iter().find_map(Result::err).expect("the run fails");
    assert_eq!(refused.code, SPEND_LIMIT_EXCEEDED_CODE);
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    assert_eq!(spent(&ledger, SpendKey::org("acme")).await, 30_000);
}

#[tokio::test]
async fn a_response_without_cost_commits_the_estimate() {
    let ledger = Arc::new(InMemorySpendLedger::default());
    run_once("researcher", PricedModel::new("gpt-5.2", None), ledger.clone()).await;
    assert_eq!(spent(&ledger, SpendKey::org("acme").with_vendor("openai")).await, 30_000);
}

#[tokio::test]
async fn a_call_that_fails_before_streaming_releases_its_hold() {
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 30_000),
    ));
    let events =
        run_once("researcher", PricedModel::failing("gemini-3.7-flash"), ledger.clone()).await;
    assert!(events.iter().any(Result::is_err));
    assert_eq!(spent(&ledger, SpendKey::org("acme")).await, 0);

    // The released hold leaves the whole cap for the next call.
    let next = PricedModel::bounded("gemini-3.7-flash", Some(0.01), 30_000);
    run_once("researcher", next.clone(), ledger.clone()).await;
    assert_eq!(next.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn llm_routing_spend_is_committed_under_the_router() {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "acme".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let ledger = Arc::new(InMemorySpendLedger::default());
    let worker = LlmAgentBuilder::new("worker")
        .model(PricedModel::new("claude-sonnet-4-6", Some(0.002)) as Arc<dyn Llm>)
        .build()
        .unwrap();
    let router = adk_agent::LlmConditionalAgent::builder(
        "router",
        PricedModel::new("gemini-3.7-flash", Some(0.004)) as Arc<dyn Llm>,
    )
    .instruction("Answer with one word.")
    .route("done", Arc::new(worker))
    .build()
    .unwrap();
    let runner = Runner::builder()
        .app_name("acme")
        .agent(Arc::new(router))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().spend_ledger(ledger.clone()).build())
        .build()
        .unwrap();

    let events: Vec<_> = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap()
        .collect()
        .await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let router_gemini =
        SpendKey::org("acme").with_agent("urn:adk:agent:acme/router").with_vendor("gemini");
    assert_eq!(spent(&ledger, router_gemini).await, 4_000);
    assert_eq!(spent(&ledger, SpendKey::org("acme").with_vendor("anthropic")).await, 2_000);
}

#[tokio::test]
async fn the_built_in_summarizer_commits_spend_under_the_current_scope() {
    use adk_core::{BaseEventsSummarizer, BudgetTracker, Event, ModelCallScope, RunBudget};

    let ledger = Arc::new(InMemorySpendLedger::default());
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let model = PricedModel::new("gemini-3.7-flash", Some(0.003));
    let summarizer = adk_agent::LlmEventSummarizer::new(model.clone() as Arc<dyn Llm>);
    let mut event = Event::new("inv");
    event.author = "user".to_string();
    event.set_content(Content::new("user").with_text("a long conversation"));
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "acme")
        .with_agent("assistant");

    let summary = scope.within(summarizer.summarize_events(&[event])).await.unwrap();

    assert!(summary.is_some());
    assert_eq!(tracker.usage().model_calls, 1);
    assert_eq!(tracker.usage().total_tokens, 15);
    assert_eq!(spent(&ledger, SpendKey::org("acme").with_agent("assistant")).await, 3_000);
}

#[tokio::test]
async fn a_call_costing_more_than_the_estimate_never_pushes_spend_past_the_cap() {
    // Each call costs 0.04 USD, more than the runner's 0.03 USD estimate, and is bounded at
    // 0.045 USD. Reserving the estimate would admit the second call (40_000 + 30_000 ≤
    // 70_000) and commit 80_000; reserving the bound refuses it.
    let cap = 70_000;
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), cap),
    ));
    let first = PricedModel::bounded("gpt-5.4-mini", Some(0.04), 45_000);
    assert!(run_once("researcher", first, ledger.clone()).await.iter().all(Result::is_ok));

    let second = PricedModel::bounded("gpt-5.4-mini", Some(0.04), 45_000);
    let events = run_once("researcher", second.clone(), ledger.clone()).await;

    let refused = events.into_iter().find_map(Result::err).expect("the second run is refused");
    assert_eq!(refused.code, SPEND_LIMIT_EXCEEDED_CODE);
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    let total = spent(&ledger, SpendKey::org("acme")).await;
    assert_eq!(total, 40_000);
    assert!(total <= cap);
}

#[tokio::test]
async fn an_unpriced_model_is_refused_under_a_cap() {
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 1_000_000_000),
    ));
    let model = PricedModel::new("my-fine-tune", None);
    let events = run_once("researcher", model.clone(), ledger.clone()).await;

    let refused = events.into_iter().find_map(Result::err).expect("the run is refused");
    assert_eq!(refused.code, SPEND_LIMIT_EXCEEDED_CODE);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}
