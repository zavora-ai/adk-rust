//! Device pairing on a real kernel's control plane (spec 3.3 §10 K2, §11 S-I7, S-I10), over
//! HTTP on loopback as `adk-rust up` serves it:
//!
//! - scenario (c): a device paired with `read` only reads but cannot decide; once revoked,
//!   its token is refused;
//! - a device with `stop` stops everything and cannot resume;
//! - a device token on loopback is the device, never the operator;
//! - a device decides low and medium risk, never high risk without confirming it is the
//!   person;
//! - a code pairs one device, and pairing never widens the bind;
//! - every pairing, removal and device action is recorded.

mod common;

use std::sync::Arc;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    AdkError, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ApproverSelector, ErrorCategory, GovernanceControl,
    InMemoryApprovalStore, StoreApprovalService, ToolConfirmationRequest,
};
use adk_kernel::control::api::{IssueRequest, SpawnRequest};
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig};
use adk_kernel::pairing::{DeviceScope, DeviceState, InMemoryDeviceStore, Pairing};
use adk_kernel::{Kernel, KernelConfig};
use common::{APP, World};
use serde_json::json;

/// A kernel serving its control plane on loopback with device pairing, the approvals inbox
/// routed to the `operator` group, and an audit trail.
struct Plane {
    kernel: Kernel,
    plane: ControlPlane,
    approvals: Arc<dyn ApprovalService>,
    recorder: InMemoryAuditRecorder,
    _world: World,
}

impl Plane {
    async fn start() -> Self {
        let world = World::answer();
        let control = GovernanceControl::new();
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
            ),
        );
        let recorder = InMemoryAuditRecorder::new();
        let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()))
            .with_home_url("https://home.example");
        let config = KernelConfig::default()
            .with_app_name(APP)
            .with_governance(control.clone())
            .with_control(
                ControlPlaneConfig::default()
                    .without_socket()
                    .with_http("127.0.0.1:0".parse().unwrap())
                    .with_pairing(pairing),
            );
        let kernel = Kernel::builder()
            .config(config)
            .run_store(world.runs.clone())
            .session_service(world.sessions.clone())
            .approval_service(approvals.clone())
            .audit(AuditTrail::new(Arc::new(recorder.clone())))
            .agent(common::TARGET, world.runner(&control))
            .build()
            .unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        Self { kernel, plane, approvals, recorder, _world: world }
    }

    fn base(&self) -> String {
        format!("http://{}", self.plane.http_addr().unwrap())
    }

    /// The person at the kernel: a loopback caller without a token is the operator.
    fn operator(&self) -> ControlClient {
        ControlClient::http(self.base())
    }

    /// Pairs a device with `scopes` and returns a client holding its token.
    async fn device(&self, scopes: &[DeviceScope]) -> (ControlClient, String) {
        let request = IssueRequest {
            name: Some("Ana's phone".into()),
            scopes: scopes.to_vec(),
            expires_in_days: None,
        };
        let issued = self.operator().issue_pairing_code(&request).await.unwrap();
        assert_eq!(
            issued.url.as_deref(),
            Some(&*format!("https://home.example/pair#code={}", issued.code))
        );
        // The claim needs no caller: the code is the credential.
        let claimed =
            ControlClient::http(self.base()).claim_pairing(&issued.code, None).await.unwrap();
        assert_eq!(claimed.device.scopes, scopes);
        (ControlClient::http(self.base()).with_bearer_token(claimed.token), claimed.device.id)
    }

    /// Submits a held call of `ana` at `risk`, routed to the `operator` group.
    async fn ask(&self, risk: ApprovalRisk) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new("send_email", json!({"to": "client@example.com"}));
        let request = ApprovalRequest::new(
            format!("decision-{}", uuid::Uuid::new_v4().simple()),
            ApprovalScope::new(APP, "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
        )
        .with_summary("Send the invoice")
        .with_risk(risk);
        self.approvals.submit(request).await.unwrap()
    }

    /// The `action` fields of the `kernel.control` records written so far.
    fn control_actions(&self) -> Vec<(String, serde_json::Value)> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == adk_kernel::audit::CONTROL)
            .map(|record| {
                let action = record.ext["action"].as_str().unwrap_or_default().to_string();
                (action, serde_json::to_value(&record.ext).unwrap())
            })
            .collect()
    }
}

fn category(error: adk_kernel::KernelError) -> ErrorCategory {
    AdkError::from(error).category
}

/// Scenario (c): a device with `read` only reads but cannot decide; revoked, its token is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_only_device_cannot_decide_and_a_revoked_token_is_refused() {
    let plane = Plane::start().await;
    let (device, id) = plane.device(&[DeviceScope::Read]).await;
    let request = plane.ask(ApprovalRisk::Low).await;

    let inbox = device.approvals().await.expect("a read device sees the decisions");
    assert_eq!(inbox.len(), 1);
    device.health().await.expect("and the kernel's health");
    let refused = device.decide(&request.id, request.version, ApprovalAction::Approve, None).await;
    assert_eq!(category(refused.unwrap_err()), ErrorCategory::Forbidden);
    assert_eq!(
        plane.approvals.get(&request.id).await.unwrap().unwrap().state,
        ApprovalState::Pending
    );
    assert_eq!(category(device.freeze(None).await.unwrap_err()), ErrorCategory::Forbidden);

    let revoked = plane.operator().revoke_device(&id).await.unwrap();
    assert_eq!(revoked.state, DeviceState::Revoked);
    let error = device.health().await.expect_err("a revoked token is refused");
    assert_eq!(category(error), ErrorCategory::Unauthorized);
    let listed = plane.operator().devices().await.unwrap();
    assert_eq!(
        listed.iter().map(|device| device.state).collect::<Vec<_>>(),
        [DeviceState::Revoked]
    );
}

/// A device with `stop` stops everything; it cannot resume, which only the home or the
/// kernel's own terminal does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_stops_everything_and_cannot_resume() {
    let plane = Plane::start().await;
    let (device, id) = plane.device(&[DeviceScope::Stop]).await;
    let stopped = device.freeze(Some("from the phone")).await.unwrap();
    assert!(stopped.frozen);
    assert!(plane.kernel.health().frozen);
    assert_eq!(category(device.unfreeze().await.unwrap_err()), ErrorCategory::Forbidden);
    assert!(plane.kernel.health().frozen, "a device cannot resume");
    plane.operator().unfreeze().await.unwrap();
    assert!(!plane.kernel.health().frozen);
    let freeze = plane
        .control_actions()
        .into_iter()
        .find(|(action, _)| action == "freeze")
        .expect("the stop is recorded");
    assert_eq!(freeze.1["device"], json!(id), "the record names the device");
}

/// A device token on loopback is the device: it cannot do what the operator may.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_token_on_loopback_is_the_device_not_the_operator() {
    let plane = Plane::start().await;
    let (device, _) = plane.device(&DeviceScope::ALL).await;
    let spawn = SpawnRequest::new(common::TARGET, json!({ "text": "hello" }));
    let error = device.spawn(&spawn).await.expect_err("devices start no process");
    assert_eq!(category(error), ErrorCategory::Forbidden);
    assert_eq!(category(device.devices().await.unwrap_err()), ErrorCategory::Forbidden);
    let issue = device.issue_pairing_code(&IssueRequest::default()).await;
    assert_eq!(category(issue.unwrap_err()), ErrorCategory::Forbidden, "a device pairs no device");
    // A forged token is no fallback to the loopback operator either.
    let forged = ControlClient::http(plane.base()).with_bearer_token("adkd_0123456789abcdef_00");
    assert_eq!(category(forged.health().await.unwrap_err()), ErrorCategory::Unauthorized);
    plane.operator().spawn(&spawn).await.expect("the operator may");
}

/// A device decides low and medium risk; high risk waits for confirming it is the person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_decides_low_and_medium_risk_but_not_high_risk() {
    let plane = Plane::start().await;
    let (device, id) = plane.device(&[DeviceScope::Decide, DeviceScope::Read]).await;
    let medium = plane.ask(ApprovalRisk::Medium).await;
    device.decide(&medium.id, medium.version, ApprovalAction::Approve, None).await.unwrap();
    let decided = plane.approvals.get(&medium.id).await.unwrap().unwrap();
    assert_eq!(decided.state, ApprovalState::Approved);
    assert_eq!(decided.decisions[0].approver.id, adk_kernel_os_user());

    let high = plane.ask(ApprovalRisk::High).await;
    let error =
        device.decide(&high.id, high.version, ApprovalAction::Approve, None).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::Forbidden);
    assert_eq!(plane.approvals.get(&high.id).await.unwrap().unwrap().state, ApprovalState::Pending);

    let decide = plane
        .control_actions()
        .into_iter()
        .find(|(action, _)| action == "approvals.decide")
        .expect("the device's answer is recorded");
    assert_eq!(decide.1["device"], json!(id));
    assert_eq!(decide.1["target"], json!(medium.id));
}

/// The user a loopback operator, and so the device it pairs, acts as.
fn adk_kernel_os_user() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

/// A code pairs one device, and its issue, claim and removal are recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_code_pairs_one_device_and_pairing_is_recorded() {
    let plane = Plane::start().await;
    let issued = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap();
    assert_eq!(issued.scopes, DeviceScope::ALL);
    let claimed = ControlClient::http(plane.base())
        .claim_pairing(&issued.code, Some("Laptop"))
        .await
        .unwrap();
    assert_eq!(claimed.device.name, "Laptop");
    let again = ControlClient::http(plane.base()).claim_pairing(&issued.code, Some("Other")).await;
    assert_eq!(category(again.unwrap_err()), ErrorCategory::Unauthorized);
    plane.operator().revoke_device(&claimed.device.id).await.unwrap();
    let actions: Vec<String> =
        plane.control_actions().into_iter().map(|(action, _)| action).collect();
    for action in ["pairing.issue", "pairing.claim", "devices.revoke"] {
        assert!(actions.contains(&action.to_string()), "{action} in {actions:?}");
    }
}

/// Pairing never widens the bind: beyond loopback the control plane still needs a token or
/// SSO.
#[test]
fn pairing_never_widens_the_bind() {
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    let exposed = ControlPlaneConfig::default()
        .with_http("0.0.0.0:8080".parse().unwrap())
        .with_pairing(pairing);
    assert!(exposed.validate().is_err());
}
