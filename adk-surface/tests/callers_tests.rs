//! Device and voice callers over HTTP (spec 3.3 §5.6, §7.2, §10 K2, §11 S-I10): a device
//! token's context is never the web home and never a paired local terminal, commands are
//! closed to it, a spoken answer never arrives as a declared one, and every audit record
//! names the device and the presence that widened an answer.

#![cfg(feature = "web")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService, ApprovalState,
    ApproverSelector, GovernanceControl, InMemoryApprovalStore, RequestContext,
    StoreApprovalService, ToolConfirmationRequest,
};
use adk_kernel::pairing::{DEVICE_ID_METADATA, DEVICE_NAME_METADATA, DEVICE_SCOPES_METADATA};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::web::{PAIRED_LOCAL_METADATA_KEY, PRESENCE_METADATA_KEY};
use adk_surface::{
    Answer, Caller, Command, CommandOutcome, LocalSource, Origin, SURFACE_AUDIT_DECIDE,
    SurfaceEngine, SurfaceKind,
};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "home";
const OWNER: &str = "ana";
const DEVICE: &str = "dev_phone_1";

struct Home {
    engine: SurfaceEngine,
    service: Arc<dyn ApprovalService>,
    audit: InMemoryAuditRecorder,
    governance: GovernanceControl,
}

async fn home() -> Home {
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
    let service: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ));
    let governance = GovernanceControl::new();
    let source = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(Arc::clone(&service))
        .governance(governance.clone())
        .build()
        .unwrap();
    let audit = InMemoryAuditRecorder::new();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .stops_need("operator")
        .voice_cancel_window(Duration::from_secs(30))
        .build();
    Home { engine, service, audit, governance }
}

async fn hold(home: &Home, tool: &str, risk: ApprovalRisk) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new(tool, json!({ "crate": "adk-surface" }));
    let request = ApprovalRequest::new(
        format!("apr-{tool}"),
        ApprovalScope::new(APP, OWNER, "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_risk(risk);
    home.service.submit(request).await.unwrap()
}

/// The owner at the web home, holding the operator group.
fn operator() -> RequestContext {
    RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata: HashMap::new(),
    }
}

/// The owner's paired device, as the kernel's device token extractor builds its context: the
/// owner's groups, the device's id, name and scopes. It also carries the two metadata keys
/// that widen an answer, which a device's context never honours.
fn device() -> RequestContext {
    RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata: HashMap::from([
            (DEVICE_ID_METADATA.to_string(), DEVICE.to_string()),
            (DEVICE_NAME_METADATA.to_string(), "Your phone".to_string()),
            (DEVICE_SCOPES_METADATA.to_string(), "decide,read,stop".to_string()),
            (PRESENCE_METADATA_KEY.to_string(), "confirmed".to_string()),
            (PAIRED_LOCAL_METADATA_KEY.to_string(), "true".to_string()),
        ]),
    }
}

fn api(engine: &SurfaceEngine, context: RequestContext) -> axum::Router {
    axum::Router::new()
        .nest("/api", adk_surface::web::router(engine.clone()))
        .layer(axum::Extension(Some(context)))
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

fn answers(card: &Value) -> Vec<String> {
    card["allowed"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["answer"].as_str().unwrap().to_string())
        .collect()
}

fn decide(id: &str, version: u64, answer: &str) -> Value {
    json!({ "type": "decide", "decisionId": id, "version": version, "answer": answer, "expanded": true })
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|record| record.action == action).collect()
}

/// A device declaring the web home reads every card as the terminal row, never paired: no
/// high-risk approval, presence or not (spec 3.3 §7.2), while the web home offers "Confirm
/// it's you and approve". Fails on `origin/main`, where the declared surface wins.
#[tokio::test]
async fn a_device_declaring_the_web_home_is_never_the_web_home() {
    let home = home().await;
    let request = hold(&home, "cargo_publish", ApprovalRisk::High).await;
    let web = api(&home.engine, operator());
    let phone = api(&home.engine, device());

    let (status, list) = call(&web, "GET", "/api/surface/decisions?surface=web", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let card = &list["decisions"][0];
    assert_eq!(answers(card), ["allowOnce", "edit", "deny"], "{card}");
    assert_eq!(card["allowed"]["options"][0]["needsPresence"], true, "{card}");

    for uri in [
        "/api/surface/decisions?surface=web".to_string(),
        format!("/api/surface/decisions/{}?surface=web", request.id),
    ] {
        let (status, body) = call(&phone, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let card = body.get("decisions").map_or(&body, |list| &list[0]);
        assert_eq!(answers(card), ["deny"], "{uri}: {card}");
        assert_eq!(card["allowed"]["openInHome"], true, "{uri}: {card}");
    }
}

/// Commands are closed to devices, whatever they declare: a device answers and stops through
/// its own routes (spec 3.3 §10 K2). Fails on `origin/main`, which runs them as the web home.
#[tokio::test]
async fn commands_are_closed_to_devices() {
    let home = home().await;
    let request = hold(&home, "send_email", ApprovalRisk::Medium).await;
    let phone = api(&home.engine, device());

    let commands = [
        decide(&request.id, request.version, "allowOnce"),
        decide(&request.id, request.version, "deny"),
        json!({ "type": "send", "sessionId": "s1", "text": "hello" }),
        json!({ "type": "setMode", "sessionId": "s1", "mode": "goAhead" }),
        json!({ "type": "stopAll" }),
    ];
    for command in commands {
        for (surface, origin) in [("web", "pointer"), ("phone", "touch"), ("terminal", "keyboard")]
        {
            let body =
                json!({ "surface": surface, "origin": { "kind": origin }, "command": command });
            let (status, problem) = call(&phone, "POST", "/api/surface/commands", Some(body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{command} from {surface}: {problem}");
            assert_eq!(problem["error"]["code"], "surface.not_here", "{problem}");
        }
    }
    let entry = home.service.get(&request.id).await.unwrap().unwrap();
    assert_eq!(entry.state, ApprovalState::Pending, "nothing was answered");
    assert!(!home.governance.is_frozen(), "nothing was stopped");
}

/// A device never resumes after a stop, even declaring the web home and holding the operator
/// group (spec 3.3 §11 S-I10). Fails on `origin/main`, where it resumes.
#[tokio::test]
async fn a_device_never_resumes() {
    let home = home().await;
    let web = api(&home.engine, operator());
    let phone = api(&home.engine, device());
    let command = |kind: &str, surface: &str, origin: &str| json!({ "surface": surface, "origin": { "kind": origin }, "command": { "type": kind } });

    let (status, outcome) =
        call(&web, "POST", "/api/surface/commands", Some(command("stopAll", "web", "pointer")))
            .await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("stopped")), "{outcome}");
    for (surface, origin) in [("web", "pointer"), ("terminal", "keyboard"), ("phone", "touch")] {
        let (status, problem) = call(
            &phone,
            "POST",
            "/api/surface/commands",
            Some(command("resumeAll", surface, origin)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{surface}: {problem}");
        assert_eq!(problem["error"]["code"], "surface.not_here", "{problem}");
        assert!(home.governance.is_frozen(), "{surface}: still stopped");
    }
}

/// A spoken answer comes from the voice surface's own transcript, so a `decide` declaring the
/// voice surface or origin over HTTP is refused, and nothing is answered; cancelling a spoken
/// answer stays open from any screen (spec 3.3 §5.6, §7.2). Fails on `origin/main`, which
/// accepts the declared voice answer and starts its cancel window.
#[tokio::test]
async fn a_spoken_decide_over_http_is_refused_and_its_cancel_is_not() {
    let home = home().await;
    let request = hold(&home, "send_email", ApprovalRisk::Medium).await;
    let web = api(&home.engine, operator());

    for (surface, origin, answer) in [
        ("voice", "voice", "allowOnce"),
        ("web", "voice", "allowOnce"),
        ("voice", "pointer", "deny"),
    ] {
        let body = json!({
            "surface": surface,
            "origin": { "kind": origin },
            "command": decide(&request.id, request.version, answer),
        });
        let (status, problem) = call(&web, "POST", "/api/surface/commands", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{surface}/{origin}: {problem}");
        assert_eq!(problem["error"]["code"], "surface.invalid", "{problem}");
    }
    let entry = home.service.get(&request.id).await.unwrap().unwrap();
    assert_eq!(entry.state, ApprovalState::Pending, "nothing was answered");

    // The voice surface gives its answer in process, with its transcript (spec 3.3 §5.6); the
    // screen beside it cancels it.
    #[cfg(feature = "voice")]
    {
        let voice = Caller::human(OWNER, SurfaceKind::Voice, Origin::Voice).with_groups(["owner"]);
        let spoken = home
            .engine
            .decide_spoken(&voice, &request.id, request.version, Answer::AllowOnce, "allow once")
            .await
            .unwrap()
            .outcome;
        assert!(matches!(spoken, CommandOutcome::Pending { .. }), "{spoken:?}");
        let body = json!({
            "surface": "voice",
            "origin": { "kind": "pointer" },
            "command": { "type": "cancelAnswer", "decisionId": request.id },
        });
        let (status, outcome) = call(&web, "POST", "/api/surface/commands", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{outcome}");
        assert_eq!(outcome, json!({ "type": "answerCancelled", "decisionId": request.id }));
        let entry = home.service.get(&request.id).await.unwrap().unwrap();
        assert_eq!(entry.state, ApprovalState::Pending, "the spoken answer never landed");
    }
}

/// Every audit record names the device a caller acts from and the presence it confirmed, so
/// "What your agents did" can say where a high-risk call was approved (spec 3.3 §11); a caller
/// with neither carries neither. Fails on `origin/main`, whose records have no such fields.
#[tokio::test]
async fn audit_records_carry_the_device_and_the_presence() {
    let home = home().await;
    let high = hold(&home, "cargo_publish", ApprovalRisk::High).await;
    let medium = hold(&home, "send_email", ApprovalRisk::Medium).await;

    let phone = Caller::human(OWNER, SurfaceKind::Phone, Origin::Touch)
        .with_groups(["owner"])
        .with_device(DEVICE)
        .with_presence_confirmed();
    let outcome = home
        .engine
        .execute(
            &phone,
            Command::Decide {
                decision_id: high.id.clone(),
                version: high.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");

    let web = Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"]);
    home.engine
        .execute(
            &web,
            Command::Decide {
                decision_id: medium.id.clone(),
                version: medium.version,
                answer: Answer::Deny,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();

    let decided = records(&home.audit, SURFACE_AUDIT_DECIDE);
    assert_eq!(decided.len(), 2, "{decided:?}");
    let fields = |record: &AuditRecordDraft| {
        (
            record.ext.get("approvalId").cloned(),
            record.ext.get("surface").cloned(),
            record.ext.get("device").cloned(),
            record.ext.get("presence").cloned(),
        )
    };
    assert_eq!(
        fields(&decided[0]),
        (Some(json!(high.id)), Some(json!("phone")), Some(json!(DEVICE)), Some(json!("confirmed")))
    );
    assert_eq!(fields(&decided[1]), (Some(json!(medium.id)), Some(json!("web")), None, None));
}

/// An answer's record names the agent whose call it decided, so "What your agents did" can say
/// "You let Finance send one email" (spec 3.3 §11). Fails on `origin/main`, whose
/// `surface.decide` records name no agent.
#[tokio::test]
async fn an_answers_record_names_the_agent_whose_call_it_decided() {
    let home = home().await;
    let call = ToolConfirmationRequest::new("send_email", json!({ "to": "kim" }));
    let request = ApprovalRequest::new(
        "apr-finance",
        ApprovalScope::new(APP, OWNER, "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_acting(adk_core::ActingContext::derived(
        APP,
        "finance",
        OWNER,
        adk_core::SubjectKind::Human,
    ));
    let request = home.service.submit(request).await.unwrap();
    let web = Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"]);
    home.engine
        .execute(
            &web,
            Command::Decide {
                decision_id: request.id.clone(),
                version: request.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    let decided = records(&home.audit, SURFACE_AUDIT_DECIDE);
    assert_eq!(
        decided[0].ext.get("actingAgent"),
        Some(&json!(format!("urn:adk:agent:{APP}/finance"))),
        "{decided:?}"
    );
}
