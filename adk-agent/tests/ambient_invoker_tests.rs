//! A trigger must be able to drive a real `Runner` without hand-written wiring.
//!
//! Two defects motivated `AmbientAgent::with_invoker`:
//!
//! 1. Every ambient caller wrote the same closure — build `Content`, invent a session id, call
//!    `Runner::run` — and `AmbientAgent::start` refuses to run without it, so the shipped
//!    `ambient_cron_agent` example failed at `start()`.
//! 2. `Runner::run` resolves an *existing* session and yields `session.not_found` through the
//!    stream when there is none. A cron tick has no opportunity to register one first, so the
//!    obvious wiring fails at the first trigger — and fails inside the stream, not at the call
//!    site.

#![cfg(feature = "ambient")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::ambient::{
    AmbientAgent, EventSource, RunnerTriggerConfig, TriggerEvent, TriggerSessionPolicy,
};
use adk_core::{Agent, AgentInvoker, Content, Event, EventStream, InvocationContext, Result};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::time::timeout;

const APP: &str = "ambient-invoker-tests";

/// Records how many times it was run; these tests assert dispatch, not model behaviour.
///
/// Yields one event per run so the ambient output channel carries something observable.
/// `AmbientAgent::take_output` leaves a sender in the struct, so a receiver never sees the
/// channel close while the agent is alive — these tests read a fixed count rather than draining.
#[derive(Debug, Default)]
struct RecordingAgent {
    runs: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct SlowRecordingAgent {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for SlowRecordingAgent {
    fn name(&self) -> &str {
        "slow-recorder"
    }

    fn description(&self) -> &str {
        "records concurrent execution"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let active = Arc::clone(&self.active);
        let max_active = Arc::clone(&self.max_active);
        Ok(Box::pin(async_stream::stream! {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            let mut event = Event::new("slow-inv");
            event.author = "slow-recorder".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("done"));
            yield Ok(event);
        }))
    }
}

#[derive(Debug)]
struct DifferentAgent;

#[async_trait]
impl Agent for DifferentAgent {
    fn name(&self) -> &str {
        "different-agent"
    }

    fn description(&self) -> &str {
        "must be replaced by the invoker's executable root"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        unreachable!("the ambient wrapper must use the runner's agent")
    }
}

#[async_trait]
impl Agent for RecordingAgent {
    fn name(&self) -> &str {
        "recorder"
    }
    fn description(&self) -> &str {
        "records that it ran"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let mut event = Event::new("inv");
        event.author = "recorder".to_string();
        event.llm_response.content = Some(Content::new("model").with_text("done"));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

/// Emits `count` events immediately, then ends so the ambient loop drains and stops.
struct BurstSource {
    count: usize,
}

#[async_trait]
impl EventSource for BurstSource {
    fn name(&self) -> &str {
        "burst"
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>> {
        let events: Vec<TriggerEvent> = (0..self.count)
            .map(|index| TriggerEvent {
                source: "burst".to_string(),
                payload: serde_json::json!({ "index": index }),
                principal: None,
            })
            .collect();
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A runner over an in-memory session service, plus the agent's run counter.
fn runner_with_counter() -> (Arc<Runner>, Arc<AtomicUsize>, Arc<InMemorySessionService>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let agent: Arc<dyn Agent> = Arc::new(RecordingAgent { runs: Arc::clone(&runs) });
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(agent)
            .session_service(sessions.clone() as Arc<dyn SessionService>)
            .build()
            .expect("runner builds"),
    );
    (runner, runs, sessions)
}

/// Reads exactly `count` produced events, failing rather than hanging if they do not arrive.
async fn drain(outputs: &mut tokio::sync::mpsc::Receiver<Result<Event>>, count: usize) {
    for index in 0..count {
        timeout(Duration::from_secs(5), outputs.recv())
            .await
            .unwrap_or_else(|_| panic!("produced event {index} should arrive"))
            .unwrap_or_else(|| panic!("output channel closed before event {index}"))
            .unwrap_or_else(|error| panic!("event {index} failed: {error}"));
    }
}

#[tokio::test]
async fn invoke_creates_a_session_that_does_not_exist_yet() {
    let (runner, runs, sessions) = runner_with_counter();

    // No session was registered. Runner::run alone would yield `session.not_found`.
    let mut events = runner
        .invoke("system", "fresh-session", Content::new("user").with_text("go"))
        .await
        .expect("invoke should create the session rather than fail");
    while futures::StreamExt::next(&mut events).await.is_some() {}

    assert_eq!(runs.load(Ordering::SeqCst), 1, "the agent should have run");
    assert!(
        sessions
            .get(adk_session::GetRequest {
                app_name: APP.to_string(),
                user_id: "system".to_string(),
                session_id: "fresh-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .is_ok(),
        "the session should exist after the invocation"
    );
}

#[tokio::test]
async fn invoke_reuses_a_session_that_already_exists() {
    let (runner, runs, sessions) = runner_with_counter();
    sessions
        .create(adk_session::CreateRequest {
            app_name: APP.to_string(),
            user_id: "system".to_string(),
            session_id: Some("preexisting".to_string()),
            state: std::collections::HashMap::new(),
        })
        .await
        .expect("seed session");

    for _ in 0..2 {
        let mut events = runner
            .invoke("system", "preexisting", Content::new("user").with_text("go"))
            .await
            .expect("invoke");
        while futures::StreamExt::next(&mut events).await.is_some() {}
    }

    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let sessions_for_user = sessions
        .list(adk_session::ListRequest {
            app_name: APP.to_string(),
            user_id: "system".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list");
    assert_eq!(sessions_for_user.len(), 1, "reusing a session must not create a second one");
}

#[tokio::test]
async fn with_invoker_drives_the_agent_on_every_trigger() {
    let (runner, runs, _sessions) = runner_with_counter();
    let agent: Arc<dyn Agent> = Arc::new(RecordingAgent::default());

    let mut ambient = AmbientAgent::new(agent, Arc::new(BurstSource { count: 3 }))
        .with_invoker(runner, RunnerTriggerConfig::new("system"));

    let mut outputs = ambient.take_output(16);
    ambient.start().await.expect("start should not require a hand-written handler");

    drain(&mut outputs, 3).await;

    assert_eq!(
        runs.load(Ordering::SeqCst),
        3,
        "each trigger should invoke the agent through the runner"
    );
}

#[tokio::test]
async fn with_invoker_adopts_the_runners_agent_for_diagnostics() {
    let (runner, _runs, _sessions) = runner_with_counter();
    let ambient = AmbientAgent::new(Arc::new(DifferentAgent), Arc::new(BurstSource { count: 0 }))
        .with_invoker(runner, RunnerTriggerConfig::new("system"));
    let debug = format!("{ambient:?}");

    assert!(debug.contains("recorder"), "got {debug}");
    assert!(!debug.contains("different-agent"), "got {debug}");
}

#[tokio::test]
async fn per_trigger_sessions_keep_runs_isolated() {
    let (runner, _runs, sessions) = runner_with_counter();
    let agent: Arc<dyn Agent> = Arc::new(RecordingAgent::default());

    let mut ambient = AmbientAgent::new(agent, Arc::new(BurstSource { count: 3 })).with_invoker(
        runner,
        RunnerTriggerConfig::new("system").with_session_policy(TriggerSessionPolicy::PerTrigger),
    );

    let mut outputs = ambient.take_output(16);
    ambient.start().await.expect("start");
    drain(&mut outputs, 3).await;

    let created = sessions
        .list(adk_session::ListRequest {
            app_name: APP.to_string(),
            user_id: "system".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list");
    assert_eq!(
        created.len(),
        3,
        "PerTrigger gives each tick its own session so history cannot accumulate"
    );
}

#[tokio::test]
async fn a_shared_session_accumulates_across_triggers() {
    let (runner, _runs, sessions) = runner_with_counter();
    let agent: Arc<dyn Agent> = Arc::new(RecordingAgent::default());

    let mut ambient = AmbientAgent::new(agent, Arc::new(BurstSource { count: 3 })).with_invoker(
        runner,
        RunnerTriggerConfig::new("system")
            .with_session_policy(TriggerSessionPolicy::Shared("sweep".to_string())),
    );

    let mut outputs = ambient.take_output(16);
    ambient.start().await.expect("start");
    drain(&mut outputs, 3).await;

    let created = sessions
        .list(adk_session::ListRequest {
            app_name: APP.to_string(),
            user_id: "system".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list");
    assert_eq!(created.len(), 1, "Shared reuses one session for every tick");
    assert!(
        created[0].events().len() >= 6,
        "three serialized turns should retain their user and agent events"
    );
}

#[tokio::test]
async fn shared_session_invocations_are_serialized_until_their_streams_finish() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let agent: Arc<dyn Agent> =
        Arc::new(SlowRecordingAgent { active, max_active: Arc::clone(&max_active) });
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::clone(&agent))
            .session_service(sessions as Arc<dyn SessionService>)
            .build()
            .expect("runner builds"),
    );
    let mut ambient = AmbientAgent::new(agent, Arc::new(BurstSource { count: 3 })).with_invoker(
        runner,
        RunnerTriggerConfig::new("system")
            .with_session_policy(TriggerSessionPolicy::Shared("serialized".to_string())),
    );
    let mut outputs = ambient.take_output(16);

    ambient.start().await.expect("start");
    drain(&mut outputs, 3).await;

    assert_eq!(
        max_active.load(Ordering::SeqCst),
        1,
        "the same shared session must never execute overlapping turns"
    );
}

#[tokio::test]
async fn per_trigger_sessions_can_execute_concurrently() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let agent: Arc<dyn Agent> =
        Arc::new(SlowRecordingAgent { active, max_active: Arc::clone(&max_active) });
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::clone(&agent))
            .session_service(sessions as Arc<dyn SessionService>)
            .build()
            .expect("runner builds"),
    );
    let mut ambient = AmbientAgent::new(agent, Arc::new(BurstSource { count: 3 })).with_invoker(
        runner,
        RunnerTriggerConfig::new("system").with_session_policy(TriggerSessionPolicy::PerTrigger),
    );
    let mut outputs = ambient.take_output(16);

    ambient.start().await.expect("start");
    drain(&mut outputs, 3).await;

    assert!(
        max_active.load(Ordering::SeqCst) > 1,
        "locking one session must not serialize unrelated sessions"
    );
}
