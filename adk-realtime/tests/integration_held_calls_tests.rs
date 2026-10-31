//! Held calls on the integrated realtime runner (ROADMAP "Held calls and delegation":
//! realtime runners ignore an `AgentTool` delegate's hold).
//!
//! On `main`, a call the governed path held was answered to the model as a plain error and
//! vanished: nothing surfaced it, nothing recorded it, and a decision made later had nothing
//! to resume. A tool whose delegate held a call of its own was treated as a finished call.
//! These tests fail there: `ServerEvent::ToolConfirmationRequested` and
//! `resume_held_calls` do not exist, and `held_calls()` finds nothing.

#![cfg(all(feature = "integration", feature = "scripted"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    ApprovalScope, ApprovalStore, DeclarativePolicy, EventActions, InMemoryApprovalStore, Result,
    RunConfig, Tool, ToolApproval, ToolConfirmationRequest, ToolContext, ToolEffect,
};
use adk_realtime::config::ToolDefinition;
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::runner::{DEFAULT_REALTIME_AGENT_NAME, FnToolHandler, REALTIME_HANDLER_ORIGIN};
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::{ServerEvent, ToolCall};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "voice";
const USER: &str = "ana";
const SESSION: &str = "s1";

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
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ran": args["command"] }))
    }
}

/// A tool whose delegate held a call: it reports the delegate's request through the
/// context's actions, as `AgentTool` does when its agent holds a call.
struct Delegating;

#[async_trait]
impl Tool for Delegating {
    fn name(&self) -> &str {
        "reviewer"
    }
    fn description(&self) -> &str {
        "delegates to a reviewer agent"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let request = ToolConfirmationRequest::new("write_file", json!({ "path": "src/lib.rs" }))
            .with_function_call_id("inner-1");
        ctx.set_actions(EventActions {
            tool_confirmation: Some(request),
            ..EventActions::default()
        });
        Ok(adk_core::confirmation_pending_response("the reviewer is waiting for a confirmation"))
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

async fn events_of(sessions: &InMemorySessionService) -> Vec<adk_core::Event> {
    sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
}

fn scope() -> ApprovalScope {
    ApprovalScope::new(APP, USER, SESSION)
}

/// Drains the session until it ends, returning every event.
async fn drain(runner: &IntegratedRealtimeRunner) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Some(event) = runner.next_event().await {
        events.push(event.unwrap());
    }
    events
}

fn ask_before_bash(store: Arc<dyn ApprovalStore>) -> RunConfig {
    let policy = DeclarativePolicy::builder()
        .allow_read_only()
        .require_approval("bash", "commands need approval")
        .build();
    RunConfig::builder().tool_policy(Arc::new(policy)).approval_store(store).build()
}

async fn held_bash_runner(
    concurrent: bool,
) -> (
    IntegratedRealtimeRunner,
    ScriptedRealtimeModel,
    Arc<InMemorySessionService>,
    Arc<AtomicUsize>,
    Arc<InMemoryApprovalStore>,
) {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::response_created()]).on_connect(),
        ScriptedReply::new(scripted::calls("call-1", "bash", json!({ "command": "cargo test" })))
            .on_text(),
        ScriptedReply::new(scripted::spoken("That needs your approval.")).on_tool_output(),
        ScriptedReply::new(scripted::spoken("Two tests fail.")).on_tool_output(),
    ])
    .with_idle_timeout(Duration::from_millis(100));
    let sessions = sessions().await;
    let executions = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryApprovalStore::new());
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model.clone()))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone())
        .adk_tool(Arc::new(Shell { executions: executions.clone() }))
        .tool_governance(ask_before_bash(store.clone()))
        .concurrent_tool_dispatch(concurrent)
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    (runner, model, sessions, executions, store)
}

fn holds(events: &[ServerEvent]) -> Vec<(String, ToolConfirmationRequest)> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::ToolConfirmationRequested { call_id, request } => {
                Some((call_id.clone(), request.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_held_call_is_surfaced_recorded_and_resumed_once_decided() {
    let (runner, model, sessions, executions, store) = held_bash_runner(false).await;

    runner.send_text("what's failing in the build?").await.unwrap();
    let events = drain(&runner).await;

    // Surfaced as a realtime event, with the call's final arguments.
    let held = holds(&events);
    assert_eq!(held.len(), 1, "one hold surfaced: {events:?}");
    let (call_id, request) = &held[0];
    assert_eq!(call_id, "call-1");
    assert_eq!(request.tool_name, "bash");
    assert_eq!(request.args, json!({ "command": "cargo test" }));
    assert_eq!(executions.load(Ordering::SeqCst), 0, "a held call does not run");

    // The model was told the call is waiting, not that it failed.
    let outputs = model.log().tool_outputs();
    assert_eq!(outputs.len(), 1);
    assert!(adk_core::is_confirmation_pending(&outputs[0].output), "{:?}", outputs[0].output);

    // Recorded in the session under the runner's agent name, where an inbox finds it.
    let recorded = events_of(&sessions).await;
    assert!(
        recorded.iter().any(|event| {
            event.author == DEFAULT_REALTIME_AGENT_NAME && event.actions.tool_confirmation.is_some()
        }),
        "the hold is persisted: {recorded:?}"
    );
    let held_calls = runner.held_calls().await.unwrap();
    assert_eq!(held_calls.len(), 1);
    assert_eq!(held_calls[0].request.fingerprint(), request.fingerprint());
    // And in the approval store, keyed by the request's fingerprint.
    let pending = store.pending(&scope()).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].fingerprint, request.fingerprint());

    // Undecided: nothing resumes.
    assert!(runner.resume_held_calls().await.unwrap().is_empty());
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    // Decided through the store (the inbox's route): the call runs once, under its call ID.
    store.decide(&scope(), &request.fingerprint(), ToolApproval::approve()).await.unwrap();
    let resumed = runner.resume_held_calls().await.unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].call_id, "call-1");
    assert_eq!(resumed[0].result, json!({ "ran": "cargo test" }));
    assert_eq!(executions.load(Ordering::SeqCst), 1);

    // The provider still knew the call, so the result went back as its output and the
    // follow-up response was requested; the hold is cleared.
    let outputs = model.log().tool_outputs();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[1].call_id, "call-1");
    assert_eq!(outputs[1].output, json!({ "ran": "cargo test" }));
    assert!(runner.held_calls().await.unwrap().is_empty());
    assert!(store.pending(&scope()).await.unwrap().is_empty());

    // A second resume runs nothing: the decision was consumed by the call.
    assert!(runner.resume_held_calls().await.unwrap().is_empty());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_denied_call_is_answered_as_not_run() {
    let (runner, model, _sessions, executions, store) = held_bash_runner(false).await;
    runner.send_text("run the tests").await.unwrap();
    let events = drain(&runner).await;
    let (_, request) = holds(&events).remove(0);

    store
        .decide(&scope(), &request.fingerprint(), ToolApproval::deny().with_message("not now"))
        .await
        .unwrap();
    let resumed = runner.resume_held_calls().await.unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 0, "a denied call never runs");
    let answer = &resumed[0].result;
    assert!(answer.get("error").is_some(), "the model is told the call was not run: {answer}");
    assert!(!adk_core::is_confirmation_pending(answer));
    assert!(runner.held_calls().await.unwrap().is_empty(), "a denial clears the hold");
    assert_eq!(model.log().tool_outputs().len(), 2);
    let _ = runner.close().await;
}

#[tokio::test]
async fn concurrent_dispatch_surfaces_the_hold_while_events_keep_flowing() {
    let (runner, _model, _sessions, executions, _store) = held_bash_runner(true).await;
    runner.send_text("run the tests").await.unwrap();
    let events = drain(&runner).await;
    let held = holds(&events);
    assert_eq!(held.len(), 1, "{events:?}");
    assert_eq!(held[0].0, "call-1");
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    // The provider's own events were still read after the call was dispatched.
    assert!(events.iter().any(|event| matches!(event, ServerEvent::TranscriptDelta { .. })));
    let _ = runner.close().await;
}

/// Every hold the integrated runner records names the runner's agent: an ADK tool's with
/// the tool's own origin, a native handler's with the `realtime` origin of its definition
/// (#1210).
#[tokio::test]
async fn every_hold_names_the_runner_agent_and_the_tool_origin() {
    let agent = format!("urn:adk:agent:{APP}/{DEFAULT_REALTIME_AGENT_NAME}");
    let names_agent = |request: &ToolConfirmationRequest| {
        assert_eq!(request.acting.as_ref().map(|a| a.agent.id.as_str()), Some(agent.as_str()));
        assert_eq!(
            ToolConfirmationRequest::fingerprint_agent(&request.fingerprint()),
            Some(agent.as_str())
        );
    };

    let (runner, _model, _sessions, _executions, _store) = held_bash_runner(false).await;
    runner.send_text("what's failing in the build?").await.unwrap();
    let (_, request) = holds(&drain(&runner).await).remove(0);
    names_agent(&request);
    let shell = Shell { executions: Arc::new(AtomicUsize::new(0)) };
    assert_eq!(request.origin, Some(adk_core::ToolOrigin::of(&shell)));
    let _ = runner.close().await;

    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::response_created()]).on_connect(),
        ScriptedReply::new(scripted::calls("call-2", "refund", json!({ "order": "A-1" })))
            .on_text(),
        ScriptedReply::new(scripted::spoken("That needs your approval.")).on_tool_output(),
    ])
    .with_idle_timeout(Duration::from_millis(100));
    let store = Arc::new(InMemoryApprovalStore::new());
    let refund = ToolDefinition::new("refund").with_description("Refunds an order");
    let policy = DeclarativePolicy::builder().require_approval("refund", "refunds need a person");
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, USER, SESSION)
        .session_service(sessions().await)
        .tool(
            refund.clone(),
            FnToolHandler::new(|_call: &ToolCall| Ok(json!({ "refunded": true }))),
        )
        .tool_governance(
            RunConfig::builder()
                .tool_policy(Arc::new(policy.build()))
                .approval_store(store.clone())
                .build(),
        )
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("refund order A-1").await.unwrap();
    drain(&runner).await;
    let pending = store.pending(&scope()).await.unwrap();
    let [held] = pending.as_slice() else { panic!("the native call is held: {pending:?}") };
    names_agent(&held.request);
    assert_eq!(held.request.origin, Some(refund.handler_origin()));
    assert_eq!(
        held.request.origin.as_ref().map(|origin| origin.source.as_str()),
        Some(REALTIME_HANDLER_ORIGIN)
    );
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_delegates_hold_is_surfaced_not_ignored() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls("call-7", "reviewer", json!({ "task": "review" })))
            .on_text(),
    ])
    .with_idle_timeout(Duration::from_millis(100));
    let sessions = sessions().await;
    let policy = DeclarativePolicy::builder().allow("reviewer").build();
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model.clone()))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone())
        .adk_tool(Arc::new(Delegating))
        .tool_governance(RunConfig::builder().tool_policy(Arc::new(policy)).build())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("have the reviewer check the diff").await.unwrap();
    let events = drain(&runner).await;

    let held = holds(&events);
    assert_eq!(held.len(), 1, "the delegate's hold is surfaced: {events:?}");
    let (call_id, request) = &held[0];
    assert_eq!(call_id, "call-7", "held under the agent tool's own call");
    assert_eq!(request.tool_name, "write_file", "the request is the delegate's");
    assert_eq!(request.function_call_id.as_deref(), Some("call-7"), "under the agent tool's call");

    let outputs = model.log().tool_outputs();
    assert!(adk_core::is_confirmation_pending(&outputs[0].output));
    let held_calls = runner.held_calls().await.unwrap();
    assert_eq!(held_calls.len(), 1);
    assert_eq!(held_calls[0].name, "reviewer", "the agent-tool call is what resumes");
    let _ = runner.close().await;
}

#[tokio::test]
async fn typed_text_and_spoken_input_are_persisted_as_user_turns() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::spoken("Hello.")).on_text(),
        ScriptedReply::new(vec![scripted::input_transcript("yes please")]).on_audio(),
    ])
    .with_idle_timeout(Duration::from_millis(100));
    let sessions = sessions().await;
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("hi there").await.unwrap();
    runner.send_audio("AAAA").await.unwrap();
    drain(&runner).await;

    let recorded = events_of(&sessions).await;
    let user_turns: Vec<String> = recorded
        .iter()
        .filter(|event| event.author == "user")
        .filter_map(|event| event.content().and_then(|c| c.parts[0].text().map(str::to_string)))
        .collect();
    assert_eq!(user_turns, vec!["hi there".to_string(), "yes please".to_string()]);
    assert!(recorded.iter().any(|event| event.author == "model"), "{recorded:?}");
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_frozen_kill_switch_ends_the_dispatch_with_an_error() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls("call-1", "bash", json!({ "command": "ls" }))).on_text(),
    ])
    .with_idle_timeout(Duration::from_millis(100));
    let sessions = sessions().await;
    let control = adk_core::GovernanceControl::new();
    control.freeze("incident");
    let config = RunConfig::builder()
        .tool_policy(Arc::new(DeclarativePolicy::builder().allow("bash").build()))
        .governance(control)
        .build();
    let executions = Arc::new(AtomicUsize::new(0));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, USER, SESSION)
        .session_service(sessions)
        .adk_tool(Arc::new(Shell { executions: executions.clone() }))
        .tool_governance(config)
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("ls").await.unwrap();
    drain(&runner).await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(runner.held_calls().await.unwrap().is_empty());
    assert!(matches!(runner.resume_held_calls().await, Ok(resumed) if resumed.is_empty()));
    let _ = runner.close().await;
}

/// The function response part the session records for `call_id`, the last one written.
fn recorded_answer(events: &[adk_core::Event], call_id: &str) -> adk_core::Part {
    events
        .iter()
        .filter_map(|event| event.content())
        .flat_map(|content| content.parts.iter())
        .filter(|part| {
            matches!(part, adk_core::Part::FunctionResponse { id: Some(id), .. } if id == call_id)
        })
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("{call_id} is answered: {events:?}"))
}

fn annotated(
    name: &str,
    call_id: &str,
    response: Value,
    annotations: Option<Value>,
) -> adk_core::Part {
    adk_core::Part::FunctionResponse {
        function_response: adk_core::FunctionResponseData::new(name, response),
        id: Some(call_id.to_string()),
        annotations,
    }
}

/// The session records a held call `held` and its denial `refused`, as `LlmAgent` does.
#[tokio::test]
async fn the_recorded_answers_carry_their_status() {
    let (runner, _model, sessions, _executions, store) = held_bash_runner(false).await;
    runner.send_text("run the tests").await.unwrap();
    let (_, request) = holds(&drain(&runner).await).remove(0);
    assert_eq!(
        recorded_answer(&events_of(&sessions).await, "call-1"),
        annotated(
            "bash",
            "call-1",
            adk_core::confirmation_pending_response(
                "Tool 'bash' requires confirmation: commands need approval"
            ),
            Some(json!({ adk_core::TOOL_RESULT_STATUS_KEY: "held" })),
        )
    );

    store.decide(&scope(), &request.fingerprint(), ToolApproval::deny()).await.unwrap();
    let resumed = runner.resume_held_calls().await.unwrap();
    assert_eq!(
        recorded_answer(&events_of(&sessions).await, "call-1"),
        annotated(
            "bash",
            "call-1",
            resumed[0].result.clone(),
            Some(json!({ adk_core::TOOL_RESULT_STATUS_KEY: "refused" })),
        )
    );
    let _ = runner.close().await;
}

/// Hangs past the runner's tool timeout with `effect`.
struct Hangs(ToolEffect);

#[async_trait]
impl Tool for Hangs {
    fn name(&self) -> &str {
        "charge"
    }
    fn description(&self) -> &str {
        "charges a card"
    }
    fn effect(&self) -> ToolEffect {
        self.0
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok(json!({ "charged": true }))
    }
}

/// #1006 labelled every realtime timeout `failed`, though a non-idempotent call that timed out
/// may have taken effect.
#[tokio::test(start_paused = true)]
async fn a_timeout_is_an_unknown_outcome_only_for_a_non_idempotent_tool() {
    for (effect, expected) in
        [(ToolEffect::NonIdempotent, "outcome_unknown"), (ToolEffect::Idempotent, "failed")]
    {
        let sessions = sessions().await;
        let tool: Arc<dyn Tool> = Arc::new(Hangs(effect));
        let runner = IntegratedRealtimeRunner::builder()
            .model(Arc::new(ScriptedRealtimeModel::new(vec![])))
            .identity(APP, USER, SESSION)
            .session_service(sessions.clone())
            .adk_tool(tool.clone())
            .tool_timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let call = ToolCall {
            call_id: "call-9".to_string(),
            name: "charge".to_string(),
            arguments: json!({}),
        };

        let result = runner.execute_tool_with_plugins_for_test(&tool, &call).await.unwrap();

        assert_eq!(result, json!({ "error": "Tool 'charge' timed out after 50ms" }));
        assert_eq!(
            recorded_answer(&events_of(&sessions).await, "call-9"),
            annotated(
                "charge",
                "call-9",
                result,
                Some(json!({
                    adk_core::TOOL_RESULT_STATUS_KEY: expected,
                    adk_core::TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
                    adk_core::TOOL_RESULT_ERROR_CATEGORY_KEY: "timeout",
                })),
            ),
            "{effect:?}"
        );
    }
}

/// Asks for a confirmation as an agent tool's delegate does, then hangs past the timeout.
struct DelegatesThenHangs(Arc<InMemoryApprovalStore>);

fn delegated() -> ToolConfirmationRequest {
    ToolConfirmationRequest::new("write_file", json!({ "path": "src/lib.rs" }))
        .with_function_call_id("inner-1")
}

#[async_trait]
impl Tool for DelegatesThenHangs {
    fn name(&self) -> &str {
        "reviewer"
    }
    fn description(&self) -> &str {
        "delegates to a reviewer agent"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.0.record_pending(&scope(), &delegated()).await?;
        ctx.set_actions(EventActions {
            tool_confirmation: Some(delegated()),
            ..EventActions::default()
        });
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok(json!({ "reviewed": true }))
    }
}

/// #1006 left a superseded request pending, so a later approval could authorize an identical
/// call.
#[tokio::test(start_paused = true)]
async fn an_unknown_outcome_supersedes_a_delegates_request_and_withdraws_it() {
    let sessions = sessions().await;
    let store = Arc::new(InMemoryApprovalStore::new());
    let tool: Arc<dyn Tool> = Arc::new(DelegatesThenHangs(store.clone()));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(ScriptedRealtimeModel::new(vec![])))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone())
        .adk_tool(tool.clone())
        .tool_timeout(Duration::from_millis(50))
        .tool_governance(RunConfig::builder().approval_store(store.clone()).build())
        .build()
        .unwrap();
    let call = ToolCall {
        call_id: "call-3".to_string(),
        name: "reviewer".to_string(),
        arguments: json!({}),
    };

    let result = runner.execute_tool_with_plugins_for_test(&tool, &call).await.unwrap();

    assert_eq!(result, json!({ "error": "Tool 'reviewer' timed out after 50ms" }));
    assert_eq!(
        recorded_answer(&events_of(&sessions).await, "call-3"),
        annotated(
            "reviewer",
            "call-3",
            result,
            Some(json!({
                adk_core::TOOL_RESULT_STATUS_KEY: "outcome_unknown",
                adk_core::TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
                adk_core::TOOL_RESULT_ERROR_CATEGORY_KEY: "timeout",
            })),
        )
    );
    assert!(runner.held_calls().await.unwrap().is_empty(), "the call is not held");
    assert!(store.pending(&scope()).await.unwrap().is_empty(), "the request is withdrawn");
}
