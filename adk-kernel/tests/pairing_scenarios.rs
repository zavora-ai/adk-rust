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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditRecorder, AuditTrail, InMemoryAuditRecorder};
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
use futures::StreamExt;
use serde_json::json;

/// An in-memory recorder that refuses every record while `failing` is set.
#[derive(Clone, Default)]
struct Switchable {
    records: InMemoryAuditRecorder,
    failing: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl AuditRecorder for Switchable {
    fn digest(&self, value: &serde_json::Value) -> String {
        self.records.digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> adk_core::Result<()> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(AdkError::session("the audit store is down"));
        }
        self.records.record(draft).await
    }
}

/// A kernel serving its control plane on loopback with device pairing, the approvals inbox
/// routed to the `operator` group, and an audit trail.
struct Plane {
    kernel: Kernel,
    plane: ControlPlane,
    approvals: Arc<dyn ApprovalService>,
    recorder: InMemoryAuditRecorder,
    audit_down: Arc<AtomicBool>,
    _world: World,
}

impl Plane {
    async fn start() -> Self {
        Self::start_with(|pairing| pairing).await
    }

    async fn start_with(pairing: impl FnOnce(Pairing) -> Pairing) -> Self {
        let pairing = pairing(
            Pairing::new(Arc::new(InMemoryDeviceStore::new()))
                .with_home_url("https://home.example"),
        );
        Self::build(Some(pairing)).await
    }

    async fn build(pairing: Option<Pairing>) -> Self {
        let world = World::answer();
        let control = GovernanceControl::new();
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
            ),
        );
        let audit = Switchable::default();
        let control_plane = ControlPlaneConfig::default()
            .without_socket()
            .with_http("127.0.0.1:0".parse().unwrap());
        let control_plane = match pairing {
            Some(pairing) => control_plane.with_pairing(pairing),
            None => control_plane,
        };
        let config = KernelConfig::default()
            .with_app_name(APP)
            .with_governance(control.clone())
            .with_control(control_plane);
        let kernel = Kernel::builder()
            .config(config)
            .run_store(world.runs.clone())
            .session_service(world.sessions.clone())
            .approval_service(approvals.clone())
            .audit(AuditTrail::new(Arc::new(audit.clone())))
            .agent(common::TARGET, world.runner(&control))
            .build()
            .unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        Self {
            kernel,
            plane,
            approvals,
            recorder: audit.records,
            audit_down: audit.failing,
            _world: world,
        }
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

/// Waits up to `limit` for `stream` to end; `true` when it ended.
async fn ends_within(
    stream: &mut futures::stream::BoxStream<'static, adk_kernel::Result<adk_kernel::EventRecord>>,
    limit: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Err(_) => return false,
            Ok(None | Some(Err(_))) => return true,
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Revoking a device ends the event stream it holds open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_device_ends_its_open_event_stream() {
    let plane = Plane::start().await;
    let (device, id) = plane.device(&[DeviceScope::Read]).await;
    let mut events = device.events(None).await.expect("a read device follows the events");
    assert!(!ends_within(&mut events, Duration::from_millis(300)).await, "the stream stays open");
    plane.operator().revoke_device(&id).await.unwrap();
    assert!(ends_within(&mut events, Duration::from_secs(3)).await, "revocation ends the stream");
}

/// A device's pairing ending ends the event stream it holds open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pairing_that_ends_ends_its_open_event_stream() {
    let plane = Plane::start_with(|pairing| pairing.with_device_ttl(Duration::from_secs(1))).await;
    let (device, _) = plane.device(&[DeviceScope::Read]).await;
    let mut events = device.events(None).await.expect("the stream opens before expiry");
    assert!(ends_within(&mut events, Duration::from_secs(4)).await, "expiry ends the stream");
}

/// The issue record names the scopes the code grants and the stored name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_issue_record_names_what_the_code_grants() {
    let plane = Plane::start().await;
    let request = IssueRequest {
        name: Some("Ana's\u{1b}[2J phone".into()),
        scopes: Vec::new(),
        expires_in_days: None,
    };
    plane.operator().issue_pairing_code(&request).await.unwrap();
    let (_, record) = plane
        .control_actions()
        .into_iter()
        .find(|(action, _)| action == "pairing.issue")
        .expect("the issue is recorded");
    assert_eq!(record["scopes"], json!(["decide", "read", "stop"]));
    assert_eq!(record["name"], json!("Ana's[2J phone"));
}

/// A claim whose record cannot be written leaves no device behind
/// and the code still claimable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_whose_record_fails_leaves_the_code_and_no_device() {
    let plane = Plane::start().await;
    let issued = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap();
    plane.audit_down.store(true, Ordering::SeqCst);
    let refused = ControlClient::http(plane.base()).claim_pairing(&issued.code, None).await;
    assert!(refused.is_err(), "an unrecorded claim pairs nothing");
    plane.audit_down.store(false, Ordering::SeqCst);
    assert!(plane.operator().devices().await.unwrap().is_empty(), "no device row is left");
    let claimed = ControlClient::http(plane.base()).claim_pairing(&issued.code, None).await;
    assert!(claimed.is_ok(), "the code still pairs once the record can be written");
}

/// A pairing of zero days, or of more than a year, is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pairing_length_outside_one_to_365_days_is_refused() {
    let plane = Plane::start().await;
    for days in [0, 366] {
        let request = IssueRequest { expires_in_days: Some(days), ..IssueRequest::default() };
        let error = plane.operator().issue_pairing_code(&request).await.unwrap_err();
        assert_eq!(category(error), ErrorCategory::InvalidInput, "{days} days");
    }
    let request = IssueRequest { expires_in_days: Some(365), ..IssueRequest::default() };
    plane.operator().issue_pairing_code(&request).await.expect("a year is allowed");
}

/// A kernel that pairs nothing answers the pairing commands with what to
/// set, not an empty `404`, and lists no chat accounts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kernel_without_pairing_says_what_it_lacks() {
    let plane = Plane::build(None).await;
    let error = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap_err();
    assert!(error.to_string().contains("with_pairing"), "{error}");
    assert_eq!(category(error), ErrorCategory::InvalidInput);
    let error = plane.operator().devices().await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::InvalidInput);
    let accounts = plane.operator().channel_accounts().await.expect("no chat accounts to list");
    assert!(accounts.accounts.is_empty() && accounts.waiting.is_empty());
    let error = plane.operator().accept_channel("ABCD-EFGH-2345").await.unwrap_err();
    assert!(error.to_string().contains("chat"), "{error}");
    assert_eq!(category(error), ErrorCategory::InvalidInput);
}

/// A client still holding a revoked token re-pairs, on the kernel's
/// listener and on an app behind `guard_devices`, where the claim route skips the guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_holding_a_revoked_token_re_pairs() {
    let plane = Plane::start().await;
    let issued = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap();
    let claimed =
        ControlClient::http(plane.base()).claim_pairing(&issued.code, None).await.unwrap();
    let old = claimed.token.clone();
    plane.operator().revoke_device(&claimed.device.id).await.unwrap();

    // The kernel's own listener.
    let issued = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap();
    let stale = ControlClient::http(plane.base()).with_bearer_token(old.clone());
    stale.claim_pairing(&issued.code, None).await.expect("re-pairs on the listener");

    // An app that serves the kernel's routes behind the device guard.
    let app = axum::Router::new()
        .nest("/api", adk_kernel::control::router(plane.kernel.clone()))
        .merge(adk_kernel::control::claim_router(plane.kernel.clone()));
    let app = adk_kernel::control::guard_devices(&plane.kernel, app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let issued = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap();
    // The raw request still carries the revoked token: the guard lets the claim through.
    let response = reqwest::Client::new()
        .post(format!("{base}/api/kernel/pairing/claim"))
        .bearer_auth(&old)
        .json(&json!({ "code": issued.code }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK, "{}", response.text().await.unwrap());
    let issued = plane.operator().issue_pairing_code(&IssueRequest::default()).await.unwrap();
    let stale = ControlClient::http(base).with_bearer_token(old);
    stale.claim_pairing(&issued.code, None).await.expect("re-pairs behind the guard");
}

/// A chat account whose pairing cannot be recorded is not
/// paired, and its code still works once the record can be written.
#[cfg(feature = "channels")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_account_whose_pairing_fails_to_record_stays_unpaired() {
    use adk_kernel::channels::{
        ChannelBindingStore, ChannelGateway, InMemoryChannel, InMemoryChannelBindingStore,
        InboundOutcome,
    };

    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let plane = Plane::start_with(|pairing| pairing.with_channel_bindings(bindings.clone())).await;
    let chat = Arc::new(InMemoryChannel::new("chat"));
    let gateway = ChannelGateway::builder(Arc::new(plane.kernel.clone()), bindings.clone())
        .sink(chat.clone())
        .build();
    let hello = chat.text("stranger", "t-1", "hello");
    assert_eq!(gateway.receive(hello).await.unwrap(), InboundOutcome::PairingRequested);
    let code = chat.pairing_code("t-1").unwrap();

    plane.audit_down.store(true, Ordering::SeqCst);
    assert!(plane.operator().accept_channel(&code).await.is_err(), "an unrecorded pairing");
    plane.audit_down.store(false, Ordering::SeqCst);
    assert!(bindings.list_bindings().await.unwrap().is_empty(), "no account row is left");
    let account = plane.operator().accept_channel(&code).await.expect("the code still works");
    assert_eq!(account.account, "stranger");
}

/// The operator dismisses chat accounts waiting to be paired, so
/// strangers cannot hold every place in the queue.
#[cfg(feature = "channels")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operator_dismisses_a_waiting_chat_account() {
    use adk_kernel::channels::{
        ChannelBindingStore, ChannelGateway, InMemoryChannel, InMemoryChannelBindingStore,
        InboundOutcome,
    };

    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let plane = Plane::start_with(|pairing| pairing.with_channel_bindings(bindings.clone())).await;
    let chat = Arc::new(InMemoryChannel::new("chat"));
    let gateway = ChannelGateway::builder(Arc::new(plane.kernel.clone()), bindings.clone())
        .sink(chat.clone())
        .max_waiting(2)
        .build();
    for stranger in ["s1", "s2"] {
        let hello = chat.text(stranger, stranger, "hi");
        assert_eq!(gateway.receive(hello).await.unwrap(), InboundOutcome::PairingRequested);
    }
    let owner = chat.text("ana-phone", "t-ana", "hi");
    assert_eq!(gateway.receive(owner).await.unwrap(), InboundOutcome::PairingBusy);

    let waiting = plane.operator().channel_accounts().await.unwrap().waiting;
    assert_eq!(waiting.len(), 2);
    for request in &waiting {
        let dismissed = plane.operator().dismiss_channel_request(&request.id).await.unwrap();
        assert_eq!(dismissed.account, request.account);
    }
    assert!(plane.operator().channel_accounts().await.unwrap().waiting.is_empty());
    let actions: Vec<String> =
        plane.control_actions().into_iter().map(|(action, _)| action).collect();
    assert_eq!(actions.iter().filter(|action| *action == "channels.dismiss").count(), 2);

    let owner = chat.text("ana-phone", "t-ana", "hi again");
    assert_eq!(gateway.receive(owner).await.unwrap(), InboundOutcome::PairingRequested);
    let error = plane.operator().dismiss_channel_request(&waiting[0].id).await.unwrap_err();
    assert_eq!(category(error), ErrorCategory::NotFound);
}
