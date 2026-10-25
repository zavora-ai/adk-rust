//! The phone's routes and wake-ups over in-memory stores (spec 3.3 §5.4, §7.2, §8.2, §11):
//! "Confirm it's you" bound to one answer, device answers by touch, the operator's routes,
//! and wake-ups that say nothing, wait through quiet time and go out once.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService, ApprovalState,
    ApproverSelector, InMemoryApprovalStore, ManualClock, RequestContext, StoreApprovalService,
    ToolConfirmationRequest,
};
use adk_kernel::QuietHours;
use adk_kernel::pairing::{
    DEVICE_ID_METADATA, DEVICE_KIND_METADATA, DEVICE_NAME_METADATA, DEVICE_SCOPES_METADATA,
    InMemoryDeviceStore, IssueCode, Pairing,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use base64ct::{Base64UrlUnpadded, Encoding as _};
use chrono::{DateTime, NaiveTime, TimeZone as _, Utc};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::*;

#[path = "../../tests/mobile_support/authenticator.rs"]
mod authenticator;
#[path = "../../tests/mobile_support/fake_push.rs"]
mod fake_push;

use crate::engine::SurfaceEngine;
use crate::{LocalSource, Place};
use authenticator::Authenticator;
use fake_push::{Browser, FakePush};

const APP: &str = "home";
const OWNER: &str = "ana";
const HOME: &str = "http://localhost:4100";
const RP_ID: &str = "localhost";

fn at(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, hour, minute, 0).unwrap()
}

struct Rig {
    phone: Phone,
    approvals: Arc<dyn ApprovalService>,
    pairing: Pairing,
    clock: Arc<ManualClock>,
    audit: InMemoryAuditRecorder,
    device: String,
}

struct Options {
    home: Option<&'static str>,
    quiet: bool,
    allowlist: PushAllowlist,
}

impl Default for Options {
    fn default() -> Self {
        Self { home: Some(HOME), quiet: false, allowlist: PushAllowlist::default() }
    }
}

async fn rig(options: Options) -> Rig {
    let clock = Arc::new(ManualClock::new(at(24, 18, 0)));
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
    let doorbell = Arc::new(PushDoorbell::new());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            )
            .with_notifier(doorbell.clone()),
    );
    let source = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(Arc::clone(&approvals))
        .governance(adk_core::GovernanceControl::new())
        .build()
        .unwrap();
    let audit = InMemoryAuditRecorder::new();
    let trail = AuditTrail::new(Arc::new(audit.clone()));
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(trail.clone())
        .places([Place::phone()])
        .stops_need("operator")
        .build();
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_clock(clock.clone());
    let device = pair(&pairing).await;
    let mut builder = Phone::builder(engine, pairing.clone())
        .push_key(PushKey::generate().unwrap())
        .audit(trail)
        .clock(clock.clone())
        .doorbell(doorbell)
        .approvals(Arc::clone(&approvals))
        .push_allowlist(options.allowlist)
        .quiet_poll(Duration::from_millis(20))
        .push_backoff(Duration::from_millis(10));
    if let Some(home) = options.home {
        builder = builder.home_url(home);
    }
    if options.quiet {
        builder = builder.quiet_hours(
            QuietHours::new(
                NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            )
            .with_utc_offset_minutes(0),
        );
    }
    Rig { phone: builder.build().unwrap(), approvals, pairing, clock, audit, device }
}

/// A phone paired for the owner, as `adk-rust pair` and a claim make it.
async fn pair(pairing: &Pairing) -> String {
    let issued = pairing
        .issue(IssueCode::new(OWNER, ["owner", "operator"]).with_name("Your phone"))
        .await
        .unwrap();
    pairing.claim(&issued.code, None).await.unwrap().device.id
}

fn device_context(device: &str, kind: Option<&str>) -> RequestContext {
    let mut metadata = HashMap::from([
        (DEVICE_ID_METADATA.to_string(), device.to_string()),
        (DEVICE_NAME_METADATA.to_string(), "Your phone".to_string()),
        (DEVICE_SCOPES_METADATA.to_string(), "decide,read,stop".to_string()),
    ]);
    if let Some(kind) = kind {
        metadata.insert(DEVICE_KIND_METADATA.to_string(), kind.to_string());
    }
    RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata,
    }
}

fn person(groups: &[&str]) -> RequestContext {
    RequestContext {
        user_id: OWNER.into(),
        scopes: groups.iter().map(|group| (*group).to_string()).collect(),
        metadata: HashMap::new(),
    }
}

fn app(phone: &Phone, context: RequestContext) -> axum::Router {
    let engine = phone.inner.engine.clone();
    axum::Router::new()
        .nest(
            "/api",
            crate::web::router(engine.clone())
                .merge(crate::web::devices_router(engine))
                .merge(router(phone.clone())),
        )
        .layer(axum::Extension(Some(context)))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = raw(app, method, uri, body).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn raw(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = app.clone().oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}

async fn hold(rig: &Rig, tool: &str, risk: ApprovalRisk) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new(tool, json!({ "crate": "adk-surface" }));
    let request = ApprovalRequest::new(
        format!("apr-{tool}"),
        ApprovalScope::new(APP, OWNER, "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_risk(risk);
    rig.approvals.submit(request).await.unwrap()
}

async fn state(rig: &Rig, id: &str) -> ApprovalState {
    rig.approvals.get(id).await.unwrap().unwrap().state
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|record| record.action == action).collect()
}

fn code(problem: &Value) -> &str {
    problem["error"]["code"].as_str().unwrap_or_default()
}

/// Sets up `authenticator`'s passkey through the routes.
async fn register(app: &axum::Router, authenticator: &mut Authenticator) -> (StatusCode, Value) {
    let (status, options) = call(app, "POST", "/api/surface/phone/passkey/options", None).await;
    if status != StatusCode::OK {
        return (status, options);
    }
    let credential = authenticator.register(&options["publicKey"]);
    let body = json!({ "registrationId": options["registrationId"], "credential": credential });
    call(app, "POST", "/api/surface/phone/passkey", Some(body)).await
}

async fn confirm_options(app: &axum::Router, id: &str, body: Value) -> (StatusCode, Value) {
    call(app, "POST", &format!("/api/surface/decisions/{id}/confirm/options"), Some(body)).await
}

/// Answers `options` with `authenticator`'s assertion on decision `id`, adding `extra` to the
/// body.
async fn confirm(
    app: &axum::Router,
    id: &str,
    options: &Value,
    authenticator: &mut Authenticator,
    extra: Value,
) -> (StatusCode, Value) {
    let credential = authenticator.assert(&options["publicKey"]);
    let mut body = json!({ "challengeId": options["challengeId"], "credential": credential });
    if let (Value::Object(body), Value::Object(extra)) = (&mut body, extra) {
        body.extend(extra);
    }
    call(app, "POST", &format!("/api/surface/decisions/{id}/confirm"), Some(body)).await
}

fn allow(version: u64) -> Value {
    json!({ "version": version, "answer": "allowOnce", "expanded": true })
}

fn authenticator() -> Authenticator {
    Authenticator::new(HOME, RP_ID)
}

/// A phone approves a high-risk call only through "Confirm it's you": the plain answer route
/// is refused, and the confirmation decides once, recorded with presence and the device
/// (spec 3.3 §7.2, §8.2, §11).
#[tokio::test]
async fn a_phone_confirms_a_high_risk_call_with_its_passkey() {
    let rig = rig(Options::default()).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let mut face_id = authenticator();
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;

    let (status, problem) = call(
        &phone,
        "POST",
        &format!("/api/surface/decisions/{}/answer", release.id),
        Some(allow(release.version)),
    )
    .await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.confirm_presence"));

    let (status, options) = call(&phone, "POST", "/api/surface/phone/passkey/options", None).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    let creation = &options["publicKey"];
    assert_eq!(creation["rp"]["id"], RP_ID);
    assert_eq!(creation["attestation"], "none");
    assert_eq!(creation["authenticatorSelection"]["userVerification"], "required");
    assert_eq!(creation["authenticatorSelection"]["authenticatorAttachment"], "platform");
    let credential = face_id.register(creation);
    let body = json!({ "registrationId": options["registrationId"], "credential": credential });
    let (status, body) = call(&phone, "POST", "/api/surface/phone/passkey", Some(body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, info) = call(&phone, "GET", "/api/surface/phone", None).await;
    assert_eq!(
        (info["passkey"].clone(), info["canAddPasskey"].clone()),
        (json!(true), json!(false))
    );

    let (status, options) = confirm_options(&phone, &release.id, allow(release.version)).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["publicKey"]["userVerification"], "required");
    assert_eq!(options["publicKey"]["rpId"], RP_ID);
    let (status, outcome) = confirm(&phone, &release.id, &options, &mut face_id, json!({})).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("answered")), "{outcome}");
    assert_eq!(state(&rig, &release.id).await, ApprovalState::Approved);

    let decided = records(&rig.audit, crate::SURFACE_AUDIT_DECIDE);
    let last = decided.last().unwrap();
    assert_eq!(
        (
            last.ext["surface"].clone(),
            last.ext["origin"].clone(),
            last.ext["device"].clone(),
            last.ext["presence"].clone()
        ),
        (json!("phone"), json!({ "kind": "touch" }), json!(rig.device), json!("confirmed"))
    );
    let added = records(&rig.audit, SURFACE_AUDIT_PHONE_PASSKEY);
    assert_eq!(added.len(), 1);
    assert_eq!(
        (added[0].ext["device"].clone(), added[0].ext["outcome"].clone()),
        (json!(rig.device), json!("added"))
    );

    // The assertion cannot be used again, on this decision or another.
    let other = hold(&rig, "cargo_yank", ApprovalRisk::High).await;
    let (status, problem) = confirm(&phone, &other.id, &options, &mut face_id, json!({})).await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.phone.challenge"));
    assert_eq!(state(&rig, &other.id).await, ApprovalState::Pending);
}

/// A confirmation answers only what it was started for: this decision, this answer, these
/// arguments, this phone's own passkey, its origin and relying party, with user
/// verification and a counter that goes forward (spec 3.3 §8.2).
#[tokio::test]
async fn a_confirmation_is_bound_to_its_decision_answer_and_arguments() {
    let rig = rig(Options::default()).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let web = app(&rig.phone, person(&["owner", "operator"]));
    let mut face_id = authenticator().counting_from(5);
    let mut computer = authenticator();
    assert_eq!(register(&phone, &mut face_id).await.0, StatusCode::NO_CONTENT);
    assert_eq!(register(&web, &mut computer).await.0, StatusCode::NO_CONTENT);
    let a = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    let b = hold(&rig, "cargo_yank", ApprovalRisk::High).await;

    // Started for A, presented on B.
    let (_, options) = confirm_options(&phone, &a.id, allow(a.version)).await;
    let (status, problem) = confirm(&phone, &b.id, &options, &mut face_id, json!({})).await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.phone.challenge"));
    // Used up by the refusal: A refuses it too.
    let (status, _) = confirm(&phone, &a.id, &options, &mut face_id, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Another answer than the one bound.
    let (_, options) = confirm_options(&phone, &a.id, allow(a.version)).await;
    let (status, problem) =
        confirm(&phone, &a.id, &options, &mut face_id, json!({ "answer": "edit" })).await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.phone.challenge"));

    // Other edited arguments than the ones bound.
    let edit = json!({ "version": a.version, "answer": "edit", "editedArgs": { "crate": "adk-core" }, "expanded": true });
    let (_, options) = confirm_options(&phone, &a.id, edit).await;
    let (status, problem) = confirm(
        &phone,
        &a.id,
        &options,
        &mut face_id,
        json!({ "editedArgs": { "crate": "adk-rust" } }),
    )
    .await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.phone.challenge"));

    let other_origin = face_id.with_origin("https://evil.example.com");
    let other_rp = face_id.with_rp_id("evil.example.com");
    let unverified = face_id.unverified();
    let backwards = face_id.stuck_at(2);
    for (case, mut presented) in [
        ("another origin", other_origin),
        ("another relying party", other_rp),
        ("no user verification", unverified),
        ("a counter going backwards", backwards),
        ("another holder's passkey", computer.clone()),
    ] {
        let (_, options) = confirm_options(&phone, &a.id, allow(a.version)).await;
        let (status, problem) = confirm(&phone, &a.id, &options, &mut presented, json!({})).await;
        assert_eq!(
            (status, code(&problem)),
            (StatusCode::FORBIDDEN, "surface.phone.passkey_refused"),
            "{case}: {problem}"
        );
    }
    assert_eq!(state(&rig, &a.id).await, ApprovalState::Pending, "nothing was answered");
    assert_eq!(state(&rig, &b.id).await, ApprovalState::Pending);
    assert!(
        records(&rig.audit, crate::SURFACE_AUDIT_DECIDE)
            .iter()
            .all(|record| !record.ext.contains_key("presence")),
        "no answer was recorded with presence"
    );

    // A changed version is shown again, never retried (S-I2).
    let (_, options) = confirm_options(&phone, &a.id, allow(a.version + 1)).await;
    let (status, outcome) = confirm(&phone, &a.id, &options, &mut face_id, json!({})).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("changed")), "{outcome}");

    // The counter the last assertion moved to is kept: presenting it again is refused.
    let replayed = face_id.stuck_at(face_id.counter);
    let (_, options) = confirm_options(&phone, &a.id, allow(a.version)).await;
    let (status, problem) =
        confirm(&phone, &a.id, &options, &mut replayed.clone(), json!({})).await;
    assert_eq!(
        (status, code(&problem)),
        (StatusCode::FORBIDDEN, "surface.phone.passkey_refused"),
        "{problem}"
    );

    // A removed phone confirms nothing.
    rig.pairing.revoke(&rig.device).await.unwrap();
    let (status, problem) = confirm_options(&phone, &a.id, allow(a.version)).await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.not_here"));
}

/// A phone sets up one passkey, within 10 minutes of pairing; replacing it means pairing
/// again (spec 3.3 §8.2).
#[tokio::test]
async fn a_phone_sets_up_one_passkey_within_ten_minutes_of_pairing() {
    let rig = rig(Options::default()).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    assert_eq!(register(&phone, &mut authenticator()).await.0, StatusCode::NO_CONTENT);
    let (status, problem) = register(&phone, &mut authenticator()).await;
    assert_eq!((status, code(&problem)), (StatusCode::CONFLICT, "surface.phone.passkey_exists"));

    let late = pair(&rig.pairing).await;
    let late_phone = app(&rig.phone, device_context(&late, Some("phone")));
    let (_, info) = call(&late_phone, "GET", "/api/surface/phone", None).await;
    assert_eq!(info["canAddPasskey"], true);
    rig.clock.advance(Duration::from_secs(10 * 60 + 1));
    let (_, info) = call(&late_phone, "GET", "/api/surface/phone", None).await;
    assert_eq!(info["canAddPasskey"], false);
    let (status, problem) = register(&late_phone, &mut authenticator()).await;
    assert_eq!(
        (status, code(&problem)),
        (StatusCode::FORBIDDEN, "surface.phone.registration_closed")
    );
}

/// A remote terminal's token, or a device paired without a kind, is not a phone: the phone's
/// routes refuse it (spec 3.3 §7.2, §10 K2).
#[tokio::test]
async fn a_device_that_is_not_a_phone_is_refused_on_the_phone_routes() {
    let rig = rig(Options::default()).await;
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    for kind in [Some("terminal"), Some("tray"), None] {
        let device = app(&rig.phone, device_context(&rig.device, kind));
        for (method, uri, body) in [
            ("GET", "/api/surface/phone".to_string(), None),
            ("POST", "/api/surface/phone/passkey/options".to_string(), None),
            (
                "PUT",
                "/api/surface/phone/push".to_string(),
                Some(
                    json!({ "endpoint": "https://web.push.apple.com/x", "keys": { "p256dh": "x", "auth": "y" } }),
                ),
            ),
            (
                "POST",
                format!("/api/surface/decisions/{}/confirm/options", release.id),
                Some(allow(release.version)),
            ),
        ] {
            let (status, problem) = call(&device, method, &uri, body).await;
            assert_eq!(
                (status, code(&problem)),
                (StatusCode::FORBIDDEN, "surface.not_here"),
                "{kind:?} {method} {uri}"
            );
        }
    }
}

/// The web home confirms with the person's own passkey on the computer, listed and removable
/// (spec 3.3 §8.2, WP M step 6).
#[tokio::test]
async fn the_web_home_confirms_with_the_person_s_passkey() {
    let rig = rig(Options::default()).await;
    let web = app(&rig.phone, person(&["owner", "operator"]));
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    // Before a passkey is set up, the refusal says where to set one up.
    let (status, problem) = confirm_options(&web, &release.id, allow(release.version)).await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.phone.no_passkey"));
    assert!(
        problem["error"]["message"].as_str().unwrap().contains("on this computer"),
        "{problem}"
    );
    let mut touch_id = authenticator();
    assert_eq!(register(&web, &mut touch_id).await.0, StatusCode::NO_CONTENT);
    let (_, listed) = call(&web, "GET", "/api/surface/phone/devices", None).await;
    let computer = listed["computerPasskeys"][0]["id"].as_str().unwrap().to_string();

    let (_, options) = confirm_options(&web, &release.id, allow(release.version)).await;
    let (status, outcome) = confirm(&web, &release.id, &options, &mut touch_id, json!({})).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("answered")), "{outcome}");
    let last = records(&rig.audit, crate::SURFACE_AUDIT_DECIDE).pop().unwrap();
    assert_eq!(
        (last.ext["surface"].clone(), last.ext["presence"].clone()),
        (json!("web"), json!("confirmed"))
    );
    assert!(!last.ext.contains_key("device"));

    let (status, _) =
        call(&web, "DELETE", &format!("/api/surface/phone/passkey/{computer}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, listed) = call(&web, "GET", "/api/surface/phone/devices", None).await;
    assert_eq!(listed["computerPasskeys"], json!([]));
}

/// Without the home's URL the phone's routes say how to start the server (spec 3.3 §5.4).
#[tokio::test]
async fn without_a_home_url_the_phone_routes_say_so() {
    let rig = rig(Options { home: None, ..Options::default() }).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let (status, problem) = call(&phone, "GET", "/api/surface/phone", None).await;
    assert_eq!((status, code(&problem)), (StatusCode::BAD_REQUEST, "surface.phone.needs_home_url"));
    assert!(problem["error"]["message"].as_str().unwrap().contains("--home-url"), "{problem}");
}

/// The QR code, the device list and the deliveries are the operator's, in the home; a phone
/// never reaches them (spec 3.3 §5.4, WP M step 7).
#[tokio::test]
async fn the_qr_code_and_the_device_list_are_the_operator_s() {
    let rig = rig(Options::default()).await;
    let operator = app(&rig.phone, person(&["owner", "operator"]));
    let viewer = app(&rig.phone, person(&["owner"]));
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let link = json!({ "url": format!("{HOME}/ui/pair#code=ABCD-EFGH") });

    let (status, headers, svg) =
        raw(&operator, "POST", "/api/surface/phone/qr", Some(link.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/svg+xml");
    assert!(String::from_utf8(svg).unwrap().contains("<svg"));
    let (status, problem) = call(
        &operator,
        "POST",
        "/api/surface/phone/qr",
        Some(json!({ "url": "https://evil.example.com/ui/pair" })),
    )
    .await;
    assert_eq!((status, code(&problem)), (StatusCode::BAD_REQUEST, "surface.phone.qr_url"));

    for (method, uri, body) in [
        ("POST", "/api/surface/phone/qr", Some(link)),
        ("GET", "/api/surface/phone/devices", None),
        ("GET", "/api/surface/phone/push/deliveries", None),
    ] {
        let (status, problem) = call(&phone, method, uri, body.clone()).await;
        assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.not_here"), "{uri}");
        let (status, problem) = call(&viewer, method, uri, body).await;
        assert_eq!(
            (status, code(&problem)),
            (StatusCode::FORBIDDEN, "surface.operator_only"),
            "{uri}"
        );
    }
    let (_, listed) = call(&operator, "GET", "/api/surface/phone/devices", None).await;
    assert_eq!(
        listed["devices"],
        json!([{ "deviceId": rig.device, "passkey": false, "push": false }])
    );
}

/// A phone answers by touch, as itself: a medium-risk answer is recorded with the device, a
/// high-risk deny needs no confirmation, and Stop all goes through its own route (spec 3.3
/// §7.2, §11 S-I10).
#[tokio::test]
async fn a_phone_answers_by_touch_and_stops_through_its_own_route() {
    let rig = rig(Options::default()).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let web = app(&rig.phone, person(&["owner", "operator"]));
    let invoice = hold(&rig, "send_email", ApprovalRisk::Medium).await;
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;

    let (status, outcome) = call(
        &phone,
        "POST",
        &format!("/api/surface/decisions/{}/answer", invoice.id),
        Some(allow(invoice.version)),
    )
    .await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("answered")), "{outcome}");
    let deny = json!({ "version": release.version, "answer": "deny", "expanded": true });
    let (status, outcome) =
        call(&phone, "POST", &format!("/api/surface/decisions/{}/answer", release.id), Some(deny))
            .await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("answered")), "{outcome}");
    assert_eq!(state(&rig, &release.id).await, ApprovalState::Rejected);
    let first = &records(&rig.audit, crate::SURFACE_AUDIT_DECIDE)[0];
    assert_eq!(
        (first.ext["surface"].clone(), first.ext["origin"].clone(), first.ext["device"].clone()),
        (json!("phone"), json!({ "kind": "touch" }), json!(rig.device))
    );

    let (status, problem) = call(&web, "POST", "/api/surface/stop", Some(json!({}))).await;
    assert_eq!((status, code(&problem)), (StatusCode::FORBIDDEN, "surface.not_here"));
    let (status, outcome) = call(&phone, "POST", "/api/surface/stop", Some(json!({}))).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("stopped")), "{outcome}");
}

/// After a relaunch the phone reads what was answered since, here or elsewhere: the owner's
/// decided cards only, with how far each answer reaches (spec 3.3 §5.4, S-I8).
#[tokio::test]
async fn decided_since_reads_the_owner_s_answers_with_their_reach() {
    let rig = rig(Options::default()).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let since = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let invoice = hold(&rig, "send_email", ApprovalRisk::Medium).await;
    let open = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    // Someone else's decision, answered: never the phone's to read.
    let asked = ToolConfirmationRequest::new("send_email", json!({}));
    let theirs = rig
        .approvals
        .submit(ApprovalRequest::new(
            "apr-bo",
            ApprovalScope::new(APP, "bo", "s2"),
            &asked,
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ))
        .await
        .unwrap();
    rig.approvals
        .decide(
            &theirs.id,
            theirs.version,
            adk_core::ApprovalDecision::approve(adk_core::principal::SubjectRef::human("bo"))
                .with_groups(["owner"]),
        )
        .await
        .unwrap();
    let (status, _) = call(
        &phone,
        "POST",
        &format!("/api/surface/decisions/{}/answer", invoice.id),
        Some(allow(invoice.version)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, before) = call(&phone, "GET", "/api/surface/decisions?surface=phone", None).await;
    assert_eq!(status, StatusCode::OK);
    let ids = |list: &Value| -> Vec<String> {
        list["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids(&before), vec![open.id.clone()], "open ones only without decidedSince");
    let uri =
        format!("/api/surface/decisions?surface=phone&decidedSince={}", since.replace('+', "%2B"));
    let (status, after) = call(&phone, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(ids(&after), vec![open.id, invoice.id], "{after}");
    let decided = &after["decisions"][1]["decided"];
    assert!(
        decided["text"].as_str().unwrap().starts_with("You said yes on your phone at "),
        "{decided}"
    );
    assert_eq!(decided["scopeText"], "done once", "{decided}");
}

/// A removed phone's open feed closes at once (spec 3.3 §10 K2, S-I7).
#[tokio::test]
async fn a_removed_phone_s_feed_closes() {
    use futures::StreamExt as _;

    let rig = rig(Options::default()).await;
    let engine = rig.phone.inner.engine.clone();
    let app = axum::Router::new()
        .nest("/api", crate::web::router_with_devices(engine, rig.pairing.clone()))
        .layer(axum::Extension(Some(device_context(&rig.device, Some("phone")))));
    let request =
        Request::get("/api/surface/sessions/s1/feed?surface=phone").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    // The snapshot arrives, then the feed stays open.
    let _ = tokio::time::timeout(Duration::from_millis(200), body.next()).await;
    let open = tokio::time::timeout(Duration::from_millis(200), body.next()).await;
    assert!(open.is_err(), "the feed stays open while the phone is paired");
    rig.pairing.revoke(&rig.device).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        while body.next().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "the feed ended with the phone");
}

// ---------------------------------------------------------------------------
// Wake-ups
// ---------------------------------------------------------------------------

fn loopback() -> PushAllowlist {
    PushAllowlist::new(["localhost"]).with_plain_http_to_localhost()
}

/// Subscribes the rig's phone to `base`'s push service.
async fn subscribe(rig: &Rig, browser: &Browser, base: &str) {
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let (status, body) = call(
        &phone,
        "PUT",
        "/api/surface/phone/push",
        Some(browser.subscription(&format!("{base}/push/1"))),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

fn outcomes(audit: &InMemoryAuditRecorder) -> Vec<(String, String)> {
    records(audit, SURFACE_AUDIT_NOTIFY)
        .into_iter()
        .map(|record| {
            (
                record.ext["device"].as_str().unwrap().to_string(),
                record.ext["outcome"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// A subscription must point at a push service, over https: http, IP addresses and unknown
/// hosts are refused (spec 3.3 §5.4).
#[tokio::test]
async fn a_subscription_is_refused_outside_the_push_services() {
    let rig = rig(Options::default()).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let browser = Browser::new();
    for endpoint in [
        "http://web.push.apple.com/x",
        "https://192.0.2.7/x",
        "https://[2001:db8::1]/x",
        "https://push.example.com/x",
        "http://localhost:4100/x",
    ] {
        let (status, problem) =
            call(&phone, "PUT", "/api/surface/phone/push", Some(browser.subscription(endpoint)))
                .await;
        assert_eq!(
            (status, code(&problem)),
            (StatusCode::BAD_REQUEST, "surface.phone.push_endpoint"),
            "{endpoint}"
        );
    }
    let (status, _) = call(
        &phone,
        "PUT",
        "/api/surface/phone/push",
        Some(browser.subscription("https://web.push.apple.com/QGu1")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let subscribed = records(&rig.audit, SURFACE_AUDIT_PHONE_PUSH);
    assert_eq!(subscribed.last().unwrap().ext["device"], json!(rig.device));
}

/// A wake-up is encrypted for the phone (aes128gcm), signed with the kernel's VAPID key for
/// the push service's origin, urgent, and says nothing but a version and an id; the phone's
/// acknowledgement is a delivery the operator reads (spec 3.3 §5.4, §11 S-I5).
#[tokio::test]
async fn a_wake_up_is_encrypted_signed_and_says_nothing() {
    let (fake, base, server) = FakePush::start().await;
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    let browser = Browser::new();
    subscribe(&rig, &browser, &base).await;
    let task = rig.phone.start_push().unwrap();

    hold(&rig, "send_email", ApprovalRisk::Medium).await;
    fake.wait_for(1).await;
    let (headers, body) = fake.got.lock().unwrap()[0].clone();
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["urgency"], "high");
    assert_eq!(headers["ttl"], "43200");
    let authorization = headers["authorization"].to_str().unwrap();
    let (token, key) = authorization.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
    assert_eq!(key, rig.phone.inner.sender.vapid().key().public_key());
    let claims = token.split('.').nth(1).unwrap();
    let claims: Value =
        serde_json::from_slice(&Base64UrlUnpadded::decode_vec(claims).unwrap()).unwrap();
    assert_eq!(claims["aud"], base.as_str());
    assert_eq!(claims["sub"], HOME);
    let exp = claims["exp"].as_i64().unwrap();
    assert!(
        exp > rig.clock.now().timestamp()
            && exp <= (rig.clock.now() + chrono::Duration::hours(24)).timestamp()
    );
    let payload = browser.decrypt(&body);
    assert_eq!(payload["v"], 1);
    assert_eq!(payload.as_object().unwrap().len(), 2, "{payload}");
    let plain = payload.to_string();
    assert!(!plain.contains("send_email") && !plain.contains("adk-surface"), "{plain}");
    assert_eq!(outcomes(&rig.audit), vec![(rig.device.clone(), "sent".to_string())]);

    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    rig.clock.advance(Duration::from_millis(900));
    let (status, _) = call(
        &phone,
        "POST",
        "/api/surface/phone/push/received",
        Some(json!({ "id": payload["id"] })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let operator = app(&rig.phone, person(&["owner", "operator"]));
    let (_, deliveries) = call(&operator, "GET", "/api/surface/phone/push/deliveries", None).await;
    assert_eq!(deliveries["deliveries"][0]["latencyMs"], 900);
    assert_eq!(deliveries["deliveries"][0]["deviceId"], json!(rig.device));
    task.abort();
    server.abort();
}

/// A push service that says the subscription is gone (`410`) ends it: the next decision wakes
/// nothing (spec 3.3 §5.4).
#[tokio::test]
async fn a_gone_subscription_is_dropped() {
    let (fake, base, server) = FakePush::start().await;
    *fake.status.lock().unwrap() = Some(410);
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    let browser = Browser::new();
    subscribe(&rig, &browser, &base).await;
    let task = rig.phone.start_push().unwrap();

    hold(&rig, "send_email", ApprovalRisk::Medium).await;
    fake.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(outcomes(&rig.audit), vec![(rig.device.clone(), "dropped".to_string())]);
    hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.count(), 1, "the gone subscription was not used again");
    task.abort();
    server.abort();
}

/// Quiet time 22:00–07:00 (spec 3.3 §5.4, §12 phase 1): a decision at 18:00 wakes the phone at
/// once; one at 22:30 is held; one answered before 07:00 is never pushed; at 07:00 the phone
/// gets exactly one wake-up, with two notifiers over one store and after a restart during
/// quiet time; a removed phone gets nothing.
#[tokio::test]
async fn wake_ups_wait_through_quiet_time_and_go_out_once() {
    let (fake, base, server) = FakePush::start().await;
    let rig = rig(Options { allowlist: loopback(), quiet: true, ..Options::default() }).await;
    let browser = Browser::new();
    subscribe(&rig, &browser, &base).await;
    let first = rig.phone.start_push().unwrap();

    hold(&rig, "send_email", ApprovalRisk::Medium).await;
    fake.wait_for(1).await;

    rig.clock.set(at(24, 22, 30));
    tokio::time::sleep(Duration::from_millis(60)).await;
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    let answered = hold(&rig, "cargo_yank", ApprovalRisk::High).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fake.count(), 1, "nothing is sent during quiet time");
    let web = app(&rig.phone, person(&["owner", "operator"]));
    let deny = json!({ "type": "decide", "decisionId": answered.id, "version": answered.version, "answer": "deny", "expanded": true });
    let (status, _) = call(
        &web,
        "POST",
        "/api/surface/commands",
        Some(json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": deny })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A restart during quiet time: the first notifier stops; two new ones over the same stores
    // start, and send nothing while it is quiet.
    rig.clock.set(at(24, 23, 0));
    first.abort();
    let replica = |doorbell: Arc<PushDoorbell>| {
        Phone::builder(rig.phone.inner.engine.clone(), rig.pairing.clone())
            .home_url(HOME)
            .push_key(rig.phone.inner.sender.vapid().key().clone())
            .clock(rig.clock.clone())
            .doorbell(doorbell)
            .approvals(Arc::clone(&rig.approvals))
            .audit(AuditTrail::new(Arc::new(rig.audit.clone())))
            .push_allowlist(loopback())
            .quiet_poll(Duration::from_millis(20))
            .quiet_hours(
                QuietHours::new(
                    NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                    NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
                )
                .with_utc_offset_minutes(0),
            )
            .build()
            .unwrap()
    };
    let second = replica(Arc::new(PushDoorbell::new())).start_push().unwrap();
    let third = replica(Arc::new(PushDoorbell::new())).start_push().unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fake.count(), 1, "a restart during quiet time sends nothing");

    rig.clock.set(at(25, 7, 0));
    fake.wait_for(2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.count(), 2, "exactly one wake-up at 07:00");
    let sent: Vec<_> = records(&rig.audit, SURFACE_AUDIT_NOTIFY)
        .into_iter()
        .filter(|record| record.ext["outcome"] == "sent")
        .collect();
    assert_eq!(sent.len(), 2);
    let woken = sent[1].ext["approvalIds"].as_array().unwrap();
    assert!(woken.contains(&json!(release.id)), "{woken:?}");
    assert!(!woken.contains(&json!(answered.id)), "the answered decision is not pushed");
    assert!(sent.iter().all(|record| record.ext["device"] == json!(rig.device)));

    // A removed phone gets nothing.
    rig.pairing.revoke(&rig.device).await.unwrap();
    hold(&rig, "send_invoice", ApprovalRisk::Medium).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.count(), 2);
    second.abort();
    third.abort();
    server.abort();
}

/// A confirmation's binding verifies only its own tuple: the holder, the decision, the answer
/// and the edited arguments it was started for (spec 3.3 §8.2).
#[test]
fn a_binding_verifies_only_its_own_tuple() {
    use proptest::prelude::*;

    use super::passkey::{Passkeys, RelyingParty};
    use super::store::Holder;
    use super::webauthn::WebauthnVerifier;
    use crate::rules::Answer;
    use crate::web::phone::{ConfirmOptionsRequest, ConfirmRequest};

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let rp = RelyingParty { id: RP_ID.into(), origin: HOME.into() };
    let verifier = Arc::new(WebauthnVerifier::new(&rp).unwrap());
    let tuple = || (0usize..2, 0usize..3, any::<bool>(), 0usize..3);
    proptest!(ProptestConfig::with_cases(100), |(bound in tuple(), presented in tuple())| {
        let ok = runtime.block_on(async {
            let now = Utc::now();
            let passkeys = Passkeys::new(
                Some(verifier.clone()),
                super::store::Attachments::new(Arc::new(InMemoryDeviceStore::new())),
            );
            let holders = [Holder::Device("d1".into()), Holder::User(OWNER.into())];
            let mut authenticators = [authenticator(), authenticator()];
            for (holder, authenticator) in holders.iter().zip(authenticators.iter_mut()) {
                let options = passkeys.registration_options(holder.clone(), "ana", now).await.unwrap();
                let credential = authenticator.register(&options.public_key);
                passkeys.register(holder, &options.registration_id, &credential, now).await.unwrap();
            }
            let decisions = ["apr-a", "apr-b", "apr-c"];
            let args = [json!({ "x": 1 }), json!({ "x": 2 }), json!({ "y": 1 })];
            let answer = |edit: bool| if edit { Answer::Edit } else { Answer::AllowOnce };
            let (bound_holder, bound_decision, bound_edit, bound_args) = bound;
            let (holder, decision, edit, sent_args) = presented;
            let options = passkeys
                .confirm_options(
                    holders[bound_holder].clone(),
                    decisions[bound_decision],
                    ConfirmOptionsRequest {
                        version: 1,
                        answer: answer(bound_edit),
                        edited_args: bound_edit.then(|| args[bound_args].clone()),
                        expanded: true,
                    },
                    now,
                )
                .await
                .unwrap();
            let request = ConfirmRequest {
                challenge_id: options.challenge_id.clone(),
                credential: authenticators[holder].assert(&options.public_key),
                answer: Some(answer(edit)),
                edited_args: edit.then(|| args[sent_args].clone()),
            };
            passkeys.confirm(&holders[holder], decisions[decision], &request, now).await.is_ok()
        });
        let (bound_holder, bound_decision, bound_edit, bound_args) = bound;
        let (holder, decision, edit, sent_args) = presented;
        let own = bound_holder == holder
            && bound_decision == decision
            && bound_edit == edit
            && (!edit || bound_args == sent_args);
        prop_assert_eq!(ok, own);
    });
}

/// A passkey Chrome's platform authenticator made (CTAP2, internal, user verification),
/// recorded on an `http://localhost` origin, registers and confirms through the verifier; the
/// same assertion for another challenge, with a changed signature, or a registration for
/// another origin is refused (spec 3.3 §8.2).
#[test]
fn a_recorded_chrome_passkey_registers_and_confirms() {
    use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration};

    use super::passkey::{Ceremony, PasskeyVerifier as _, RelyingParty, StoredPasskey};
    use super::webauthn::WebauthnVerifier;

    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/mobile_support/fixtures/chrome-virtual-platform.json"
    ))
    .unwrap();
    let origin = fixture["origin"].as_str().unwrap();
    let rp = RelyingParty { id: "localhost".into(), origin: origin.into() };
    let verifier = WebauthnVerifier::new(&rp).unwrap();
    // The ceremony the recording answered: a fresh one with the recorded challenge.
    fn recorded<T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static>(
        ceremony: &Ceremony,
        key: &str,
        challenge: &Value,
    ) -> Ceremony {
        let mut state = serde_json::to_value(ceremony.downcast_ref::<T>().unwrap()).unwrap();
        state[key]["challenge"] = challenge.clone();
        Arc::new(serde_json::from_value::<T>(state).unwrap())
    }

    let registration = &fixture["registration"];
    let (_, ceremony) = verifier.start_registration([7; 16], "ana", &[]).unwrap();
    let ceremony = recorded::<PasskeyRegistration>(&ceremony, "rs", &registration["challenge"]);
    let created = verifier.finish_registration(&registration["credential"], &ceremony).unwrap();
    assert_eq!(created.credential_id, registration["credential"]["rawId"].as_str().unwrap());
    let stored = StoredPasskey {
        credential_id: created.credential_id,
        record: created.record,
        created_at: Utc::now(),
    };

    let assertion = &fixture["assertion"];
    let (_, fresh) = verifier.start_authentication(std::slice::from_ref(&stored)).unwrap();
    let ceremony = recorded::<PasskeyAuthentication>(&fresh, "ast", &assertion["challenge"]);
    let confirmed = verifier
        .finish_authentication(&assertion["credential"], &ceremony, std::slice::from_ref(&stored))
        .unwrap();
    assert_eq!(confirmed.credential_id, stored.credential_id);

    // Another challenge: the ceremony as started, not the recorded one.
    assert!(
        verifier
            .finish_authentication(&assertion["credential"], &fresh, std::slice::from_ref(&stored))
            .is_err()
    );
    // A changed signature.
    let mut forged = assertion["credential"].clone();
    let mut signature =
        Base64UrlUnpadded::decode_vec(forged["response"]["signature"].as_str().unwrap()).unwrap();
    let last = signature.len() - 1;
    signature[last] ^= 1;
    forged["response"]["signature"] = json!(Base64UrlUnpadded::encode_string(&signature));
    let ceremony = recorded::<PasskeyAuthentication>(&fresh, "ast", &assertion["challenge"]);
    assert!(
        verifier.finish_authentication(&forged, &ceremony, std::slice::from_ref(&stored)).is_err()
    );
    // Another origin.
    let elsewhere = WebauthnVerifier::new(&RelyingParty {
        id: "localhost".into(),
        origin: "http://localhost:1".into(),
    })
    .unwrap();
    let (_, ceremony) = elsewhere.start_registration([7; 16], "ana", &[]).unwrap();
    let ceremony = recorded::<PasskeyRegistration>(&ceremony, "rs", &registration["challenge"]);
    assert!(elsewhere.finish_registration(&registration["credential"], &ceremony).is_err());
}
