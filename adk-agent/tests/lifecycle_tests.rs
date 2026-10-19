//! `LlmAgent` and `ParallelAgent` lifecycle stages under a `Runner`.
//!
//! - The after-agent stage runs on every exit — an answer, a transfer, a turn-ending tool
//!   result, a cancellation — except a held call, which resumes the agent in a later run.
//! - A failed model call or response stream reaches the run's `on_model_error` hooks, and a
//!   response they return answers the turn; a budget refusal does not reach them.
//! - A before-agent callback's answer is stored under `output_key`.
//! - A `ParallelAgent` branch that escalates stops the other branches.
//! - `Agent::close` closes an agent's toolsets and its sub-agents.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_agent::{LlmAgentBuilder, ParallelAgent};
use adk_core::{
    AdkError, AfterAgentCallback, Agent, Content, Event, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, ReadonlyContext, Result, RunBudget, RunConfig, SessionId, Tool,
    ToolContext, Toolset, UserId,
};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const SESSION: &str = "session";

/// One scripted model response: a whole response, a failed call, or a stream that fails
/// after a partial delta.
enum Turn {
    Respond(Box<LlmResponse>),
    FailCall,
    FailStream,
}

/// Plays scripted turns in order and answers `done` once they run out.
struct ScriptedModel {
    turns: Mutex<Vec<Turn>>,
    calls: AtomicUsize,
}

impl ScriptedModel {
    fn new(mut turns: Vec<Turn>) -> Arc<Self> {
        turns.reverse();
        Arc::new(Self { turns: Mutex::new(turns), calls: AtomicUsize::new(0) })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self
            .turns
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Turn::Respond(Box::new(text("done"))))
        {
            Turn::Respond(response) => Ok(Box::pin(futures::stream::iter([Ok(*response)]))),
            Turn::FailCall => Err(AdkError::model("upstream unavailable")),
            Turn::FailStream => {
                let partial = LlmResponse {
                    content: Some(Content::new("model").with_text("par")),
                    partial: true,
                    ..Default::default()
                };
                Ok(Box::pin(futures::stream::iter([
                    Ok(partial),
                    Err(AdkError::model("stream reset by upstream")),
                ])))
            }
        }
    }
}

fn text(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        turn_complete: true,
        ..Default::default()
    }
}

fn call(name: &str, args: Value) -> Turn {
    Turn::Respond(Box::new(LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args,
                id: Some(format!("{name}-1")),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }))
}

fn counting_after(counter: Arc<AtomicUsize>) -> AfterAgentCallback {
    Box::new(move |_ctx| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    })
}

/// A tool that sets `update` on its actions.
fn acting_tool(name: &str, update: fn(&mut adk_core::EventActions)) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(name, "acts", move |ctx: Arc<dyn ToolContext>, _args| async move {
        let mut actions = ctx.actions();
        update(&mut actions);
        ctx.set_actions(actions);
        Ok(json!({"status": "ok"}))
    }))
}

fn slow_tool(name: &str, finished: Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(name, "slow", move |_ctx, _args| {
        let finished = finished.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"status": "found"}))
        }
    }))
}

struct Harness {
    sessions: Arc<InMemorySessionService>,
    runner: Runner,
}

impl Harness {
    async fn new(agent: Arc<dyn Agent>, plugins: Option<Plugin>) -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: "app".to_string(),
                user_id: "user".to_string(),
                session_id: Some(SESSION.to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        let builder = Runner::builder()
            .app_name("app")
            .agent(agent)
            .session_service(sessions.clone() as Arc<dyn SessionService>);
        let runner = match plugins {
            Some(plugin) => builder.plugin_manager(Arc::new(PluginManager::new(vec![plugin]))),
            None => builder,
        }
        .build()
        .unwrap();
        Self { sessions, runner }
    }

    async fn run(&self, config: Option<RunConfig>) -> (Vec<Event>, Option<AdkError>) {
        let mut stream = self
            .runner
            .run_with_config(
                UserId::new("user").unwrap(),
                SessionId::new(SESSION).unwrap(),
                Content::new("user").with_text("hello"),
                config,
            )
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(error) => return (events, Some(error)),
            }
        }
        (events, None)
    }

    async fn state(&self) -> HashMap<String, Value> {
        self.sessions
            .get(GetRequest {
                app_name: "app".to_string(),
                user_id: "user".to_string(),
                session_id: SESSION.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .state()
            .all()
    }
}

fn final_text(events: &[Event]) -> String {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect()
}

#[tokio::test]
async fn after_agent_runs_when_the_agent_transfers() {
    let (coordinator_after, worker_after) =
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let worker = LlmAgentBuilder::new("worker")
        .model(ScriptedModel::new(vec![]))
        .after_callback(counting_after(worker_after.clone()))
        .build()
        .unwrap();
    let coordinator = LlmAgentBuilder::new("coordinator")
        .model(ScriptedModel::new(vec![call("transfer_to_agent", json!({"agent_name": "worker"}))]))
        .after_callback(counting_after(coordinator_after.clone()))
        .sub_agent(Arc::new(worker))
        .build()
        .unwrap();
    let harness = Harness::new(Arc::new(coordinator), None).await;
    let (_, error) = harness.run(None).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        (coordinator_after.load(Ordering::SeqCst), worker_after.load(Ordering::SeqCst)),
        (1, 1)
    );
}

#[tokio::test]
async fn after_agent_runs_after_a_turn_ending_tool_result() {
    let after = Arc::new(AtomicUsize::new(0));
    let desk = LlmAgentBuilder::new("desk")
        .model(ScriptedModel::new(vec![call("file_ticket", json!({}))]))
        .tool(acting_tool("file_ticket", |actions| actions.skip_summarization = true))
        .after_callback(counting_after(after.clone()))
        .build()
        .unwrap();
    let harness = Harness::new(Arc::new(desk), None).await;
    let (_, error) = harness.run(None).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(after.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn after_agent_runs_when_the_run_is_cancelled() {
    let (after, finished) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let desk = LlmAgentBuilder::new("desk")
        .model(ScriptedModel::new(vec![call("slow_lookup", json!({}))]))
        .tool(slow_tool("slow_lookup", finished.clone()))
        .after_callback(counting_after(after.clone()))
        .build()
        .unwrap();
    let harness = Arc::new(Harness::new(Arc::new(desk), None).await);
    let running = harness.clone();
    let run = tokio::spawn(async move { running.run(None).await });
    for _ in 0..100 {
        if !harness.runner.active_runs().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(harness.runner.interrupt(SESSION));
    run.await.unwrap();
    for _ in 0..100 {
        if after.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!((after.load(Ordering::SeqCst), finished.load(Ordering::SeqCst)), (1, 0));
}

#[tokio::test]
async fn after_agent_waits_for_a_held_call() {
    let after = Arc::new(AtomicUsize::new(0));
    let desk = LlmAgentBuilder::new("desk")
        .model(ScriptedModel::new(vec![call("refund", json!({"amount": 5}))]))
        .tool(acting_tool("refund", |_| {}))
        .require_tool_confirmation("refund")
        .after_callback(counting_after(after.clone()))
        .build()
        .unwrap();
    let harness = Harness::new(Arc::new(desk), None).await;
    let (events, error) = harness.run(None).await;
    assert!(error.is_none(), "{error:?}");
    assert!(events.iter().any(|event| event.actions.tool_confirmation.is_some()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(after.load(Ordering::SeqCst), 0);
}

/// A plugin whose `on_model_error` records each error and answers `fallback answer`.
fn fallback_plugin(errors: Arc<Mutex<Vec<String>>>) -> Plugin {
    Plugin::new(PluginConfig {
        name: "fallback".to_string(),
        on_model_error: Some(Box::new(move |_ctx, _request, error| {
            let errors = errors.clone();
            Box::pin(async move {
                errors.lock().unwrap().push(error);
                Ok(Some(text("fallback answer")))
            })
        })),
        ..Default::default()
    })
}

#[tokio::test]
async fn on_model_error_answers_a_failed_call_or_stream() {
    for turn in [Turn::FailCall, Turn::FailStream] {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let desk = LlmAgentBuilder::new("desk")
            .model(ScriptedModel::new(vec![turn]))
            .output_key("answer")
            .build()
            .unwrap();
        let harness = Harness::new(Arc::new(desk), Some(fallback_plugin(errors.clone()))).await;
        let (events, error) = harness.run(None).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert_eq!(final_text(&events), "fallback answer");
        assert_eq!(harness.state().await.get("answer"), Some(&json!("fallback answer")));
    }
}

#[tokio::test]
async fn on_model_error_does_not_answer_a_budget_refusal() {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let desk = LlmAgentBuilder::new("desk")
        .model(ScriptedModel::new(vec![call("lookup", json!({}))]))
        .tool(acting_tool("lookup", |_| {}))
        .build()
        .unwrap();
    let harness = Harness::new(Arc::new(desk), Some(fallback_plugin(errors.clone()))).await;
    let config =
        RunConfig { budget: Some(RunBudget::new().max_model_calls(1)), ..RunConfig::default() };
    let (_, error) = harness.run(Some(config)).await;
    assert_eq!(error.map(|error| error.code), Some("budget.model_calls"));
    assert!(errors.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_before_agent_answer_is_stored_under_output_key() {
    let model = ScriptedModel::new(vec![]);
    let desk = LlmAgentBuilder::new("desk")
        .model(model.clone())
        .output_key("answer")
        .before_callback(Box::new(|_ctx| {
            Box::pin(async move { Ok(Some(Content::new("model").with_text("cached: 42"))) })
        }))
        .build()
        .unwrap();
    let harness = Harness::new(Arc::new(desk), None).await;
    let (_, error) = harness.run(None).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.state().await.get("answer"), Some(&json!("cached: 42")));
}

#[tokio::test]
async fn a_parallel_branch_that_escalates_stops_the_others() {
    let finished = Arc::new(AtomicUsize::new(0));
    let escalator = LlmAgentBuilder::new("escalator")
        .model(ScriptedModel::new(vec![call("stop_now", json!({}))]))
        .tool(acting_tool("stop_now", |actions| actions.escalate = true))
        .build()
        .unwrap();
    let slowpoke = LlmAgentBuilder::new("slowpoke")
        .model(ScriptedModel::new(vec![call("slow_scan", json!({}))]))
        .tool(slow_tool("slow_scan", finished.clone()))
        .build()
        .unwrap();
    let fanout = ParallelAgent::new("fanout", vec![Arc::new(escalator), Arc::new(slowpoke)]);
    let harness = Harness::new(Arc::new(fanout), None).await;
    let started = Instant::now();
    let (events, error) = harness.run(None).await;
    assert!(error.is_none(), "{error:?}");
    assert!(events.iter().any(|event| event.actions.escalate));
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert_eq!(finished.load(Ordering::SeqCst), 0);
}

struct ClosingToolset(Arc<AtomicUsize>);

#[async_trait]
impl Toolset for ClosingToolset {
    fn name(&self) -> &str {
        "closing"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(Vec::new())
    }

    async fn close(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn close_closes_the_toolsets_of_the_agent_and_its_sub_agents() {
    let (own, child) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let worker = LlmAgentBuilder::new("worker")
        .model(ScriptedModel::new(vec![]))
        .toolset(Arc::new(ClosingToolset(child.clone())))
        .disallow_transfer_to_parent(true)
        .build()
        .unwrap();
    assert!(!worker.allows_transfer_to_parent() && worker.allows_transfer_to_peers());
    let desk = LlmAgentBuilder::new("desk")
        .model(ScriptedModel::new(vec![]))
        .toolset(Arc::new(ClosingToolset(own.clone())))
        .sub_agent(Arc::new(worker))
        .build()
        .unwrap();
    desk.close().await;
    assert_eq!((own.load(Ordering::SeqCst), child.load(Ordering::SeqCst)), (1, 1));
}
