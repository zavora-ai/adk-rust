//! `DecisionAgent` routes a turn by a typed decision, records the decision, takes its fallback
//! below the confidence floor, and meters the decision against the run budget.

use std::sync::Arc;

use adk_agent::{DecisionAgent, LlmAgentBuilder};
use adk_core::{
    Agent, Choice, ChoiceOption, Content, ErrorCategory, Event, Llm, RunBudget, RunConfig,
    SessionId, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};

fn decision(choice: &str, confidence: f64) -> ScriptedResponse {
    ScriptedResponse::text(
        json!({"choice": choice, "rationale": "the customer was charged twice", "confidence": confidence})
            .to_string(),
    )
}

fn answering(name: &str, reply: &str) -> (Arc<dyn Agent>, Arc<ScriptedLlm>) {
    let model = Arc::new(ScriptedLlm::new(name, vec![ScriptedResponse::text(reply)]));
    let agent = LlmAgentBuilder::new(name).model(model.clone()).build().unwrap();
    (Arc::new(agent), model)
}

struct Outcome {
    events: Vec<adk_core::Result<Event>>,
    state: Option<Value>,
}

async fn run(agent: DecisionAgent, budget: Option<RunBudget>) -> Outcome {
    run_with_input(agent, budget, "I was charged twice this month").await
}

async fn run_with_input(agent: DecisionAgent, budget: Option<RunBudget>, input: &str) -> Outcome {
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "alice".into(),
            session_id: None,
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut builder =
        Runner::builder().app_name("app").agent(Arc::new(agent)).session_service(sessions.clone());
    if let Some(budget) = budget {
        builder = builder.run_config(RunConfig::builder().budget(budget).build());
    }
    let runner = builder.build().unwrap();
    let events = runner
        .run(
            UserId::try_from("alice").unwrap(),
            SessionId::try_from(session.id()).unwrap(),
            Content::new("user").with_text(input),
        )
        .await
        .unwrap()
        .collect()
        .await;
    let state = sessions
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "alice".into(),
            session_id: session.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .state()
        .get("triage");
    Outcome { events, state }
}

fn authors(outcome: &Outcome) -> Vec<String> {
    outcome
        .events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .map(|event| event.author.clone())
        .collect()
}

#[tokio::test]
async fn the_turn_goes_to_the_decided_route_and_the_decision_is_recorded() {
    let router_model = Arc::new(ScriptedLlm::new("router", vec![decision("billing", 0.92)]));
    let (billing, billing_model) = answering("billing", "Refund issued.");
    let (technical, technical_model) = answering("technical", "Restart it.");
    let agent = DecisionAgent::builder("triage", router_model.clone())
        .instruction("Route the support request.")
        .route("billing", "Invoices, refunds and payment problems", billing)
        .route("technical", "Errors, outages and how-to questions", technical)
        .output_key("triage")
        .build()
        .unwrap();
    let outcome = run(agent, None).await;
    assert!(outcome.events.iter().all(Result::is_ok));
    assert_eq!(authors(&outcome), ["triage", "billing"]);
    let routing = outcome.events[0].as_ref().unwrap();
    let text: String =
        routing.content().unwrap().parts.iter().filter_map(|part| part.text()).collect();
    assert_eq!(text, "[Decision: billing (0.92)] the customer was charged twice");
    assert_eq!(
        outcome.state.unwrap(),
        json!({
            "option": "billing",
            "rationale": "the customer was charged twice",
            "confidence": 0.92,
            "followed": true,
            "route": "billing",
        })
    );
    assert_eq!((billing_model.remaining(), technical_model.remaining()), (0, 1));
    let request = &router_model.requests()[0];
    assert!(request.tools.is_empty(), "no tool, so no forced tool_choice");
    let schema = request.config.as_ref().unwrap().response_schema.clone().unwrap();
    assert_eq!(schema["properties"]["choice"]["enum"], json!(["billing", "technical"]));
}

#[tokio::test]
async fn a_decision_below_the_floor_takes_the_fallback_or_fails_without_one() {
    let (billing, _) = answering("billing", "Refund issued.");
    let (general, _) = answering("general", "What is it about?");
    let agent = DecisionAgent::builder(
        "triage",
        Arc::new(ScriptedLlm::new("router", vec![decision("billing", 0.4)])),
    )
    .route("billing", "Invoices, refunds and payment problems", billing.clone())
    .fallback(general)
    .min_confidence(0.6)
    .output_key("triage")
    .build()
    .unwrap();
    let outcome = run(agent, None).await;
    assert_eq!(authors(&outcome), ["triage", "general"]);
    assert_eq!(outcome.state.unwrap()["followed"], json!(false));

    let strict = DecisionAgent::builder(
        "triage",
        Arc::new(ScriptedLlm::new("router", vec![decision("billing", 0.4)])),
    )
    .route("billing", "Invoices, refunds and payment problems", billing)
    .min_confidence(0.6)
    .build()
    .unwrap();
    let outcome = run(strict, None).await;
    let error = outcome.events.iter().find_map(|event| event.as_ref().err()).expect("refused");
    assert_eq!(error.code, "decision.low_confidence");
}

#[tokio::test]
async fn the_decision_counts_against_the_run_budget() {
    let (billing, billing_model) = answering("billing", "Refund issued.");
    let agent = DecisionAgent::builder(
        "triage",
        Arc::new(ScriptedLlm::new("router", vec![decision("billing", 0.9)])),
    )
    .route("billing", "Invoices, refunds and payment problems", billing)
    .build()
    .unwrap();
    // The decision is the run's one allowed model call, so the routed agent's call is refused.
    let outcome = run(agent, Some(RunBudget::new().max_model_calls(1))).await;
    let error = outcome.events.iter().find_map(|event| event.as_ref().err()).expect("stops");
    assert_eq!(error.category, ErrorCategory::ResourceExhausted, "{error}");
    assert_eq!(billing_model.remaining(), 1, "the routed agent's call was refused");
}

#[derive(Debug, PartialEq)]
enum Route {
    Billing,
    Technical,
}

impl Choice for Route {
    fn options() -> Vec<ChoiceOption> {
        vec![
            ChoiceOption::new("billing", "Invoices, refunds and payment problems"),
            ChoiceOption::new("technical", "Errors, outages and how-to questions"),
        ]
    }

    fn from_option(name: &str) -> Option<Self> {
        match name {
            "billing" => Some(Self::Billing),
            "technical" => Some(Self::Technical),
            _ => None,
        }
    }
}

#[tokio::test]
async fn routes_come_from_a_choice_type() {
    let (billing, _) = answering("billing", "Refund issued.");
    let (technical, _) = answering("technical", "Restart it.");
    let agent = DecisionAgent::builder(
        "triage",
        Arc::new(ScriptedLlm::new("router", vec![decision("technical", 0.8)])) as Arc<dyn Llm>,
    )
    .routes::<Route>(|route| match route {
        Route::Billing => billing.clone(),
        Route::Technical => technical.clone(),
    })
    .build()
    .unwrap();
    let names: Vec<&str> = agent.options().iter().map(|option| option.name.as_str()).collect();
    assert_eq!(names, ["billing", "technical"]);
    let outcome = run(agent, None).await;
    assert_eq!(authors(&outcome), ["triage", "technical"]);
}

#[test]
fn a_router_needs_distinct_routes_and_a_floor_within_range() {
    let model: Arc<dyn Llm> = Arc::new(ScriptedLlm::new("router", Vec::<ScriptedResponse>::new()));
    let (agent, _) = answering("billing", "x");
    assert!(DecisionAgent::builder("triage", model.clone()).build().is_err());
    let twice = DecisionAgent::builder("triage", model.clone())
        .route("billing", "a", agent.clone())
        .route("billing", "b", agent.clone());
    assert!(twice.build().is_err());
    let floor =
        DecisionAgent::builder("triage", model).route("billing", "a", agent).min_confidence(1.5);
    assert!(floor.build().is_err());
}

/// The request bodies a live client sent.
type Bodies = Arc<std::sync::Mutex<Vec<Value>>>;

/// A Claude client that records each request body it sends; `None` without
/// `ANTHROPIC_API_KEY`.
fn live_claude() -> Option<(Arc<dyn Llm>, Bodies)> {
    let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    let bodies: Bodies = Arc::default();
    let seen = bodies.clone();
    let adapter: adk_model::anthropic::RequestAdapter = Arc::new(move |body, _headers| {
        seen.lock().unwrap().push(body.clone());
        Ok(())
    });
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new(key, "claude-sonnet-5-5"),
    )
    .unwrap()
    .with_request_adapter(adapter);
    Some((Arc::new(model), bodies))
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn live_claude_decides_a_typed_route_with_structured_output() {
    let Some((model, bodies)) = live_claude() else { return };
    let decision =
        adk_core::decide::<Route>(model, "My card was charged twice for the same invoice.")
            .await
            .unwrap();
    println!(
        "decide: {:?} {} {:.2} {}",
        decision.choice, decision.option, decision.confidence, decision.rationale
    );
    assert_eq!(decision.choice, Route::Billing);
    assert!(decision.confidence > 0.5 && !decision.rationale.is_empty(), "{decision:?}");
    let body = bodies.lock().unwrap()[0].clone();
    println!("output_config: {}", body["output_config"]);
    assert_eq!(body["output_config"]["format"]["type"], "json_schema", "{body}");
    assert_eq!(
        body["output_config"]["format"]["schema"]["properties"]["choice"]["enum"],
        json!(["billing", "technical"])
    );
    assert!(body.get("tool_choice").is_none() && body.get("tools").is_none(), "{body}");
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn live_claude_routes_a_decision_agent_under_a_budget() {
    let Some((model, bodies)) = live_claude() else { return };
    let (billing, _) = answering("billing", "Refund issued.");
    let (technical, technical_model) = answering("technical", "Clear the cache and restart.");
    let agent = DecisionAgent::builder("triage", model)
        .instruction("Route the support request to the team that should answer it.")
        .routes::<Route>(|route| match route {
            Route::Billing => billing.clone(),
            Route::Technical => technical.clone(),
        })
        .min_confidence(0.5)
        .output_key("triage")
        .build()
        .unwrap();
    let outcome = run_with_input(
        agent,
        Some(RunBudget::new().max_model_calls(2)),
        "The app crashes on startup with error E42 since the last update.",
    )
    .await;
    for event in outcome.events.iter().flatten() {
        let text: String = event
            .content()
            .map(|content| content.parts.iter().filter_map(|part| part.text()).collect())
            .unwrap_or_default();
        println!("{}: {text}", event.author);
    }
    println!("state triage = {}", outcome.state.clone().unwrap_or_default());
    assert!(outcome.events.iter().all(Result::is_ok));
    assert_eq!(authors(&outcome), ["triage", "technical"]);
    assert_eq!(technical_model.remaining(), 0);
    let state = outcome.state.unwrap();
    assert_eq!(state["option"], "technical");
    assert_eq!(state["followed"], true);
    assert_eq!(bodies.lock().unwrap().len(), 1, "one decision call");
}
