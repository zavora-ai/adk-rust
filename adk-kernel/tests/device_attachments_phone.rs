//! The kernel side of the phone (spec 3.3 §5.4, §7.2, §10 K2, K3): device kinds the issuer
//! sets, the server's routes for devices beside the kernel's, attachments that go with a
//! revoked device, feeds that end on revoke, and the quiet time a wake-up waits for.

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApproverSelector, InMemoryApprovalStore, InMemoryRunStore, StoreApprovalService,
    ToolConfirmationRequest,
};
use adk_kernel::control::api::IssueRequest;
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig, InMemoryControlStore};
use adk_kernel::pairing::{
    AttachmentHolder, DEVICE_KIND_METADATA, DeviceAttachment, DeviceKind, DeviceRoutes,
    DeviceScope, DeviceStore, DeviceTokenExtractor, InMemoryDeviceStore, IssueCode, Pairing,
};
use adk_kernel::{
    InMemoryProcessStore, Kernel, KernelConfig, Partition, PartitionId, Partitions, ProcessStore,
    QuietHours,
};
use adk_server::auth_bridge::RequestContextExtractor as _;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, TimeZone as _, Utc};
use serde_json::json;

const OWNER: &str = "ana";

/// The routes a surface server opens to devices: the phone's answer and Stop all.
fn surface_routes() -> DeviceRoutes {
    Arc::new(|method: &Method, path: &str| {
        let path = path.split('?').next().unwrap_or_default();
        let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
        match (method.as_str(), segments.as_slice()) {
            (_, ["api", "surface", "commands"]) => None,
            ("GET", ["api", "surface", ..]) => Some(DeviceScope::Read),
            ("POST", ["api", "surface", "decisions", _, "answer"]) => Some(DeviceScope::Decide),
            ("POST", ["api", "surface", "stop"]) => Some(DeviceScope::Stop),
            _ => None,
        }
    })
}

async fn paired(pairing: &Pairing, kind: Option<DeviceKind>) -> (String, String) {
    let mut code = IssueCode::new(OWNER, ["operator"]);
    if let Some(kind) = kind {
        code = code.with_kind(kind);
    }
    let issued = pairing.issue(code).await.unwrap();
    let claimed = pairing.claim(&issued.code, Some("Ana's laptop")).await.unwrap();
    (claimed.token, claimed.device.id)
}

/// The issuer sets a device's kind; the claim keeps it; a device paired without one is a
/// remote terminal, never a phone (spec 3.3 §7.2).
#[tokio::test]
async fn the_issuer_sets_the_kind_and_the_claim_keeps_it() {
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    for (kind, expected) in [
        (Some(DeviceKind::Phone), DeviceKind::Phone),
        (Some(DeviceKind::Tray), DeviceKind::Tray),
        (None, DeviceKind::Terminal),
    ] {
        let (token, id) = paired(&pairing, kind).await;
        let device = pairing.authenticate(&token).await.unwrap();
        assert_eq!((device.kind, device.kind()), (kind, expected));
        assert_eq!(device.view(Utc::now()).kind, expected);
        let context = pairing.authorize(&token, &Method::GET, "/api/kernel/health").await.unwrap();
        assert_eq!(context.metadata[DEVICE_KIND_METADATA], expected.as_str(), "{id}");
    }
}

/// The server's map opens its own routes to devices after the kernel's, which it never
/// overrides: commands and `unfreeze` stay closed (spec 3.3 §10 K2, S-I10).
#[test]
fn the_server_s_routes_follow_the_kernel_s() {
    let pairing =
        Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_device_routes(surface_routes());
    let cases = [
        (Method::GET, "/api/surface/decisions?surface=phone", Some(DeviceScope::Read)),
        (Method::POST, "/api/surface/decisions/apr-1/answer", Some(DeviceScope::Decide)),
        (Method::POST, "/api/surface/stop", Some(DeviceScope::Stop)),
        (Method::POST, "/api/surface/commands", None),
        (Method::POST, "/api/approvals/apr-1/decide", Some(DeviceScope::Decide)),
        (Method::POST, "/api/kernel/freeze", Some(DeviceScope::Stop)),
        (Method::POST, "/api/kernel/unfreeze", None),
        (Method::GET, "/api/kernel/health", Some(DeviceScope::Read)),
    ];
    for (method, path, scope) in cases {
        assert_eq!(pairing.scope_for(&method, path), scope, "{method} {path}");
    }
    let without = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    assert_eq!(without.scope_for(&Method::GET, "/api/surface/status"), None);
}

/// A kernel with device pairing over the server's routes and an approval inbox.
async fn kernel(pairing: Pairing) -> (Kernel, Arc<dyn ApprovalService>) {
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
        ));
    let config = KernelConfig::default()
        .with_app_name("phone")
        .with_control(ControlPlaneConfig::default().without_socket().with_pairing(pairing));
    let kernel = Kernel::builder()
        .config(config)
        .run_store(Arc::new(InMemoryRunStore::new()))
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .approval_service(approvals.clone())
        .build()
        .unwrap();
    (kernel, approvals)
}

async fn hold_high_risk(approvals: &Arc<dyn ApprovalService>) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new("cargo_publish", json!({ "crate": "adk-kernel" }));
    let request = ApprovalRequest::new(
        "apr-release",
        ApprovalScope::new("phone", OWNER, "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
    )
    .with_risk(ApprovalRisk::High);
    approvals.submit(request).await.unwrap()
}

/// The guard keeps its risk check and record for the kernel's decide route only: a phone's
/// high-risk answer on the server's answer route is admitted for the server's engine, which
/// applies §7.2 itself; the same phone's high-risk yes on `POST /approvals/{id}/decide` is
/// refused. Behind the guard the extractor admits the server's decide route; without it, it
/// refuses it.
#[tokio::test]
async fn the_guard_checks_risk_on_the_kernel_s_decide_route_only() {
    let pairing =
        Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_device_routes(surface_routes());
    let (kernel, approvals) = kernel(pairing.clone()).await;
    let release = hold_high_risk(&approvals).await;
    let (token, _) = paired(&pairing, Some(DeviceKind::Phone)).await;
    let extractor = DeviceTokenExtractor::new(pairing.clone());
    let reached = move |request: Request<Body>| {
        let extractor = extractor.clone();
        async move {
            let (parts, _) = request.into_parts();
            match extractor.extract(&parts).await {
                Ok(context) => (StatusCode::OK, context.metadata[DEVICE_KIND_METADATA].clone()),
                Err(error) => (StatusCode::UNAUTHORIZED, error.to_string()),
            }
        }
    };
    let app = axum::Router::new()
        .route("/api/surface/decisions/{id}/answer", axum::routing::post(reached.clone()))
        .route("/api/approvals/{id}/decide", axum::routing::post(reached.clone()));
    let serve = |app: axum::Router| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, server)
    };
    let (guarded, guarded_server) =
        serve(adk_kernel::control::guard_devices(&kernel, app.clone())).await;
    let (bare, bare_server) = serve(app).await;
    let client = reqwest::Client::new();
    let post = |url: String, body: serde_json::Value| {
        client.post(url).bearer_auth(&token).json(&body).send()
    };

    let answer = format!("/api/surface/decisions/{}/answer", release.id);
    let deny = json!({ "version": release.version, "answer": "deny", "expanded": true });
    let response = post(format!("{guarded}{answer}"), deny.clone()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "admitted for the engine");
    assert_eq!(response.text().await.unwrap(), "phone");

    let decide = format!("{guarded}/api/approvals/{}/decide", release.id);
    let response = post(decide, json!({ "action": "approve" })).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN, "the kernel's route checks the risk");

    let response = post(format!("{bare}{answer}"), deny).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "never without the guard");
    guarded_server.abort();
    bare_server.abort();
}

/// Revoking a device removes its attachments and nobody else's, and its open feeds end at
/// once (spec 3.3 §10 K2, S-I7).
#[tokio::test]
async fn a_revoked_device_loses_its_attachments_and_its_feeds() {
    let store = Arc::new(InMemoryDeviceStore::new());
    let pairing = Pairing::new(store.clone()).with_revocation_check(Duration::from_secs(3600));
    let (_, phone) = paired(&pairing, Some(DeviceKind::Phone)).await;
    let (_, other) = paired(&pairing, Some(DeviceKind::Phone)).await;
    let attach = |id: &str, kind: &str| {
        DeviceAttachment::new(
            AttachmentHolder::Device(id.to_string()),
            kind,
            "only",
            json!({ "id": id }),
            Utc::now(),
        )
    };
    for (id, kind) in [(&phone, "push"), (&phone, "passkey"), (&other, "push")] {
        store.put_attachment(attach(id, kind)).await.unwrap();
    }
    let person = AttachmentHolder::User(OWNER.into());
    store
        .put_attachment(DeviceAttachment::new(
            person.clone(),
            "passkey",
            "c1",
            json!({}),
            Utc::now(),
        ))
        .await
        .unwrap();

    let ends = tokio::spawn(pairing.until_device_ends(phone.clone()));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!ends.is_finished(), "a paired device's feed stays open");
    pairing.revoke(&phone).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), ends).await.expect("ended at once").unwrap();

    let holder = AttachmentHolder::Device(phone.clone());
    assert!(store.attachments(&holder, "push").await.unwrap().is_empty());
    assert!(store.attachments(&holder, "passkey").await.unwrap().is_empty());
    let theirs = AttachmentHolder::Device(other);
    assert_eq!(store.attachments(&theirs, "push").await.unwrap().len(), 1);
    assert_eq!(store.attachments(&person, "passkey").await.unwrap().len(), 1);
}

/// `pairing.issue` records the kind the code grants, as `adk-rust pair --kind` asks.
#[tokio::test]
async fn the_issue_route_takes_and_records_the_kind() {
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    let plane_kernel = {
        let config = KernelConfig::default().with_app_name("phone").with_control(
            ControlPlaneConfig::default()
                .without_socket()
                .with_http("127.0.0.1:0".parse().unwrap())
                .with_pairing(pairing.clone()),
        );
        Kernel::builder()
            .config(config)
            .run_store(Arc::new(InMemoryRunStore::new()))
            .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
            .build()
            .unwrap()
    };
    let plane = ControlPlane::start(&plane_kernel).await.unwrap();
    let operator = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    let request = IssueRequest { kind: Some(DeviceKind::Phone), ..IssueRequest::default() };
    let issued = operator.issue_pairing_code(&request).await.unwrap();
    let claimed = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()))
        .claim_pairing(&issued.code, Some("Ana's phone"))
        .await
        .unwrap();
    assert_eq!(claimed.device.kind, DeviceKind::Phone);
    let listed = operator.devices().await.unwrap();
    assert_eq!(listed[0].kind, DeviceKind::Phone);
    plane.shutdown().await.unwrap();
}

fn utc(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, hour, minute, 0).unwrap()
}

fn window(text: &str) -> QuietHours {
    text.parse::<QuietHours>().unwrap().with_utc_offset_minutes(0)
}

/// The quiet time a wake-up waits for: the kernel's, followed into a partition's window that
/// touches it, and the person's change made while the kernel runs (spec 3.3 §5.4, §10 K3).
#[tokio::test]
async fn quiet_until_follows_partitions_and_changes() {
    let partitions = Partitions::in_memory();
    partitions
        .put(
            Partition::new("home", "home").with_quiet_hours(window("07:00-09:00")),
            AuditActor::default(),
        )
        .await
        .unwrap();
    partitions
        .put(
            Partition::new("work", "work").with_quiet_hours(window("12:00-14:00")),
            AuditActor::default(),
        )
        .await
        .unwrap();
    let config = KernelConfig::default()
        .with_app_name("phone")
        .with_quiet_hours(window("22:00-07:00"))
        .with_control(
            ControlPlaneConfig::default()
                .without_socket()
                .with_http("127.0.0.1:0".parse().unwrap())
                .with_control_store(Arc::new(InMemoryControlStore::new())),
        );
    let kernel = Kernel::builder()
        .config(config)
        .run_store(Arc::new(InMemoryRunStore::new()))
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .partitions(partitions)
        .build()
        .unwrap();
    let default = PartitionId::default();
    let (home, work) = (PartitionId::new("home"), PartitionId::new("work"));

    assert_eq!(kernel.quiet_until(&default, utc(24, 18, 0)).await, None);
    assert_eq!(kernel.quiet_until(&default, utc(24, 22, 30)).await, Some(utc(25, 7, 0)));
    assert_eq!(
        kernel.quiet_until(&home, utc(24, 22, 30)).await,
        Some(utc(25, 9, 0)),
        "touching windows"
    );
    assert_eq!(kernel.quiet_until(&work, utc(25, 13, 0)).await, Some(utc(25, 14, 0)));
    assert_eq!(kernel.quiet_until(&default, utc(25, 13, 0)).await, None, "the partition's own");

    let plane = ControlPlane::start(&kernel).await.unwrap();
    let operator = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    operator.set_quiet_time("default", Some(window("23:00-06:00"))).await.unwrap();
    assert_eq!(kernel.quiet_until(&default, utc(24, 22, 30)).await, None, "the change holds");
    assert_eq!(kernel.quiet_until(&default, utc(24, 23, 30)).await, Some(utc(25, 6, 0)));
    plane.shutdown().await.unwrap();
}
