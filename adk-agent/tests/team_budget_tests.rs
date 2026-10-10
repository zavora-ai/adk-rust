//! Team budgets count non-partial events only, bound the receipts a long-lived
//! team retains, and stop members before the call that would start past a limit.

use adk_agent::{
    BlackboardPolicy, BlackboardSchedule, BlackboardSpec, RelationshipKind, TeamBudget,
    TeamExecutionStatus, TeamMemberSpec, TeamPolicy, TeamRelationship, TeamSpec,
};
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, Event, EventStream, InvocationContext, Result,
    UsageMetadata,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

/// Streams `partial_chunks` partial events, each repeating usage the way Gemini
/// chunks do, then one final event. Calls no model through a budget meter, so
/// the team counts its usage from the events.
struct StreamingSpeaker {
    name: String,
    partial_chunks: usize,
    cost: Option<f64>,
}

#[async_trait]
impl Agent for StreamingSpeaker {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "streams partial chunks"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let name = self.name.clone();
        let partial_chunks = self.partial_chunks;
        let cost = self.cost;
        Ok(Box::pin(async_stream::stream! {
            let usage = UsageMetadata { total_token_count: 100, cost, ..Default::default() };
            for _ in 0..partial_chunks {
                let mut event = Event::new(&invocation_id);
                event.author = name.clone();
                event.llm_response.partial = true;
                event.llm_response.usage_metadata = Some(usage.clone());
                event.llm_response.content = Some(Content::new("model").with_text("."));
                yield Ok(event);
            }
            let mut event = Event::new(&invocation_id);
            event.author = name.clone();
            event.llm_response.turn_complete = true;
            event.llm_response.usage_metadata = Some(usage);
            event.llm_response.model = Some("remote-model".to_string());
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            yield Ok(event);
        }))
    }
}

async fn runner_for(agent: Arc<dyn Agent>) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "team-app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    Runner::builder().app_name("team-app").agent(agent).session_service(sessions).build().unwrap()
}

async fn run(runner: &Runner) -> Vec<std::result::Result<Event, AdkError>> {
    runner
        .run_str("user", "session", Content::new("user").with_text("go"))
        .await
        .unwrap()
        .collect()
        .await
}

fn speaker(name: &str, partial_chunks: usize, cost: Option<f64>) -> Arc<dyn Agent> {
    Arc::new(StreamingSpeaker { name: name.to_string(), partial_chunks, cost })
}

fn team(budget: TeamBudget, cost: Option<f64>) -> Arc<adk_agent::CompiledTeam> {
    Arc::new(
        TeamSpec {
            name: "remote_team".to_string(),
            description: String::new(),
            coordinator: "lead".to_string(),
            members: vec![TeamMemberSpec::new("lead"), TeamMemberSpec::new("helper")],
            relationships: vec![TeamRelationship::new("lead", "helper", RelationshipKind::Handoff)],
            policy: TeamPolicy { budget, ..TeamPolicy::default() },
        }
        .compile([speaker("lead", 40, cost), speaker("helper", 0, cost)])
        .unwrap(),
    )
}

#[tokio::test]
async fn partial_chunks_do_not_count_against_team_budgets() {
    // Forty partial chunks each repeating 100 tokens would read as 4,100 tokens and
    // 41 events; the call used 100 tokens in one event.
    let team = team(
        TeamBudget { max_events: Some(2), max_tokens: Some(150), ..TeamBudget::default() },
        Some(0.01),
    );
    let runner = runner_for(team.clone()).await;

    let results = run(&runner).await;

    assert!(results.iter().all(std::result::Result::is_ok));
    let receipt = team.execution_snapshots().pop().unwrap();
    assert_eq!(receipt.usage.events, 1);
    assert_eq!(receipt.usage.model_requests, 1);
    assert_eq!(receipt.usage.tokens, 100);
    assert_eq!(receipt.usage.cost_microusd, 10_000);
    assert_eq!(receipt.status, TeamExecutionStatus::Completed);
}

#[tokio::test]
async fn a_team_cost_cap_fails_closed_on_unpriced_usage() {
    let team =
        team(TeamBudget { max_cost_microusd: Some(1_000_000), ..TeamBudget::default() }, None);
    let runner = runner_for(team.clone()).await;

    let results = run(&runner).await;

    let error = results.last().unwrap().as_ref().unwrap_err();
    assert_eq!(error.category, ErrorCategory::ResourceExhausted);
    assert_eq!(error.code, "budget.cost_unknown");
    assert!(error.message.contains("cost unknown for model 'remote-model'"), "{error}");
    // The member's final event is kept; the stop event follows it.
    let kept = results[results.len() - 3].as_ref().unwrap();
    assert_eq!(kept.author, "lead");
    assert!(!kept.llm_response.partial);
    let receipt = team.execution_snapshots().pop().unwrap();
    assert_eq!(receipt.status, TeamExecutionStatus::BudgetExceeded);

    let allowed = self::team(
        TeamBudget {
            max_cost_microusd: Some(1_000_000),
            allow_unpriced_models: true,
            ..TeamBudget::default()
        },
        None,
    );
    let results = run(&runner_for(allowed).await).await;
    assert!(results.iter().all(std::result::Result::is_ok));
}

/// Finished invocations no longer accumulate in the team runtime (D8).
#[tokio::test]
async fn finished_invocation_receipts_are_bounded() {
    let team = team(TeamBudget::default(), Some(0.0));
    let runner = runner_for(team.clone()).await;

    let mut last_invocation = String::new();
    for _ in 0..70 {
        let results = run(&runner).await;
        last_invocation = results[0].as_ref().unwrap().invocation_id.clone();
    }

    let receipts = team.execution_snapshots();
    assert_eq!(receipts.len(), 64);
    assert!(receipts.iter().all(|receipt| receipt.status == TeamExecutionStatus::Completed));
    assert!(team.execution_snapshot(&last_invocation).is_some());
}

#[tokio::test]
async fn the_default_blackboard_budget_survives_streaming_speakers() {
    // Four rounds of two speakers streaming 50 partial chunks each is 400 partial
    // events: three times the default 128-event budget when chunks counted.
    let spec = BlackboardSpec {
        name: "blackboard".to_string(),
        description: String::new(),
        members: vec!["first".to_string(), "second".to_string()],
        schedule: BlackboardSchedule::RoundRobin,
        transitions: Vec::new(),
        policy: BlackboardPolicy::default(),
    };
    assert_eq!(spec.policy.budget.max_events, Some(128));
    let blackboard = Arc::new(
        spec.compile([speaker("first", 50, Some(0.0)), speaker("second", 50, Some(0.0))]).unwrap(),
    );
    let runner = runner_for(blackboard.clone()).await;

    let results = run(&runner).await;

    assert!(results.iter().all(std::result::Result::is_ok));
    let finals = results
        .iter()
        .filter(|result| result.as_ref().is_ok_and(|event| !event.llm_response.partial))
        .count();
    assert_eq!(finals, 8);
}
