//! `eval_judge_fail_closed`: an LLM judge passes a correct answer, fails a wrong one, and a
//! judged criterion without a judge fails instead of passing (#758).

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm};
use adk_eval::criteria::EvaluationCriteria;
use adk_eval::schema::ContentData;
use adk_eval::{EvalCase, EvaluationConfig, EvaluationResult, Evaluator, Turn};

use crate::common::{Config, Provider, Verdict, brief, fail};

fn capital_case() -> EvalCase {
    EvalCase {
        eval_id: "capital-of-france".to_string(),
        description: "The agent names the capital of France".to_string(),
        conversation: vec![Turn {
            invocation_id: "turn-1".to_string(),
            user_content: ContentData::text("What is the capital of France?"),
            final_response: Some(ContentData::model_response("Paris")),
            intermediate_data: None,
        }],
        session_input: Default::default(),
        tags: Vec::new(),
        metadata: None,
    }
}

fn agent(model: Arc<dyn Llm>, name: &str, instruction: &str) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(LlmAgentBuilder::new(name).model(model).instruction(instruction).build()?))
}

fn describe(result: &EvaluationResult) -> String {
    let failures: Vec<String> = result
        .failures
        .iter()
        .map(|failure| {
            format!(
                "{} score {:.2} ({})",
                failure.criterion,
                failure.score,
                brief(failure.details.as_deref().unwrap_or("no details"))
                    .chars()
                    .take(400)
                    .collect::<String>()
            )
        })
        .collect();
    format!("passed={} [{}]", result.passed, failures.join("; "))
}

pub async fn run(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let model = cfg.model(provider)? as Arc<dyn Llm>;
    let honest = agent(
        Arc::clone(&model),
        "capital_bot",
        "Answer geography questions with the city name only.",
    )?;
    let wrong_instruction = "Whatever the user asks, reply with exactly one word: Berlin";
    let wrong = agent(Arc::clone(&model), "wrong_bot", wrong_instruction)?;
    let case = capital_case();
    let criteria = EvaluationCriteria { semantic_match_score: Some(0.8), ..Default::default() };

    // The judge runs on the provider's own model with the default config, which sends no
    // sampling parameters, so models that reject them can judge.
    let judged = Evaluator::with_llm_judge(
        EvaluationConfig::with_criteria(criteria.clone()),
        Arc::clone(&model),
    );
    let good = judged.evaluate_case(Arc::clone(&honest), &case).await?;
    let bad = judged.evaluate_case(wrong, &case).await?;
    let unjudged = Evaluator::new(EvaluationConfig::with_criteria(criteria))
        .evaluate_case(Arc::clone(&honest), &case)
        .await?;

    let judge_error = |result: &EvaluationResult| {
        result
            .failures
            .iter()
            .find_map(|failure| {
                failure.details.as_deref().filter(|d| d.contains("LLM judge error"))
            })
            .map(str::to_string)
    };
    let mut issues = Vec::new();
    if unjudged.passed {
        issues.push("semantic criterion without a judge passed".to_string());
    }
    if bad.passed {
        issues.push(format!("wrong answer passed: {}", describe(&bad)));
    }
    if !good.passed {
        issues.push(format!("correct answer failed: {}", describe(&good)));
    }
    // A judge error fails closed, but here it means the judge request itself was rejected.
    for (label, result) in [("correct", &good), ("wrong", &bad)] {
        if let Some(detail) = judge_error(result) {
            issues.push(format!("{label} case: {}", brief(&detail)));
        }
    }
    Ok(if issues.is_empty() {
        Verdict::Pass(format!(
            "correct case passed (score {:.2}); wrong case failed (judge score {:.2}); no-judge case failed ({})",
            good.scores.get("semantic_match").copied().unwrap_or_default(),
            bad.scores.get("semantic_match").copied().unwrap_or_default(),
            unjudged
                .failures
                .first()
                .and_then(|failure| failure.details.clone())
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect::<String>()
        ))
    } else {
        Verdict::Fail(issues.join("; "))
    })
}
