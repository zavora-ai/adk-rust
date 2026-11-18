//! The computer use toolset acts only through the governed tool path.
//!
//! A real `LlmAgent` runs under a `Runner` with a scripted model that asks for a batch of
//! actions in one turn; a recording actuator shows which actions reached the environment.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_computer_use::{COMPUTER_BATCH_HALT_TEXT, ComputerUseMember, ComputerUseToolset};
use adk_core::{
    ActuationOutput, ApprovalScope, ApprovalStore, ComputerAction, ComputerActionKind,
    ComputerActuator, Content, DeclarativePolicy, Event, InMemoryApprovalStore, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, Part, Result, RunBudget, RunConfig, ScreenPoint, Screenshot,
    SessionId, ToolApproval, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest, ToolEffect, ToolResultStatus, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Records every action it is asked to perform.
#[derive(Default)]
struct RecordingActuator {
    actions: Mutex<Vec<ComputerAction>>,
}

impl RecordingActuator {
    fn kinds(&self) -> Vec<ComputerActionKind> {
        self.actions.lock().unwrap().iter().map(ComputerAction::kind).collect()
    }
}

#[async_trait]
impl ComputerActuator for RecordingActuator {
    fn display_size(&self) -> Option<(u32, u32)> {
        Some((1280, 800))
    }

    async fn perform(&self, action: ComputerAction) -> Result<ActuationOutput> {
        let kind = action.kind();
        self.actions.lock().unwrap().push(action);
        Ok(match kind {
            ComputerActionKind::Screenshot => {
                ActuationOutput::Image(Screenshot::png(PNG.to_vec()).unwrap())
            }
            ComputerActionKind::CursorPosition => ActuationOutput::Cursor(ScreenPoint::new(3, 4)),
            _ => ActuationOutput::Done,
        })
    }
}

/// Replays scripted responses and records every request.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()), requests: Mutex::new(Vec::new()) })
    }

    /// The function responses of the latest request, by call id.
    fn answers(&self) -> HashMap<String, (Value, Option<ToolResultStatus>, usize)> {
        let requests = self.requests.lock().unwrap();
        let Some(last) = requests.last() else { return HashMap::new() };
        last.contents
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, id, .. } => Some((
                    id.clone().unwrap_or_default(),
                    (
                        function_response.response.clone(),
                        part.tool_result_status(),
                        function_response.inline_data.len(),
                    ),
                )),
                _ => None,
            })
            .collect()
    }

    fn declared_tools(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap();
        let mut names: Vec<String> = requests[0].tools.keys().cloned().collect();
        names.sort();
        names
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req);
        let response = self.responses.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// One model turn asking for each `(member, args, id)` in order.
fn batch(calls: &[(&str, Value, &str)]) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: calls
                .iter()
                .map(|(name, args, id)| Part::FunctionCall {
                    name: format!("computer.{name}"),
                    args: args.clone(),
                    id: Some((*id).to_string()),
                    thought_signature: None,
                })
                .collect(),
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// Click a field, type into it, press Enter: the batch a model sends to fill a form.
fn form_batch() -> LlmResponse {
    batch(&[
        ("left_click", json!({ "coordinate": [10, 20] }), "c1"),
        ("type", json!({ "text": "hunter2" }), "c2"),
        ("key", json!({ "text": "Return" }), "c3"),
    ])
}

fn agent(model: Arc<ScriptedModel>, actuator: Arc<RecordingActuator>) -> LlmAgent {
    LlmAgentBuilder::new("operator")
        .model(model)
        .toolset(Arc::new(ComputerUseToolset::new(actuator)))
        .build()
        .unwrap()
}

struct Harness {
    runner: Runner,
}

async fn harness(agent: LlmAgent, config: RunConfig) -> Harness {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "desk".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("desk")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(config)
        .build()
        .unwrap();
    Harness { runner }
}

impl Harness {
    async fn run(&self, text: &str) -> Vec<Result<Event>> {
        let stream = self
            .runner
            .run(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                Content::new("user").with_text(text),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish")
    }
}

fn halted(answer: &(Value, Option<ToolResultStatus>, usize)) -> bool {
    answer.0 == json!(COMPUTER_BATCH_HALT_TEXT) && answer.1 == Some(ToolResultStatus::NotRun)
}

#[tokio::test]
async fn the_toolset_declares_each_member_with_its_display() {
    let actuator = Arc::new(RecordingActuator::default());
    let toolset = ComputerUseToolset::new(actuator).without(ComputerUseMember::HoldKey);
    let tools = toolset.member_tools();
    // Zoom is unsupported by the actuator and hold_key is withheld.
    assert_eq!(tools.len(), 15);
    let screenshot = tools.iter().find(|tool| tool.name() == "computer.screenshot").unwrap();
    assert_eq!(
        screenshot.declaration()["x-adk-anthropic-toolset-member"],
        json!({
            "toolset": "computer",
            "member": "screenshot",
            "display_width_px": 1280,
            "display_height_px": 800
        })
    );
    assert_eq!(screenshot.effect(), ToolEffect::ReadOnly);
    let click = tools.iter().find(|tool| tool.name() == "computer.left_click").unwrap();
    assert_eq!(click.effect(), ToolEffect::NonIdempotent);
    assert_eq!(click.batch_group().unwrap().name, "computer");
}

#[tokio::test]
async fn an_allowed_batch_reaches_the_actuator_in_order() {
    let actuator = Arc::new(RecordingActuator::default());
    let model = ScriptedModel::new(vec![form_batch()]);
    let harness = harness(agent(model.clone(), actuator.clone()), RunConfig::default()).await;
    harness.run("fill the form").await;

    assert_eq!(
        actuator.kinds(),
        vec![ComputerActionKind::Click, ComputerActionKind::Type, ComputerActionKind::Key]
    );
    let answers = model.answers();
    assert_eq!(answers["c1"].0, json!("OK"));
    assert_eq!(answers["c3"].0, json!("OK"));
    assert!(model.declared_tools().contains(&"computer.left_click".to_string()));
}

#[tokio::test]
async fn a_denied_action_never_acts_and_halts_the_rest_of_the_batch() {
    let actuator = Arc::new(RecordingActuator::default());
    let model = ScriptedModel::new(vec![form_batch()]);
    let policy = DeclarativePolicy::builder()
        .deny("computer.left_click", "clicks need a supervisor")
        .allow("computer.*")
        .build();
    let config = RunConfig::builder().tool_policy(Arc::new(policy)).build();
    let harness = harness(agent(model.clone(), actuator.clone()), config).await;
    harness.run("fill the form").await;

    // The typed text never reaches whatever field happened to have focus.
    assert!(actuator.kinds().is_empty());
    let answers = model.answers();
    assert_eq!(answers["c1"].1, Some(ToolResultStatus::Refused));
    assert!(halted(&answers["c2"]), "{:?}", answers["c2"]);
    assert!(halted(&answers["c3"]), "{:?}", answers["c3"]);
}

#[tokio::test]
async fn an_action_that_needs_approval_is_held_and_runs_once_approved() {
    let actuator = Arc::new(RecordingActuator::default());
    let model = ScriptedModel::new(vec![form_batch()]);
    let store = Arc::new(InMemoryApprovalStore::new());
    let policy = DeclarativePolicy::builder()
        .require_approval("computer.type", "typing needs a person")
        .allow("computer.*")
        .build();
    let config =
        RunConfig::builder().tool_policy(Arc::new(policy)).approval_store(store.clone()).build();
    let harness = harness(agent(model.clone(), actuator.clone()), config).await;
    let first = harness.run("fill the form").await;

    let held: Vec<ToolConfirmationRequest> = first
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.actions.tool_confirmation.clone())
        .collect();
    assert_eq!(held.len(), 1, "{first:?}");
    assert_eq!(held[0].tool_name, "computer.type");
    // The click ran; the held typing and the Enter after it did not.
    assert_eq!(actuator.kinds(), vec![ComputerActionKind::Click]);

    let scope = ApprovalScope::new("desk", "user", "session");
    store.decide(&scope, &held[0].fingerprint(), ToolApproval::approve()).await.unwrap();
    harness.run("").await;
    assert_eq!(actuator.kinds(), vec![ComputerActionKind::Click, ComputerActionKind::Type]);
    let typed = actuator.actions.lock().unwrap()[1].clone();
    assert_eq!(typed, ComputerAction::Type { text: "hunter2".into() });
}

#[derive(Debug)]
struct Approve;

#[async_trait]
impl ToolConfirmationHandler for Approve {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        Ok(ToolConfirmationDecision::Approve)
    }
}

#[derive(Debug)]
struct Decline;

#[async_trait]
impl ToolConfirmationHandler for Decline {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        Ok(ToolConfirmationDecision::Deny)
    }
}

#[tokio::test]
async fn a_person_approves_or_declines_each_action_in_the_run() {
    for (handler, expected) in [
        (Arc::new(Approve) as Arc<dyn ToolConfirmationHandler>, 3),
        (Arc::new(Decline) as Arc<dyn ToolConfirmationHandler>, 0),
    ] {
        let actuator = Arc::new(RecordingActuator::default());
        let model = ScriptedModel::new(vec![form_batch()]);
        let policy =
            DeclarativePolicy::builder().require_approval("computer.*", "every action").build();
        let config = RunConfig::builder()
            .tool_policy(Arc::new(policy))
            .tool_confirmation_handler(handler)
            .build();
        let harness = harness(agent(model.clone(), actuator.clone()), config).await;
        harness.run("fill the form").await;
        assert_eq!(actuator.kinds().len(), expected);
        if expected == 0 {
            let answers = model.answers();
            assert_eq!(answers["c1"].1, Some(ToolResultStatus::Refused));
            assert!(halted(&answers["c2"]) && halted(&answers["c3"]), "{answers:?}");
        }
    }
}

#[tokio::test]
async fn a_frozen_run_performs_no_action() {
    let actuator = Arc::new(RecordingActuator::default());
    let model = ScriptedModel::new(vec![form_batch()]);
    let harness = harness(agent(model, actuator.clone()), RunConfig::default()).await;
    harness.runner.governance().freeze("incident 42");
    let events = harness.run("fill the form").await;
    assert!(actuator.kinds().is_empty());
    assert!(events.iter().any(Result::is_err), "the frozen run ends with an error: {events:?}");
}

#[tokio::test]
async fn a_batch_over_the_tool_budget_performs_no_action() {
    let actuator = Arc::new(RecordingActuator::default());
    let model = ScriptedModel::new(vec![form_batch()]);
    let config = RunConfig::builder().budget(RunBudget::new().max_tool_calls(2)).build();
    let harness = harness(agent(model, actuator.clone()), config).await;
    harness.run("fill the form").await;
    assert!(actuator.kinds().is_empty());
}

#[tokio::test]
async fn screenshots_flow_back_to_the_model_as_images() {
    let actuator = Arc::new(RecordingActuator::default());
    let model = ScriptedModel::new(vec![batch(&[
        ("screenshot", json!({}), "s1"),
        ("cursor_position", json!({}), "s2"),
        ("left_click", json!({ "coordinate": [5000, 20] }), "s3"),
        ("type", json!({ "text": "never" }), "s4"),
    ])]);
    let harness = harness(agent(model.clone(), actuator.clone()), RunConfig::default()).await;
    harness.run("look").await;

    let answers = model.answers();
    assert_eq!(answers["s1"].0, json!("Screenshot captured."));
    assert_eq!(answers["s1"].2, 1, "the screenshot travels as inline image data");
    assert_eq!(answers["s2"].0, json!("X=3, Y=4"));
    // An off-screen click fails before the actuator sees it, and stops the batch.
    assert_eq!(answers["s3"].1, Some(ToolResultStatus::Failed));
    assert!(halted(&answers["s4"]), "{:?}", answers["s4"]);
    assert_eq!(
        actuator.kinds(),
        vec![ComputerActionKind::Screenshot, ComputerActionKind::CursorPosition]
    );
}
