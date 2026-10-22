//! Two of the realtime Hardening Backlog rows the harness voice surface is gated on (spec 3.1
//! §7.17), asserted only through APIs that exist on `main`, so each test compiles there and
//! fails on its assertion:
//!
//! | Row | On `main` |
//! |---|---|
//! | Realtime runners ignore an `AgentTool` delegate's hold | A held call is answered `{"error": …}` and nothing records it; a delegate's hold is dropped |
//! | Realtime usage is unmetered | A response's usage never reaches the run's `SpendLedger` |
//!
//! The third row, output guardrails on realtime text, adds a builder method that `main` lacks;
//! `integration_output_guardrails_tests.rs` covers it.

#![cfg(feature = "integration")]

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    ApprovalScope, DeclarativePolicy, EventActions, InMemoryApprovalStore, InMemorySpendLedger,
    RunConfig, SpendKey, SpendLedger, SpendLimits, SpendPeriod, Tool, ToolConfirmationRequest,
    ToolContext,
};
use adk_realtime::audio::AudioChunk;
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::session::ContextMutationOutcome;
use adk_realtime::{
    AudioFormat, ClientEvent, RealtimeConfig, RealtimeModel, RealtimeRunner, RealtimeSession,
    ServerEvent, ToolCall, ToolResponse,
};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::Stream;
use serde_json::{Value, json};

const APP: &str = "voice";
const USER: &str = "ana";
const SESSION: &str = "s1";

/// A session that emits a fixed list of provider events, then ends.
struct QueueSession {
    events: Mutex<VecDeque<ServerEvent>>,
}

#[async_trait]
impl RealtimeSession for QueueSession {
    fn session_id(&self) -> &str {
        "queue"
    }
    fn is_connected(&self) -> bool {
        true
    }
    async fn send_audio(&self, _audio: &AudioChunk) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn send_audio_base64(&self, _audio_base64: &str) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn send_text(&self, _text: &str) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn send_tool_response(&self, _response: ToolResponse) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn clear_audio(&self) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn create_response(&self) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn interrupt(&self) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn send_event(&self, _event: ClientEvent) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn next_event(&self) -> Option<adk_realtime::Result<ServerEvent>> {
        self.events.lock().unwrap().pop_front().map(Ok)
    }
    fn events(&self) -> Pin<Box<dyn Stream<Item = adk_realtime::Result<ServerEvent>> + Send + '_>> {
        Box::pin(futures::stream::empty())
    }
    async fn close(&self) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn mutate_context(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::Result<ContextMutationOutcome> {
        Ok(ContextMutationOutcome::Applied)
    }
}

/// A model whose sessions emit `events`; `gpt-realtime` names the OpenAI vendor.
struct QueueModel {
    events: Vec<ServerEvent>,
}

#[async_trait]
impl RealtimeModel for QueueModel {
    fn provider(&self) -> &str {
        "queue"
    }
    fn model_id(&self) -> &str {
        "gpt-realtime"
    }
    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn available_voices(&self) -> Vec<&str> {
        vec![]
    }
    async fn connect(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::Result<Box<dyn RealtimeSession>> {
        Ok(Box::new(QueueSession { events: Mutex::new(self.events.clone().into()) }))
    }
}

/// A `bash`-like tool that counts its executions.
struct Shell {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Shell {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "runs a command"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ran": true }))
    }
}

/// A tool whose delegate held a call, reporting it through the context's actions as
/// `AgentTool` does when its agent holds a call.
struct Delegating;

#[async_trait]
impl Tool for Delegating {
    fn name(&self) -> &str {
        "reviewer"
    }
    fn description(&self) -> &str {
        "delegates to a reviewer agent"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        ctx.set_actions(EventActions {
            tool_confirmation: Some(
                ToolConfirmationRequest::new("write_file", json!({ "path": "src/lib.rs" }))
                    .with_function_call_id("inner-1"),
            ),
            ..EventActions::default()
        });
        Ok(adk_core::confirmation_pending_response("the reviewer is waiting"))
    }
}

async fn sessions() -> Arc<InMemorySessionService> {
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
    sessions
}

/// The calls the session records as held by the integrated runner's agent, `realtime`.
async fn held_calls(sessions: &InMemorySessionService) -> Vec<adk_core::HeldToolCall> {
    let session = sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    adk_core::held_tool_calls(&session.events().all(), "realtime", "")
}

fn call(name: &str) -> ToolCall {
    ToolCall { call_id: "call-1".to_string(), name: name.to_string(), arguments: json!({}) }
}

#[tokio::test]
async fn a_held_call_is_answered_as_pending_and_recorded_for_resumption() {
    let sessions = sessions().await;
    let executions = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(Shell { executions: executions.clone() });
    let policy = DeclarativePolicy::builder().require_approval("bash", "needs a person").build();
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(QueueModel { events: vec![] }))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone())
        .adk_tool(tool.clone())
        .tool_governance(
            RunConfig::builder()
                .tool_policy(Arc::new(policy))
                .approval_store(Arc::new(InMemoryApprovalStore::new()))
                .build(),
        )
        .build()
        .unwrap();

    let result = runner.execute_tool_with_plugins_for_test(&tool, &call("bash")).await.unwrap();

    assert_eq!(executions.load(Ordering::SeqCst), 0, "a held call does not run");
    assert!(
        adk_core::is_confirmation_pending(&result),
        "answered as waiting, not failed: {result}"
    );
    let held = held_calls(&sessions).await;
    assert_eq!(held.len(), 1, "the hold is recorded where a decision can resume it");
    assert_eq!(held[0].name, "bash");
}

#[tokio::test]
async fn a_delegates_hold_holds_the_agent_tool_call() {
    let sessions = sessions().await;
    let tool: Arc<dyn Tool> = Arc::new(Delegating);
    let policy = DeclarativePolicy::builder().allow("reviewer").build();
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(QueueModel { events: vec![] }))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone())
        .adk_tool(tool.clone())
        .tool_governance(RunConfig::builder().tool_policy(Arc::new(policy)).build())
        .build()
        .unwrap();

    runner.execute_tool_with_plugins_for_test(&tool, &call("reviewer")).await.unwrap();

    let held = held_calls(&sessions).await;
    assert_eq!(held.len(), 1, "the delegate's hold is not dropped");
    assert_eq!(held[0].name, "reviewer", "the agent-tool call is what resumes");
    assert_eq!(held[0].request.tool_name, "write_file", "the request is the delegate's");
    assert_eq!(held[0].request.function_call_id.as_deref(), Some("call-1"));
}

/// A response that reports usage, as OpenAI's `response.done` does.
fn metered_response() -> Vec<ServerEvent> {
    vec![
        ServerEvent::ResponseCreated { event_id: "e1".to_string(), response: json!({}) },
        ServerEvent::ResponseDone {
            event_id: "e2".to_string(),
            response: json!({ "response": { "usage": {
                "input_tokens": 120, "output_tokens": 80, "total_tokens": 200
            } } }),
        },
    ]
}

#[tokio::test]
async fn the_integrated_runner_meters_realtime_usage_in_the_spend_ledger() {
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(QueueModel { events: metered_response() }))
        .identity(APP, USER, SESSION)
        .tool_governance(RunConfig::builder().spend_ledger(ledger.clone()).build())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    while let Some(event) = runner.next_event().await {
        event.unwrap();
    }

    let spent = ledger.spent(&SpendKey::org(APP).with_vendor("openai"), SpendPeriod::Day).await;
    assert!(spent.unwrap() > 0, "the response is committed under the run's spend key");
}

#[tokio::test]
async fn the_plain_runner_meters_realtime_usage_in_the_spend_ledger() {
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let runner = RealtimeRunner::builder()
        .model(Arc::new(QueueModel { events: metered_response() }))
        .tool_governance(
            RunConfig::builder().spend_ledger(ledger.clone()).build(),
            ApprovalScope::new(APP, USER, SESSION),
        )
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    while let Some(event) = runner.next_event().await {
        event.unwrap();
    }

    let spent = ledger.spent(&SpendKey::org(APP).with_vendor("openai"), SpendPeriod::Day).await;
    assert!(spent.unwrap() > 0, "the response is committed under the run's spend key");
}
