//! Compaction summaries are model calls made on behalf of the run, so they count
//! against the run's budget and spend ledger like the agent's own calls, whether the runner's
//! summarizer or the model's provider writes them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::history::ProviderOrigin;
use adk_core::intra_compaction::{compaction_summary_text, is_native_compaction};
use adk_core::{
    Agent, BaseEventsSummarizer, BudgetTracker, CompactionTrigger, Content, Event, EventActions,
    EventCompaction, EventStream, EventsCompactionConfig, FunctionResponseData, GovernanceControl,
    HistoryCompactionRequest, InMemorySpendLedger, IntraCompactionConfig, InvocationContext, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, NativeCompaction, NativeCompactionRequest, Part,
    Result, RunBudget, RunConfig, SessionId, SpendKey, SpendLedger, SpendPeriod, ToolDeclarations,
    UsageMetadata, UserId, confirmation_pending_response,
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

/// Replies without calling a model, after asking the run's compactor to compact its history,
/// as an agent that calls a model does before each call.
struct CompactingReplyAgent;

#[async_trait]
impl Agent for CompactingReplyAgent {
    fn name(&self) -> &str {
        "assistant"
    }
    fn description(&self) -> &str {
        "compacts, then replies without a model"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let mut events = Vec::new();
        if let Some(compactor) = ctx.run_config().history_compactor.clone() {
            let history = ctx.session().conversation_history();
            let request =
                HistoryCompactionRequest::new(&history, CompactionTrigger::BeforeModelCall);
            let model: Arc<dyn Llm> = Arc::new(PricedModel { calls: Arc::default() });
            if let Some(compacted) = compactor.compact(ctx.clone(), model, request).await? {
                events.extend(compacted.event);
            }
        }
        let mut event = Event::new(ctx.invocation_id());
        event.author = "assistant".to_string();
        event.set_content(Content::new("model").with_text("a fairly long reply ".repeat(20)));
        events.push(event);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
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
        .agent(Arc::new(CompactingReplyAgent))
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .intra_compaction_config(IntraCompactionConfig::at_tokens(1).with_overlap_event_count(1))
        .intra_compaction_summarizer(fixture.summarizer())
        .build()
        .unwrap();
    let ledger = Arc::new(InMemorySpendLedger::default());

    // The first run leaves history; the agent compacts it in the second.
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

/// A model whose provider compacts conversations itself: a compaction request gets a signed
/// compaction with fixed usage and cost, and every request is recorded.
struct NativeModel {
    requests: Mutex<Vec<LlmRequest>>,
}

impl NativeModel {
    fn new() -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(Vec::new()) })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for NativeModel {
    fn name(&self) -> &str {
        "native-model"
    }

    fn native_compaction(&self) -> Option<NativeCompaction> {
        Some(NativeCompaction::new("native-vendor").keeping_thinking())
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        let asked = NativeCompactionRequest::of(&request).is_some();
        self.requests.lock().unwrap().push(request);
        let content = if asked {
            Content {
                role: "model".to_string(),
                parts: vec![
                    Part::Text { text: compaction_summary_text("the user said hello") },
                    Part::ServerToolResponse {
                        server_tool_response: serde_json::json!({
                            "type": "native_message",
                            "adk_replay_only": true,
                            "adk_native_compaction": true,
                            "content": [{"type": "compaction", "content": "the user said hello", "signature": "sig"}],
                        }),
                    },
                ],
            }
        } else {
            Content::new("model").with_text("a reply")
        };
        let mut response = LlmResponse {
            content: Some(content),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 400,
                candidates_token_count: 100,
                total_token_count: 500,
                cost: Some(0.002),
                ..Default::default()
            }),
            provider: Some("native-vendor".to_string()),
            model: Some("native-model".to_string()),
            turn_complete: true,
            ..Default::default()
        };
        ProviderOrigin::new("native-vendor", "native-model")
            .record(&mut response.provider_metadata);
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

/// Asks the run's compactor to compact `history` with a model that compacts natively, after
/// freezing `freeze` when given, records what came back, then replies without a model.
struct NativeCompactingAgent {
    model: Arc<NativeModel>,
    history: Vec<Content>,
    tools: ToolDeclarations,
    freeze: Option<GovernanceControl>,
    compacted: Arc<Mutex<Option<Vec<Content>>>>,
}

#[async_trait]
impl Agent for NativeCompactingAgent {
    fn name(&self) -> &str {
        "assistant"
    }
    fn description(&self) -> &str {
        "compacts natively, then replies without a model"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        if let Some(control) = &self.freeze {
            control.freeze("incident review");
        }
        let mut events = Vec::new();
        if let Some(compactor) = ctx.run_config().history_compactor.clone() {
            let request =
                HistoryCompactionRequest::new(&self.history, CompactionTrigger::BeforeModelCall)
                    .with_tools(&self.tools);
            let model: Arc<dyn Llm> = self.model.clone();
            if let Some(compacted) = compactor.compact(ctx.clone(), model, request).await? {
                *self.compacted.lock().unwrap() = Some(compacted.history);
                events.extend(compacted.event);
            }
        }
        let mut event = Event::new(ctx.invocation_id());
        event.author = "assistant".to_string();
        event.set_content(Content::new("model").with_text("done"));
        events.push(event);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

fn native_runner(fixture: &Fixture, agent: NativeCompactingAgent) -> Runner {
    Runner::builder()
        .app_name("acme")
        .agent(Arc::new(agent))
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .intra_compaction_config(IntraCompactionConfig::at_tokens(1).with_overlap_event_count(1))
        .build()
        .unwrap()
}

fn lookup_tools() -> ToolDeclarations {
    let mut tools = ToolDeclarations::new();
    tools.insert("lookup".to_string(), serde_json::json!({"description": "reads a page"}));
    tools
}

#[tokio::test]
async fn a_native_compaction_is_written_by_the_provider_and_metered() {
    let fixture = Fixture::new().await;
    let model = NativeModel::new();
    let compacted = Arc::new(Mutex::new(None));
    let history = vec![
        Content::new("user").with_text("hello there, ".repeat(40)),
        Content::new("model").with_text("hello back"),
        Content::new("user").with_text("what did I say?"),
    ];
    let runner = native_runner(
        &fixture,
        NativeCompactingAgent {
            model: model.clone(),
            history: history.clone(),
            tools: lookup_tools(),
            freeze: None,
            compacted: compacted.clone(),
        },
    );
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());

    fixture.run(&runner, run_config(&tracker, &ledger)).await;

    // One provider compaction, with the agent's tools, and no summarizer call.
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert!(NativeCompactionRequest::of(&requests[0]).is_some());
    assert!(requests[0].tools.contains_key("lookup"));
    assert_eq!(requests[0].contents, history[..2].to_vec());
    let compacted = compacted.lock().unwrap().clone().expect("compacted");
    assert!(is_native_compaction(&compacted[0]));
    assert_eq!(compacted[1..], history[2..]);
    // The compaction counts against the run's budget and spend ledger.
    let usage = tracker.usage();
    assert_eq!((usage.model_calls, usage.total_tokens, usage.cost_micro_usd), (1, 500, 2_000));
    let key = SpendKey::org("acme")
        .with_agent("urn:adk:agent:acme/assistant")
        .with_vendor("native-vendor");
    assert_eq!(ledger.spent(&key, SpendPeriod::Lifetime).await.unwrap(), 2_000);
}

#[tokio::test]
async fn a_frozen_run_sends_no_native_compaction() {
    let fixture = Fixture::new().await;
    let model = NativeModel::new();
    let compacted = Arc::new(Mutex::new(None));
    let control = GovernanceControl::new();
    let runner = Runner::builder()
        .app_name("acme")
        .agent(Arc::new(NativeCompactingAgent {
            model: model.clone(),
            history: vec![
                Content::new("user").with_text("hello there, ".repeat(40)),
                Content::new("model").with_text("hello back"),
                Content::new("user").with_text("what did I say?"),
            ],
            tools: lookup_tools(),
            freeze: Some(control.clone()),
            compacted: compacted.clone(),
        }))
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .intra_compaction_config(IntraCompactionConfig::at_tokens(1).with_overlap_event_count(1))
        .governance(control)
        .build()
        .unwrap();
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());

    fixture.run(&runner, run_config(&tracker, &ledger)).await;

    assert!(model.requests().is_empty(), "a frozen run sends no compaction call");
    assert!(compacted.lock().unwrap().is_none());
    assert_eq!(fixture.compaction_events().await, 0);
    assert_eq!(tracker.usage().model_calls, 0);
}

#[tokio::test]
async fn a_held_call_stays_out_of_a_native_summary() {
    let fixture = Fixture::new().await;
    let model = NativeModel::new();
    let compacted = Arc::new(Mutex::new(None));
    let held_call = Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "transfer".to_string(),
            args: serde_json::json!({"amount": 10}),
            id: Some("call-hold".to_string()),
            thought_signature: None,
        }],
    };
    let pending = Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new(
                "transfer",
                confirmation_pending_response("waiting for approval"),
            ),
            id: Some("call-hold".to_string()),
            annotations: None,
        }],
    };
    let history = vec![
        Content::new("user").with_text("background, ".repeat(60)),
        held_call.clone(),
        pending.clone(),
        Content::new("user").with_text("is it done?"),
    ];
    let runner = native_runner(
        &fixture,
        NativeCompactingAgent {
            model: model.clone(),
            history: history.clone(),
            tools: lookup_tools(),
            freeze: None,
            compacted: compacted.clone(),
        },
    );
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());

    fixture.run(&runner, run_config(&tracker, &ledger)).await;

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].contents, history[..1].to_vec(), "the held call is not summarized");
    let compacted = compacted.lock().unwrap().clone().expect("compacted");
    assert_eq!(compacted[1..], [held_call, pending, history[3].clone()]);
}
