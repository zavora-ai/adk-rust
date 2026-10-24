//! Device pairing on the resident harness `adk-rust up` serves (spec 3.3 §10 K2, §11
//! S-I7): the claim route needs no caller, a device token reaches only the routes its scopes
//! open on the whole app, the server's own routes included, and on loopback a device token
//! is the device, never the operator.

#![cfg(feature = "harness")]

use std::net::SocketAddr;
use std::sync::Arc;

use adk_cli::harness::HarnessLauncher;
use adk_core::{InMemoryApprovalStore, InMemoryRunStore, StoreApprovalService};
use adk_harness::{HarnessAgent, HarnessMode};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_kernel::{Kernel, KernelConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:40000";
const REMOTE: &str = "100.64.0.7:40000";

/// The resident app of a harness on a kernel that pairs devices, admitting loopback peers.
fn app() -> (Router, tempfile::TempDir) {
    let (app, _, workspace) = app_with_approvals();
    (app, workspace)
}

/// [`app`], with the approval service its inbox serves.
fn app_with_approvals() -> (Router, Arc<StoreApprovalService>, tempfile::TempDir) {
    let workspace = tempfile::tempdir().unwrap();
    let agent = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("hi")])))
        .workspace(adk_devtools::Workspace::new(workspace.path()))
        .mode(HarnessMode::Plan)
        .build()
        .unwrap();
    // The inbox and the kernel share the service, as `adk-rust up` shares the harness's, so
    // the device guard reads a decision's risk.
    let approvals = Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let kernel = Kernel::builder()
        .config(
            KernelConfig::default().with_app_name("harness").with_control(
                ControlPlaneConfig::default()
                    .without_socket()
                    .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new()))),
            ),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .approval_service(approvals.clone())
        .build()
        .unwrap();
    let app = HarnessLauncher::new(agent)
        .with_user_id("ana")
        .with_approval_service(approvals.clone())
        .with_kernel(kernel)
        .build_app()
        .unwrap();
    (app, approvals, workspace)
}

fn request(
    method: &str,
    uri: &str,
    body: Option<Value>,
    peer: &str,
    token: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let mut request =
        builder.body(body.map_or_else(Body::empty, |body| Body::from(body.to_string()))).unwrap();
    request.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    request
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Pairs a device with `scopes` from a remote peer and returns its token.
async fn pair(app: &Router, scopes: Value) -> String {
    let issue =
        request("POST", "/api/kernel/pairing", Some(json!({ "scopes": scopes })), LOOPBACK, None);
    let (status, issued) = call(app, issue).await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    let code = issued["code"].as_str().unwrap().to_string();
    // The phone is not on loopback and has no credentials: the code is its credential.
    let claim =
        request("POST", "/api/kernel/pairing/claim", Some(json!({ "code": code })), REMOTE, None);
    let (status, claimed) = call(app, claim).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    claimed["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_device_token_reaches_only_its_scoped_routes_on_the_resident_app() {
    let (app, _workspace) = app();
    let token = pair(&app, json!(["read"])).await;

    let (status, _) =
        call(&app, request("GET", "/api/kernel/health", None, REMOTE, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "read reaches the kernel's health from anywhere");
    let (status, _) =
        call(&app, request("GET", "/api/approvals", None, REMOTE, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "and the decisions");

    let decide = request(
        "POST",
        "/api/approvals/a1/decide",
        Some(json!({"expectedVersion": 1, "action": "approve"})),
        REMOTE,
        Some(&token),
    );
    assert_eq!(call(&app, decide).await.0, StatusCode::FORBIDDEN, "read may not decide");
    // The server's own routes are closed to devices too.
    for (method, uri) in
        [("GET", "/api/rules"), ("GET", "/api/apps"), ("POST", "/api/kernel/unfreeze")]
    {
        let (status, body) = call(&app, request(method, uri, None, LOOPBACK, Some(&token))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }
    // Without the token the remote peer is nobody; with a forged one it is refused.
    assert_eq!(
        call(&app, request("GET", "/api/kernel/health", None, REMOTE, None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let forged = request("GET", "/api/kernel/health", None, LOOPBACK, Some("adkd_00_11"));
    assert_eq!(
        call(&app, forged).await.0,
        StatusCode::UNAUTHORIZED,
        "no fallback to the loopback operator"
    );
}

#[tokio::test]
async fn a_revoked_device_is_refused_on_the_resident_app() {
    let (app, _workspace) = app();
    let token = pair(&app, json!(["read", "stop"])).await;
    let (status, devices) =
        call(&app, request("GET", "/api/kernel/devices", None, LOOPBACK, None)).await;
    assert_eq!(status, StatusCode::OK);
    let id = devices["devices"][0]["id"].as_str().unwrap().to_string();
    let revoke = request("POST", &format!("/api/kernel/devices/{id}/revoke"), None, LOOPBACK, None);
    assert_eq!(call(&app, revoke).await.0, StatusCode::OK);
    let (status, _) =
        call(&app, request("GET", "/api/kernel/health", None, REMOTE, Some(&token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Review finding 17: the claim route keeps the server's host and origin checks, so a page
/// rebinding its name to the loopback address cannot spend the waiting codes.
#[tokio::test]
async fn the_claim_route_keeps_the_host_and_origin_checks() {
    let (app, _workspace) = app();
    let body = json!({ "code": "0000-0000-0000" });
    let mut rebound =
        request("POST", "/api/kernel/pairing/claim", Some(body.clone()), LOOPBACK, None);
    rebound.headers_mut().insert("host", "evil.example:8080".parse().unwrap());
    assert_eq!(call(&app, rebound).await.0, StatusCode::FORBIDDEN, "a foreign host");
    let mut cross = request("POST", "/api/kernel/pairing/claim", Some(body), LOOPBACK, None);
    cross.headers_mut().insert("origin", "https://evil.example".parse().unwrap());
    assert_eq!(call(&app, cross).await.0, StatusCode::FORBIDDEN, "a foreign origin");
}

/// Review finding 18: a client still holding a removed device's token re-pairs on the
/// resident app: the claim route skips the device guard.
#[tokio::test]
async fn a_client_holding_a_removed_token_re_pairs_on_the_resident_app() {
    let (app, _workspace) = app();
    let old = pair(&app, json!(["read"])).await;
    let (_, devices) =
        call(&app, request("GET", "/api/kernel/devices", None, LOOPBACK, None)).await;
    let id = devices["devices"][0]["id"].as_str().unwrap().to_string();
    let revoke = request("POST", &format!("/api/kernel/devices/{id}/revoke"), None, LOOPBACK, None);
    assert_eq!(call(&app, revoke).await.0, StatusCode::OK);

    let issue = request("POST", "/api/kernel/pairing", Some(json!({})), LOOPBACK, None);
    let (_, issued) = call(&app, issue).await;
    let claim = request(
        "POST",
        "/api/kernel/pairing/claim",
        Some(json!({ "code": issued["code"] })),
        REMOTE,
        Some(&old),
    );
    let (status, claimed) = call(&app, claim).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
}

/// Review finding 5: on the resident app a `decide` device answers an everyday decision,
/// which the device guard checked and recorded before the extractor admitted it, and is
/// refused a high-risk one.
#[tokio::test]
async fn a_decide_device_answers_an_everyday_decision_on_the_resident_app() {
    use adk_core::{
        ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
        ApproverSelector, ToolConfirmationRequest,
    };

    let (app, approvals, _workspace) = app_with_approvals();
    let token = pair(&app, json!(["decide"])).await;
    let ask = |id: &str, risk: ApprovalRisk| {
        ApprovalRequest::new(
            id,
            ApprovalScope::new("harness", "ana", "s1"),
            &ToolConfirmationRequest::new("send_email", json!({})),
            ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
        )
        .with_risk(risk)
    };
    let everyday = approvals.submit(ask("everyday", ApprovalRisk::Medium)).await.unwrap();
    let high = approvals.submit(ask("high", ApprovalRisk::High)).await.unwrap();
    let decide = |id: &str, version: u64| {
        request(
            "POST",
            &format!("/api/approvals/{id}/decide"),
            Some(json!({ "expectedVersion": version, "action": "approve" })),
            REMOTE,
            Some(&token),
        )
    };
    let (status, body) = call(&app, decide("everyday", everyday.version)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, decide("high", high.version)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
