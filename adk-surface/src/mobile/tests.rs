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
    /// An in-process kernel with a mounted driver declaring `post_update` `publish_public`,
    /// `pay_invoice` `financial` and `save_draft` `write_local`.
    drivers: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { home: Some(HOME), quiet: false, allowlist: PushAllowlist::default(), drivers: false }
    }
}

/// A kernel with the `social` driver mounted for the owner (spec 3.2 §7.7).
async fn kernel_with_drivers() -> adk_kernel::Kernel {
    use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
    use adk_kernel::stores::InMemoryDriverStore;

    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
    let manifest = DriverManifest::from_toml(
        r#"
        id        = "social"
        command   = "mcp-social"
        lifecycle = "lazy"

        [tools]
        post_update = "publish_public"
        pay_invoice = "financial"
        save_draft  = "write_local"
        "#,
    )
    .unwrap();
    drivers.mount(manifest, OWNER).await.unwrap();
    adk_kernel::Kernel::builder()
        .config(adk_kernel::KernelConfig::default().with_app_name(APP))
        .run_store(Arc::new(adk_core::InMemoryRunStore::new()))
        .drivers(drivers)
        .build()
        .unwrap()
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
    let mut source = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(Arc::clone(&approvals))
        .governance(adk_core::GovernanceControl::new());
    if options.drivers {
        source = source.kernel(kernel_with_drivers().await, APP);
    }
    let source = source.build().unwrap();
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

/// A call a mounted driver declares `publish_public` or `financial` is high risk on the phone
/// and on a remote terminal whatever its request says, as the kernel's own decide route and
/// chat rate it (spec 3.3 §7.2, spec 3.2 K-I9): the card asks to confirm it is you, the plain
/// answer route refuses Allow once, and a terminal device cannot approve it at all. The
/// driver's everyday call is answered by touch.
#[tokio::test]
async fn a_driver_s_public_or_financial_call_needs_confirm_it_s_you() {
    let rig = rig(Options { drivers: true, ..Options::default() }).await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let terminal = app(&rig.phone, device_context(&rig.device, None));
    for tool in ["post_update", "pay_invoice"] {
        let held = hold(&rig, tool, ApprovalRisk::Medium).await;
        let (status, card) =
            call(&phone, "GET", &format!("/api/surface/decisions/{}?surface=phone", held.id), None)
                .await;
        assert_eq!(status, StatusCode::OK, "{card}");
        assert_eq!(card["risk"], json!("high"), "{tool}: {card}");
        let allow_once = card["allowed"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["answer"] == json!("allowOnce"))
            .cloned()
            .unwrap_or_default();
        assert_eq!(allow_once["needsPresence"], json!(true), "{tool}: {card}");
        let answer = format!("/api/surface/decisions/{}/answer", held.id);
        let (status, problem) = call(&phone, "POST", &answer, Some(allow(held.version))).await;
        assert_eq!(
            (status, code(&problem)),
            (StatusCode::FORBIDDEN, "surface.confirm_presence"),
            "{tool}: {problem}"
        );
        let (status, problem) = call(&terminal, "POST", &answer, Some(allow(held.version))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{tool}: {problem}");
        assert_eq!(state(&rig, &held.id).await, ApprovalState::Pending, "{tool}");
    }
    let everyday = hold(&rig, "save_draft", ApprovalRisk::Medium).await;
    let (status, outcome) = call(
        &phone,
        "POST",
        &format!("/api/surface/decisions/{}/answer", everyday.id),
        Some(allow(everyday.version)),
    )
    .await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("answered")), "{outcome}");
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

/// A quiet time the test moves: quiet until its end while the clock is before it.
#[derive(Clone, Default)]
struct MovableQuiet(Arc<std::sync::Mutex<Option<DateTime<Utc>>>>);

impl MovableQuiet {
    fn ends_at(&self, end: DateTime<Utc>) {
        *self.0.lock().unwrap() = Some(end);
    }
}

#[async_trait::async_trait]
impl super::quiet::QuietTime for MovableQuiet {
    async fn until(&self, _run_id: Option<&str>, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.0.lock().unwrap().filter(|end| *end > at)
    }
}

/// The rig's push task parts over `quiet`, for driving one path of it.
fn ringer(rig: &Rig, quiet: MovableQuiet) -> super::doorbell::Ringer {
    let inner = &rig.phone.inner;
    super::doorbell::Ringer {
        pairing: inner.pairing.clone(),
        store: inner.store.clone(),
        sender: inner.sender.clone(),
        quiet: Arc::new(quiet),
        approvals: Arc::clone(&rig.approvals),
        audit: inner.audit.clone(),
        clock: Arc::clone(&inner.clock),
        deliveries: Arc::clone(&inner.deliveries),
        poll: inner.poll,
    }
}

/// The keys of the wake-up claims and marks the rig's phone holds.
async fn wake_rows(rig: &Rig) -> Vec<String> {
    let holder = super::store::Holder::Device(rig.device.clone());
    let rows = rig.phone.inner.store.list(&holder, super::store::WAKE).await.unwrap();
    let mut keys: Vec<String> = rows.into_iter().map(|row| row.key).collect();
    keys.sort();
    keys
}

/// The keys of the wake-up rows once they equal `want`, or after four seconds.
async fn wake_rows_become(rig: &Rig, want: &[String]) -> Vec<String> {
    for _ in 0..400 {
        let keys = wake_rows(rig).await;
        if keys == want {
            return keys;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    wake_rows(rig).await
}

/// A wake-up held for quiet time follows a change the person makes meanwhile (spec 3.3 §5.4):
/// quiet time moved from 07:00 to 09:00 sends nothing at 07:00 and one wake-up at 09:00,
/// claimed by the new end, which the sweep at 09:00 shares.
#[tokio::test]
async fn a_held_wake_up_follows_quiet_time_changed_meanwhile() {
    let (fake, base, server) = FakePush::start().await;
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    subscribe(&rig, &Browser::new(), &base).await;
    let quiet = MovableQuiet::default();
    quiet.ends_at(at(25, 7, 0));
    let ringer = ringer(&rig, quiet.clone());
    rig.clock.set(at(24, 23, 0));
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    ringer.submitted(release).await;

    quiet.ends_at(at(25, 9, 0));
    rig.clock.set(at(25, 7, 0));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.count(), 0, "nothing is sent at the old end");
    rig.clock.set(at(25, 9, 0));
    fake.wait_for(1).await;
    let claim = format!("quiet:{}", at(25, 9, 0).to_rfc3339());
    assert!(wake_rows(&rig).await.contains(&claim), "{:?}", wake_rows(&rig).await);
    server.abort();
}

/// The wake-up at the end of quiet time marks every decision it announced, whichever held
/// wake-up takes its claim, so a restart wakes no phone again for one of them (spec 3.3 §5.4).
#[tokio::test]
async fn the_wake_up_at_the_end_of_quiet_time_marks_every_decision_it_announced() {
    let (fake, base, server) = FakePush::start().await;
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    subscribe(&rig, &Browser::new(), &base).await;
    let quiet = MovableQuiet::default();
    quiet.ends_at(at(25, 7, 0));
    let ringer = ringer(&rig, quiet);
    rig.clock.set(at(24, 23, 0));
    let invoice = hold(&rig, "send_email", ApprovalRisk::Medium).await;
    ringer.submitted(invoice).await;
    rig.clock.set(at(24, 23, 30));
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    ringer.submitted(release).await;

    rig.clock.set(at(25, 7, 0));
    fake.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fake.count(), 1, "one wake-up at 07:00");
    // A restart at 09:00 with both still waiting.
    rig.clock.set(at(25, 9, 0));
    ringer.sweep(None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.count(), 1, "a restart wakes no phone twice for one decision");
    server.abort();
}

/// What a wake-up claimed goes once nothing needs it (spec 3.3 §5.4): after a restart, the
/// claim of a decision answered more than ten minutes ago is gone and a waiting one's stays;
/// once the pairing ends, every attachment of the device goes.
#[tokio::test]
async fn spent_wake_up_claims_are_removed() {
    let (fake, base, server) = FakePush::start().await;
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    subscribe(&rig, &Browser::new(), &base).await;
    let first = rig.phone.start_push().unwrap();
    let invoice = hold(&rig, "send_email", ApprovalRisk::Medium).await;
    let release = hold(&rig, "cargo_publish", ApprovalRisk::High).await;
    fake.wait_for(1).await;
    let both = vec![
        format!("{}:{}", release.id, release.version),
        format!("{}:{}", invoice.id, invoice.version),
    ];
    let mut sorted = both.clone();
    sorted.sort();
    assert_eq!(wake_rows_become(&rig, &sorted).await, sorted, "both decisions marked");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sent = fake.count();
    let web = app(&rig.phone, person(&["owner", "operator"]));
    let deny = json!({ "type": "decide", "decisionId": invoice.id, "version": invoice.version, "answer": "deny", "expanded": true });
    let (status, _) = call(
        &web,
        "POST",
        "/api/surface/commands",
        Some(json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": deny })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    first.abort();
    rig.clock.advance(Duration::from_secs(11 * 60));
    let second = rig.phone.start_push().unwrap();
    let kept = vec![both[0].clone()];
    assert_eq!(wake_rows_become(&rig, &kept).await, kept, "the answered decision's claim went");
    assert_eq!(fake.count(), sent, "a restart wakes nothing it announced");

    second.abort();
    rig.clock.advance(Duration::from_secs(91 * 24 * 60 * 60));
    let third = rig.phone.start_push().unwrap();
    assert_eq!(wake_rows_become(&rig, &[]).await, Vec::<String>::new(), "an ended device's claims");
    let holder = super::store::Holder::Device(rig.device.clone());
    let push = rig.phone.inner.store.list(&holder, super::store::PUSH).await.unwrap();
    assert!(push.is_empty(), "an ended device's subscription went: {push:?}");
    third.abort();
    server.abort();
}

/// A wake-up the push service could not take, busy on every attempt, is sent again while its
/// decision waits (spec 3.3 §5.4, §7.3).
#[tokio::test]
async fn a_wake_up_the_push_service_could_not_take_is_sent_again() {
    let (fake, base, server) = FakePush::start().await;
    *fake.status.lock().unwrap() = Some(503);
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    subscribe(&rig, &Browser::new(), &base).await;
    let task = rig.phone.start_push().unwrap();
    hold(&rig, "send_email", ApprovalRisk::Medium).await;
    fake.wait_for(3).await;
    *fake.status.lock().unwrap() = None;
    for _ in 0..400 {
        if outcomes(&rig.audit).iter().any(|(_, outcome)| outcome == "sent") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        outcomes(&rig.audit).iter().any(|(_, outcome)| outcome == "sent"),
        "the wake-up was sent again: {:?}",
        outcomes(&rig.audit)
    );
    assert_eq!(outcomes(&rig.audit)[0].1, "dropped", "the first try is recorded");
    task.abort();
    server.abort();
}

/// After the push key changes, a phone subscribed under the old key is not woken, so it is
/// told to allow notifications again, and subscribing under the new key turns them back on
/// (spec 3.3 §5.4 "The push key").
#[tokio::test]
async fn a_new_push_key_asks_the_phone_to_allow_notifications_again() {
    let rig = rig(Options { allowlist: loopback(), ..Options::default() }).await;
    let browser = Browser::new();
    subscribe(&rig, &browser, "http://localhost:4999").await;
    let phone = app(&rig.phone, device_context(&rig.device, Some("phone")));
    let (_, info) = call(&phone, "GET", "/api/surface/phone", None).await;
    assert_eq!(info["push"], json!(true), "{info}");

    // The key file was deleted: the server starts with a new key.
    let renewed = Phone::builder(rig.phone.inner.engine.clone(), rig.pairing.clone())
        .home_url(HOME)
        .push_key(PushKey::generate().unwrap())
        .clock(rig.clock.clone())
        .approvals(Arc::clone(&rig.approvals))
        .push_allowlist(loopback())
        .build()
        .unwrap();
    let phone = app(&renewed, device_context(&rig.device, Some("phone")));
    let (_, info) = call(&phone, "GET", "/api/surface/phone", None).await;
    assert_eq!(info["push"], json!(false), "{info}");
    let operator = app(&renewed, person(&["owner", "operator"]));
    let (_, devices) = call(&operator, "GET", "/api/surface/phone/devices", None).await;
    assert_eq!(devices["devices"][0]["push"], json!(false), "{devices}");

    let (status, body) = call(
        &phone,
        "PUT",
        "/api/surface/phone/push",
        Some(browser.subscription("http://localhost:4999/push/2")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, info) = call(&phone, "GET", "/api/surface/phone", None).await;
    assert_eq!(info["push"], json!(true), "{info}");
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
