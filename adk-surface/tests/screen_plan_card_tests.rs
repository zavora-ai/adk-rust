//! Screen plans on decision cards (spec 3.7 Q8): a harness with the computer-use pack and no
//! live prompter parks each held `computer_use_execute`, and the surface shows one card for a
//! previewed plan — its window, every step and target, the screenshot each target was found on —
//! whose one answer decides every step with the plan's binding and the same
//! `harness.computer_use.plan_decided` record a live prompter's answer writes. A step whose
//! target moved is held again on a fresh card, a plan too long to show is approved only once
//! shown in full, Stop all closes an open plan card, and no standing rule covers a desktop
//! action.
//!
//! The model is `ScriptedLlm`; the desktop is an in-process double that previews clicks with
//! target evidence and typing without, can move a target between the plan's preview and a
//! step's execution, and counts every mutation. The harness, its approval service, the engine
//! and the source are real.

#![cfg(feature = "computer-use")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPostcondition, ActionPreview, Bounds, ComputerUseError,
    ComputerUseRuntime, ControlLease, ExecutionCapability, ExecutionMode, ExecutionReceipt,
    LeaseBoundaries, PolicyDecision as RuntimeDecision, ReceiptStatus, ScopeAuthorizer,
    TargetEvidence, VerificationOutcome,
};
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApprovalState, ApproverSelector, Event, InMemoryApprovalStore,
    Part, SessionId, StoreApprovalService, UserId,
};
use adk_harness::packs::computer_use::{
    COMPUTER_USE_AUDIT_EXECUTED, COMPUTER_USE_AUDIT_PLAN_DECIDED, COMPUTER_USE_EXECUTE,
    COMPUTER_USE_PLAN_DEVIATION_CODE, COMPUTER_USE_PREVIEW, ComputerUsePack,
};
use adk_harness::{Harness, HarnessAgent, HeldPlanState, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::rules::RuleViolation;
use adk_surface::{
    Answer, Caller, Command, CommandOutcome, DecidedAnswer, DecisionCard, LocalSource, Origin,
    SURFACE_AUDIT_ANSWER, SURFACE_AUDIT_STOP_ALL, Source, SurfaceEngine, SurfaceError, SurfaceKind,
    words,
};
use serde_json::{Value, json};

const APP: &str = "desk";
const OWNER: &str = "alice";
const SESSION: &str = "s1";

/// One proposed action: a click on a labelled target in a window, or typing.
fn click(id: &str, window: &str, label: &str, x: f64) -> Value {
    json!({ "id": id, "operation": "left_click", "window": window, "label": label, "x": x })
}

fn typing(id: &str, text: &str) -> Value {
    json!({ "id": id, "operation": "type", "text": text })
}

/// A desktop whose targets can move between a plan's preview and a step's execution.
#[derive(Default)]
struct Desktop {
    previews: Mutex<HashMap<String, usize>>,
    moved: Mutex<HashMap<String, f64>>,
    performed: Mutex<Vec<String>>,
}

impl Desktop {
    fn performed(&self) -> Vec<String> {
        self.performed.lock().unwrap().clone()
    }

    fn move_after_preview(&self, id: &str, x: f64) {
        self.moved.lock().unwrap().insert(id.to_string(), x);
    }

    fn target(&self, action: &Value) -> Option<TargetEvidence> {
        let id = action["id"].as_str().unwrap_or_default();
        let window = action.get("window")?.as_str()?;
        let seen = *self.previews.lock().unwrap().get(id).unwrap_or(&0);
        let x = match self.moved.lock().unwrap().get(id) {
            Some(x) if seen > 1 => *x,
            _ => action["x"].as_f64().unwrap_or_default(),
        };
        Some(TargetEvidence {
            platform: "test".into(),
            app_id: "app.forms".into(),
            pid: None,
            window_id: Some(json!(window)),
            window_title_digest: None,
            display_id: Some("built-in".into()),
            role: None,
            label_digest: action["label"].as_str().map(|label| format!("label:{label}")),
            bounds: Some(Bounds::points(x, 300.0, 90.0, 40.0, 2.0)),
            observation_id: "observation".into(),
            screenshot_hash: Some("sha256:screen".into()),
            device_pixel_ratio: Some(2.0),
            ui_tree_revision: None,
            confidence: 1.0,
            captured_at: "2026-11-24T10:00:00Z".into(),
        })
    }
}

#[async_trait::async_trait]
impl ComputerUseRuntime for Desktop {
    async fn discover_capabilities(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "operations": ["left_click", "type"] }))
    }
    async fn observe_visual(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "kind": "visual" }))
    }
    async fn observe_semantic(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "kind": "semantic" }))
    }
    async fn preview_action(&self, action: Value) -> Result<ActionPreview, ComputerUseError> {
        let id = action["id"].as_str().unwrap_or_default().to_string();
        *self.previews.lock().unwrap().entry(id.clone()).or_default() += 1;
        let operation = action["operation"].as_str().unwrap_or_default().to_string();
        Ok(ActionPreview {
            envelope: ActionEnvelope {
                action_id: format!("action-{id}"),
                session_id: "desktop".into(),
                execution_group_id: None,
                principal_id: OWNER.into(),
                agent_id: Some("harness".into()),
                tool: "computer".into(),
                operation: operation.clone(),
                action_class: ActionClass::EditReversible,
                requested_mode: ExecutionMode::Background,
                target: self.target(&action),
                target_sensitivity: None,
                resource: None,
                provenance: None,
                data_labels: Vec::new(),
                postcondition: Some(ActionPostcondition::UiElement {
                    role: None,
                    label: None,
                    exists: true,
                    value_digest: None,
                }),
                reversible: true,
                external_side_effect: false,
                proposed_at: "2026-11-24T10:00:00Z".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
                args_digest: format!("digest-{id}"),
            },
            executable: false,
            blocker: Some("approval_required".into()),
            policy: RuntimeDecision {
                decision: "confirm".into(),
                policy_digest: "policy".into(),
                reasons: Vec::new(),
                grant_id: None,
            },
            capability: ExecutionCapability {
                app_id: "*".into(),
                operation,
                backend: "test".into(),
                supported_modes: vec![ExecutionMode::Background],
                interference: "none".into(),
                confidence: 1.0,
                verified_at: None,
                verification_source: "platform_rule".into(),
            },
        })
    }
    async fn acquire_lease(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<ControlLease, ComputerUseError> {
        Ok(ControlLease {
            lease_id: format!("lease-{}", envelope.action_id),
            revision: 1,
            session_id: envelope.session_id.clone(),
            principal_id: envelope.principal_id.clone(),
            agent_id: envelope.agent_id.clone(),
            kind: "cooperative".into(),
            execution_mode: envelope.requested_mode,
            state: "active".into(),
            acquired_at: None,
            expires_at: envelope.expires_at.clone(),
            action_budget: 1,
            actions_used: 0,
            boundaries: LeaseBoundaries::default(),
        })
    }
    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        _: &ControlLease,
        _: Option<&str>,
    ) -> Result<ExecutionReceipt, ComputerUseError> {
        let id = envelope.action_id.trim_start_matches("action-").to_string();
        let mut performed = self.performed.lock().unwrap();
        performed.push(id);
        Ok(ExecutionReceipt {
            receipt_id: format!("receipt-{}-{}", envelope.action_id, performed.len()),
            session_id: envelope.session_id.clone(),
            action_id: envelope.action_id.clone(),
            action_digest: envelope.args_digest.clone(),
            attempt: 1,
            status: ReceiptStatus::Committed,
            created_at: None,
            updated_at: None,
            result: None,
            error: None,
        })
    }
    async fn verify(
        &self,
        _: &ExecutionReceipt,
        _: Option<&ActionPostcondition>,
    ) -> Result<VerificationOutcome, ComputerUseError> {
        Ok(VerificationOutcome::Verified)
    }
}

/// The summary `computer_use_preview` returns for an action, which the model echoes.
fn summary(action: &Value) -> Value {
    json!({
        "tool": "computer",
        "operation": action["operation"],
        "actionClass": "edit_reversible",
        "requestedMode": "background",
        "targetApp": if action.get("window").is_some() { json!("app.forms") } else { Value::Null },
        "reversible": true,
        "externalSideEffect": false,
    })
}

fn execute(action: &Value) -> ScriptedResponse {
    ScriptedResponse::function_call(
        COMPUTER_USE_EXECUTE,
        json!({
            "actionDigest": format!("digest-{}", action["id"].as_str().unwrap_or_default()),
            "policyDigest": "policy",
            "action": summary(action),
        }),
    )
}

fn preview_all(actions: &[Value]) -> ScriptedResponse {
    ScriptedResponse::function_call(COMPUTER_USE_PREVIEW, json!({ "actions": actions }))
}

fn preview_one(action: &Value) -> ScriptedResponse {
    ScriptedResponse::function_call(COMPUTER_USE_PREVIEW, json!({ "action": action }))
}

/// A harness with the computer-use pack and no live prompter, so each held call parks, and
/// the surface engine over it.
struct Desk {
    _dir: tempfile::TempDir,
    desktop: Arc<Desktop>,
    harness: Arc<Harness>,
    source: Arc<LocalSource>,
    engine: SurfaceEngine,
    audit: InMemoryAuditRecorder,
    surface_audit: InMemoryAuditRecorder,
}

async fn desk(script: Vec<ScriptedResponse>) -> Desk {
    let desktop = Arc::new(Desktop::default());
    let runtime: Arc<dyn ComputerUseRuntime> = desktop.clone();
    let pack = ComputerUsePack::new(
        runtime,
        ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]),
    );
    let service: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ));
    let dir = tempfile::tempdir().unwrap();
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(Arc::new(ScriptedLlm::new("scripted", script)))
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .pack_computer_use(pack)
            .approval_service(Arc::clone(&service))
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let audit = InMemoryAuditRecorder::new();
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness = Arc::new(Harness::with_runner(agent, runner).unwrap());
    harness.open_session(&user(), Some(&session())).await.unwrap();
    let source = Arc::new(
        LocalSource::builder(APP)
            .approvals(service)
            .harness(Arc::clone(&harness))
            .governance(harness.runner().governance().clone())
            .build()
            .unwrap(),
    );
    let surface_audit = InMemoryAuditRecorder::new();
    let engine = SurfaceEngine::builder(Arc::clone(&source) as Arc<dyn Source>)
        .audit(AuditTrail::new(Arc::new(surface_audit.clone())))
        .build();
    Desk { _dir: dir, desktop, harness, source, engine, audit, surface_audit }
}

fn user() -> UserId {
    UserId::new(OWNER).unwrap()
}

fn session() -> SessionId {
    SessionId::new(SESSION).unwrap()
}

fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn chat() -> Caller {
    Caller::human(OWNER, SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })
        .with_groups(["owner"])
}

impl Desk {
    /// Sends `text` and runs the turn to its end.
    async fn send(&self, text: &str) {
        let stream = self.harness.send(&user(), &session(), text).await.unwrap();
        let events: Vec<_> = futures::StreamExt::collect(stream).await;
        for event in events {
            event.unwrap();
        }
    }

    async fn cards(&self) -> Vec<DecisionCard> {
        self.engine.decisions(&web()).await.unwrap()
    }

    async fn one_card(&self) -> DecisionCard {
        let cards = self.cards().await;
        assert_eq!(cards.len(), 1, "{cards:#?}");
        cards.into_iter().next().unwrap()
    }

    async fn decide(&self, card: &DecisionCard, answer: Answer, expanded: bool) -> CommandOutcome {
        self.engine
            .execute(
                &web(),
                Command::Decide {
                    decision_id: card.id.clone(),
                    version: card.version,
                    answer,
                    edited_args: None,
                    reason: (answer == Answer::Deny).then(|| "not this form".to_string()),
                    expanded,
                    evidence: Vec::new(),
                },
            )
            .await
            .unwrap()
    }

    async fn events(&self) -> Vec<Event> {
        self.source.transcript(OWNER, SESSION).await.unwrap().unwrap()
    }

    /// Every `computer_use_execute` response of the session, in order.
    async fn executions(&self) -> Vec<Value> {
        self.events()
            .await
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. }
                    if function_response.name == COMPUTER_USE_EXECUTE =>
                {
                    Some(function_response.response.clone())
                }
                _ => None,
            })
            .filter(|response| !adk_core::is_confirmation_pending(response))
            .collect()
    }

    /// Waits for the resumed turn to say `done`.
    async fn until_done(&self) {
        for _ in 0..500 {
            let done = self.events().await.iter().any(|event| {
                event.llm_response.content.as_ref().is_some_and(|content| {
                    content.parts.iter().any(|part| part.text() == Some("done"))
                })
            });
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the resumed turn never finished: {:#?}", self.executions().await);
    }

    /// Waits until a card is open again, for the resumed turn's next held call.
    async fn until_card(&self) -> DecisionCard {
        for _ in 0..500 {
            if let Some(card) = self.cards().await.into_iter().next() {
                return card;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no card opened: {:#?}", self.executions().await);
    }

    fn records(&self, action: &str) -> Vec<Value> {
        self.audit
            .records()
            .into_iter()
            .filter(|record| record.action == action)
            .map(|record| Value::Object(record.ext.into_iter().collect()))
            .collect()
    }
}

fn answers(card: &DecisionCard) -> Vec<Answer> {
    card.allowed.options.iter().map(|option| option.answer).collect()
}

/// One card for a four-step plan; one approval runs every step, each with the plan's binding,
/// recorded as the live prompter records it; chat may only deny it; no standing rule follows.
#[tokio::test(flavor = "multi_thread")]
async fn one_card_approves_every_step_of_a_plan_with_its_binding() {
    let name = click("a1", "Customer form", "Name", 100.0);
    let ada = typing("a2", "Ada Lovelace");
    let save = click("a3", "Customer form", "Save", 900.0);
    let note = typing("a4", "copied from the CRM");
    let plan = [name.clone(), ada.clone(), save.clone(), note.clone()];
    let desk = desk(vec![
        preview_all(&plan),
        execute(&name),
        execute(&ada),
        execute(&save),
        execute(&note),
        ScriptedResponse::text("done"),
    ])
    .await;
    desk.send("Fill the customer form.").await;

    let card = desk.one_card().await;
    let view = card.plan.clone().expect("a plan card");
    assert_eq!(view.steps.len(), 4);
    assert!(view.digest.starts_with("sha256:"), "{}", view.digest);
    assert_eq!(view.window, "app.forms, window \"Customer form\"");
    assert_eq!(card.heading, words::screen_plan_heading(4, &view.window));
    let targets: Vec<Option<&str>> = view.steps.iter().map(|step| step.target.as_deref()).collect();
    assert_eq!(
        targets,
        [Some("(100, 300)-(190, 340) pt"), None, Some("(900, 300)-(990, 340) pt"), None]
    );
    assert_eq!(view.steps[0].screenshot.as_deref(), Some("sha256:screen"));
    assert_eq!(view.steps[1].proposed, ada);
    assert!(card.call.text.contains("Ada Lovelace") && !card.call.truncated, "{}", card.call.text);
    assert_eq!(
        card.detail.as_ref().map(|detail| detail.text.as_str()),
        Some(words::SCREEN_PLAN_DETAIL)
    );
    // No standing rule can cover a desktop action, and a plan is never edited.
    assert_eq!(answers(&card), vec![Answer::AllowOnce, Answer::Deny]);
    assert_eq!(card.allowed.options[0].label, words::ALLOW_THIS_PLAN);

    // Chat shows no window or steps, and cannot allow what it cannot show.
    let on_chat = desk.engine.decisions(&chat()).await.unwrap();
    assert_eq!(on_chat.len(), 1);
    assert_eq!(on_chat[0].heading, words::screen_plan_chat_heading(4));
    assert!(on_chat[0].plan.is_none() && on_chat[0].call.args.is_null());
    assert_eq!(answers(&on_chat[0]), vec![Answer::Deny]);
    assert!(on_chat[0].allowed.open_in_home);

    let CommandOutcome::Answered { card: decided } =
        desk.decide(&card, Answer::AllowOnce, false).await
    else {
        panic!("not answered");
    };
    let said = decided.decided.as_ref().unwrap();
    assert_eq!(said.answer, DecidedAnswer::Yes, "{said:?}");
    assert!(decided.plan.is_some());
    desk.until_done().await;

    assert_eq!(desk.desktop.performed(), ["a1", "a2", "a3", "a4"], "each step once, in order");
    let runs = desk.executions().await;
    let bindings: Vec<Value> = runs.iter().map(|run| run["plan"].clone()).collect();
    assert_eq!(
        bindings,
        (1..=4).map(|step| json!({ "digest": view.digest, "step": step })).collect::<Vec<_>>(),
        "{runs:#?}"
    );
    assert!(desk.cards().await.is_empty(), "no further card");

    let decided = desk.records(COMPUTER_USE_AUDIT_PLAN_DECIDED);
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0]["planDigest"], view.digest);
    assert_eq!(decided[0]["answer"], "approve");
    assert_eq!(decided[0]["approvalId"], card.id);
    assert_eq!(decided[0]["steps"], json!(["digest-a1", "digest-a2", "digest-a3", "digest-a4"]));
    assert_eq!(decided[0]["window"]["windowId"], "Customer form");
    let steps: Vec<Value> = desk
        .records(COMPUTER_USE_AUDIT_EXECUTED)
        .iter()
        .map(|record| record["step"].clone())
        .collect();
    assert_eq!(steps, [json!(1), json!(2), json!(3), json!(4)]);
    let answered: Vec<Value> = desk
        .surface_audit
        .records()
        .into_iter()
        .filter(|record| record.action == SURFACE_AUDIT_ANSWER)
        .filter_map(|record| record.ext.get("planDigest").cloned())
        .collect();
    assert_eq!(answered, [json!(view.digest)]);
    let rules = desk.harness.standing_rules().list(APP, OWNER).await.unwrap();
    assert!(rules.is_empty(), "{rules:?}");
}

/// Deny on the card runs none of the plan's steps: the later ones are refused without cards.
#[tokio::test(flavor = "multi_thread")]
async fn a_denied_plan_card_runs_no_step() {
    let name = click("b1", "Customer form", "Name", 100.0);
    let ada = typing("b2", "Ada");
    let desk = desk(vec![
        preview_all(&[name.clone(), ada.clone()]),
        execute(&name),
        execute(&ada),
        ScriptedResponse::text("done"),
    ])
    .await;
    desk.send("Fill the form.").await;
    let card = desk.one_card().await;
    assert!(card.plan.is_some());
    desk.decide(&card, Answer::Deny, false).await;
    desk.until_done().await;

    assert!(desk.desktop.performed().is_empty());
    let runs = desk.executions().await;
    assert_eq!(runs.len(), 2, "{runs:#?}");
    for run in &runs {
        let error = run["error"].as_str().unwrap_or_default();
        assert!(error.contains("not this form"), "{run}");
    }
    assert!(desk.cards().await.is_empty(), "the second step took no card");
    let decided = desk.records(COMPUTER_USE_AUDIT_PLAN_DECIDED);
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0]["answer"], "deny");
}

/// A step whose target moved between the plan's preview and its run is refused, runs nothing,
/// and is held again on a fresh card of its own; the plan's approval answers no later step.
#[tokio::test(flavor = "multi_thread")]
async fn a_deviation_is_held_again_on_a_fresh_card() {
    let name = click("c1", "Customer form", "Name", 100.0);
    let ada = typing("c2", "Ada Lovelace");
    let save = click("c3", "Customer form", "Save", 900.0);
    let note = typing("c4", "copied from the CRM");
    let desk = desk(vec![
        preview_all(&[name.clone(), ada.clone(), save.clone(), note.clone()]),
        execute(&name),
        execute(&ada),
        execute(&save),
        preview_one(&save),
        execute(&save),
        execute(&note),
        ScriptedResponse::text("done"),
    ])
    .await;
    desk.desktop.move_after_preview("c3", 40.0);
    desk.send("Fill the customer form.").await;
    let card = desk.one_card().await;
    let digest = card.plan.as_ref().unwrap().digest.clone();
    desk.decide(&card, Answer::AllowOnce, false).await;

    // The moved Save waits on a card of its own, with no plan on it.
    let fresh = desk.until_card().await;
    assert_ne!(fresh.id, card.id);
    assert!(fresh.plan.is_none(), "{fresh:#?}");
    assert_eq!(fresh.call.args["actionDigest"], "digest-c3");
    assert_eq!(desk.desktop.performed(), ["c1", "c2"]);
    desk.decide(&fresh, Answer::AllowOnce, false).await;

    // The step after it took no answer from the plan.
    let next = desk.until_card().await;
    assert_eq!(next.call.args["actionDigest"], "digest-c4", "{next:#?}");
    assert!(next.plan.is_none());
    desk.decide(&next, Answer::AllowOnce, false).await;
    desk.until_done().await;

    assert_eq!(desk.desktop.performed(), ["c1", "c2", "c3", "c4"], "each action once");
    let runs = desk.executions().await;
    assert_eq!(runs.len(), 5, "{runs:#?}");
    assert_eq!(runs[0]["plan"], json!({ "digest": digest, "step": 1 }));
    assert_eq!(runs[1]["plan"], json!({ "digest": digest, "step": 2 }));
    let error = runs[2]["error"].as_str().unwrap_or_default();
    assert!(error.contains(COMPUTER_USE_PLAN_DEVIATION_CODE), "{}", runs[2]);
    assert!(runs[3].get("plan").is_none(), "the moved Save ran on its own card");
    assert!(runs[4].get("plan").is_none(), "the plan ended at the deviation");
    let refused: Vec<Value> = desk
        .records(COMPUTER_USE_AUDIT_EXECUTED)
        .into_iter()
        .filter(|record| record.get("refusal").is_some())
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["deviation"], "wrongTarget");
    assert_eq!(desk.records(COMPUTER_USE_AUDIT_PLAN_DECIDED).len(), 1);
}

/// A plan too long for its card is approved only once the person showed it in full.
#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_plan_is_approved_only_once_shown_in_full() {
    let mut plan = vec![click("d0", "Customer form", "Notes", 100.0)];
    plan.extend(
        (1..10).map(|i| typing(&format!("d{i}"), &format!("line {i}: {}", "x".repeat(300)))),
    );
    let mut script = vec![preview_all(&plan)];
    script.extend(plan.iter().map(execute));
    script.push(ScriptedResponse::text("done"));
    let desk = desk(script).await;
    desk.send("Paste the notes.").await;

    let card = desk.one_card().await;
    assert_eq!(card.plan.as_ref().unwrap().steps.len(), 10);
    assert!(card.call.truncated, "{}", card.call.text.len());
    let allow = card.allowed.option(Answer::AllowOnce).unwrap();
    assert!(allow.needs_expanded);
    let refused = desk
        .engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: false,
                evidence: Vec::new(),
            },
        )
        .await;
    assert!(matches!(refused, Err(SurfaceError::Rule(RuleViolation::NotExpanded))), "{refused:?}");
    assert_eq!(desk.one_card().await.id, card.id, "the card still waits");
    assert!(desk.records(COMPUTER_USE_AUDIT_PLAN_DECIDED).is_empty());

    let CommandOutcome::Answered { .. } = desk.decide(&card, Answer::AllowOnce, true).await else {
        panic!("not answered once shown in full");
    };
    desk.until_done().await;
    assert_eq!(desk.desktop.performed().len(), 10);
}

/// Stop all closes an open plan card: its held step is cancelled, the plan ends, nothing runs,
/// and previewing again is the way to a fresh card.
#[tokio::test(flavor = "multi_thread")]
async fn stop_all_closes_an_open_plan_card() {
    let name = click("e1", "Customer form", "Name", 100.0);
    let ada = typing("e2", "Ada");
    let desk = desk(vec![
        preview_all(&[name.clone(), ada.clone()]),
        execute(&name),
        ScriptedResponse::text("stopped"),
    ])
    .await;
    desk.send("Fill the form.").await;
    let card = desk.one_card().await;
    assert!(card.plan.is_some());

    desk.engine.execute(&web(), Command::StopAll { reason: None }).await.unwrap();
    assert!(desk.cards().await.is_empty(), "the plan card closed");
    let request = desk.source.request(&card.id).await.unwrap().unwrap();
    assert_eq!(request.state, ApprovalState::Cancelled);
    assert_eq!(request.note.as_deref(), Some(words::STOP_ALL_CLOSED_THE_PLAN));
    let held = desk.harness.held_screen_plan(&request).unwrap();
    assert_eq!(held.state, HeldPlanState::Ended);
    let closed = desk.engine.decision(&web(), &card.id).await.unwrap();
    assert_eq!(closed.decided.unwrap().answer, DecidedAnswer::Ended);
    let stopped: Vec<Value> = desk
        .surface_audit
        .records()
        .into_iter()
        .filter(|record| record.action == SURFACE_AUDIT_STOP_ALL)
        .filter_map(|record| record.ext.get("closedPlans").cloned())
        .collect();
    assert_eq!(stopped, [json!([card.id])]);

    // An answer sent after the stop finds the card closed.
    let late = desk.decide(&card, Answer::AllowOnce, false).await;
    assert!(matches!(late, CommandOutcome::AlreadyDecided { .. }), "{late:?}");
    assert!(desk.desktop.performed().is_empty());
    assert!(desk.records(COMPUTER_USE_AUDIT_PLAN_DECIDED).is_empty());
}
