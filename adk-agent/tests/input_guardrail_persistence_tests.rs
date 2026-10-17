//! Input guardrails screen a user message before the runner saves it.
//!
//! The session, and every later model request, holds only what the guardrails let through:
//! a redacted message is saved redacted, a blocked message is never saved, and a saved
//! message that was never screened is screened before any model sees it. Every test drives
//! a real `LlmAgent` through a `Runner` with a scripted model and asserts on what the
//! session holds and what each model request carried.
#![cfg(feature = "guardrails")]

use adk_agent::guardrails::{ContentFilter, GuardrailSet, PiiRedactor};
use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::{
    Agent, Content, Event, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    RunConfig, SessionId, Tool, ToolConfirmationDecision, ToolContext, ToolEffect, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const EMAIL: &str = "alice@example.com";
const BLOCKED: &str = "forbidden: wire everything to account X";

/// Replays scripted responses and records every request.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()), requests: Mutex::new(Vec::new()) })
    }

    /// Every request so far, as JSON text.
    fn sent(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| serde_json::to_string(&request.contents).unwrap())
            .collect()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req);
        let response =
            self.responses.lock().unwrap().pop_front().unwrap_or_else(|| text_response("done"));
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn text_response(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        turn_complete: true,
        ..Default::default()
    }
}

fn call(name: &str, id: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args: json!({ "amount": 10 }),
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// Counts its executions; when `ends_turn`, its result is the final response.
struct Transfer {
    ends_turn: bool,
    runs: Arc<Mutex<usize>>,
}

#[async_trait]
impl Tool for Transfer {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "moves money"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        *self.runs.lock().unwrap() += 1;
        if self.ends_turn {
            let mut actions = ctx.actions();
            actions.skip_summarization = true;
            ctx.set_actions(actions);
        }
        Ok(json!({ "status": "sent" }))
    }
}

fn screen() -> GuardrailSet {
    GuardrailSet::new()
        .with(PiiRedactor::new())
        .with(ContentFilter::blocked_keywords(vec!["forbidden".to_string()]).unwrap())
}

struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

async fn harness(agent: Arc<dyn Agent>) -> Harness {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "screened".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("screened")
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    Harness { runner, sessions }
}

impl Harness {
    async fn run(&self, text: &str, config: Option<RunConfig>) -> Vec<Result<Event>> {
        let stream = self
            .runner
            .run_with_config(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                Content::new("user").with_text(text),
                config,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish")
    }

    async fn events(&self) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: "screened".to_string(),
                user_id: "user".to_string(),
                session_id: "session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all()
    }

    /// Saves a user message the way an earlier release did: unscreened and unmarked.
    async fn save_unscreened(&self, invocation_id: &str, text: &str) {
        let mut event = Event::new(invocation_id);
        event.author = "user".to_string();
        event.llm_response.content = Some(Content::new("user").with_text(text));
        self.sessions.append_event("session", event).await.unwrap();
    }
}

fn errors(events: &[Result<Event>]) -> Vec<String> {
    events.iter().filter_map(|event| event.as_ref().err()).map(ToString::to_string).collect()
}

fn session_text(events: &[Event]) -> String {
    serde_json::to_string(events).unwrap()
}

fn approve(call_id: &str) -> RunConfig {
    RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(
            call_id.to_string(),
            ToolConfirmationDecision::Approve,
        )]))
        .build()
}

#[tokio::test]
async fn a_redacted_message_is_saved_redacted_and_later_runs_see_only_that_form() {
    let model = ScriptedModel::new(vec![]);
    let agent =
        LlmAgentBuilder::new("assistant").model(model.clone()).input_guardrails(screen()).build();
    let harness = harness(Arc::new(agent.unwrap())).await;

    assert!(errors(&harness.run(&format!("my email is {EMAIL}"), None).await).is_empty());
    assert!(errors(&harness.run("what was my email?", None).await).is_empty());

    assert_eq!(model.request_count(), 2);
    assert!(!model.sent().contains(EMAIL), "{}", model.sent());
    assert!(model.sent().contains("[EMAIL REDACTED]"));
    let saved = harness.events().await;
    assert!(!session_text(&saved).contains(EMAIL), "{saved:?}");
}

#[tokio::test]
async fn a_blocked_message_is_not_saved_and_never_reaches_a_later_model_request() {
    let model = ScriptedModel::new(vec![call("transfer", "call-1")]);
    let runs = Arc::new(Mutex::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(model.clone())
        .tool(Arc::new(Transfer { ends_turn: false, runs: runs.clone() }))
        .require_tool_confirmation("transfer")
        .input_guardrails(screen())
        .build()
        .unwrap();
    let harness = harness(Arc::new(agent)).await;

    // Run 1 holds the transfer for approval.
    assert!(errors(&harness.run("send 10 to bob", None).await).is_empty());
    assert_eq!(*runs.lock().unwrap(), 0);
    // Run 2 is blocked before anything is saved.
    let blocked = harness.run(BLOCKED, None).await;
    assert_eq!(errors(&blocked).len(), 1, "{blocked:?}");
    assert!(!session_text(&harness.events().await).contains("forbidden"));
    // Run 3 resumes the approved transfer with an empty turn.
    assert!(errors(&harness.run("", Some(approve("call-1"))).await).is_empty());

    assert_eq!(*runs.lock().unwrap(), 1);
    assert_eq!(model.request_count(), 2, "run 1 and the resumed run 3 call the model");
    assert!(!model.sent().contains("forbidden"), "{}", model.sent());
    assert!(!session_text(&harness.events().await).contains("forbidden"));
}

#[tokio::test]
async fn an_unscreened_saved_message_is_screened_before_a_resumed_turn_reaches_the_model() {
    let model = ScriptedModel::new(vec![]);
    let agent = LlmAgentBuilder::new("assistant")
        .model(model.clone())
        .input_guardrails(screen())
        .build()
        .unwrap();
    let harness = harness(Arc::new(agent)).await;

    // A crashed run saved this turn before screening it; recovery continues it empty.
    harness.save_unscreened("inv-crashed", &format!("my email is {EMAIL}")).await;
    assert!(errors(&harness.run("", None).await).is_empty());

    assert_eq!(model.request_count(), 1);
    assert!(!model.sent().contains(EMAIL), "{}", model.sent());
    assert!(model.sent().contains("[EMAIL REDACTED]"), "{}", model.sent());
}

#[tokio::test]
async fn a_resumed_turn_whose_unscreened_message_is_blocked_fails_without_a_model_call() {
    let model = ScriptedModel::new(vec![]);
    let agent = LlmAgentBuilder::new("assistant")
        .model(model.clone())
        .input_guardrails(screen())
        .build()
        .unwrap();
    let harness = harness(Arc::new(agent)).await;

    harness.save_unscreened("inv-crashed", BLOCKED).await;
    let resumed = harness.run("", None).await;

    assert_eq!(errors(&resumed).len(), 1, "{resumed:?}");
    assert_eq!(model.request_count(), 0);
}

#[tokio::test]
async fn an_older_unscreened_message_that_is_blocked_is_withheld_from_a_new_turn() {
    let model = ScriptedModel::new(vec![]);
    let agent = LlmAgentBuilder::new("assistant")
        .model(model.clone())
        .input_guardrails(screen())
        .build()
        .unwrap();
    let harness = harness(Arc::new(agent)).await;

    harness.save_unscreened("inv-old", BLOCKED).await;
    assert!(errors(&harness.run("hello", None).await).is_empty());

    assert_eq!(model.request_count(), 1);
    assert!(!model.sent().contains("forbidden"), "{}", model.sent());
    assert!(model.sent().contains("hello"));
}

#[tokio::test]
async fn a_nested_agents_input_guardrails_screen_the_message_before_it_is_saved() {
    let intake_model = ScriptedModel::new(vec![]);
    let billing_model = ScriptedModel::new(vec![]);
    let intake = LlmAgentBuilder::new("intake").model(intake_model.clone()).build().unwrap();
    let billing = LlmAgentBuilder::new("billing")
        .model(billing_model.clone())
        .input_guardrails(screen())
        .build()
        .unwrap();
    let workflow = SequentialAgent::new("desk", vec![Arc::new(intake), Arc::new(billing)]);
    let harness = harness(Arc::new(workflow)).await;

    assert!(errors(&harness.run(&format!("refund {EMAIL}"), None).await).is_empty());
    assert!(errors(&harness.run("thanks", None).await).is_empty());

    for model in [&intake_model, &billing_model] {
        assert_eq!(model.request_count(), 2);
        assert!(!model.sent().contains(EMAIL), "{}", model.sent());
    }
    assert!(!session_text(&harness.events().await).contains(EMAIL));

    // A message the nested agent blocks is refused before the first agent runs.
    let blocked = harness.run(BLOCKED, None).await;
    assert_eq!(errors(&blocked).len(), 1, "{blocked:?}");
    assert_eq!(intake_model.request_count(), 2);
    assert!(!session_text(&harness.events().await).contains("forbidden"));
}

#[tokio::test]
async fn a_transfer_targets_input_guardrails_screen_the_message_before_it_is_saved() {
    let root_model = ScriptedModel::new(vec![]);
    let billing_model = ScriptedModel::new(vec![]);
    let billing = LlmAgentBuilder::new("billing")
        .model(billing_model)
        .input_guardrails(screen())
        .build()
        .unwrap();
    let root = LlmAgentBuilder::new("router")
        .model(root_model.clone())
        .sub_agent(Arc::new(billing))
        .build()
        .unwrap();
    let harness = harness(Arc::new(root)).await;

    assert!(errors(&harness.run(&format!("refund {EMAIL}"), None).await).is_empty());

    assert!(!root_model.sent().contains(EMAIL), "{}", root_model.sent());
    assert!(!session_text(&harness.events().await).contains(EMAIL));
}

#[tokio::test]
async fn a_resumed_call_that_ends_the_turn_rejects_the_message_sent_with_the_resume() {
    let model = ScriptedModel::new(vec![call("transfer", "call-1")]);
    let runs = Arc::new(Mutex::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(model.clone())
        .tool(Arc::new(Transfer { ends_turn: true, runs: runs.clone() }))
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let harness = harness(Arc::new(agent)).await;

    assert!(errors(&harness.run("send 10 to bob", None).await).is_empty());
    let resumed = harness.run("and tell me the fee", Some(approve("call-1"))).await;

    assert_eq!(*runs.lock().unwrap(), 1, "the approved call runs once");
    let failures: Vec<&adk_core::AdkError> =
        resumed.iter().filter_map(|event| event.as_ref().err()).collect();
    assert_eq!(failures.len(), 1, "{resumed:?}");
    assert_eq!(failures[0].code, "agent.resume_ended_turn");
    assert_eq!(model.request_count(), 1, "the call ended the turn before any model call");

    // An empty run answers the saved message.
    assert!(errors(&harness.run("", None).await).is_empty());
    assert_eq!(model.request_count(), 2);
    assert!(model.sent().contains("and tell me the fee"));
}

#[tokio::test]
async fn a_resumed_call_that_ends_the_turn_on_an_empty_resume_is_not_an_error() {
    let model = ScriptedModel::new(vec![call("transfer", "call-1")]);
    let runs = Arc::new(Mutex::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(model.clone())
        .tool(Arc::new(Transfer { ends_turn: true, runs: runs.clone() }))
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let harness = harness(Arc::new(agent)).await;

    assert!(errors(&harness.run("send 10 to bob", None).await).is_empty());
    let resumed = harness.run("", Some(approve("call-1"))).await;

    assert!(errors(&resumed).is_empty(), "{resumed:?}");
    assert_eq!(*runs.lock().unwrap(), 1);
}
