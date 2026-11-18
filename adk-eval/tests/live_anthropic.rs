//! Live evaluation against Anthropic: a real `LlmAgent` on `claude-haiku-5-5`, judged by the
//! same model, under one budget for the agent's and the judge's calls.
//!
//! The tests are ignored by default. Run them with `ANTHROPIC_API_KEY` set:
//!
//! ```text
//! cargo nextest run -p adk-eval --test live_anthropic --run-ignored only --no-capture
//! ```

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, InMemorySpendLedger, Llm, RunBudget, RunConfig, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod, Tool, ToolContext,
};
use adk_eval::schema::{ContentData, IntermediateData};
use adk_eval::{
    EvalCase, EvaluationConfig, EvaluationCriteria, Evaluator, JUDGE_AGENT_NAME, MultiRunConfig,
    TestFile, ToolUse, Turn,
};
use async_trait::async_trait;
use serde_json::{Value, json};

const MODEL: &str = "claude-haiku-5-5";

fn model() -> Arc<dyn Llm> {
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY is set");
    let config = adk_model::anthropic::AnthropicConfig::new(key, MODEL);
    Arc::new(adk_model::AnthropicClient::new(config).expect("the Anthropic client builds"))
}

/// Reports fixed weather for any city.
struct GetWeather;

#[async_trait]
impl Tool for GetWeather {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Returns the current weather for a city."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"city": {"type": "string", "description": "City name, such as Nairobi"}},
            "required": ["city"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let city = args["city"].as_str().unwrap_or("unknown");
        Ok(json!({"city": city, "condition": "sunny", "temperature_c": 24}))
    }
}

fn weather_agent() -> Arc<dyn Agent> {
    Arc::new(
        LlmAgentBuilder::new("weather_agent")
            .model(model())
            .instruction(
                "You answer weather questions. Always call get_weather with the city name, then \
                 answer in one short sentence with the condition and temperature.",
            )
            .tool(Arc::new(GetWeather))
            .build()
            .expect("the agent builds"),
    )
}

fn weather_set() -> TestFile {
    TestFile {
        eval_set_id: "live_weather".to_string(),
        name: "Live weather".to_string(),
        description: String::new(),
        eval_cases: vec![EvalCase {
            eval_id: "nairobi".to_string(),
            description: String::new(),
            conversation: vec![Turn {
                invocation_id: "nairobi-1".to_string(),
                user_content: ContentData::text("What's the weather in Nairobi right now?"),
                final_response: Some(ContentData::model_response(
                    "It is sunny and 24°C in Nairobi.",
                )),
                intermediate_data: Some(IntermediateData {
                    tool_uses: vec![
                        ToolUse::new("get_weather").with_args(json!({"city": "Nairobi"})),
                    ],
                    ..Default::default()
                }),
            }],
            session_input: Default::default(),
            tags: vec![],
            metadata: None,
        }],
    }
}

fn evaluator(config: RunConfig) -> Evaluator {
    let criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0),
        semantic_match_score: Some(0.6),
        ..Default::default()
    };
    let mut evaluator = Evaluator::with_llm_judge(
        EvaluationConfig {
            collect_turn_details: true,
            ..EvaluationConfig::with_criteria(criteria)
        },
        model(),
    );
    evaluator.set_run_config(config);
    evaluator
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn three_runs_of_a_live_agent_report_pass_rates_under_one_budget() {
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let evaluator = evaluator(
        RunConfig::builder()
            .budget(RunBudget::new().max_model_calls(30).max_cost_usd(0.50))
            .spend_ledger(ledger.clone())
            .build(),
    );
    let tracker = evaluator.run_config().unwrap().budget_tracker.clone().unwrap();

    let report = evaluator
        .evaluate_test_file_runs(weather_agent(), &weather_set(), &MultiRunConfig::new(3).unwrap())
        .await
        .unwrap();

    println!("{}", report.format_summary());
    let usage = tracker.usage();
    let judge_key =
        SpendKey::org("eval_app").with_agent(format!("urn:adk:agent:eval_app/{JUDGE_AGENT_NAME}"));
    let agent_key = SpendKey::org("eval_app").with_agent("urn:adk:agent:eval_app/weather_agent");
    let judge_spend = ledger.spent(&judge_key, SpendPeriod::Day).await.unwrap();
    let agent_spend = ledger.spent(&agent_key, SpendPeriod::Day).await.unwrap();
    println!(
        "budget: {} model calls, {} tokens, {} micro-USD; ledger: agent {agent_spend}, judge {judge_spend} micro-USD",
        usage.model_calls, usage.total_tokens, usage.cost_micro_usd
    );
    assert_eq!(report.completed_runs(), 3, "{:?}", report.stopped);
    assert_eq!(report.cases[0].runs, 3);
    // Every run is a tool call, an answer and a judge call at least.
    assert!(usage.model_calls >= 9, "{usage:?}");
    assert!(judge_spend > 0 && agent_spend > 0);
    assert!(report.cases[0].scores.contains_key("semantic_match"));
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn a_spent_live_budget_stops_the_runs() {
    // One run takes at least three calls, so the second run cannot finish.
    let evaluator =
        evaluator(RunConfig::builder().budget(RunBudget::new().max_model_calls(4)).build());

    let report = evaluator
        .evaluate_test_file_runs(weather_agent(), &weather_set(), &MultiRunConfig::new(3).unwrap())
        .await
        .unwrap();

    println!("{}", report.format_summary());
    assert_eq!(report.completed_runs(), 1, "{:?}", report.stopped);
    let stopped = report.stopped.as_deref().unwrap();
    assert!(stopped.starts_with("run 2 of 3"), "{stopped}");
    assert!(!report.all_passed());
}
