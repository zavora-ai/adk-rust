//! `RealtimeAgent` under `Runner` records a non-idempotent call in the run's action ledger
//! and counts every call against the run's tool-call budget, as `LlmAgent` does: a call the
//! provider issues twice runs once, and a call past the budget is answered as not run and
//! ends the run.

#![cfg(feature = "scripted")]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    ActionLedger, ActionOutcome, Content, Event, InMemoryActionLedger, Part, RunBudget, Tool,
    ToolContext, ToolEffect,
};
use adk_realtime::RealtimeAgent;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "voice-app";
const USER: &str = "alice";
const SESSION: &str = "s1";

/// A non-idempotent tool that counts its executions and, when `hangs`, never returns.
struct Refund {
    executions: Arc<AtomicUsize>,
    hangs: bool,
}

#[async_trait]
impl Tool for Refund {
    fn name(&self) -> &str {
        "refund"
    }
    fn description(&self) -> &str {
        "refunds the caller's last order"
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        if self.hangs {
            std::future::pending::<()>().await;
        }
        Ok(json!({ "refunded": true }))
    }
}

/// Runs a voice agent whose provider issues `script` once the session connects, under a
/// runner with `ledger` and `budget`; returns the run's events, errors included.
async fn run(
    script: Vec<adk_realtime::ServerEvent>,
    tool: Refund,
    tool_timeout: Duration,
    ledger: Option<Arc<dyn ActionLedger>>,
    budget: Option<RunBudget>,
) -> Vec<adk_core::Result<Event>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let model = ScriptedRealtimeModel::new(vec![ScriptedReply::new(script).on_connect()])
        .with_idle_timeout(Duration::from_millis(100));
    let agent = RealtimeAgent::builder("voice")
        .model(Arc::new(model))
        .tool(Arc::new(tool))
        .tool_timeout(tool_timeout)
        .build()
        .unwrap();
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>);
    if let Some(ledger) = ledger {
        builder = builder.action_ledger(ledger);
    }
    if let Some(budget) = budget {
        builder = builder.budget(budget);
    }
    let runner = builder.build().unwrap();
    runner.run_str(USER, SESSION, Content::new("user")).await.unwrap().collect().await
}

/// The responses the run recorded for tool calls, in order.
fn tool_responses(events: &[adk_core::Result<Event>]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

/// A provider that issues the same call twice — a redelivery after a reconnect, say — gets
/// one execution; the second is answered from the ledger.
#[tokio::test]
async fn a_repeated_non_idempotent_call_runs_once() {
    let executions = Arc::new(AtomicUsize::new(0));
    let ledger = Arc::new(InMemoryActionLedger::new());
    let mut script = scripted::calls("c1", "refund", json!({}));
    script.extend(scripted::calls("c1", "refund", json!({})));

    let events = run(
        script,
        Refund { executions: executions.clone(), hangs: false },
        Duration::from_secs(5),
        Some(ledger.clone() as Arc<dyn ActionLedger>),
        None,
    )
    .await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let responses = tool_responses(&events);
    assert_eq!(responses.len(), 2, "{responses:?}");
    assert_eq!(responses[0], json!({ "refunded": true }));
    assert_eq!(responses[1]["status"], "already_succeeded");

    let invocation = events[0].as_ref().unwrap().invocation_id.clone();
    let record = ledger
        .get(&format!("{APP}/{USER}/{SESSION}/{invocation}/c1"))
        .await
        .unwrap()
        .expect("the call is recorded under its idempotency key");
    assert!(matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })), "{record:?}");
}

/// A non-idempotent call that times out may have taken effect: its record stays begun, it is
/// answered as an unknown outcome, and a redelivery of it does not run again.
#[tokio::test]
async fn a_timed_out_call_is_not_run_again() {
    let executions = Arc::new(AtomicUsize::new(0));
    let ledger = Arc::new(InMemoryActionLedger::new());
    let mut script = scripted::calls("c1", "refund", json!({}));
    script.extend(scripted::calls("c1", "refund", json!({})));

    let events = run(
        script,
        Refund { executions: executions.clone(), hangs: true },
        Duration::from_millis(50),
        Some(ledger.clone() as Arc<dyn ActionLedger>),
        None,
    )
    .await;

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let responses = tool_responses(&events);
    assert_eq!(responses.len(), 2, "{responses:?}");
    assert!(adk_core::is_outcome_unknown(&responses[0]), "{responses:?}");
    assert!(adk_core::is_outcome_unknown(&responses[1]), "{responses:?}");
}

/// The run's tool-call budget bounds a voice agent: the call past it is answered as not run
/// and the run ends with the budget error.
#[tokio::test]
async fn the_run_budget_bounds_voice_tool_calls() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut script = scripted::calls("c1", "refund", json!({}));
    script.extend(scripted::calls("c2", "refund", json!({})));

    let events = run(
        script,
        Refund { executions: executions.clone(), hangs: false },
        Duration::from_secs(5),
        None,
        Some(RunBudget::new().max_tool_calls(1)),
    )
    .await;

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let responses = tool_responses(&events);
    assert_eq!(responses.len(), 2, "{responses:?}");
    assert!(
        responses[1]["error"].as_str().unwrap_or_default().starts_with("not run: "),
        "{responses:?}"
    );
    let error = events.iter().find_map(|event| event.as_ref().err()).expect("the run ends");
    assert!(error.code.starts_with("budget."), "{error:?}");
}
