//! Spec 3.3 §12 developer track "Governed form" (U1): a kernel-hosted harness agent, Finance,
//! draws a form whose Send asks for `send_invoice`; the person sends it from the web home,
//! the call is held as Finance in its own kernel target, and nothing runs until they decide.
//!
//! | Step | What happens | What holds |
//! |---|---|---|
//! | 1 Draw | Finance draws two invoice forms, then a screen with "Allow once" and "Approved" | Two drawings, version 1, by Finance, each Send "Sending runs send_invoice and asks you first"; the screen is a plain notice; no decision; the render tools asked nobody |
//! | 4 Forced | Go ahead, a policy that allows `send_invoice` and a confirmation handler that approves everything, set before anything is sent | — |
//! | 2 Submit | The person types the address and presses Send | One decision for `send_invoice` with the typed values and `invoice` INV-10 from the stored form, still waiting under step 4's conditions; its timeline starts "You pressed Send on a form Finance drew"; no Always allow; the chat card carries no arguments; nothing ran; `surface.drawn.submitted` recorded (web, pointer) |
//! | 3 Tampering | Version 0, a forged INV-99, action `approve`, another person's session, an agent's, a chat's and a spoken send | `surface.drawn_changed`, `surface.invalid`, `surface.invalid`, `surface.not_found`, `surface.not_here`; still one decision, same version |
//! | 5 Restart | The engine and kernel are dropped and rebuilt over the same SQLite stores | Both decisions still wait; the forms say "Sent at … · waiting for you" |
//! | 6 Decide on the phone | Allow once on the first, Deny on its twin | `send_invoice` runs exactly once with the card's arguments: "Sent · done"; the denied twin never runs: "Sent · you said no"; the records name their own surfaces |
//!
//! Step 4's conditions are set before the first send: a mode loosened while a call waits
//! ends that call (spec 3.1 §9.2 I14).
//!
//! The model is `ScriptedLlm`; the kernel, its durable worker, the harness, the approval
//! service, the sessions, the runs, the approvals and the processes are real, on SQLite. The
//! surface's audit trail is in memory, shared across the restart. Areas and added agents run
//! in adk-cli's `genui_up` test; the terminal's fill-in pane in the terminal's own tests.

#![cfg(all(feature = "genui", feature = "web"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, DeclarativePolicy, DurableRunStore, Llm,
    PolicyDecision, RequestContext, Result as AdkResult, StoreApprovalService, Tool,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest, ToolContext,
    ToolEffect, ToolPolicy, ToolPolicyRequest,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{Kernel, KernelConfig, ProcessStore};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::SessionService;
use adk_surface::genui::{DrawToolset, FormStatus, RefusalKind};
use adk_surface::{
    Answer, Caller, Command, CommandOutcome, DecisionCard, LocalSource, Mode, Origin, Place,
    SurfaceEngine, SurfaceError, SurfaceKind, ThreadItem,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "home";
const OWNER: &str = "ana";
const WITHIN: Duration = Duration::from_secs(20);

/// `send_invoice`: it records every call it runs.
struct SendInvoice {
    runs: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends an invoice to a client."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "to": { "type": "string" },
                "amount": { "type": "number" },
                "invoice": { "type": "string" }
            },
            "required": ["to", "amount", "invoice"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.runs.lock().unwrap().push(args);
        Ok(json!({ "sent": true }))
    }
}

/// Says yes to every call it is asked about.
#[derive(Debug)]
struct YesToEverything;

#[async_trait]
impl ToolConfirmationHandler for YesToEverything {
    async fn decide(
        &self,
        _request: &ToolConfirmationRequest,
    ) -> AdkResult<ToolConfirmationDecision> {
        Ok(ToolConfirmationDecision::Approve)
    }
}

/// The operator's rule: `wipe_invoices` never runs, and `send_invoice` is allowed until
/// `tightened` says it is denied.
#[derive(Debug)]
struct InvoiceRule {
    tightened: Arc<AtomicBool>,
}

#[async_trait]
impl ToolPolicy for InvoiceRule {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        if request.tool_name == "wipe_invoices"
            || (request.tool_name == "send_invoice" && self.tightened.load(Ordering::SeqCst))
        {
            return PolicyDecision::deny("not on this home");
        }
        PolicyDecision::Allow
    }
}

/// What lives across a restart: the database, the record and what the tool ran.
struct Lasting {
    dir: tempfile::TempDir,
    audit: InMemoryAuditRecorder,
    runs: Arc<Mutex<Vec<Value>>>,
    tightened: Arc<AtomicBool>,
}

impl Lasting {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            audit: InMemoryAuditRecorder::new(),
            runs: Arc::new(Mutex::new(Vec::new())),
            tightened: Arc::new(AtomicBool::new(false)),
        }
    }

    fn ran(&self) -> Vec<Value> {
        self.runs.lock().unwrap().clone()
    }
}

/// The kernel-hosted harness Finance over SQLite in `lasting`, the engine over it, and the
/// web home's routes; `forced` adds a policy that allows `send_invoice` and a confirmation
/// handler that approves everything.
struct Home {
    kernel: Kernel,
    engine: SurfaceEngine,
    api: axum::Router,
}

async fn home(lasting: &Lasting, model: Arc<dyn Llm>, forced: bool) -> Home {
    home_with(lasting, model, forced, ControlPlaneConfig::default().without_socket()).await
}

async fn home_with(
    lasting: &Lasting,
    model: Arc<dyn Llm>,
    forced: bool,
    control: ControlPlaneConfig,
) -> Home {
    let path = lasting.dir.path().join("harness.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let sqlite = adk_session::SqliteSessionService::new(&url).await.unwrap();
    sqlite.migrate().await.unwrap();
    let pool = sqlite.pool().clone();
    let approvals = adk_session::SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await.unwrap();
    let runs = adk_session::SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await.unwrap();
    let processes = adk_kernel::SqliteProcessStore::new(pool.clone());
    processes.migrate().await.unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(runs);
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(approvals))
            .with_run_store(Arc::clone(&runs))
            .with_router(
                ApprovalPolicy::new([
                    ApproverSelector::group("owner"),
                    ApproverSelector::group("operator"),
                ])
                .allow_self_approval(),
            ),
    );
    let workspace = lasting.dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut agent = HarnessAgent::builder("finance")
        .model(model)
        .workspace(adk_devtools::Workspace::new(&workspace))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .without_compaction()
        .without_default_members()
        .without_shell()
        .mode(HarnessMode::Ask)
        .approval_service(Arc::clone(&service))
        .tool(Arc::new(SendInvoice { runs: Arc::clone(&lasting.runs) }))
        .tool(Arc::new(WipeInvoices))
        .toolset(Arc::new(DrawToolset::forms()))
        .add_operator_policy(Arc::new(InvoiceRule { tightened: Arc::clone(&lasting.tightened) }));
    if forced {
        agent = agent
            .add_operator_policy(Arc::new(
                DeclarativePolicy::builder().allow("send_invoice").allow_read_only().build(),
            ))
            .confirmation_handler(Arc::new(YesToEverything));
    }
    let agent = Arc::new(agent.build().unwrap());
    let sessions: Arc<dyn SessionService> = Arc::new(sqlite);
    let runner = agent
        .configure_runner(
            Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(Arc::clone(&sessions)),
        )
        .build()
        .unwrap();
    let harness =
        Arc::new(Harness::with_runner(agent, runner).unwrap().with_run_store(Arc::clone(&runs)));
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_millis(600))
                        .with_poll_interval(Duration::from_millis(20)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(control),
        )
        .process_store(Arc::new(processes) as Arc<dyn ProcessStore>)
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .harness(harness)
        .kernel(kernel.clone(), APP)
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(lasting.audit.clone())))
        .places([Place::web(), Place::phone(), Place::chat("Telegram")])
        .agent_name(APP, "Finance")
        .poll_interval(Duration::from_millis(20))
        .build();
    let routes = adk_kernel::control::router(kernel.clone())
        .merge(adk_surface::web::router(engine.clone()))
        .merge(adk_surface::web::genui::router(engine.clone()));
    let api =
        axum::Router::new().nest("/api", routes).layer(axum::Extension(Some(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into(), "operator".into()],
            metadata: Default::default(),
        })));
    Home { kernel, engine, api }
}

/// `wipe_invoices`: the operator's policy never lets it run.
struct WipeInvoices;

#[async_trait]
impl Tool for WipeInvoices {
    fn name(&self) -> &str {
        "wipe_invoices"
    }

    fn description(&self) -> &str {
        "Deletes every invoice."
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> AdkResult<Value> {
        panic!("wipe_invoices never runs");
    }
}

fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn phone() -> Caller {
    Caller::human(OWNER, SurfaceKind::Phone, Origin::Touch).with_groups(["owner"])
}

fn telegram() -> Caller {
    Caller::human(OWNER, SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })
        .with_groups(["owner"])
}

async fn eventually<T, F, Fut>(what: &str, probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    eventually_within(what, WITHIN, probe).await
}

async fn eventually_within<T, F, Fut>(what: &str, within: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= within {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn call(
    api: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = api.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn invoice_form(surface: &str, invoice: &str) -> Value {
    json!({
        "surfaceId": surface,
        "title": format!("Invoice {invoice}"),
        "fields": [
            { "name": "to", "label": "To", "type": "email", "required": true },
            { "name": "amount", "label": "Amount", "type": "number", "value": 4200 },
            { "name": "due", "label": "Due date", "type": "date", "value": "2026-11-15" },
            { "name": "copy", "label": "Send a copy to me", "type": "checkbox" }
        ],
        "actions": [{
            "id": "send",
            "label": "Send",
            "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "amount": { "path": "/amount" }, "invoice": invoice }
        }]
    })
}

/// A screen with an "Allow once" button and an "Approved" line: never shown.
fn fake_decision() -> Value {
    json!({
        "surfaceId": "approve-it",
        "components": [
            { "id": "root", "component": "Card", "child": "body" },
            { "id": "body", "component": "Column", "children": ["c7", "c9"] },
            { "id": "c7", "component": "Button", "child": "c7-label", "action": { "event": { "name": "go" } } },
            { "id": "c7-label", "component": "Text", "text": "Allow once" },
            { "id": "c9", "component": "Text", "text": "Approved" }
        ],
        "actions": [{ "id": "go", "label": "Go", "tool": "send_invoice", "args": {} }]
    })
}

/// The drawings, refusals and sends of the session `s1`.
async fn items(engine: &SurfaceEngine, caller: &Caller) -> Vec<ThreadItem> {
    engine.thread(caller, "s1").await.unwrap().into_iter().flat_map(|entry| entry.items).collect()
}

/// The open decisions of `s1`, once `count` wait.
async fn waiting(engine: &SurfaceEngine, count: usize) -> Vec<DecisionCard> {
    let started = tokio::time::Instant::now();
    loop {
        let cards = engine.session_decisions(&web(), "s1").await.unwrap();
        if cards.len() == count {
            return cards;
        }
        if started.elapsed() > WITHIN {
            panic!(
                "{} decisions wait, not {count}; the thread: {:#?}",
                cards.len(),
                items(engine, &web()).await
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until no turn of `s1` runs.
async fn settled(home: &Home) {
    eventually("the turn to end", || async {
        (!home.engine.source().busy(OWNER, "s1").await.unwrap()).then_some(())
    })
    .await;
}

fn submit(surface: &str, version: u64, action: &str, values: Value) -> Command {
    Command::SubmitDrawn {
        session_id: "s1".into(),
        surface_id: surface.into(),
        version,
        action_id: action.into(),
        values: values.as_object().cloned().unwrap_or_default(),
    }
}

fn code(error: SurfaceError) -> String {
    error.code()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drawn_form_is_sent_held_as_finance_and_runs_once_on_a_yes() {
    let lasting = Lasting::new();
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("render_form", invoice_form("invoice-10", "INV-10")),
            ScriptedResponse::function_call("render_form", invoice_form("invoice-11", "INV-11")),
            ScriptedResponse::function_call("render_screen", fake_decision()),
            ScriptedResponse::text(
                "Here is the invoice. Check the details and press Send when it is right.",
            ),
        ],
    ));
    // Step 4's conditions from the start: Go ahead, a policy that allows `send_invoice` and a
    // handler that says yes to everything. (Loosening the mode while a call waits would end
    // it, spec 3.1 §9.2 I14.)
    let home1 = home(&lasting, model, true).await;

    // 1. Draw.
    let sent = home1
        .engine
        .execute(
            &web(),
            Command::Send {
                session_id: "s1".into(),
                text: "Prepare the October invoice for ACME".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "s1".into() });
    eventually("the drawings and the refusal", || async {
        let items = items(&home1.engine, &web()).await;
        items
            .iter()
            .any(|item| {
                matches!(item, ThreadItem::Message { text, .. } if text.starts_with("Here is the invoice"))
            })
            .then_some(())
    })
    .await;
    settled(&home1).await;
    let drawn = items(&home1.engine, &web()).await;
    let surfaces: Vec<_> = drawn
        .iter()
        .filter_map(|item| match item {
            ThreadItem::Drawn { surface } => Some(surface.as_ref().clone()),
            _ => None,
        })
        .collect();
    assert_eq!(surfaces.len(), 2, "{drawn:#?}");
    let form = &surfaces[0];
    assert_eq!(
        (
            form.surface_id.as_str(),
            form.version,
            form.catalog.as_str(),
            form.agent.display_name.as_str()
        ),
        ("invoice-10", 1, "forms 1.0", "Finance")
    );
    assert_eq!(form.title.as_deref(), Some("Invoice INV-10"));
    assert_eq!(form.actions[0].note, "Sending runs send_invoice and asks you first");
    assert!(form.drawn_text.starts_with("Drawn by Finance · "));
    let refusals: Vec<_> = drawn
        .iter()
        .filter_map(|item| match item {
            ThreadItem::DrawnRefused { refusal } => Some(refusal.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(refusals.len(), 1, "{drawn:#?}");
    assert_eq!(refusals[0].kind, RefusalKind::LooksLikeDecision);
    assert_eq!(
        refusals[0].text,
        "Finance tried to show something that looks like a decision. It wasn't shown."
    );
    assert!(refusals[0].reason.contains("reserved word \"Allow once\""), "{}", refusals[0].reason);
    assert!(home1.engine.decisions(&web()).await.unwrap().is_empty(), "drawing asked nobody");
    home1
        .engine
        .execute(&web(), Command::SetMode { session_id: "s1".into(), mode: Mode::GoAhead })
        .await
        .unwrap();

    // 2. Submit from the web home, by pointer: it waits, whatever would answer for the person.
    let body = json!({
        "surface": "web",
        "origin": { "kind": "pointer" },
        "command": {
            "type": "submitDrawn", "sessionId": "s1", "surfaceId": "invoice-10", "version": 1,
            "actionId": "send", "values": { "/to": "ana@example.com", "/amount": 4200 }
        }
    });
    let (status, outcome) = call(&home1.api, "POST", "/api/surface/commands", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["type"], "formSent");
    let cards = waiting(&home1.engine, 1).await;
    let card = &cards[0];
    assert_eq!(card.call.tool, "send_invoice");
    assert_eq!(
        card.call.args,
        json!({ "to": "ana@example.com", "amount": 4200, "invoice": "INV-10" })
    );
    assert_eq!(card.agent.display_name, "Finance");
    assert!(
        card.timeline[0].text.starts_with("You pressed Send on a form Finance drew · "),
        "{:?}",
        card.timeline
    );
    assert_eq!(
        card.detail.as_ref().map(|detail| detail.text.as_str()),
        Some("From a form Finance drew")
    );
    let answers: Vec<Answer> = card.allowed.options.iter().map(|option| option.answer).collect();
    assert!(!answers.contains(&Answer::AlwaysAllow), "{answers:?}");
    assert!(answers.contains(&Answer::AllowOnce));
    let chat = &home1.engine.session_decisions(&telegram(), "s1").await.unwrap()[0];
    assert_eq!(chat.call.args, Value::Null, "a chat card carries no arguments");
    let on_phone = &home1.engine.session_decisions(&phone(), "s1").await.unwrap()[0];
    assert_eq!(on_phone.timeline[0], card.timeline[0]);
    assert!(lasting.ran().is_empty(), "nothing ran");
    let submitted: Vec<_> = lasting
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == "surface.drawn.submitted")
        .collect();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].ext.get("surface"), Some(&json!("web")));
    assert_eq!(submitted[0].ext.get("origin"), Some(&json!({ "kind": "pointer" })));
    assert_eq!(submitted[0].ext.get("tool"), Some(&json!("send_invoice")));
    assert!(
        !submitted[0].ext.values().any(|value| value == "ana@example.com"),
        "the record carries a digest of the values"
    );
    let (_, forms) = call(&home1.api, "GET", "/api/surface/genui/sessions/s1/forms", None).await;
    assert_eq!(forms["forms"][0]["status"], "waiting", "{forms}");
    assert!(forms["forms"][0]["text"].as_str().unwrap().ends_with(" · waiting for you"));

    // 3. Tampering and origin.
    let refused = |command| {
        let engine = home1.engine.clone();
        async move { engine.execute(&web(), command).await.map_err(code) }
    };
    assert_eq!(
        refused(submit("invoice-10", 0, "send", json!({ "/to": "ana@example.com" }))).await,
        Err("surface.drawn_changed".into())
    );
    assert_eq!(
        refused(submit(
            "invoice-10",
            1,
            "send",
            json!({ "/to": "ana@example.com", "/invoice": "INV-99" })
        ))
        .await,
        Err("surface.invalid".into())
    );
    assert_eq!(
        refused(submit("invoice-10", 1, "approve", json!({}))).await,
        Err("surface.invalid".into())
    );
    let bob = Caller::human("bob", SurfaceKind::Web, Origin::Pointer);
    assert_eq!(
        home1.engine.execute(&bob, submit("invoice-10", 1, "send", json!({}))).await.map_err(code),
        Err("surface.not_found".into())
    );
    for origin in [
        Origin::Agent { agent: "finance".into() },
        Origin::Chat { channel: "Telegram".into() },
        Origin::Voice,
    ] {
        let caller = Caller::human(OWNER, SurfaceKind::Web, origin.clone());
        assert_eq!(
            home1
                .engine
                .execute(&caller, submit("invoice-10", 1, "send", json!({})))
                .await
                .map_err(code),
            Err("surface.not_here".into()),
            "{origin:?}"
        );
    }
    let still = waiting(&home1.engine, 1).await;
    assert_eq!((still[0].id.as_str(), still[0].version), (card.id.as_str(), card.version));

    // The twin, sent from the second form.
    home1
        .engine
        .execute(&web(), submit("invoice-11", 1, "send", json!({ "/to": "bo@example.com" })))
        .await
        .unwrap();
    let cards = waiting(&home1.engine, 2).await;
    assert!(lasting.ran().is_empty(), "the press is never the approval");
    home1.kernel.stop().await.unwrap();
    drop(home1);

    // 5. Restart over the same stores.
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![ScriptedResponse::text("Sent."), ScriptedResponse::text("Not sent.")],
    ));
    let home3 = home(&lasting, model, false).await;
    let after = waiting(&home3.engine, 2).await;
    assert_eq!(
        after.iter().map(|card| (&card.id, card.version)).collect::<Vec<_>>(),
        cards.iter().map(|card| (&card.id, card.version)).collect::<Vec<_>>()
    );
    // The cards say how they came about from the session itself, after the restart too.
    for card in &after {
        assert!(
            card.timeline[0].text.starts_with("You pressed Send on a form Finance drew · "),
            "{:?}",
            card.timeline
        );
        assert!(!card.allowed.options.iter().any(|option| option.answer == Answer::AlwaysAllow));
    }
    let (_, forms) = call(&home3.api, "GET", "/api/surface/genui/sessions/s1/forms", None).await;
    let statuses: Vec<&str> = forms["forms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|form| form["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["waiting", "waiting"], "{forms}");

    // 6. Decide on the phone: Allow once on the first, Deny on the twin.
    let first = after.iter().find(|card| card.call.args["invoice"] == "INV-10").unwrap();
    let twin = after.iter().find(|card| card.call.args["invoice"] == "INV-11").unwrap();
    for (card, answer) in [(first, Answer::AllowOnce), (twin, Answer::Deny)] {
        home3
            .engine
            .execute(
                &phone(),
                Command::Decide {
                    decision_id: card.id.clone(),
                    version: card.version,
                    answer,
                    edited_args: None,
                    reason: None,
                    expanded: true,
                },
            )
            .await
            .unwrap();
    }
    let forms = eventually("both forms to end", || async {
        let (_, forms) =
            call(&home3.api, "GET", "/api/surface/genui/sessions/s1/forms", None).await;
        let ended = forms["forms"].as_array()?.iter().all(|form| {
            form["status"] == json!(FormStatus::Done) || form["status"] == json!(FormStatus::SaidNo)
        });
        ended.then_some(forms)
    })
    .await;
    let by_surface: Vec<(&str, &str)> = forms["forms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|form| (form["surfaceId"].as_str().unwrap(), form["status"].as_str().unwrap()))
        .collect();
    assert_eq!(by_surface, [("invoice-10", "done"), ("invoice-11", "saidNo")], "{forms}");
    assert_eq!(forms["forms"][1]["text"], "Sent · you said no");
    assert!(forms["forms"][0]["text"].as_str().unwrap().starts_with("Sent · done at "));
    settled(&home3).await;
    assert_eq!(
        lasting.ran(),
        [json!({ "to": "ana@example.com", "amount": 4200, "invoice": "INV-10" })],
        "the allowed call ran once with the card's arguments; the denied one never ran"
    );
    let surfaces: Vec<(String, Value)> = lasting
        .audit
        .records()
        .into_iter()
        .filter(|record| {
            record.action == "surface.drawn.submitted" || record.action == "surface.decide"
        })
        .map(|record| {
            (record.action.clone(), record.ext.get("surface").cloned().unwrap_or(Value::Null))
        })
        .collect();
    assert_eq!(
        surfaces,
        [
            ("surface.drawn.submitted".to_string(), json!("web")),
            ("surface.drawn.submitted".to_string(), json!("web")),
            ("surface.decide".to_string(), json!("phone")),
            ("surface.decide".to_string(), json!("phone")),
        ]
    );
    home3.kernel.stop().await.unwrap();
}

/// A form bound to a tool the operator's policy denies makes no decision, and says so; a rule
/// tightened after a form was sent refuses its call when it resumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_drawing_agents_rules_refuse_a_sent_form_before_and_after_it_is_held() {
    let lasting = Lasting::new();
    let mut wipe = invoice_form("wipe", "INV-10");
    wipe["title"] = json!("Clean up");
    wipe["actions"] =
        json!([{ "id": "wipe", "label": "Clean up", "tool": "wipe_invoices", "args": {} }]);
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("render_form", invoice_form("invoice-10", "INV-10")),
            ScriptedResponse::function_call("render_form", wipe),
            ScriptedResponse::text("Both forms are ready."),
            ScriptedResponse::text("It was not sent."),
        ],
    ));
    let home = home(&lasting, model, false).await;
    home.engine
        .execute(&web(), Command::Send { session_id: "s1".into(), text: "Prepare them".into() })
        .await
        .unwrap();
    eventually("both forms", || async {
        let items = items(&home.engine, &web()).await;
        (items.iter().filter(|item| matches!(item, ThreadItem::Drawn { .. })).count() == 2)
            .then_some(())
    })
    .await;
    settled(&home).await;

    // Denied by the policy: no decision, and the form can be sent again.
    home.engine
        .execute(
            &web(),
            Command::SubmitDrawn {
                session_id: "s1".into(),
                surface_id: "wipe".into(),
                version: 1,
                action_id: "wipe".into(),
                values: json!({ "/to": "ana@example.com" }).as_object().cloned().unwrap(),
            },
        )
        .await
        .unwrap();
    let (_, forms) = eventually("the refusal", || async {
        let (status, forms) =
            call(&home.api, "GET", "/api/surface/genui/sessions/s1/forms", None).await;
        (forms["forms"].as_array().is_some_and(|forms| !forms.is_empty()))
            .then_some((status, forms))
    })
    .await;
    assert_eq!(forms["forms"][0]["status"], "refused", "{forms}");
    assert_eq!(forms["forms"][0]["text"], "Finance can't use wipe_invoices here");
    assert_eq!(forms["forms"][0]["locked"], false);
    settled(&home).await;
    assert!(home.engine.decisions(&web()).await.unwrap().is_empty(), "no inbox entry");

    // Held, then the rule tightens before the answer: refused when it resumes.
    home.engine
        .execute(&web(), submit("invoice-10", 1, "send", json!({ "/to": "ana@example.com" })))
        .await
        .unwrap();
    let card = waiting(&home.engine, 1).await.remove(0);
    lasting.tightened.store(true, Ordering::SeqCst);
    home.engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    let forms = eventually("the refusal at resume", || async {
        let (_, forms) = call(&home.api, "GET", "/api/surface/genui/sessions/s1/forms", None).await;
        let invoice = forms["forms"]
            .as_array()?
            .iter()
            .find(|form| form["surfaceId"] == "invoice-10")?
            .clone();
        (invoice["status"] == "notRun").then_some(invoice)
    })
    .await;
    assert_eq!(forms["text"], "Not run: Finance can't use send_invoice any more");
    assert!(lasting.ran().is_empty());
    home.kernel.stop().await.unwrap();
}

/// The user the kernel's loopback HTTP control plane authenticates callers as.
fn os_user() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

/// Spec 3.3 §5.2: the terminal attached to the kernel's control plane sends a drawn form, and
/// it waits as Finance like any other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_terminal_over_the_control_plane_sends_a_drawn_form() {
    let lasting = Lasting::new();
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("render_form", invoice_form("invoice-10", "INV-10")),
            ScriptedResponse::text("Here is the invoice."),
        ],
    ));
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap());
    let home = home_with(&lasting, model, false, control).await;
    let source = adk_surface::ControlSource::new(
        adk_kernel::control::ControlClient::http(format!("http://127.0.0.1:{port}")),
        APP,
        APP,
    );
    let remote =
        SurfaceEngine::builder(Arc::new(source)).poll_interval(Duration::from_millis(20)).build();
    let terminal = Caller::human(os_user(), SurfaceKind::Terminal, Origin::Keyboard);

    remote
        .execute(&terminal, Command::Send { session_id: "t1".into(), text: "Prepare it".into() })
        .await
        .unwrap();
    let version = eventually("the form over the control plane", || async {
        let thread = remote.thread(&terminal, "t1").await.ok()?;
        thread.into_iter().flat_map(|entry| entry.items).find_map(|item| match item {
            ThreadItem::Drawn { surface } => Some(surface.version),
            _ => None,
        })
    })
    .await;
    eventually("the drawing turn to end", || async {
        (!remote.source().busy(terminal.user_id(), "t1").await.unwrap()).then_some(())
    })
    .await;
    let outcome = remote
        .execute(
            &terminal,
            Command::SubmitDrawn {
                session_id: "t1".into(),
                surface_id: "invoice-10".into(),
                version,
                action_id: "send".into(),
                values: json!({ "/to": "ana@example.com" }).as_object().cloned().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::FormSent { .. }), "{outcome:?}");
    let card = eventually("one waiting card", || async {
        remote.session_decisions(&terminal, "t1").await.unwrap().into_iter().next()
    })
    .await;
    assert_eq!(card.call.tool, "send_invoice");
    assert_eq!(card.agent.name, "finance", "held as Finance");
    assert!(card.timeline[0].text.starts_with("You pressed Send on a form Finance drew · "));
    let offered: Vec<Answer> = card.allowed.options.iter().map(|option| option.answer).collect();
    assert!(!offered.contains(&Answer::AlwaysAllow), "{offered:?}");
    assert!(lasting.ran().is_empty());
    home.kernel.stop().await.unwrap();
}

/// How long a live model may take to draw or to answer.
const LIVE: Duration = Duration::from_secs(180);

/// The live model: `GENUI_LIVE_PROVIDER` picks `anthropic` (`claude-sonnet-5-5`, the
/// default), `openai` (`gpt-5.6-terra` on the Responses API), `gemini` (`gemini-3.7-flash`,
/// with `GEMINI_API_KEY` or `GOOGLE_API_KEY`) or `openrouter` (`google/gemini-3.7-flash`), and
/// `GENUI_LIVE_MODEL` another model; the key is read from the repository's `.env` or the file
/// `ADK_ENV_FILE` names.
fn live_model() -> Arc<dyn Llm> {
    let env = std::env::var_os("ADK_ENV_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    if env.exists() {
        dotenvy::from_path_override(&env).expect("the .env file loads");
    }
    let key = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| std::env::var(name).ok())
            .unwrap_or_else(|| panic!("one of {names:?}"))
    };
    let provider = std::env::var("GENUI_LIVE_PROVIDER").unwrap_or_default();
    let model = |default: &str| std::env::var("GENUI_LIVE_MODEL").unwrap_or(default.to_string());
    match provider.as_str() {
        "gemini" => Arc::new(
            adk_model::GeminiModel::new(
                key(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]),
                model("gemini-3.7-flash"),
            )
            .unwrap(),
        ),
        "openai" => Arc::new(
            adk_model::openai::OpenAIResponsesClient::new(
                adk_model::openai::OpenAIResponsesConfig::new(
                    key(&["OPENAI_API_KEY"]),
                    model("gpt-5.6-terra"),
                ),
            )
            .unwrap(),
        ),
        "openrouter" => Arc::new(
            adk_model::OpenAIClient::compatible(
                key(&["OPENROUTER_API_KEY"]),
                "https://openrouter.ai/api/v1",
                model("google/gemini-3.7-flash"),
            )
            .unwrap(),
        ),
        _ => Arc::new(
            adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
                key(&["ANTHROPIC_API_KEY"]),
                model("claude-sonnet-5-5"),
            ))
            .unwrap(),
        ),
    }
}

/// The fields of a drawing, in order.
fn fields(node: &adk_surface::genui::DrawnNode, out: &mut Vec<adk_surface::genui::DrawnField>) {
    out.extend(node.field.clone());
    for child in &node.children {
        fields(child, out);
    }
}

/// The governed form on a live model (spec 3.3 §12 "Governed form"): the model draws the
/// invoice form itself, the person sends it from the web home, the call waits for them,
/// **Allow once** on the phone runs it once, and the model's next turn reads a call it never
/// made.
///
/// ```text
/// cargo test -p adk-surface --features genui,web --test genui_scenario_tests -- --ignored --nocapture
/// GENUI_LIVE_PROVIDER=gemini cargo test -p adk-surface --features genui,web --test genui_scenario_tests -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "calls a model provider's API"]
async fn a_live_model_draws_a_form_whose_call_waits_for_the_person() {
    use adk_surface::genui::{DrawnInput, DrawnKind};

    // What the provider says, with `RUST_LOG`.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let lasting = Lasting::new();
    let home1 = home(&lasting, live_model(), false).await;
    home1
        .engine
        .execute(
            &web(),
            Command::Send {
                session_id: "s1".into(),
                text: "Prepare the October invoice INV-10 for ACME. Show me a form with \
                       render_form: a field for the recipient's email address and one for the \
                       amount, 4200, and a Send action that calls send_invoice with the two \
                       fields and invoice INV-10. Do not call send_invoice yourself."
                    .into(),
            },
        )
        .await
        .unwrap();
    // The turn ends with the form drawn, or says why it was not.
    eventually_within("the drawing turn to start", LIVE, || async {
        let processes = home1.kernel.processes().ps(Default::default()).await.unwrap();
        (!processes.is_empty()).then_some(())
    })
    .await;
    // Ended, or parked on a call the model made itself.
    let finished = eventually_within("the drawing turn to end", LIVE, || async {
        let processes = home1.kernel.processes().ps(Default::default()).await.unwrap();
        processes
            .iter()
            .all(|process| {
                process.run.state.is_terminal() || process.run.state == adk_core::RunState::Waiting
            })
            .then_some(processes)
    })
    .await;
    let thread = items(&home1.engine, &web()).await;
    let form = thread.iter().find_map(|item| match item {
        ThreadItem::Drawn { surface }
            if surface.kind == DrawnKind::Form && !surface.actions.is_empty() =>
        {
            Some(surface.as_ref().clone())
        }
        ThreadItem::Message { .. }
        | ThreadItem::Activity { .. }
        | ThreadItem::Result { .. }
        | ThreadItem::Decision { .. }
        | ThreadItem::Drawn { .. }
        | ThreadItem::DrawnRefused { .. }
        | ThreadItem::DrawnSent { .. } => None,
    });
    let Some(form) = form else {
        println!("{} runs", finished.len());
        for process in &finished {
            println!("run {} {:?} {:?}", process.run.run_id, process.run.state, process.run.error);
        }
        for item in &thread {
            println!("{}", serde_json::to_string(item).unwrap());
        }
        panic!("the model drew no form with an action");
    };
    println!("drawn: {}", serde_json::to_string_pretty(&form).unwrap());

    let mut drawn = Vec::new();
    if let Some(root) = &form.root {
        fields(root, &mut drawn);
    }
    let values: serde_json::Map<String, Value> = drawn
        .iter()
        .map(|field| {
            let path = field.path.to_lowercase();
            let value = match field.input {
                DrawnInput::Number | DrawnInput::Slider => json!(4200),
                DrawnInput::Checkbox => json!(false),
                DrawnInput::Date => json!("2026-11-15"),
                DrawnInput::Choice | DrawnInput::Choices => {
                    field.options.first().map_or(field.value.clone(), |option| json!(option.value))
                }
                DrawnInput::ShortText | DrawnInput::LongText
                    if ["to", "email", "recipient", "address"]
                        .iter()
                        .any(|word| path.contains(word)) =>
                {
                    json!("ana@example.com")
                }
                DrawnInput::ShortText
                | DrawnInput::LongText
                | DrawnInput::Time
                | DrawnInput::DateTime => field.value.clone(),
            };
            (field.path.clone(), value)
        })
        .collect();
    let action = &form.actions[0];
    assert_eq!(action.tool, "send_invoice");
    let (status, outcome) = call(
        &home1.api,
        "POST",
        "/api/surface/commands",
        Some(json!({
            "surface": "web",
            "origin": { "kind": "pointer" },
            "command": {
                "type": "submitDrawn", "sessionId": "s1", "surfaceId": form.surface_id,
                "version": form.version, "actionId": action.id, "values": values
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");

    let card = waiting(&home1.engine, 1).await.remove(0);
    println!("card: {}", serde_json::to_string_pretty(&card).unwrap());
    assert_eq!(card.call.tool, "send_invoice");
    assert!(card.timeline[0].text.starts_with("You pressed Send on a form Finance drew · "));
    assert!(!card.allowed.options.iter().any(|option| option.answer == Answer::AlwaysAllow));
    assert!(lasting.ran().is_empty(), "nothing ran before the decision");

    let decided = home1
        .engine
        .execute(
            &phone(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(decided, CommandOutcome::Answered { .. }), "{decided:?}");
    eventually_within("send_invoice to run", LIVE, || async {
        (!lasting.ran().is_empty()).then_some(())
    })
    .await;
    let finished = eventually_within("every run to finish", LIVE, || async {
        let processes = home1.kernel.processes().ps(Default::default()).await.unwrap();
        processes.iter().all(|process| process.run.state.is_terminal()).then_some(processes)
    })
    .await;
    for process in &finished {
        println!("run {} {:?} {:?}", process.run.run_id, process.run.state, process.run.error);
    }
    let (_, forms) = call(&home1.api, "GET", "/api/surface/genui/sessions/s1/forms", None).await;
    println!("forms: {forms}");
    for entry in home1.engine.thread(&web(), "s1").await.unwrap() {
        for item in &entry.items {
            println!("{} {}", entry.at_text, serde_json::to_string(item).unwrap());
        }
    }
    assert_eq!(lasting.ran().len(), 1, "send_invoice ran once: {:?}", lasting.ran());
    assert_eq!(forms["forms"][0]["status"], "done");
    assert!(
        finished.iter().all(|process| process.run.state == adk_core::RunState::Completed),
        "the model's turn after the call it never made ended well"
    );
    home1.kernel.stop().await.unwrap();
}
