//! Compaction summaries are model calls made on behalf of the run, so they count
//! against the run's budget and spend ledger like the agent's own calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{
    Agent, BaseEventsSummarizer, BudgetTracker, Content, Event, EventActions, EventCompaction,
    EventStream, EventsCompactionConfig, InMemorySpendLedger, IntraCompactionConfig,
    InvocationContext, Llm, LlmRequest, LlmResponse, LlmResponseStream, Result, RunBudget,
    RunConfig, SessionId, SpendKey, SpendLedger, SpendPeriod, UsageMetadata, UserId,
};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;

/// Replies without calling a model, so every model call in a run is a compaction call.
struct ReplyAgent;

#[async_trait]
impl Agent for ReplyAgent {
    fn name(&self) -> &str {
        "assistant"
    }
    fn description(&self) -> &str {
        "replies without a model"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        Ok(Box::pin(futures::stream::once(async move {
            let mut event = Event::new(&invocation_id);
            event.author = "assistant".to_string();
            event.set_content(Content::new("model").with_text("a fairly long reply ".repeat(20)));
            Ok(event)
        })))
    }
}

/// Answers every call with fixed usage and cost, counting how often it is called.
struct PricedModel {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Llm for PricedModel {
    fn name(&self) -> &str {
        "summary-model"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("summary")),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 400,
                candidates_token_count: 100,
                total_token_count: 500,
                cost: Some(0.002),
                ..Default::default()
            }),
            provider: Some("summary-vendor".to_string()),
            model: Some("summary-model".to_string()),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

/// Summarizes through `adk_core::generate_metered`, as the built-in summarizers do.
struct ModelSummarizer {
    model: PricedModel,
}

#[async_trait]
impl BaseEventsSummarizer for ModelSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        let request =
            LlmRequest::new(self.model.name(), vec![Content::new("user").with_text("summarize")]);
        let mut stream = adk_core::generate_metered(&self.model, request, false).await?;
        let mut summary = None;
        while let Some(response) = stream.next().await {
            if let Some(content) = response?.content {
                summary = Some(content);
            }
        }
        let Some(compacted_content) = summary else {
            return Ok(None);
        };
        let mut event = Event::new("compaction");
        event.author = "system".to_string();
        event.actions = EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: events.first().map(|e| e.timestamp).unwrap_or_default(),
                end_timestamp: events.last().map(|e| e.timestamp).unwrap_or_default(),
                compacted_content,
            }),
            ..Default::default()
        };
        Ok(Some(event))
    }
}

struct Fixture {
    calls: Arc<AtomicUsize>,
    sessions: Arc<InMemorySessionService>,
}

impl Fixture {
    async fn new() -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(adk_session::CreateRequest {
                app_name: "acme".to_string(),
                user_id: "user-1".to_string(),
                session_id: Some("session-1".to_string()),
                state: Default::default(),
            })
            .await
            .unwrap();
        Self { calls: Arc::new(AtomicUsize::new(0)), sessions }
    }

    fn summarizer(&self) -> Arc<ModelSummarizer> {
        Arc::new(ModelSummarizer { model: PricedModel { calls: self.calls.clone() } })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    async fn run(&self, runner: &Runner, config: RunConfig) {
        let mut events = runner
            .run_with_config(
                UserId::new("user-1").unwrap(),
                SessionId::new("session-1").unwrap(),
                Content::new("user").with_text("hello there"),
                Some(config),
            )
            .await
            .unwrap();
        while let Some(event) = events.next().await {
            event.unwrap();
        }
    }

    async fn compaction_events(&self) -> usize {
        let session = self
            .sessions
            .get(adk_session::GetRequest {
                app_name: "acme".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-1".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        session.events().all().iter().filter(|event| event.actions.compaction.is_some()).count()
    }
}

fn run_config(tracker: &Arc<BudgetTracker>, ledger: &Arc<InMemorySpendLedger>) -> RunConfig {
    let mut config = RunConfig::builder().spend_ledger(ledger.clone()).build();
    config.budget_tracker = Some(tracker.clone());
    config
}

fn post_run_compaction(fixture: &Fixture) -> Runner {
    Runner::builder()
        .app_name("acme")
        .agent(Arc::new(ReplyAgent))
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .compaction_config(EventsCompactionConfig {
            compaction_interval: 1,
            overlap_size: 0,
            summarizer: fixture.summarizer(),
        })
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_compaction_call_counts_against_the_run_budget_and_spend_ledger() {
    let fixture = Fixture::new().await;
    let runner = post_run_compaction(&fixture);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(10)));
    let ledger = Arc::new(InMemorySpendLedger::default());

    fixture.run(&runner, run_config(&tracker, &ledger)).await;

    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.compaction_events().await, 1);
    let usage = tracker.usage();
    assert_eq!((usage.model_calls, usage.total_tokens, usage.cost_micro_usd), (1, 500, 2_000));
    let key = SpendKey::org("acme")
        .with_agent("urn:adk:agent:acme/assistant")
        .with_vendor("summary-vendor");
    assert_eq!(ledger.spent(&key, SpendPeriod::Lifetime).await.unwrap(), 2_000);
}

#[tokio::test]
async fn a_compaction_call_over_the_budget_is_never_sent() {
    let fixture = Fixture::new().await;
    let runner = post_run_compaction(&fixture);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(1)));
    drop(tracker.begin_model_call("main-model").unwrap());
    let ledger = Arc::new(InMemorySpendLedger::default());

    fixture.run(&runner, run_config(&tracker, &ledger)).await;

    assert_eq!(fixture.calls(), 0, "the refused summary call must not reach the model");
    assert_eq!(fixture.compaction_events().await, 0);
    assert_eq!(tracker.usage().model_calls, 1);
    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Lifetime).await.unwrap(), 0);
}

#[tokio::test]
async fn a_compaction_call_over_the_spend_cap_is_never_sent() {
    let fixture = Fixture::new().await;
    let runner = post_run_compaction(&fixture);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::new(
        adk_core::SpendLimits::new().limit(SpendKey::org("acme"), 10_000),
    ));

    fixture.run(&runner, run_config(&tracker, &ledger)).await;

    // The default hold, 0.05 USD, does not fit under a 0.01 USD cap.
    assert_eq!(fixture.calls(), 0);
    assert_eq!(tracker.usage().model_calls, 0);
}

#[tokio::test]
async fn intra_invocation_compaction_is_metered_and_refused_over_budget() {
    let fixture = Fixture::new().await;
    let runner = Runner::builder()
        .app_name("acme")
        .agent(Arc::new(ReplyAgent))
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .intra_compaction_config(IntraCompactionConfig {
            token_threshold: 1,
            overlap_event_count: 1,
            chars_per_token: 4,
        })
        .intra_compaction_summarizer(fixture.summarizer())
        .build()
        .unwrap();
    let ledger = Arc::new(InMemorySpendLedger::default());

    // The first run leaves history; the second compacts it before the agent runs.
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    fixture.run(&runner, run_config(&tracker, &ledger)).await;
    fixture.run(&runner, run_config(&tracker, &ledger)).await;
    let calls = fixture.calls();
    assert!(calls >= 1, "intra-invocation compaction ran");
    assert_eq!(tracker.usage().model_calls, calls as u64);
    assert_eq!(tracker.usage().total_tokens, 500 * calls as u64);
    let key = SpendKey::org("acme").with_agent("urn:adk:agent:acme/assistant");
    assert_eq!(ledger.spent(&key, SpendPeriod::Lifetime).await.unwrap(), 2_000 * calls as u64);

    let exhausted = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(0)));
    fixture.run(&runner, run_config(&exhausted, &ledger)).await;
    assert_eq!(fixture.calls(), calls, "no summary call is sent over the budget");
    assert_eq!(exhausted.usage().model_calls, 0);
}
