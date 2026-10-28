//! The web home's decisions over HTTP (spec 3.3 §7.2, §7.4, §8.2, §10 H2): Edit only where
//! the card shows the call's own arguments, the queued input read back after a reload, and
//! "Take over, then Done" delivering the typed secret to the one call that asked.

#![cfg(feature = "web")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
    ApproverSelector, InMemoryApprovalStore, RequestContext, Result, StoreApprovalService, Tool,
    ToolConfirmationRequest, ToolContext, ToolEffect, resource_indicator,
};
use adk_harness::take_over::{TAKE_OVER_TOOL, TakeOver, TakeOverAsk};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessAgent, HarnessMode, InstructionsConfig,
    PromptAnswer,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::{
    Answer, Caller, Command, LocalSource, Origin, SurfaceEngine, SurfaceError, SurfaceKind,
};
use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "home";
const OWNER: &str = "ana";

fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner".to_string()])
}

fn service() -> Arc<dyn ApprovalService> {
    Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ),
    )
}

async fn sessions() -> Arc<dyn SessionService> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    sessions
}

fn api(engine: SurfaceEngine, take_over: Option<TakeOver>) -> axum::Router {
    let mut routes = adk_surface::web::router(engine.clone());
    if let Some(take_over) = take_over {
        routes = routes.merge(adk_surface::web::take_over_router(engine, take_over));
    }
    axum::Router::new().nest("/api", routes).layer(axum::Extension(Some(RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into()],
        metadata: Default::default(),
    })))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = app.clone().oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// As [`call`], from the web home's own page: a browser's same-origin request.
async fn call_from_home(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request =
        Request::builder().method(method).uri(uri).header("sec-fetch-site", "same-origin");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = app.clone().oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A held call of `s1` whose inbox entry shows `shown` to approvers.
async fn hold(service: &Arc<dyn ApprovalService>, id: &str, args: Value, shown: Value) {
    let call = ToolConfirmationRequest::new("pay", args);
    let request = ApprovalRequest::new(
        id,
        ApprovalScope::new(APP, OWNER, "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_redacted_args(shown);
    service.submit(request).await.unwrap();
}

/// Edit replaces the call's arguments with what the card sends back (spec 3.3 §7.2), so a
/// card over a redacted view offers no Edit and the engine refuses one: the person would
/// otherwise submit the masks as the call's real arguments.
#[tokio::test]
async fn edit_is_offered_only_over_the_call_s_own_arguments() {
    let service = service();
    let source = LocalSource::builder(APP)
        .sessions(sessions().await)
        .approvals(service.clone())
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let real = json!({ "amount": 500, "card": "4242424242424242" });
    hold(&service, "apr-masked", real.clone(), json!({ "amount": 500, "card": "****4242" })).await;
    hold(&service, "apr-plain", json!({ "amount": 20 }), json!({ "amount": 20 })).await;

    let masked = engine.decision(&web(), "apr-masked").await.unwrap();
    let answers: Vec<Answer> = masked.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(answers, vec![Answer::AllowOnce, Answer::Deny], "no Edit over masks");
    let refused = engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: "apr-masked".into(),
                version: masked.version,
                answer: Answer::Edit,
                edited_args: Some(json!({ "amount": 400, "card": "****4242" })),
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(refused, SurfaceError::Rule(_)), "{refused:?}");
    let entry = service.get("apr-masked").await.unwrap().unwrap();
    assert_eq!(entry.decisions, vec![], "nothing was decided");

    let plain = engine.decision(&web(), "apr-plain").await.unwrap();
    let answers: Vec<Answer> = plain.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(answers, vec![Answer::AllowOnce, Answer::Edit, Answer::Deny]);
    assert!(entry.allowed_actions.contains(&ApprovalAction::Edit), "the inbox allows it");
}

/// What is typed while a decision is open waits on the server, and the page reads it back,
/// so a reload or a switch of threads still shows it (spec 3.3 §7.4 rule 2, S-I4).
#[tokio::test]
async fn queued_input_is_read_back() {
    let service = service();
    let source = LocalSource::builder(APP)
        .sessions(sessions().await)
        .approvals(service.clone())
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    hold(&service, "apr-1", json!({ "amount": 20 }), json!({ "amount": 20 })).await;
    let app = api(engine, None);

    let (status, empty) = call(&app, "GET", "/api/surface/sessions/s1/queued", None).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty, json!({ "sessionId": "s1", "queued": [] }));
    let send = json!({
        "surface": "web",
        "origin": { "kind": "keyboard" },
        "command": { "type": "send", "sessionId": "s1", "text": "also attach the timesheet" },
    });
    let (_, outcome) = call(&app, "POST", "/api/surface/commands", Some(send)).await;
    assert_eq!(outcome["type"], "queued", "{outcome}");
    let (_, queued) = call(&app, "GET", "/api/surface/sessions/s1/queued", None).await;
    assert_eq!(queued, json!({ "sessionId": "s1", "queued": ["also attach the timesheet"] }));
}

const BANK: &str = "https://bank.example.com/login";

/// Signs in with the one-time code the person types.
struct SignIn {
    take_over: TakeOver,
    codes: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for SignIn {
    fn name(&self) -> &str {
        "bank_sign_in"
    }

    fn description(&self) -> &str {
        "Signs the person in to their bank with a code they type themselves."
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let bank = resource_indicator(BANK)?;
        let ask = TakeOverAsk::one_time_code("Type the code your bank sent", bank.clone());
        let token = self.take_over.ask(ctx.as_ref(), ask).await?;
        self.codes.lock().unwrap().push(token.authorize_for(&bank)?.expose().to_string());
        Ok(json!({ "signedIn": true }))
    }
}

/// The person allowed the sign-in itself; the take over follows.
#[derive(Debug)]
struct AllowCalls;

#[async_trait]
impl ApprovalPrompter for AllowCalls {
    async fn prompt(&self, _prompt: ApprovalPrompt) -> Result<PromptAnswer> {
        Ok(PromptAnswer::ApproveOnce)
    }
}

/// A harness whose `bank_sign_in` asks the person to type their bank's one-time code, the
/// engine over it, its take over, and the codes the tool got.
async fn signing_in(
    dir: &std::path::Path,
) -> (Arc<dyn ApprovalService>, SurfaceEngine, TakeOver, Arc<Mutex<Vec<String>>>) {
    let service = service();
    let take_over = TakeOver::new();
    let codes = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("bank_sign_in", json!({})),
            ScriptedResponse::text("You're signed in."),
        ],
    ));
    let agent = Arc::new(
        HarnessAgent::builder("assistant")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Auto)
            .prompter(Arc::new(AllowCalls))
            .approval_service(service.clone())
            .tool(Arc::new(SignIn { take_over: take_over.clone(), codes: codes.clone() }))
            .take_over(take_over.clone())
            .build()
            .unwrap(),
    );
    let sessions = sessions().await;
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()
        .unwrap();
    let harness = Arc::new(Harness::with_runner(agent, runner).unwrap());
    let source = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(service.clone())
        .harness(harness)
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    (service, engine, take_over, codes)
}

/// Asks the agent to sign in and returns the take-over card it raises.
async fn take_over_card(engine: &SurfaceEngine, app: &axum::Router) -> Value {
    engine
        .execute(&web(), Command::Send { session_id: "s1".into(), text: "sign me in".into() })
        .await
        .unwrap();
    for _ in 0..400 {
        let (_, page) = call(app, "GET", "/api/surface/decisions?surface=web", None).await;
        if let Some(found) = page["decisions"].as_array().and_then(|cards| cards.first()) {
            return found.clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the take over is a card");
}

/// "Take over, then Done" on the web home (spec 3.3 §8.2, §10 H2): the card names what to
/// type, the typed code reaches the one call, the card is decided, and a second answer is
/// "already decided" (S-I2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_typed_on_the_web_home_reaches_the_call_that_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (_service, engine, take_over, codes) = signing_in(dir.path()).await;
    let app = api(engine.clone(), Some(take_over));
    let card = take_over_card(&engine, &app).await;
    assert_eq!(card["call"]["tool"], TAKE_OVER_TOOL, "{card}");
    assert_eq!(card["heading"], "Type the code your bank sent", "{card}");
    // Approving without a secret would end the call with nothing; only Deny is a decide.
    assert_eq!(card["allowed"]["options"].as_array().unwrap().len(), 1, "{card}");
    assert_eq!(card["allowed"]["options"][0]["answer"], "deny", "{card}");
    let uri = format!("/api/surface/decisions/{}/take-over", card["id"].as_str().unwrap());
    let body = json!({ "version": card["version"], "secret": "731905" });

    let (status, outcome) = call(&app, "POST", &uri, Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["type"], "answered", "{outcome}");
    assert_eq!(outcome["card"]["status"], "decided", "{outcome}");
    assert!(!outcome.to_string().contains("731905"), "the answer carries no secret");
    for _ in 0..400 {
        if !codes.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(*codes.lock().unwrap(), vec!["731905".to_string()], "the call got the code once");

    let (_, again) = call(&app, "POST", &uri, Some(body)).await;
    assert_eq!(again["type"], "alreadyDecided", "{again}");
    assert_eq!(codes.lock().unwrap().len(), 1, "a second answer delivers nothing");
}

/// The routes as a paired device of `kind` (a remote terminal without one) calls them.
fn device_api(engine: SurfaceEngine, take_over: TakeOver, kind: Option<&str>) -> axum::Router {
    let mut metadata = std::collections::HashMap::from([
        (adk_kernel::pairing::DEVICE_ID_METADATA.to_string(), "dev_1".to_string()),
        (adk_kernel::pairing::DEVICE_SCOPES_METADATA.to_string(), "decide,read,stop".to_string()),
    ]);
    if let Some(kind) = kind {
        metadata.insert(adk_kernel::pairing::DEVICE_KIND_METADATA.to_string(), kind.to_string());
    }
    let routes = adk_surface::web::router(engine.clone())
        .merge(adk_surface::web::take_over_router(engine, take_over));
    axum::Router::new().nest("/api", routes).layer(axum::Extension(Some(RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into()],
        metadata,
    })))
}

/// A paired device takes over as its kind's surface, whatever its body says, and only a phone
/// may (spec 3.3 §8.2): a remote terminal and a tray are refused, and a phone that sends
/// `surface: "web"` is recorded as the phone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_takes_over_as_its_own_surface_and_only_a_phone_may() {
    let dir = tempfile::tempdir().unwrap();
    let (service, engine, take_over, codes) = signing_in(dir.path()).await;
    let app = api(engine.clone(), Some(take_over.clone()));
    let card = take_over_card(&engine, &app).await;
    let id = card["id"].as_str().unwrap().to_string();
    let uri = format!("/api/surface/decisions/{id}/take-over");
    let body = json!({ "surface": "web", "version": card["version"], "secret": "731905" });
    for kind in [None, Some("tray")] {
        let device = device_api(engine.clone(), take_over.clone(), kind);
        let (status, problem) = call(&device, "POST", &uri, Some(body.clone())).await;
        assert_eq!(
            (status, problem["error"]["code"].clone()),
            (StatusCode::FORBIDDEN, json!("surface.not_here")),
            "{kind:?}: {problem}"
        );
    }
    assert!(codes.lock().unwrap().is_empty(), "nothing was delivered");

    let phone = device_api(engine, take_over, Some("phone"));
    let (status, outcome) = call(&phone, "POST", &uri, Some(body)).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("answered")), "{outcome}");
    let entry = service.get(&id).await.unwrap().unwrap();
    assert_eq!(entry.decisions[0].reason.as_deref(), Some("taken over on the phone"));
}

/// A card that asks for no secret is answered with a decide command, not taken over.
#[tokio::test]
async fn only_a_take_over_card_takes_a_secret() {
    let service = service();
    let source = LocalSource::builder(APP)
        .sessions(sessions().await)
        .approvals(service.clone())
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    hold(&service, "apr-1", json!({ "amount": 20 }), json!({ "amount": 20 })).await;
    let app = api(engine, Some(TakeOver::new()));
    let body = json!({ "version": 1, "secret": "731905" });
    let (status, problem) =
        call(&app, "POST", "/api/surface/decisions/apr-1/take-over", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    let entry = service.get("apr-1").await.unwrap().unwrap();
    assert_eq!(entry.decisions, vec![], "nothing was decided");
}

/// The router mounted with one person's scope.
fn api_as(engine: SurfaceEngine, user: &str, scope: &str) -> axum::Router {
    axum::Router::new().nest("/api", adk_surface::web::router(engine)).layer(axum::Extension(Some(
        RequestContext {
            user_id: user.into(),
            scopes: vec![scope.into()],
            metadata: Default::default(),
        },
    )))
}

/// Stop all and Resume all freeze and unfreeze every run of the source, whoever started it, so
/// an engine built with `stops_need` gives them to that group only (spec 3.3 §11 S-I10): any
/// other caller the server admits gets `403 surface.operator_only`, and nothing changes.
#[tokio::test]
async fn only_the_operator_stops_or_resumes_every_run_of_a_mounted_home() {
    let control = adk_core::GovernanceControl::new();
    let source = LocalSource::builder(APP)
        .sessions(sessions().await)
        .approvals(service())
        .governance(control.clone())
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
    let viewer = api_as(engine.clone(), "bo", "viewer");
    let operator = api_as(engine, OWNER, "operator");
    let command = |kind: &str| json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": { "type": kind } });

    let (status, problem) =
        call(&viewer, "POST", "/api/surface/commands", Some(command("stopAll"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["error"]["code"], "surface.operator_only", "{problem}");
    assert!(!control.is_frozen(), "another user froze nothing");

    let (status, outcome) =
        call(&operator, "POST", "/api/surface/commands", Some(command("stopAll"))).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("stopped")), "{outcome}");
    assert!(control.is_frozen());

    let (status, problem) =
        call(&viewer, "POST", "/api/surface/commands", Some(command("resumeAll"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["error"]["code"], "surface.operator_only", "{problem}");
    assert!(control.is_frozen(), "another user lifted no freeze");

    let (status, outcome) =
        call_from_home(&operator, "POST", "/api/surface/commands", Some(command("resumeAll")))
            .await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("resumed")), "{outcome}");
    assert!(!control.is_frozen());
}

/// The kernel reads a quiet window without an offset in the server's time zone, so the status
/// line says which zone that is, and a page elsewhere reads the window there (spec 3.3 §5.1).
#[tokio::test]
async fn the_status_line_names_the_server_s_time_zone() {
    let source = LocalSource::builder(APP).sessions(sessions().await).build().unwrap();
    let app = api(SurfaceEngine::builder(Arc::new(source)).build(), None);
    let (status, line) = call(&app, "GET", "/api/surface/status?surface=web", None).await;
    assert_eq!(status, StatusCode::OK, "{line}");
    let here = chrono::Local::now().offset().local_minus_utc() / 60;
    assert_eq!(line["hostUtcOffsetMinutes"], json!(here), "{line}");
}

/// Without a kernel ledger the briefing can list nothing, whatever ran, so it says no record is
/// kept and the home never reads that as "nothing happened" (spec 3.3 §5.1).
#[tokio::test]
async fn a_briefing_without_a_ledger_says_nothing_is_recorded() {
    let source = LocalSource::builder(APP).sessions(sessions().await).build().unwrap();
    let app = api(SurfaceEngine::builder(Arc::new(source)).build(), None);
    let (status, briefing) = call(&app, "GET", "/api/surface/briefing?surface=web", None).await;
    assert_eq!(status, StatusCode::OK, "{briefing}");
    assert_eq!(
        (briefing["lines"].clone(), briefing["recorded"].clone()),
        (json!([]), json!(false))
    );
}
