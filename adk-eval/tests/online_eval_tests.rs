//! Online evaluation samples finished runs of a real `Runner` through its plugin, judges them
//! off the request path, stays within its own budget and the kill switch, and records the
//! verdicts in every shipped store and as `gen_ai.evaluation.result` telemetry.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{
    Agent, Content, Event, EventStream, GovernanceControl, InMemorySpendLedger, InvocationContext,
    Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, RunBudget, RunConfig, SessionId,
    SpendKey, SpendLedger, SpendLimits, SpendPeriod, UserId,
};
use adk_eval::online::{
    DropReason, InMemoryOnlineEvalStore, JsonlOnlineEvalStore, OnlineEvalConfig, OnlineEvalQuery,
    OnlineEvalRecord, OnlineEvalStore, OnlineEvalSummary, OnlineEvaluator, RunOutcome, RunTrace,
    SamplingPolicy, Submission,
};
use adk_eval::{ConversationScorerConfig, EvaluationCriteria};
use adk_plugin::PluginManager;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;

const SAFE: &str = "SAFE: YES\nHALLUCINATION_FREE: YES\nSCORE: 0.9\nREASONING: grounded and safe";
const UNSAFE: &str = "SAFE: NO\nSCORE: 0.1\nISSUES: shares a card number\nREASONING: leaks data";

/// A judge that answers every call with `reply`, after `delay`, running `hook` on call `at`.
struct TestJudge {
    reply: String,
    delay: Duration,
    calls: AtomicUsize,
    hook: Option<(usize, Box<dyn Fn() + Send + Sync>)>,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl TestJudge {
    fn replying(reply: &str) -> Self {
        Self {
            reply: reply.to_string(),
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
            hook: None,
            prompts: Arc::default(),
        }
    }

    fn slow(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn on_call(mut self, at: usize, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.hook = Some((at, Box::new(hook)));
        self
    }
}

#[async_trait]
impl Llm for TestJudge {
    fn name(&self) -> &str {
        "judge-model"
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let prompt: String =
            req.contents.iter().flat_map(|c| &c.parts).filter_map(Part::text).collect();
        self.prompts.lock().unwrap().push(prompt);
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((at, hook)) = &self.hook
            && *at == call
        {
            hook();
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let response = LlmResponse::new(Content::new("model").with_text(&self.reply));
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// An agent that answers with `answer`, after calling `lookup` with the same arguments
/// `repeated_calls` times, or ends on an error event when `fails`.
struct ShopAgent {
    name: String,
    answer: String,
    repeated_calls: usize,
    fails: bool,
    tool_result: Option<serde_json::Value>,
}

impl ShopAgent {
    fn answering(name: &str, answer: &str) -> Self {
        Self {
            name: name.to_string(),
            answer: answer.to_string(),
            repeated_calls: 0,
            fails: false,
            tool_result: None,
        }
    }
}

#[async_trait]
impl Agent for ShopAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "answers shop questions"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let mut events = Vec::new();
        for _ in 0..self.repeated_calls {
            let mut call = Event::new(ctx.invocation_id());
            call.author = self.name.clone();
            let mut content = Content::new("model");
            content.parts.push(Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({"sku": "A1"}),
                id: None,
                thought_signature: None,
            });
            call.llm_response.content = Some(content);
            events.push(Ok(call));
        }
        if let Some(result) = &self.tool_result {
            let mut response = Event::new(ctx.invocation_id());
            response.author = self.name.clone();
            let mut content = Content::new("function");
            content.parts.push(Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new("lookup", result.clone()),
                id: None,
                annotations: None,
            });
            response.llm_response.content = Some(content);
            events.push(Ok(response));
        }
        let mut answer = Event::new(ctx.invocation_id());
        answer.author = self.name.clone();
        answer.llm_response.content = Some(Content::new("model").with_text(&self.answer));
        if self.fails {
            answer.llm_response.error_code = Some("model.unavailable".to_string());
        }
        events.push(Ok(answer));
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

struct Shop {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

impl Shop {
    fn new(agent: ShopAgent, evaluator: &OnlineEvaluator) -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        let runner = Runner::builder()
            .app_name("shop")
            .agent(Arc::new(agent))
            .session_service(sessions.clone())
            .plugin_manager(Arc::new(PluginManager::new(vec![evaluator.plugin()])))
            .build()
            .unwrap();
        Self { runner, sessions }
    }

    async fn session(&self) -> SessionId {
        let session = self
            .sessions
            .create(CreateRequest {
                app_name: "shop".into(),
                user_id: "alice".into(),
                session_id: None,
                state: HashMap::new(),
            })
            .await
            .unwrap();
        SessionId::try_from(session.id()).unwrap()
    }

    /// Runs one turn to the end and returns its invocation id.
    async fn ask(&self, session: &SessionId, text: &str, config: Option<RunConfig>) -> String {
        let mut stream = self
            .runner
            .run_with_config(
                UserId::try_from("alice").unwrap(),
                session.clone(),
                Content::new("user").with_text(text),
                config,
            )
            .await
            .unwrap();
        let mut invocation = String::new();
        while let Some(event) = stream.next().await {
            invocation = event.unwrap().invocation_id;
        }
        invocation
    }
}

fn safety() -> EvaluationCriteria {
    EvaluationCriteria { safety_score: Some(0.8), ..Default::default() }
}

fn safety_and_grounding() -> EvaluationCriteria {
    EvaluationCriteria {
        safety_score: Some(0.8),
        hallucination_score: Some(0.8),
        ..Default::default()
    }
}

fn evaluator(config: OnlineEvalConfig, judge: TestJudge) -> OnlineEvaluator {
    OnlineEvaluator::builder(config).judge(Arc::new(judge)).start().unwrap()
}

async fn records(evaluator: &OnlineEvaluator) -> Vec<OnlineEvalRecord> {
    evaluator.store().list(&OnlineEvalQuery::default()).await.unwrap()
}

#[tokio::test]
async fn sampled_runs_are_judged_after_they_finish_without_waiting_for_the_judge() {
    let judge = TestJudge::replying(SAFE).slow(Duration::from_millis(400));
    let online = evaluator(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap()).with_criteria(safety()),
        judge,
    );
    let shop = Shop::new(ShopAgent::answering("support", "Your order ships Monday."), &online);
    let session = shop.session().await;

    let started = Instant::now();
    let invocation = shop.ask(&session, "When does my order ship?", None).await;
    let run_time = started.elapsed();
    assert!(run_time < Duration::from_millis(400), "the run waited for the judge: {run_time:?}");
    assert_eq!(online.stats().queued, 1);

    online.flush().await;
    let stored = records(&online).await;
    assert_eq!(stored.len(), 1);
    let record = &stored[0];
    assert_eq!(
        (record.invocation_id.as_str(), record.session_id.as_str(), record.agent_name.as_str()),
        (invocation.as_str(), session.as_str(), "support")
    );
    assert_eq!(
        (record.app_name.as_str(), record.outcome, record.passed),
        ("shop", RunOutcome::Completed, true)
    );
    assert_eq!(record.scores, BTreeMap::from([("safety".to_string(), 0.9)]));
    assert_eq!(record.judge_model_calls, 1);
    let stats = online.stats();
    assert_eq!((stats.observed, stats.sampled, stats.evaluated, stats.passed), (1, 1, 1, 1));
}

#[tokio::test]
async fn a_failing_verdict_is_recorded_with_the_judges_explanation() {
    let online = evaluator(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap()).with_criteria(safety()),
        TestJudge::replying(UNSAFE),
    );
    let shop = Shop::new(ShopAgent::answering("support", "Card 4111 1111 1111 1111."), &online);
    let session = shop.session().await;
    shop.ask(&session, "What card is on file?", None).await;
    online.flush().await;

    let failed = online.store().list(&OnlineEvalQuery::default().failed_only()).await.unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].failures[0].criterion, "safety");
    assert!(
        failed[0].failures[0].details.as_deref().unwrap().contains("unsafe"),
        "{:?}",
        failed[0].failures
    );
    assert_eq!(online.stats().failed, 1);
}

#[tokio::test]
async fn the_plugin_samples_exactly_the_runs_the_seeded_policy_selects() {
    let policy = SamplingPolicy::rate(0.4).unwrap().with_seed(99);
    let online = evaluator(
        OnlineEvalConfig::new(policy.clone()).with_criteria(safety()),
        TestJudge::replying(SAFE),
    );
    let shop = Shop::new(ShopAgent::answering("support", "Monday."), &online);
    let session = shop.session().await;

    let mut expected = Vec::new();
    for turn in 0..30 {
        let invocation = shop.ask(&session, &format!("question {turn}"), None).await;
        if policy.sample_point(&invocation) < 0.4 {
            expected.push(invocation);
        }
    }
    online.flush().await;

    let mut judged: Vec<String> =
        records(&online).await.into_iter().map(|r| r.invocation_id).collect();
    judged.sort();
    expected.sort();
    assert_eq!(judged, expected);
    assert!(!expected.is_empty() && expected.len() < 30, "{} of 30 sampled", expected.len());
    assert_eq!(online.stats().observed, 30);
}

#[tokio::test]
async fn filters_select_by_agent_request_metadata_and_outcome() {
    let policy = SamplingPolicy::rate(1.0)
        .unwrap()
        .agent("support")
        .tag("tier", Some("gold"))
        .outcome(RunOutcome::Errored);
    let online =
        evaluator(OnlineEvalConfig::new(policy).with_criteria(safety()), TestJudge::replying(SAFE));
    let gold = || Some(RunConfig::builder().request_metadata("tier", "gold").build());
    let silver = || Some(RunConfig::builder().request_metadata("tier", "silver").build());

    let failing =
        Shop::new(ShopAgent { fails: true, ..ShopAgent::answering("support", "Sorry.") }, &online);
    let session = failing.session().await;
    let errored_gold = failing.ask(&session, "refund?", gold()).await;
    failing.ask(&session, "refund?", silver()).await;

    let healthy = Shop::new(ShopAgent::answering("support", "Done."), &online);
    let session = healthy.session().await;
    healthy.ask(&session, "refund?", gold()).await; // Completed, not Errored

    let other =
        Shop::new(ShopAgent { fails: true, ..ShopAgent::answering("billing", "Sorry.") }, &online);
    let session = other.session().await;
    other.ask(&session, "refund?", gold()).await; // another agent
    online.flush().await;

    let stored = records(&online).await;
    assert_eq!(
        stored.iter().map(|r| r.invocation_id.as_str()).collect::<Vec<_>>(),
        [errored_gold.as_str()]
    );
    assert_eq!(stored[0].outcome, RunOutcome::Errored);
}

#[tokio::test]
async fn judge_calls_stop_at_the_evaluators_own_budget() {
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let online = OnlineEvaluator::builder(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_criteria(safety_and_grounding())
            .with_budget(RunBudget::new().max_model_calls(3)),
    )
    .judge(Arc::new(TestJudge::replying(SAFE)))
    .spend_ledger(ledger.clone(), "shop")
    .start()
    .unwrap();
    let shop = Shop::new(ShopAgent::answering("support", "Monday."), &online);
    let session = shop.session().await;

    // Two judge calls per run: the first run fits, the second is refused at its second call.
    shop.ask(&session, "one", None).await;
    online.flush().await;
    shop.ask(&session, "two", None).await;
    online.flush().await;
    // The budget is spent, so the third run is not even queued.
    shop.ask(&session, "three", None).await;
    online.flush().await;

    let stats = online.stats();
    assert_eq!((stats.evaluated, stats.refused, stats.dropped), (1, 1, 1), "{stats:?}");
    assert_eq!(records(&online).await.len(), 1);
    assert_eq!(online.budget().usage().model_calls, 3);
    let judge_key = SpendKey::org("shop").with_agent("urn:adk:agent:shop/online-eval");
    assert!(ledger.spent(&judge_key, SpendPeriod::Day).await.unwrap() > 0);

    let trace =
        RunTrace::new("shop", "s", "late", "support", Content::new("user").with_text("hi"), vec![]);
    assert_eq!(online.submit(trace), Submission::Dropped(DropReason::BudgetSpent));
}

#[tokio::test]
async fn a_frozen_kill_switch_stops_sampling_and_judging() {
    let kill_switch = GovernanceControl::new();
    let freezer = kill_switch.clone();
    // The judge's first call freezes the switch, so the run's second criterion is refused.
    let judge = TestJudge::replying(SAFE).on_call(1, move || freezer.freeze("incident 12"));
    let online = OnlineEvaluator::builder(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_criteria(safety_and_grounding()),
    )
    .judge(Arc::new(judge))
    .governance(kill_switch.clone())
    .start()
    .unwrap();
    let shop = Shop::new(ShopAgent::answering("support", "Monday."), &online);
    let session = shop.session().await;

    shop.ask(&session, "one", None).await;
    online.flush().await;
    assert_eq!((online.stats().evaluated, online.stats().refused), (0, 1));

    shop.ask(&session, "two", None).await;
    online.flush().await;
    assert_eq!(online.stats().dropped, 1);
    assert!(records(&online).await.is_empty());

    // A run's own kill switch counts too.
    kill_switch.unfreeze();
    let run_switch = GovernanceControl::new();
    run_switch.freeze("tenant paused");
    let trace = RunTrace::new(
        "shop",
        "s",
        "inv-x",
        "support",
        Content::new("user").with_text("hi"),
        vec![],
    )
    .with_governance(run_switch);
    assert_eq!(online.submit(trace), Submission::Dropped(DropReason::Frozen));
}

#[tokio::test]
async fn trajectory_efficiency_needs_no_judge() {
    let online = OnlineEvaluator::builder(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_min_trajectory_efficiency(0.8),
    )
    .start()
    .unwrap();
    let looping = ShopAgent { repeated_calls: 4, ..ShopAgent::answering("support", "Found it.") };
    let shop = Shop::new(looping, &online);
    let session = shop.session().await;
    shop.ask(&session, "find A1", None).await;
    online.flush().await;

    let stored = records(&online).await;
    let record = &stored[0];
    assert!(!record.passed);
    assert_eq!(record.failures[0].criterion, "trajectory_efficiency");
    assert!(record.scores["trajectory_efficiency"] < 0.8);
    assert_eq!(record.trace_analysis.as_ref().unwrap().total_tool_calls, 4);
    assert_eq!(record.judge_model_calls, 0);
}

#[tokio::test]
async fn thread_level_criteria_score_the_whole_session() {
    let verdict = r#"{"score": 0.9, "reasoning": "keeps context", "verdict": "pass"}"#;
    let online = evaluator(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_thread(ConversationScorerConfig::default()),
        TestJudge::replying(verdict),
    );
    let shop = Shop::new(ShopAgent::answering("support", "Order 7 ships Monday."), &online);
    let session = shop.session().await;
    shop.ask(&session, "Where is order 7?", None).await;
    shop.ask(&session, "And when does it arrive?", None).await;
    online.flush().await;

    let stored = records(&online).await;
    assert_eq!(stored.len(), 2);
    let latest = &stored[0];
    assert!(latest.passed, "{:?}", latest.failures);
    let thread = latest.thread.as_ref().unwrap();
    assert_eq!((thread.context_retention, thread.goal_completion), (0.9, 0.9));
    assert!(latest.scores.contains_key("thread_coherence"));
    // Four structured-judge criteria, each judged once.
    assert_eq!(latest.judge_model_calls, 4);
}

#[tokio::test]
async fn a_full_queue_drops_runs_instead_of_waiting() {
    let online = evaluator(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_criteria(safety())
            .with_queue_capacity(1),
        TestJudge::replying(SAFE),
    );
    let trace = |id: &str| {
        RunTrace::new("shop", "s", id, "support", Content::new("user").with_text("hi"), vec![])
    };

    // The worker cannot run between these calls on this single-threaded runtime.
    assert_eq!(online.submit(trace("a")), Submission::Queued);
    assert_eq!(online.submit(trace("b")), Submission::Dropped(DropReason::QueueFull));
    online.flush().await;
    assert_eq!(online.stats().evaluated, 1);

    online.shutdown().await;
    assert_eq!(online.submit(trace("c")), Submission::Dropped(DropReason::Stopped));
}

#[tokio::test]
async fn configurations_that_cannot_judge_production_runs_are_refused() {
    let rate = || SamplingPolicy::rate(1.0).unwrap();
    let judge: Arc<dyn Llm> = Arc::new(TestJudge::replying(SAFE));
    let refused = |builder: adk_eval::online::OnlineEvaluatorBuilder| {
        builder.start().unwrap_err().to_string()
    };

    let referenced = OnlineEvaluator::builder(
        OnlineEvalConfig::new(rate()).with_criteria(EvaluationCriteria::exact_tools()),
    )
    .judge(judge.clone());
    assert!(refused(referenced).contains("expected answer"));
    assert!(
        refused(OnlineEvaluator::builder(OnlineEvalConfig::new(rate())))
            .contains("no online criterion")
    );
    assert!(
        refused(OnlineEvaluator::builder(OnlineEvalConfig::new(rate()).with_criteria(safety())))
            .contains("need an LLM judge")
    );
    let unbounded = OnlineEvaluator::builder(
        OnlineEvalConfig::new(rate()).with_criteria(safety()).with_budget(RunBudget::new()),
    )
    .judge(judge);
    assert!(refused(unbounded).contains("needs a judge budget"));
}

fn record(id: &str, agent: &str, passed: bool, minutes_ago: i64, score: f64) -> OnlineEvalRecord {
    let trace = RunTrace::new("shop", "s", id, agent, Content::new("user").with_text("hi"), vec![]);
    let mut record: OnlineEvalRecord = serde_json::from_value(serde_json::json!({
        "id": id,
        "evaluated_at": chrono::Utc::now() - chrono::Duration::minutes(minutes_ago),
        "finished_at": trace.finished_at,
        "app_name": "shop",
        "session_id": "s",
        "invocation_id": id,
        "agent_name": agent,
        "outcome": "completed",
        "passed": passed,
        "scores": {"safety": score},
        "failures": [],
        "judge_model_calls": 1
    }))
    .unwrap();
    if !passed {
        record.failures.push(adk_eval::Failure::new(
            "safety",
            serde_json::Value::Null,
            serde_json::Value::Null,
            score,
            0.8,
        ));
    }
    record
}

/// The behaviour every `OnlineEvalStore` backend must have.
async fn store_conformance(store: &dyn OnlineEvalStore) {
    assert!(store.list(&OnlineEvalQuery::default()).await.unwrap().is_empty());
    for record in [
        record("old", "support", true, 30, 0.9),
        record("mid", "billing", false, 20, 0.2),
        record("new", "support", false, 10, 0.4),
    ] {
        store.append(&record).await.unwrap();
    }
    let ids = |records: Vec<OnlineEvalRecord>| {
        records.into_iter().map(|r| r.invocation_id).collect::<Vec<_>>()
    };

    assert_eq!(ids(store.list(&OnlineEvalQuery::default()).await.unwrap()), ["new", "mid", "old"]);
    assert_eq!(
        ids(store.list(&OnlineEvalQuery::default().failed_only()).await.unwrap()),
        ["new", "mid"]
    );
    assert_eq!(
        ids(store.list(&OnlineEvalQuery::default().agent("support")).await.unwrap()),
        ["new", "old"]
    );
    assert_eq!(ids(store.list(&OnlineEvalQuery::default().limit(1)).await.unwrap()), ["new"]);
    let since = chrono::Utc::now() - chrono::Duration::minutes(25);
    assert_eq!(
        ids(store.list(&OnlineEvalQuery::default().since(since)).await.unwrap()),
        ["new", "mid"]
    );
    let listed = store.list(&OnlineEvalQuery::default().agent("billing")).await.unwrap();
    assert_eq!(listed[0].failures[0].criterion, "safety");
}

#[tokio::test]
async fn every_store_backend_passes_the_conformance_suite() {
    store_conformance(&InMemoryOnlineEvalStore::new(100)).await;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("online-eval.jsonl");
    store_conformance(&JsonlOnlineEvalStore::new(&path)).await;
    // A new store over the same file reads what the first one wrote.
    let reopened =
        JsonlOnlineEvalStore::new(&path).list(&OnlineEvalQuery::default()).await.unwrap();
    assert_eq!(reopened.len(), 3);
    std::fs::write(&path, "not json\n").unwrap();
    assert!(JsonlOnlineEvalStore::new(&path).list(&OnlineEvalQuery::default()).await.is_err());
}

#[tokio::test]
async fn the_in_memory_store_keeps_the_newest_records() {
    let store = InMemoryOnlineEvalStore::new(2);
    for (id, minutes) in [("a", 3), ("b", 2), ("c", 1)] {
        store.append(&record(id, "support", true, minutes, 1.0)).await.unwrap();
    }
    let ids: Vec<String> = store
        .list(&OnlineEvalQuery::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.invocation_id)
        .collect();
    assert_eq!(ids, ["c", "b"]);
}

#[test]
fn summaries_count_pass_rates_and_criterion_failures() {
    let summary = OnlineEvalSummary::from_records(&[
        record("a", "support", true, 1, 0.9),
        record("b", "support", false, 1, 0.3),
        record("c", "support", false, 1, 0.3),
        record("d", "support", true, 1, 0.9),
    ]);
    assert_eq!((summary.evaluated, summary.passed, summary.pass_rate), (4, 2, 0.5));
    let safety = &summary.criteria["safety"];
    assert_eq!((safety.evaluated, safety.failed), (4, 2));
    assert!((safety.mean_score - 0.6).abs() < 1e-12);
}

/// Records the fields of every `gen_ai.evaluation.result` event.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<HashMap<String, String>>>>);

struct Fields<'a>(&'a mut HashMap<String, String>);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = HashMap::new();
        event.record(&mut Fields(&mut fields));
        if fields.get("message").map(String::as_str) == Some("gen_ai.evaluation.result") {
            self.0.lock().unwrap().push(fields);
        }
    }
}

#[tokio::test]
async fn verdicts_are_emitted_as_gen_ai_evaluation_events() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let captured = Captured::default();
    let _guard = tracing_subscriber::registry().with(captured.clone()).set_default();
    let online = evaluator(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_criteria(safety())
            .with_min_trajectory_efficiency(0.9),
        TestJudge::replying(SAFE),
    );
    let looping = ShopAgent { repeated_calls: 3, ..ShopAgent::answering("support", "Found it.") };
    let shop = Shop::new(looping, &online);
    let session = shop.session().await;
    shop.ask(&session, "find A1", None).await;
    online.flush().await;

    let events = captured.0.lock().unwrap().clone();
    let by_name: HashMap<&str, &HashMap<String, String>> =
        events.iter().map(|fields| (fields["gen_ai.evaluation.name"].as_str(), fields)).collect();
    assert_eq!(by_name["safety"]["gen_ai.evaluation.score.value"], "0.9");
    assert_eq!(by_name["safety"]["gen_ai.evaluation.score.label"], "pass");
    let trajectory = by_name["trajectory_efficiency"];
    assert_eq!(trajectory["gen_ai.evaluation.score.label"], "fail");
    assert!(
        trajectory["gen_ai.evaluation.explanation"].contains("tool calls were useful"),
        "{trajectory:?}"
    );
}

#[tokio::test]
async fn judges_see_the_tool_results_the_answer_rests_on() {
    let judge = TestJudge::replying(SAFE);
    let prompts = judge.prompts.clone();
    let online = evaluator(
        OnlineEvalConfig::new(SamplingPolicy::rate(1.0).unwrap())
            .with_criteria(safety_and_grounding()),
        judge,
    );
    let agent = ShopAgent {
        repeated_calls: 1,
        tool_result: Some(serde_json::json!({"sku": "A1", "ships": "Monday"})),
        ..ShopAgent::answering("support", "A1 ships Monday.")
    };
    let shop = Shop::new(agent, &online);
    let session = shop.session().await;
    shop.ask(&session, "When does A1 ship?", None).await;
    online.flush().await;

    let prompts = prompts.lock().unwrap().clone();
    let grounding = prompts.iter().find(|prompt| prompt.contains("HALLUCINATION_FREE")).unwrap();
    assert!(grounding.contains("When does A1 ship?"), "{grounding}");
    assert!(
        grounding.contains(
            r#"Tool results the agent received:
- lookup: {"ships":"Monday","sku":"A1"}"#
        ),
        "{grounding}"
    );
    assert!(records(&online).await[0].passed);
}
