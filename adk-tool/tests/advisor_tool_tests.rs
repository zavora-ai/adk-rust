//! `AdvisorTool` in an agent's run: the governed tool path, the run's budget and spend ledger,
//! and the per-run cap.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    BudgetTracker, Content, Event, InMemorySpendLedger, Llm, LlmResponse, Part, Result, RunBudget,
    RunConfig, SessionId, SpendKey, SpendLedger, SpendPeriod, UsageMetadata, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::AdvisorTool;
use futures::StreamExt;
use serde_json::{Value, json};

/// Runs `worker` once with `tool`, under a spend ledger and `tracker`.
async fn run(
    worker: Arc<dyn Llm>,
    tool: AdvisorTool,
    ledger: Arc<InMemorySpendLedger>,
    tracker: Arc<BudgetTracker>,
) -> Vec<Result<Event>> {
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
    let agent = LlmAgentBuilder::new("worker")
        .instruction(
            "Answer the user. Before you answer, call consult_advisor once with your question, \
             then follow its advice.",
        )
        .model(worker)
        .tool(Arc::new(tool))
        .build()
        .unwrap();
    let mut config = RunConfig::builder().spend_ledger(ledger).build();
    config.budget_tracker = Some(tracker);
    let runner = Runner::builder()
        .app_name("acme")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(config)
        .build()
        .unwrap();
    runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text(
                "Our nightly job deletes rows older than 30 days in one DELETE. It now times \
                 out on a 400M-row table. How should we fix it safely?",
            ),
        )
        .await
        .unwrap()
        .collect()
        .await
}

/// The advisor's results the run's events carry, in order.
fn consultations(events: &[Result<Event>]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "consult_advisor" =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

fn priced(text: &str, cost: f64) -> ScriptedResponse {
    let mut response = LlmResponse::new(Content::new("model").with_text(text));
    response.provider = Some("anthropic".to_string());
    response.usage_metadata = Some(UsageMetadata {
        prompt_token_count: 100,
        candidates_token_count: 20,
        total_token_count: 120,
        cost: Some(cost),
        ..Default::default()
    });
    ScriptedResponse::response(response)
}

/// Reports a zero cost, so the ledger does not commit the per-call estimate for the turn.
fn free(reply: ScriptedResponse) -> ScriptedResponse {
    reply.with_usage(UsageMetadata { cost: Some(0.0), ..Default::default() })
}

#[tokio::test]
async fn a_consultation_in_a_run_is_governed_metered_and_capped() {
    let ledger = Arc::new(InMemorySpendLedger::default());
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(10)));
    let worker = Arc::new(ScriptedLlm::new(
        "claude-haiku-5-5",
        [
            free(ScriptedResponse::function_call(
                "consult_advisor",
                json!({ "question": "Batch it?" }),
            )),
            free(ScriptedResponse::function_call(
                "consult_advisor",
                json!({ "question": "Again?" }),
            )),
            priced("Delete in batches of 10,000 rows by primary key.", 0.001),
        ],
    ));
    let advisor = Arc::new(ScriptedLlm::new(
        "claude-sonnet-5-5",
        [priced("Batch the delete by primary key range and commit each batch.", 0.004)],
    ));
    let tool = AdvisorTool::builder(advisor.clone()).max_consultations_per_run(1).build();

    let events = run(worker, tool, ledger.clone(), tracker.clone()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let results = consultations(&events);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["status"], "ok");
    assert_eq!(
        results[0]["advice"],
        "Batch the delete by primary key range and commit each batch."
    );
    assert_eq!(results[1]["status"], "limit_reached");
    assert_eq!(advisor.requests().len(), 1);
    let handover = advisor.requests()[0].contents[1].parts[0].text().unwrap().to_string();
    assert!(handover.contains("Batch it?"), "{handover}");
    assert!(handover.contains("400M-row table"), "{handover}");

    let spent = |key: SpendKey| {
        let ledger = ledger.clone();
        async move { ledger.spent(&key, SpendPeriod::Day).await.unwrap() }
    };
    assert_eq!(spent(SpendKey::org("acme").with_agent("advisor:worker")).await, 4_000);
    assert_eq!(spent(SpendKey::org("acme").with_agent("urn:adk:agent:acme/worker")).await, 1_000);
    assert_eq!(spent(SpendKey::org("acme")).await, 5_000);
    // Three worker turns and one consultation.
    assert_eq!(tracker.usage().model_calls, 4);
}

/// Live: Claude Haiku 5.5 works the task and consults Claude Sonnet 5.5.
///
/// Run with `cargo nextest run -p adk-tool --test advisor_tool_tests --run-ignored only`.
#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn haiku_consults_sonnet_live() {
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let worker = AnthropicClient::new(
        AnthropicConfig::new(key.clone(), "claude-haiku-5-5").with_max_tokens(1_024),
    )
    .unwrap();
    let advisor =
        AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-5-5").with_max_tokens(1_024))
            .unwrap();
    let tool = AdvisorTool::builder(Arc::new(advisor))
        .max_consultations_per_run(2)
        .max_output_tokens(1_024)
        .build();
    let ledger = Arc::new(InMemorySpendLedger::default());
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(0.10)));

    let events = run(Arc::new(worker), tool, ledger.clone(), tracker.clone()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    for event in events.iter().filter_map(|event| event.as_ref().ok()) {
        let Some(content) = event.llm_response.content.as_ref() else { continue };
        for part in &content.parts {
            match part {
                Part::FunctionCall { name, args, .. } => {
                    println!("event {}: call {name} {args}", event.author)
                }
                Part::FunctionResponse { function_response, .. } => println!(
                    "event {}: result {} {}",
                    event.author, function_response.name, function_response.response
                ),
                Part::Text { text } if !event.llm_response.partial => {
                    println!("event {}: text {text}", event.author)
                }
                _ => {}
            }
        }
    }
    let advisor_spend = ledger
        .spent(&SpendKey::org("acme").with_agent("advisor:worker"), SpendPeriod::Day)
        .await
        .unwrap();
    let worker_spend = ledger
        .spent(&SpendKey::org("acme").with_agent("urn:adk:agent:acme/worker"), SpendPeriod::Day)
        .await
        .unwrap();
    let usage = tracker.usage();
    println!(
        "ledger advisor:worker = {advisor_spend} micro-USD; worker = {worker_spend} micro-USD"
    );
    println!(
        "budget: model_calls={} total_tokens={} cost_micro_usd={}",
        usage.model_calls, usage.total_tokens, usage.cost_micro_usd
    );

    let results = consultations(&events);
    assert!(!results.is_empty(), "the worker never consulted the advisor");
    assert_eq!(results[0]["status"], "ok", "{}", results[0]);
    assert!(results[0]["advisorModel"].as_str().unwrap().starts_with("claude-sonnet-5-5"));
    assert!(advisor_spend > 0);
    assert!(worker_spend > 0);
}
