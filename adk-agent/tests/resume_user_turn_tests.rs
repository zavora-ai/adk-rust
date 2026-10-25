//! A run that resumes held calls keeps the person's messages.
//!
//! A resume run carries a blank user turn — no parts, as `Harness::resume` and a `/run_sse`
//! request without parts send, or blank text. The model request it produces still holds the
//! message that led to the held call, then the call, then its resumed result, whether the
//! call was held for want of a confirmation handler or waited in the approval store after the
//! handler timed out. Every test drives real `LlmAgent`s through a `Runner` with
//! `ScriptedLlm` models and asserts on the requests the models received.

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, Content, FunctionResponseData, InMemoryApprovalStore,
    LlmRequest, LlmResponse, Part, Result, RunConfig, SessionId, Tool, ToolApproval,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest, ToolContext,
    UserId, held_tool_calls,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// --- Harness ---

/// Records the arguments of every execution.
struct Transfer {
    executions: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for Transfer {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "moves money"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "status": "sent" }))
    }
}

type Executions = Arc<Mutex<Vec<Value>>>;

fn holder(name: &str, model: Arc<dyn adk_core::Llm>, executions: &Executions) -> Arc<dyn Agent> {
    Arc::new(
        LlmAgentBuilder::new(name)
            .model(model)
            .tool(Arc::new(Transfer { executions: executions.clone() }))
            .require_tool_confirmation("transfer")
            .build()
            .unwrap(),
    )
}

fn talker(name: &str, model: Arc<dyn adk_core::Llm>) -> Arc<dyn Agent> {
    Arc::new(LlmAgentBuilder::new(name).model(model).build().unwrap())
}

/// Calls `transfer` in answer to the messages it has a plan for and answers everything else
/// with text, however many calls a run makes; records every request.
#[derive(Default)]
struct Planned {
    /// The `(call id, amount)` calls to issue when a request ends on the keyed message.
    plans: Mutex<HashMap<String, Vec<(String, u64)>>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl Planned {
    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl adk_core::Llm for Planned {
    fn name(&self) -> &str {
        "planned"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<adk_core::LlmResponseStream> {
        let message = request
            .contents
            .last()
            .filter(|content| content.role == "user")
            .and_then(|content| content.parts.iter().find_map(Part::text))
            .map(str::to_string);
        let plan = message.and_then(|message| self.plans.lock().unwrap().remove(&message));
        self.requests.lock().unwrap().push(request);
        let parts = match plan {
            Some(turn) => turn.iter().map(|(id, amount)| transfer_call(id, *amount)).collect(),
            None => vec![Part::Text { text: "noted".to_string() }],
        };
        let response = LlmResponse {
            content: Some(Content { role: "model".to_string(), parts }),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn transfer_call(id: &str, amount: u64) -> Part {
    Part::FunctionCall {
        name: "transfer".to_string(),
        args: json!({ "amount": amount }),
        id: Some(id.to_string()),
        thought_signature: None,
    }
}

/// A model turn that calls `transfer` once per `(id, amount)`.
fn calls(calls: &[(String, u64)]) -> ScriptedResponse {
    ScriptedResponse::response(LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: calls.iter().map(|(id, amount)| transfer_call(id, *amount)).collect(),
        }),
        turn_complete: true,
        ..Default::default()
    })
}

fn sent(id: &str) -> Content {
    Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new("transfer", json!({ "status": "sent" })),
            id: Some(id.to_string()),
            annotations: None,
        }],
    }
}

/// `contents` as `(role, parts)` pairs; `Content` has no `PartialEq`.
fn turns(contents: &[Content]) -> Vec<(String, Vec<Part>)> {
    contents.iter().map(|content| (content.role.clone(), content.parts.clone())).collect()
}

/// The request a resumed model call receives after "send 40 to Bob" held `call-1`.
fn asked_called_sent() -> Vec<(String, Vec<Part>)> {
    turns(&[
        Content::new("user").with_text("send 40 to Bob"),
        Content { role: "model".to_string(), parts: vec![transfer_call("call-1", 40)] },
        sent("call-1"),
    ])
}

/// The blank user turns a resume run may carry.
fn blank_turns() -> [Content; 3] {
    [
        Content::new("user"),
        Content::new("user").with_text(""),
        Content::new("user").with_text(" \n"),
    ]
}

const APP: &str = "resume";

fn scope() -> ApprovalScope {
    ApprovalScope::new(APP, "user", "session")
}

/// Never decides, so every confirmation it is asked for times out.
#[derive(Debug)]
struct Away;

#[async_trait]
impl ToolConfirmationHandler for Away {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        std::future::pending().await
    }
}

/// How a call that needs confirmation comes to wait for a decision.
#[derive(Debug, Clone, Copy)]
enum Wait {
    /// The run has no confirmation handler, so the agent holds the call.
    Held,
    /// The handler does not decide in time, so the call waits in the approval store.
    TimedOut,
}

impl Wait {
    /// A runner config with `store` whose confirmations end the way `self` names.
    fn config(self, store: Arc<InMemoryApprovalStore>) -> RunConfig {
        let builder = RunConfig::builder().approval_store(store);
        match self {
            Wait::Held => builder,
            Wait::TimedOut => builder
                .tool_confirmation_handler(Arc::new(Away))
                .tool_confirmation_timeout(Duration::from_millis(10)),
        }
        .build()
    }
}

struct Conversation {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

impl Conversation {
    async fn new(agent: Arc<dyn Agent>, config: Option<RunConfig>) -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: "user".to_string(),
                session_id: Some("session".to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        let mut builder = Runner::builder()
            .app_name(APP)
            .agent(agent)
            .session_service(sessions.clone() as Arc<dyn SessionService>);
        if let Some(config) = config {
            builder = builder.run_config(config);
        }
        Self { runner: builder.build().unwrap(), sessions }
    }

    async fn send(&self, content: Content, config: Option<RunConfig>) {
        let stream = self
            .runner
            .run_with_config(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                content,
                config,
            )
            .await
            .unwrap();
        let events = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish");
        for event in events {
            event.expect("no run error");
        }
    }

    async fn say(&self, text: &str) {
        self.send(Content::new("user").with_text(text), None).await;
    }

    /// The calls `agent` holds, in the order it held them.
    async fn held(&self, agent: &str) -> Vec<adk_core::HeldToolCall> {
        let events = self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: "user".to_string(),
                session_id: "session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all();
        held_tool_calls(&events, agent, "")
    }
}

/// The texts of the person's turns in `request`: user turns that carry input and are not
/// another agent's turns relayed as context or a turn the agent adds when there is no
/// message.
fn person_turns(request: &LlmRequest) -> Vec<String> {
    request
        .contents
        .iter()
        .filter(|content| {
            content.role == "user"
                && content.carries_user_input()
                && !adk_core::history::is_other_agent_context(content)
        })
        .flat_map(|content| &content.parts)
        .filter_map(Part::text)
        .filter(|text| !text.starts_with("(No new message from the user."))
        .map(str::to_string)
        .collect()
}

/// Whether a user turn in `request` has nothing to send, which providers reject.
fn has_blank_user_turn(request: &LlmRequest) -> bool {
    request.contents.iter().any(|content| {
        content.role == "user"
            && content
                .parts
                .iter()
                .all(|part| matches!(part, Part::Text { text } if text.trim().is_empty()))
    })
}

// --- Regressions ---

/// Makes `transfer` for "send 40 to Bob" wait as `wait` names, approves it in the approval
/// store, resumes with `resume`, and returns the request of the resumed model call.
async fn resumed_request(wait: Wait, resume: Content) -> (LlmRequest, Vec<Value>) {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [calls(&[("call-1".to_string(), 40)]), ScriptedResponse::text("Sent 40 to Bob.")],
    ));
    let executions = Executions::default();
    let store = Arc::new(InMemoryApprovalStore::new());
    let conversation = Conversation::new(
        holder("ops", model.clone(), &executions),
        Some(wait.config(store.clone())),
    )
    .await;

    conversation.say("send 40 to Bob").await;
    let held = conversation.held("ops").await;
    assert_eq!(held.len(), 1, "the call waits");
    let pending = store.pending(&scope()).await.unwrap();
    assert_eq!(pending.len(), 1, "the call waits in the store");
    assert!(executions.lock().unwrap().is_empty(), "nothing runs before the decision");
    store.decide(&scope(), &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();
    conversation.send(resume, None).await;

    assert_eq!(model.remaining(), 0, "the resumed run calls the model once");
    let executions = executions.lock().unwrap().clone();
    (model.last_request().unwrap(), executions)
}

#[tokio::test]
async fn a_resume_without_parts_keeps_the_message_that_led_to_the_held_call() {
    let (request, executions) = resumed_request(Wait::Held, Content::new("user")).await;

    assert_eq!(executions, vec![json!({ "amount": 40 })]);
    assert_eq!(turns(&request.contents), asked_called_sent());
}

#[tokio::test]
async fn a_resume_with_blank_text_keeps_the_message_that_led_to_the_held_call() {
    for resume in [Content::new("user").with_text(""), Content::new("user").with_text(" \n")] {
        let (request, _) = resumed_request(Wait::Held, resume.clone()).await;
        assert_eq!(turns(&request.contents), asked_called_sent(), "resumed with {resume:?}");
    }
}

#[tokio::test]
async fn a_resume_after_a_timed_out_call_waited_keeps_the_message() {
    for resume in blank_turns() {
        let (request, executions) = resumed_request(Wait::TimedOut, resume.clone()).await;
        assert_eq!(executions, vec![json!({ "amount": 40 })], "resumed with {resume:?}");
        assert_eq!(turns(&request.contents), asked_called_sent(), "resumed with {resume:?}");
    }
}

#[tokio::test]
async fn a_resume_keeps_every_earlier_message_in_order() {
    for resume in blank_turns() {
        let model = Arc::new(ScriptedLlm::new(
            "scripted",
            [
                ScriptedResponse::text("Hello."),
                calls(&[("call-1".to_string(), 40)]),
                ScriptedResponse::text("It waits for your approval."),
                ScriptedResponse::text("Sent 40 to Bob."),
            ],
        ));
        let executions = Executions::default();
        let conversation = Conversation::new(holder("ops", model.clone(), &executions), None).await;

        conversation.say("hello").await;
        conversation.say("send 40 to Bob").await;
        conversation.say("is it done?").await;
        let held = conversation.held("ops").await;
        assert_eq!(held.len(), 1, "a message does not decide the call");
        let approve = RunConfig::builder()
            .tool_approval(held[0].request.fingerprint(), ToolApproval::approve());
        conversation.send(resume.clone(), Some(approve.build())).await;

        assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
        let request = model.last_request().unwrap();
        assert_eq!(
            person_turns(&request),
            ["hello", "send 40 to Bob", "is it done?"],
            "resumed with {resume:?}"
        );
        // The message, the call and its result keep their order.
        let sent_turns = turns(&request.contents);
        let positions: Vec<usize> = asked_called_sent()
            .iter()
            .map(|wanted| {
                sent_turns.iter().position(|turn| turn == wanted).unwrap_or_else(|| {
                    panic!("{wanted:?} is not in {:?}", request.contents);
                })
            })
            .collect();
        assert!(positions.is_sorted(), "{:?}", request.contents);
        assert!(!has_blank_user_turn(&request), "{:?}", request.contents);
    }
}

#[tokio::test]
async fn a_workflow_sub_agent_resumed_without_parts_keeps_the_message() {
    let ops = Arc::new(ScriptedLlm::new(
        "ops",
        [calls(&[("call-1".to_string(), 40)]), ScriptedResponse::text("Sent 40 to Bob.")],
    ));
    let report = Arc::new(Planned::default());
    let executions = Executions::default();
    let pipeline = SequentialAgent::new(
        "pipeline",
        vec![holder("ops", ops.clone(), &executions), talker("report", report.clone())],
    );
    let conversation = Conversation::new(Arc::new(pipeline), None).await;

    conversation.say("send 40 to Bob").await;
    let held = conversation.held("ops").await;
    assert_eq!(held.len(), 1, "the workflow stops at the hold");
    let approve =
        RunConfig::builder().tool_approval(held[0].request.fingerprint(), ToolApproval::approve());
    conversation.send(Content::new("user"), Some(approve.build())).await;

    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
    assert_eq!(turns(&ops.last_request().unwrap().contents), asked_called_sent());
    let report_requests = report.requests();
    assert_eq!(report_requests.len(), 1, "the workflow continues after the resumed sub-agent");
    assert_eq!(person_turns(&report_requests[0]), ["send 40 to Bob"]);
}

// --- IncludeContents::None ---

/// A holder that sends its model only the current turn.
fn current_turn_holder(model: Arc<dyn adk_core::Llm>, executions: &Executions) -> Arc<dyn Agent> {
    Arc::new(
        LlmAgentBuilder::new("ops")
            .model(model)
            .tool(Arc::new(Transfer { executions: executions.clone() }))
            .require_tool_confirmation("transfer")
            .include_contents(adk_core::IncludeContents::None)
            .build()
            .unwrap(),
    )
}

#[tokio::test]
async fn a_current_turn_agent_resumed_with_an_empty_turn_sees_the_message_the_call_and_its_result()
{
    for resume in [Content::new("user"), Content::new("user").with_text("")] {
        let model = Arc::new(ScriptedLlm::new(
            "scripted",
            [
                ScriptedResponse::text("Hello."),
                calls(&[("call-1".to_string(), 40)]),
                ScriptedResponse::text("Sent 40 to Bob."),
                ScriptedResponse::text("You are welcome."),
            ],
        ));
        let executions = Executions::default();
        let conversation =
            Conversation::new(current_turn_holder(model.clone(), &executions), None).await;

        conversation.say("hello").await;
        conversation.say("send 40 to Bob").await;
        let held = conversation.held("ops").await;
        assert_eq!(held.len(), 1, "the call is held");
        let approve = RunConfig::builder()
            .tool_approval(held[0].request.fingerprint(), ToolApproval::approve());
        conversation.send(resume.clone(), Some(approve.build())).await;

        assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
        // The turn the call belongs to, without the earlier "hello".
        assert_eq!(
            turns(&model.last_request().unwrap().contents),
            asked_called_sent(),
            "resumed with {resume:?}"
        );

        // With nothing held, the next run is its own message again.
        conversation.say("thanks").await;
        assert_eq!(
            turns(&model.last_request().unwrap().contents),
            turns(&[Content::new("user").with_text("thanks")])
        );
    }
}

#[tokio::test]
async fn a_current_turn_agent_resumed_after_a_later_message_sees_the_call_and_its_result() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            calls(&[("call-1".to_string(), 40)]),
            ScriptedResponse::text("It waits for your approval."),
            ScriptedResponse::text("Sent 40 to Bob."),
        ],
    ));
    let executions = Executions::default();
    let conversation =
        Conversation::new(current_turn_holder(model.clone(), &executions), None).await;

    conversation.say("send 40 to Bob").await;
    conversation.say("is it done?").await;
    let held = conversation.held("ops").await;
    let approve =
        RunConfig::builder().tool_approval(held[0].request.fingerprint(), ToolApproval::approve());
    conversation.send(Content::new("user"), Some(approve.build())).await;

    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
    let request = model.last_request().unwrap();
    assert_eq!(person_turns(&request), ["send 40 to Bob", "is it done?"]);
    let sent_turns = turns(&request.contents);
    let positions: Vec<usize> = asked_called_sent()
        .iter()
        .map(|wanted| {
            sent_turns.iter().position(|turn| turn == wanted).unwrap_or_else(|| {
                panic!("{wanted:?} is not in {:?}", request.contents);
            })
        })
        .collect();
    assert!(positions.is_sorted(), "{:?}", request.contents);
    assert!(!has_blank_user_turn(&request), "{:?}", request.contents);
}

#[tokio::test]
async fn a_current_turn_agent_resuming_a_call_with_a_new_message_sees_the_call_and_its_result() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [calls(&[("call-1".to_string(), 40)]), ScriptedResponse::text("Sent; nothing new.")],
    ));
    let executions = Executions::default();
    let conversation =
        Conversation::new(current_turn_holder(model.clone(), &executions), None).await;

    conversation.say("send 40 to Bob").await;
    let held = conversation.held("ops").await;
    let approve =
        RunConfig::builder().tool_approval(held[0].request.fingerprint(), ToolApproval::approve());
    conversation.send(Content::new("user").with_text("any news?"), Some(approve.build())).await;

    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
    let mut expected = asked_called_sent();
    expected.extend(turns(&[Content::new("user").with_text("any news?")]));
    assert_eq!(turns(&model.last_request().unwrap().contents), expected);
}

/// Wave 2 review findings 12 and 13 (spec 3.3 §7.3): while a call waits without a decision,
/// a current-turn agent's later runs send their own message alone; only the run that resumes
/// the call begins its turn at the message that led to it.
#[tokio::test]
async fn a_current_turn_agent_sends_its_own_turn_while_a_call_waits() {
    for wait in [Wait::Held, Wait::TimedOut] {
        let model = Arc::new(ScriptedLlm::new(
            "scripted",
            [
                calls(&[("call-1".to_string(), 40)]),
                ScriptedResponse::text("Sunny."),
                ScriptedResponse::text("Sent 40 to Bob."),
            ],
        ));
        let executions = Executions::default();
        let store = Arc::new(InMemoryApprovalStore::new());
        let conversation = Conversation::new(
            current_turn_holder(model.clone(), &executions),
            Some(wait.config(store.clone())),
        )
        .await;

        conversation.say("send 40 to Bob").await;
        conversation.say("what is the weather?").await;
        assert_eq!(
            turns(&model.last_request().unwrap().contents),
            turns(&[Content::new("user").with_text("what is the weather?")]),
            "{wait:?}: the waiting call stays out of the turn"
        );

        let held = conversation.held("ops").await;
        store
            .decide(&scope(), &held[0].request.fingerprint(), ToolApproval::approve())
            .await
            .unwrap();
        conversation.send(Content::new("user"), None).await;
        assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })], "{wait:?}");
        let request = model.last_request().unwrap();
        assert_eq!(person_turns(&request), ["send 40 to Bob", "what is the weather?"], "{wait:?}");
    }
}

// --- Property ---

/// A blank user turn of one of the shapes a resume run carries.
#[derive(Debug, Clone, Copy)]
enum Blank {
    NoParts,
    EmptyText,
    Whitespace,
}

impl Blank {
    fn content(self) -> Content {
        match self {
            Blank::NoParts => Content::new("user"),
            Blank::EmptyText => Content::new("user").with_text(""),
            Blank::Whitespace => Content::new("user").with_text("  \n\t"),
        }
    }
}

#[derive(Debug, Clone)]
enum Step {
    /// The person sends a message the model answers with text.
    Chat,
    /// The person sends a message the model answers by calling `transfer` `calls` times.
    Ask { calls: usize },
    /// A blank run after the approval store records, per waiting call in hold order, an
    /// approval, a denial, or no decision; calls past the end get none.
    Resume { blank: Blank, decisions: Vec<Option<bool>> },
}

fn step() -> impl Strategy<Value = Step> {
    let blank = prop_oneof![Just(Blank::NoParts), Just(Blank::EmptyText), Just(Blank::Whitespace)];
    prop_oneof![
        Just(Step::Chat),
        (1usize..=2).prop_map(|calls| Step::Ask { calls }),
        (blank, prop::collection::vec(prop::option::of(any::<bool>()), 0..4))
            .prop_map(|(blank, decisions)| Step::Resume { blank, decisions }),
    ]
}

/// Runs `steps` against a holder agent — the root, or the first sub-agent of a sequential
/// workflow — whose calls wait as `wait` names, and checks every model request: the
/// person's messages appear exactly once each, in the order they were sent, and no blank
/// user turn is sent.
async fn check_history(workflow: bool, wait: Wait, steps: Vec<Step>) {
    let ops = Arc::new(Planned::default());
    let report = Arc::new(Planned::default());
    let executions = Executions::default();
    let root = if workflow {
        Arc::new(SequentialAgent::new(
            "pipeline",
            vec![holder("ops", ops.clone(), &executions), talker("report", report.clone())],
        )) as Arc<dyn Agent>
    } else {
        holder("ops", ops.clone(), &executions)
    };
    let store = Arc::new(InMemoryApprovalStore::new());
    let conversation = Conversation::new(root, Some(wait.config(store.clone()))).await;
    let mut messages = Vec::<String>::new();
    let mut next_call = 0u64;
    let (mut ops_seen, mut report_seen) = (0, 0);

    for (index, step) in steps.iter().enumerate() {
        match step {
            Step::Chat => {
                messages.push(format!("message {index}"));
                conversation.say(&messages[messages.len() - 1]).await;
            }
            Step::Ask { calls: count } => {
                let message = format!("message {index}");
                // Distinct arguments, so no hold replaces another with the same fingerprint.
                let turn: Vec<(String, u64)> = (0..*count)
                    .map(|_| {
                        next_call += 1;
                        (format!("call-{next_call}"), next_call)
                    })
                    .collect();
                ops.plans.lock().unwrap().insert(message.clone(), turn);
                messages.push(message);
                conversation.say(&messages[messages.len() - 1]).await;
            }
            Step::Resume { blank, decisions } => {
                let held = conversation.held("ops").await;
                for (call, decision) in held.iter().zip(decisions) {
                    let approval = match decision {
                        Some(true) => ToolApproval::approve(),
                        Some(false) => ToolApproval::deny(),
                        None => continue,
                    };
                    store.decide(&scope(), &call.request.fingerprint(), approval).await.unwrap();
                }
                conversation.send(blank.content(), None).await;
            }
        }

        let ops_requests = ops.requests();
        let report_requests = report.requests();
        let model_calls = ops_requests.len() - ops_seen;
        // A call still waiting is not put to the handler again, so every run reaches its model
        // call (spec 3.3 §7.3, §7.4).
        assert_eq!(model_calls, 1, "step {index} ({step:?}) of {steps:?}");
        for request in ops_requests[ops_seen..].iter().chain(&report_requests[report_seen..]) {
            assert_eq!(
                person_turns(request),
                messages,
                "step {index} ({step:?}) of {steps:?}: {:?}",
                request.contents
            );
            assert!(
                !has_blank_user_turn(request),
                "step {index} ({step:?}) of {steps:?}: {:?}",
                request.contents
            );
        }
        (ops_seen, report_seen) = (ops_requests.len(), report_requests.len());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// For histories of messages, held or timed-out calls, decisions and blank resumes,
    /// every model request keeps each of the person's messages exactly once and in order.
    #[test]
    fn every_message_reaches_the_model_once_and_in_order(
        workflow in any::<bool>(),
        timed_out in any::<bool>(),
        steps in prop::collection::vec(step(), 1..8),
    ) {
        let wait = if timed_out { Wait::TimedOut } else { Wait::Held };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_history(workflow, wait, steps));
    }
}
