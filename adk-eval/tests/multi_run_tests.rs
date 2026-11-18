//! Repeated eval runs report per-case pass rates and flakiness, and the evaluation's run
//! config meters every judge call: a budget, spend cap or kill switch stops the runs instead
//! of failing cases.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{
    Agent, Content, Event, EventStream, GOVERNANCE_FROZEN_CODE, GovernanceControl,
    InMemorySpendLedger, InvocationContext, Llm, LlmResponse, RunBudget, RunConfig,
    SPEND_LIMIT_EXCEEDED_CODE, SpendKey, SpendLedger, SpendLimits, SpendPeriod,
};
use adk_eval::criteria::{ResponseMatchConfig, SimilarityAlgorithm};
use adk_eval::schema::ContentData;
use adk_eval::{
    BaselineStore, EvalCase, EvalError, EvaluationConfig, EvaluationCriteria, Evaluator,
    JUDGE_AGENT_NAME, MultiRunConfig, PASS_RATE_METRIC, TestFile, Turn,
};
use async_trait::async_trait;

/// Answers "Sunny." on odd-numbered calls and "Cloudy." on even ones, and runs `hook` on
/// the call whose number it is given.
struct AlternatingAgent {
    calls: AtomicUsize,
    hook: Option<(usize, Box<dyn Fn() + Send + Sync>)>,
}

impl AlternatingAgent {
    fn new() -> Self {
        Self { calls: AtomicUsize::new(0), hook: None }
    }

    fn on_call(mut self, call: usize, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.hook = Some((call, Box::new(hook)));
        self
    }
}

#[async_trait]
impl Agent for AlternatingAgent {
    fn name(&self) -> &str {
        "weather"
    }

    fn description(&self) -> &str {
        "answers inconsistently"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((at, hook)) = &self.hook
            && *at == call
        {
            hook();
        }
        let text = if call % 2 == 1 { "Sunny." } else { "Cloudy." };
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name().to_string();
        event.llm_response.content = Some(Content::new("model").with_text(text));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

fn case(eval_id: &str, turns: usize) -> EvalCase {
    EvalCase {
        eval_id: eval_id.to_string(),
        description: String::new(),
        conversation: (0..turns)
            .map(|turn| Turn {
                invocation_id: format!("{eval_id}-{turn}"),
                user_content: ContentData::text("Weather in Nairobi?"),
                final_response: Some(ContentData::model_response("Sunny.")),
                intermediate_data: None,
            })
            .collect(),
        session_input: Default::default(),
        tags: vec![],
        metadata: None,
    }
}

fn test_file(cases: Vec<EvalCase>) -> TestFile {
    TestFile {
        eval_set_id: "weather".to_string(),
        name: "Weather".to_string(),
        description: String::new(),
        eval_cases: cases,
    }
}

fn exact_text() -> EvaluationConfig {
    EvaluationConfig::with_criteria(EvaluationCriteria {
        response_similarity: Some(1.0),
        response_match_config: Some(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Exact,
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// A judge that calls every answer equivalent.
fn judge() -> Arc<dyn Llm> {
    Arc::new(adk_model::MockLlm::new("judge-model").with_response(LlmResponse::new(
        Content::new("model").with_text("SCORE: 1.0\nEQUIVALENT: YES\nREASONING: same meaning"),
    )))
}

fn judged() -> Evaluator {
    Evaluator::with_llm_judge(
        EvaluationConfig::with_criteria(EvaluationCriteria::semantic_match(0.5)),
        judge(),
    )
}

fn budgeted(budget: RunBudget) -> RunConfig {
    RunConfig::builder().budget(budget).build()
}

#[tokio::test]
async fn a_flaky_case_reports_its_pass_rate_and_variance() {
    let evaluator = Evaluator::new(exact_text());
    let agent: Arc<dyn Agent> = Arc::new(AlternatingAgent::new());

    let report = evaluator
        .evaluate_test_file_runs(
            agent,
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(4).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(report.completed_runs(), 4);
    let summary = &report.cases[0];
    assert_eq!(
        (summary.runs, summary.passed_runs, summary.flaky, summary.passed),
        (4, 2, true, false)
    );
    assert_eq!(summary.pass_rate, 0.5);
    let stats = &summary.scores["response_similarity"];
    assert_eq!((stats.mean, stats.min, stats.max), (0.5, 0.0, 1.0));
    assert!((stats.variance - 1.0 / 3.0).abs() < 1e-12, "variance {}", stats.variance);
    assert!(!report.all_passed());
    assert!(report.format_summary().contains("FAIL (flaky)"), "{}", report.format_summary());

    // Half the runs pass, which a minimum pass rate of 0.5 accepts.
    let lenient = MultiRunConfig::new(4).unwrap().with_min_pass_rate(0.5).unwrap();
    let report = Evaluator::new(exact_text())
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("nairobi", 1)]),
            &lenient,
        )
        .await
        .unwrap();
    assert!(report.all_passed(), "{}", report.format_summary());
}

#[tokio::test]
async fn every_run_evaluates_every_case_despite_failures() {
    let mut config = exact_text();
    config.continue_on_failure = false;
    let evaluator = Evaluator::new(config);

    let report = evaluator
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("first", 1), case("second", 1)]),
            &MultiRunConfig::new(2).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(report.runs.iter().map(|run| run.results.len()).collect::<Vec<_>>(), [2, 2]);
    assert_eq!(report.cases.iter().map(|case| case.runs).collect::<Vec<_>>(), [2, 2]);
}

#[tokio::test]
async fn a_regression_check_catches_a_case_that_became_flaky() {
    let dir = tempfile::tempdir().unwrap();
    let store = BaselineStore::new(dir.path().join(".eval-baseline.json"));
    let lenient = EvaluationConfig::with_criteria(EvaluationCriteria {
        response_similarity: Some(0.0),
        response_match_config: Some(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Exact,
            ..Default::default()
        }),
        ..Default::default()
    });

    // Every run of the steady agent passes.
    let steady = Evaluator::new(exact_text())
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(1).unwrap(),
        )
        .await
        .unwrap();
    store.save("weather", &steady.baseline_metrics()).unwrap();

    let flaky = Evaluator::new(lenient)
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(2).unwrap(),
        )
        .await
        .unwrap();
    let regressions = store.check_regressions(&flaky.baseline_metrics(), 0.05).unwrap();

    let regressed: Vec<(&str, f64, Option<f64>)> = regressions
        .iter()
        .map(|r| (r.metric_name.as_str(), r.baseline_value, r.current_value))
        .collect();
    assert_eq!(
        regressed,
        [("response_similarity", 1.0, Some(0.5))],
        "the lenient criteria pass both runs, so only the mean score regresses"
    );

    let strict = Evaluator::new(exact_text())
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(2).unwrap(),
        )
        .await
        .unwrap();
    let regressions = store.check_regressions(&strict.baseline_metrics(), 0.05).unwrap();
    assert!(
        regressions
            .iter()
            .any(|r| r.metric_name == PASS_RATE_METRIC && r.current_value == Some(0.5)),
        "{regressions:?}"
    );
}

#[tokio::test]
async fn judge_calls_count_against_the_evaluation_budget() {
    let mut evaluator = judged();
    evaluator.set_run_config(budgeted(RunBudget::new().max_model_calls(2)));
    let tracker = evaluator.run_config().unwrap().budget_tracker.clone().unwrap();

    let report = evaluator
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(3).unwrap(),
        )
        .await
        .unwrap();

    // Two judged runs spend the budget; the third is refused before the agent runs.
    assert_eq!(tracker.usage().model_calls, 2);
    assert_eq!(report.completed_runs(), 2);
    assert_eq!(report.cases[0].runs, 2);
    let stopped = report.stopped.as_deref().unwrap();
    assert!(stopped.starts_with("run 3 of 3"), "{stopped}");
    assert!(!report.all_passed());
}

#[tokio::test]
async fn a_judge_call_the_budget_refuses_stops_the_case_rather_than_failing_it() {
    let mut evaluator = judged();
    evaluator.set_run_config(budgeted(RunBudget::new().max_model_calls(1)));
    let agent: Arc<dyn Agent> = Arc::new(AlternatingAgent::new());

    // The second turn's judge call is over the budget.
    let error = evaluator.evaluate_case(agent.clone(), &case("two_turns", 2)).await.unwrap_err();

    let EvalError::Refused(refusal) = &error else { panic!("expected a refusal, got {error}") };
    assert_eq!(refusal.code, "budget.model_calls");
    let error = evaluator
        .evaluate_test_file(agent, &test_file(vec![case("two_turns", 2)]))
        .await
        .unwrap_err();
    assert!(error.is_refusal(), "{error}");
}

#[tokio::test]
async fn a_spend_cap_refuses_unpriced_judge_calls_and_spend_is_attributed_to_the_judge() {
    let uncapped = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let mut evaluator = judged();
    evaluator.set_run_config(RunConfig::builder().spend_ledger(uncapped.clone()).build());

    let result = evaluator
        .evaluate_case(Arc::new(AlternatingAgent::new()), &case("nairobi", 1))
        .await
        .unwrap();
    assert!(result.passed);
    let judge_key =
        SpendKey::org("eval_app").with_agent(format!("urn:adk:agent:eval_app/{JUDGE_AGENT_NAME}"));
    assert!(uncapped.spent(&judge_key, SpendPeriod::Day).await.unwrap() > 0);

    let capped = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("eval_app").per(SpendPeriod::Day), 1_000),
    ));
    let mut evaluator = judged();
    evaluator.set_run_config(RunConfig::builder().spend_ledger(capped).build());

    let error = evaluator
        .evaluate_case(Arc::new(AlternatingAgent::new()), &case("nairobi", 1))
        .await
        .unwrap_err();
    let EvalError::Refused(refusal) = &error else { panic!("expected a refusal, got {error}") };
    assert_eq!(refusal.code, SPEND_LIMIT_EXCEEDED_CODE);
}

#[tokio::test]
async fn a_frozen_kill_switch_stops_the_runs() {
    let kill_switch = GovernanceControl::new();
    let config = RunConfig::builder().governance(kill_switch.clone()).build();

    // Frozen before the evaluation: nothing runs.
    kill_switch.freeze("incident 7");
    let mut evaluator = judged();
    evaluator.set_run_config(config.clone());
    let report = evaluator
        .evaluate_test_file_runs(
            Arc::new(AlternatingAgent::new()),
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(3).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(report.completed_runs(), 0);
    assert!(report.stopped.as_deref().unwrap().contains("incident 7"), "{:?}", report.stopped);
    assert!(!report.all_passed());

    // Frozen by the agent during the second run: its judge call is refused.
    kill_switch.unfreeze();
    let freezer = kill_switch.clone();
    let agent = AlternatingAgent::new().on_call(2, move || freezer.freeze("incident 8"));
    let report = evaluator
        .evaluate_test_file_runs(
            Arc::new(agent),
            &test_file(vec![case("nairobi", 1)]),
            &MultiRunConfig::new(3).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(report.completed_runs(), 1);
    assert!(report.stopped.as_deref().unwrap().starts_with("run 2 of 3"), "{:?}", report.stopped);

    let error = evaluator
        .evaluate_case(Arc::new(AlternatingAgent::new()), &case("nairobi", 1))
        .await
        .unwrap_err();
    let EvalError::Refused(refusal) = &error else { panic!("expected a refusal, got {error}") };
    assert_eq!(refusal.code, GOVERNANCE_FROZEN_CODE);
}
