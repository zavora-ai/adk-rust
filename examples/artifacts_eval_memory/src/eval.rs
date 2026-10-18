//! Evaluation scenarios: a live judged case, and inputs that must not pass vacuously.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Agent, Llm};
use adk_eval::schema::ContentData;
use adk_eval::{
    EvalCase, EvalError, EvaluationConfig, EvaluationCriteria, EvaluationResult, Evaluator,
    TestFile, Turn,
};

use crate::common::{Verdict, agent};

fn answering_agent(model: Arc<dyn Llm>) -> anyhow::Result<Arc<dyn Agent>> {
    agent(model, "Answer the question with a single word and nothing else.", vec![])
}

fn case(id: &str, turns: Vec<Turn>) -> EvalCase {
    EvalCase {
        eval_id: id.to_string(),
        description: String::new(),
        conversation: turns,
        session_input: Default::default(),
        tags: vec![],
        metadata: None,
    }
}

fn capital_turn(expected: Option<&str>) -> Turn {
    Turn {
        invocation_id: "capital".to_string(),
        user_content: ContentData::text("What is the capital of France?"),
        final_response: expected.map(ContentData::model_response),
        intermediate_data: None,
    }
}

fn judged(judge: Arc<dyn Llm>) -> Evaluator {
    Evaluator::with_llm_judge(
        EvaluationConfig::with_criteria(EvaluationCriteria::semantic_match(0.7)),
        judge,
    )
}

/// A rejection passes only as a configuration error; any other error (a model call that
/// failed, say) says nothing about the finding.
fn rejected(what: &str, error: EvalError) -> Verdict {
    match error {
        EvalError::ConfigError(message) => Verdict::Pass(format!("{what} is rejected: {message}")),
        other => Verdict::Fail(format!("{what} failed for an unrelated reason: {other}")),
    }
}

fn describe(result: &EvaluationResult) -> String {
    let failures: Vec<String> = result
        .failures
        .iter()
        .map(|failure| {
            format!("{}: {}", failure.criterion, failure.details.clone().unwrap_or_default())
        })
        .collect();
    format!("passed = {}, scores = {:?}, failures = {failures:?}", result.passed, result.scores)
}

/// A case the live agent answers and the live judge grades: the happy path.
pub async fn live_judged_case(model: Arc<dyn Llm>, judge: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let result = judged(judge)
        .evaluate_case(answering_agent(model)?, &case("capital", vec![capital_turn(Some("Paris"))]))
        .await?;
    let evidence = describe(&result);
    Ok(if result.passed && result.scores.contains_key("semantic_match") {
        Verdict::Pass(evidence)
    } else {
        Verdict::Fail(evidence)
    })
}

/// A test file with no cases has nothing to pass.
pub async fn zero_cases(model: Arc<dyn Llm>, judge: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let file = TestFile {
        eval_set_id: "empty".to_string(),
        name: "empty".to_string(),
        description: String::new(),
        eval_cases: vec![],
    };
    Ok(match judged(judge).evaluate_test_file(answering_agent(model)?, &file).await {
        Ok(report) if report.all_passed() => Verdict::Fail(format!(
            "an empty test file reports all_passed() with {} of {} cases passed",
            report.summary.passed, report.summary.total
        )),
        Ok(report) => Verdict::Pass(format!(
            "an empty test file does not pass: total = {}, all_passed = false",
            report.summary.total
        )),
        Err(error) => rejected("an empty test file", error),
    })
}

/// A case with no turns has nothing to judge.
pub async fn zero_turns(model: Arc<dyn Llm>, judge: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    Ok(match judged(judge).evaluate_case(answering_agent(model)?, &case("empty", vec![])).await {
        Ok(result) if result.passed => {
            Verdict::Fail(format!("a case with no turns passes: {}", describe(&result)))
        }
        Ok(result) => Verdict::Pass(format!("a case with no turns fails: {}", describe(&result))),
        Err(error) => rejected("a case with no turns", error),
    })
}

/// An evaluator with no criteria has nothing to judge a case by.
pub async fn no_criteria(model: Arc<dyn Llm>, judge: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let evaluator = Evaluator::with_llm_judge(EvaluationConfig::default(), judge);
    let eval_case = case("unjudged", vec![capital_turn(Some("Paris"))]);
    Ok(match evaluator.evaluate_case(answering_agent(model)?, &eval_case).await {
        Ok(result) if result.passed => Verdict::Fail(format!(
            "a case evaluated with no criteria passes: {}",
            describe(&result)
        )),
        Ok(result) => {
            Verdict::Pass(format!("a case with no criteria fails: {}", describe(&result)))
        }
        Err(error) => rejected("an evaluator with no criteria", error),
    })
}

/// A configured criterion that applies to no turn has judged nothing.
pub async fn nothing_judged(model: Arc<dyn Llm>, judge: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    // semantic_match compares against an expected response; this turn has none.
    let eval_case = case("no-expectation", vec![capital_turn(None)]);
    Ok(match judged(judge).evaluate_case(answering_agent(model)?, &eval_case).await {
        Ok(result) if result.passed => {
            Verdict::Fail(format!("a case no criterion applied to passes: {}", describe(&result)))
        }
        Ok(result) if result.failures.iter().any(|f| f.criterion == "not_evaluated") => {
            Verdict::Pass(format!(
                "a case no criterion applied to is not evaluated: {}",
                describe(&result)
            ))
        }
        Ok(result) => {
            Verdict::Fail(format!("the case failed for an unrelated reason: {}", describe(&result)))
        }
        Err(error) => rejected("a case no criterion applied to", error),
    })
}

/// Reads the result of `evaluate_cases_parallel`, which returns one result per case and,
/// from the release that rejects a zero concurrency, wraps them in a `Result`.
trait ParallelOutcome {
    fn rejection(self) -> Option<String>;
}

impl ParallelOutcome for Vec<adk_eval::Result<EvaluationResult>> {
    fn rejection(self) -> Option<String> {
        self.into_iter().find_map(|result| result.err().map(|error| error.to_string()))
    }
}

impl ParallelOutcome for adk_eval::Result<Vec<adk_eval::Result<EvaluationResult>>> {
    fn rejection(self) -> Option<String> {
        match self {
            Ok(results) => results.rejection(),
            Err(error) => Some(error.to_string()),
        }
    }
}

/// A concurrency of zero can never run a case.
pub async fn zero_concurrency(model: Arc<dyn Llm>, judge: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let evaluator = judged(judge);
    let cases = vec![case("capital", vec![capital_turn(Some("Paris"))])];
    let agent = answering_agent(model)?;
    let wait = Duration::from_secs(20);
    Ok(
        match tokio::time::timeout(wait, evaluator.evaluate_cases_parallel(agent, &cases, 0)).await
        {
            Err(_) => Verdict::Fail(format!(
                "evaluate_cases_parallel with concurrency 0 had not returned after {}s",
                wait.as_secs()
            )),
            Ok(outcome) => match outcome.rejection() {
                Some(error) if error.contains("concurrency") => {
                    Verdict::Pass(format!("concurrency 0 is rejected: {error}"))
                }
                Some(error) => Verdict::Fail(format!("an unrelated error: {error}")),
                None => {
                    Verdict::Fail("concurrency 0 returned results without an error".to_string())
                }
            },
        },
    )
}
