//! Item 7: custom criteria, which `Evaluator` does not score, do not count as criteria.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, LlmResponse};
use adk_eval::criteria::CustomCriterion;
use adk_eval::schema::{ContentData, EvalCase, Turn};
use adk_eval::{EvalError, EvaluationConfig, EvaluationCriteria, Evaluator};
use adk_model::ScriptedLlm;

use crate::common::{Mode, Report, Verdict, check, fail};

pub async fn run(report: &mut Report) {
    report.record_result("eval-custom-only", 7, Mode::Local, custom_only().await);
}

/// A configuration whose only criterion is custom has nothing to score, so it is rejected
/// like an empty one instead of running the agent.
async fn custom_only() -> anyhow::Result<Verdict> {
    let model = ScriptedLlm::new(
        "scripted",
        [LlmResponse::new(Content::new("model").with_text("Hello there"))],
    );
    let agent = LlmAgentBuilder::new("greeter").model(Arc::new(model)).build()?;
    let criteria = EvaluationCriteria {
        custom: vec![CustomCriterion {
            name: "tone".to_string(),
            description: "The reply is friendly".to_string(),
            threshold: 0.8,
            config: serde_json::Value::Null,
        }],
        ..Default::default()
    };
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));
    let case = EvalCase {
        eval_id: "greeting".to_string(),
        description: String::new(),
        conversation: vec![Turn {
            invocation_id: "inv-1".to_string(),
            user_content: ContentData::text("Hi"),
            final_response: Some(ContentData::model_response("Hello there")),
            intermediate_data: None,
        }],
        session_input: Default::default(),
        tags: vec![],
        metadata: None,
    };
    Ok(match evaluator.evaluate_case(Arc::new(agent), &case).await {
        Err(EvalError::ConfigError(message)) => {
            check(message.contains("no evaluation criteria"), format!("ConfigError: {message}"))
        }
        Err(error) => fail(format!("unexpected error: {error}")),
        Ok(result) => fail(format!(
            "the case ran (passed: {}, failures: {:?}) instead of being rejected",
            result.passed,
            result.failures.iter().map(|failure| failure.criterion.as_str()).collect::<Vec<_>>()
        )),
    })
}
