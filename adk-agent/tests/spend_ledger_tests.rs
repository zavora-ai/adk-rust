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
    fail: bool,
    calls: AtomicUsize,
}

impl PricedModel {
    fn new(name: &'static str, cost: Option<f64>) -> Arc<Self> {
        Arc::new(Self { name, cost, fail: false, calls: AtomicUsize::new(0) })
    }

    fn failing(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name, cost: None, fail: true, calls: AtomicUsize::new(0) })
    }
}

#[async_trait]
impl Llm for PricedModel {
    fn name(&self) -> &str {
        self.name
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
    assert_eq!(spent(&ledger, SpendKey::org("acme").with_agent("writer")).await, 20_000);
    assert_eq!(spent(&ledger, SpendKey::org("acme")).await, 32_500);
}

#[tokio::test]
async fn a_daily_cap_refuses_the_call_that_would_exceed_it() {
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000),
    ));

    let first = PricedModel::new("gemini-3.7-flash", Some(0.03));
    let events = run_once("researcher", first.clone(), ledger.clone()).await;
    assert!(events.iter().all(Result::is_ok));

    // 30_000 committed plus a 30_000 hold exceeds 50_000, so the model is never called.
    let second = PricedModel::new("gemini-3.7-flash", Some(0.03));
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
    let next = PricedModel::new("gemini-3.7-flash", Some(0.01));
    run_once("researcher", next.clone(), ledger.clone()).await;
    assert_eq!(next.calls.load(Ordering::SeqCst), 1);
}
