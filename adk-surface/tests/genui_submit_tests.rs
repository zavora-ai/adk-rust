//! Sending a drawn form (spec 3.3 §7.4 rule 5): a send waits for the thread's running turn,
//! a version is sent once at a time, and the card's "You pressed Send on a form" belongs to
//! the send's own call only.

#![cfg(feature = "genui")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
    ApproverSelector, Event, InMemoryApprovalStore, RequestedCall, SessionId, StoreApprovalService,
    ToolConfirmationRequest, UserId,
};
use adk_harness::{Harness, HarnessAgent, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::genui::DrawToolset;
use adk_surface::{
    Caller, Command, CommandOutcome, LocalSource, Origin, Source, SurfaceEngine, SurfaceError,
    SurfaceKind,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Map, Value, json};

const APP: &str = "home";
const SESSION: &str = "thread-1";

/// A source over the session Finance drew in: its busy flag is set by the test, and every
/// call asked for is kept, never run.
struct Sending {
    inner: LocalSource,
    busy: AtomicBool,
    asked: Mutex<Vec<RequestedCall>>,
}

#[async_trait]
impl Source for Sending {
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        self.inner.transcript(user, session).await
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        self.inner.requests(caller, session, open_only).await
    }

    async fn request(&self, id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        self.inner.request(id).await
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        self.inner.decide(id, expected_version, decision).await
    }

    async fn busy(&self, _user: &str, _session: &str) -> adk_surface::Result<bool> {
        Ok(self.busy.load(Ordering::SeqCst))
    }

    async fn request_call(
        &self,
        _caller: &Caller,
        _session: &str,
        _text: &str,
        call: RequestedCall,
    ) -> adk_surface::Result<()> {
        self.asked.lock().unwrap().push(call);
        Ok(())
    }
}

/// `send_invoice`, which the drawn forms ask for; a send asks the person first, so it never
/// runs here.
struct SendInvoice;

#[async_trait::async_trait]
impl adk_core::Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends an invoice."
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        _args: Value,
    ) -> adk_core::Result<Value> {
        panic!("send_invoice never runs here");
    }
}

struct Home {
    engine: SurfaceEngine,
    source: Arc<Sending>,
    approvals: Arc<dyn ApprovalService>,
    audit: InMemoryAuditRecorder,
    _dir: tempfile::TempDir,
}

/// Finance's invoice form, drawn in `thread-1` by a harness agent, and an engine over it.
async fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let form = json!({
        "surfaceId": "invoice-10",
        "title": "Invoice INV-10",
        "fields": [{ "name": "to", "label": "To", "type": "email", "required": true }],
        "actions": [{
            "id": "send",
            "label": "Send",
            "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "invoice": "INV-10" }
        }]
    });
    let agent = HarnessAgent::builder("finance")
        .model(Arc::new(ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("render_form", form),
                ScriptedResponse::text("drawn"),
            ],
        )))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .without_compaction()
        .toolset(Arc::new(DrawToolset::forms()))
        .tool(Arc::new(SendInvoice))
        .build()
        .unwrap();
    let agent = Arc::new(agent);
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let user = UserId::new("ana").unwrap();
    let session =
        harness.open_session(&user, Some(&SessionId::new(SESSION).unwrap())).await.unwrap();
    let stream = harness.send(&user, &session, "draw the invoice form").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("the drawing turn must finish");

    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let inner = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(Arc::clone(&approvals))
        .build()
        .unwrap();
    let source =
        Arc::new(Sending { inner, busy: AtomicBool::new(false), asked: Mutex::new(Vec::new()) });
    let audit = InMemoryAuditRecorder::new();
    let engine = SurfaceEngine::builder(Arc::clone(&source) as Arc<dyn Source>)
        .audit(adk_core::audit_log::AuditTrail::new(Arc::new(audit.clone())))
        .agent_name(APP, "Finance")
        .build();
    Home { engine, source, approvals, audit, _dir: dir }
}

fn web() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn send(to: &str) -> Command {
    let mut values = Map::new();
    values.insert("/to".into(), json!(to));
    Command::SubmitDrawn {
        session_id: SESSION.into(),
        surface_id: "invoice-10".into(),
        version: 1,
        action_id: "send".into(),
        values,
    }
}

/// The request a held call of `tool` with `args` makes in the thread, with ID `id`.
fn request(id: &str, call_id: &str, args: Value) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new("send_invoice", args).with_function_call_id(call_id);
    ApprovalRequest::new(
        id,
        ApprovalScope::new(APP, "ana", SESSION),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
}

/// A form sent while the thread's turn runs waits for it, as a message does: the call is
/// not asked for and nothing is recorded, so its run never writes the session beside the
/// turn's (S-I4).
#[tokio::test]
async fn a_send_waits_for_the_running_turn() {
    let home = home().await;
    home.source.busy.store(true, Ordering::SeqCst);

    let refused = home.engine.execute(&web(), send("ana@example.com")).await.unwrap_err();

    assert!(
        matches!(&refused, SurfaceError::Invalid(why) if why.contains("still working")),
        "{refused}"
    );
    assert!(home.source.asked.lock().unwrap().is_empty(), "no call is asked for");
    assert!(home.audit.records().is_empty(), "nothing is recorded");

    home.source.busy.store(false, Ordering::SeqCst);
    let sent = home.engine.execute(&web(), send("ana@example.com")).await.unwrap();
    assert!(matches!(sent, CommandOutcome::FormSent { .. }));
}

/// A second send of the same version while the first is on its way is refused, so one press
/// and a quick second press hold one call, not two.
#[tokio::test]
async fn a_version_is_sent_once_at_a_time() {
    let home = home().await;

    home.engine.execute(&web(), send("ana@example.com")).await.unwrap();
    let again = home.engine.execute(&web(), send("ana@example.com")).await.unwrap_err();

    assert!(
        matches!(&again, SurfaceError::Invalid(why) if why.contains("being sent already")),
        "{again}"
    );
    assert_eq!(home.source.asked.lock().unwrap().len(), 1, "one call is asked for");
}

/// The record of a send carries digests and never the values: no field holds what the
/// person typed, inside a string or out.
#[tokio::test]
async fn the_record_of_a_send_holds_no_typed_value() {
    let home = home().await;

    home.engine.execute(&web(), send("ana@example.com")).await.unwrap();

    let records: Vec<_> = home
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == "surface.drawn.submitted")
        .collect();
    assert_eq!(records.len(), 1);
    let written = serde_json::to_string(&records[0].ext).unwrap();
    assert!(!written.contains("ana@example.com"), "{written}");
    assert!(records[0].ext.get("argsDigest").and_then(Value::as_str).is_some(), "{written}");
    assert!(!records[0].ext.contains_key("fingerprint"), "{written}");
}

/// "You pressed Send on a form" belongs to the send's own call: a later call of the same
/// tool with the same arguments, which the model made, never borrows it.
#[tokio::test]
async fn a_later_identical_call_is_not_the_persons_send() {
    let home = home().await;
    home.engine.execute(&web(), send("ana@example.com")).await.unwrap();
    let call = home.source.asked.lock().unwrap()[0].clone();

    home.approvals
        .submit(request("r-sent", &call.function_call_id, call.args.clone()))
        .await
        .unwrap();
    let cards = home.engine.session_decisions(&web(), SESSION).await.unwrap();
    assert!(
        cards[0].timeline[0].text.starts_with("You pressed Send on a form Finance drew · "),
        "{:?}",
        cards[0].timeline
    );

    home.approvals.submit(request("r-model", "toolu_01", call.args.clone())).await.unwrap();
    let cards = home.engine.session_decisions(&web(), SESSION).await.unwrap();
    let model = cards.iter().find(|card| card.id == "r-model").expect("the model's card");
    assert!(
        !model.timeline.iter().any(|step| step.text.starts_with("You pressed Send")),
        "{:?}",
        model.timeline
    );
}
