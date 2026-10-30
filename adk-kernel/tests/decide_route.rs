//! The approvals route and resuming on a kernel (spec 3.3 §7.2, §11 S-I10, S-I11, S-I13,
//! S-I18): `POST /api/approvals/{id}/decide` answers under §7.2 for the surface the server
//! established — a device by its kind, an operator credential over HTTP as `api`, the
//! kernel's socket as its own terminal — never a declared one; a suggested agent approved
//! there is added as that surface; and `POST /api/kernel/unfreeze` resumes only on the socket.

use crate::agents_support;

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, AgentPrincipal, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
    ApprovalScope, ApprovalState, ApproverSelector, ToolConfirmationRequest,
};
use adk_kernel::agents::{AgentDefinition, AgentProposer, AgentRecordState, AgentUses, WebAccess};
use adk_kernel::control::api::IssueRequest;
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig};
use adk_kernel::pairing::{DeviceKind, DeviceScope, InMemoryDeviceStore, Pairing};
use adk_kernel::{Kernel, PartitionMode};
use agents_support::{APP, PERSON, World, eventually};
use serde_json::json;

const NOT_HERE: &str = adk_kernel::error::DECIDE_NOT_HERE_CODE;

/// A kernel over `world` with its control plane on loopback HTTP, pairing devices, and on a
/// socket at `socket` when one is given.
async fn serve(world: &World, socket: Option<&std::path::Path>) -> (Kernel, ControlPlane) {
    let mut control = ControlPlaneConfig::default()
        .without_socket()
        .with_http("127.0.0.1:0".parse().unwrap())
        .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new())));
    if let Some(socket) = socket {
        control = control.with_socket(socket);
    }
    // The kernel is not started: its plane is served here, and an answer through a surface
    // applies a suggestion at once, with no tick.
    let kernel = world.kernel_with_control("k1", control, |builder| builder);
    let plane = ControlPlane::start(&kernel).await.unwrap();
    (kernel, plane)
}

fn base(plane: &ControlPlane) -> String {
    format!("http://{}", plane.http_addr().unwrap())
}

/// Pairs a device of `kind` that decides and reads; its client and id.
async fn pair(plane: &ControlPlane, kind: DeviceKind) -> (ControlClient, String) {
    let request = IssueRequest {
        name: Some(format!("{kind}")),
        scopes: vec![DeviceScope::Decide, DeviceScope::Read],
        kind: Some(kind),
        ..IssueRequest::default()
    };
    let issued = ControlClient::http(base(plane)).issue_pairing_code(&request).await.unwrap();
    let claimed = ControlClient::http(base(plane)).claim_pairing(&issued.code, None).await.unwrap();
    (ControlClient::http(base(plane)).with_bearer_token(claimed.token), claimed.device.id)
}

/// A suggestion of Finance by the primary agent; its inbox request's id and version.
async fn suggest(kernel: &Kernel) -> (String, u64) {
    let proposer = AgentProposer::new(AgentPrincipal::derived(APP, APP).id, PERSON, "s-1");
    let definition = AgentDefinition::new("Finance", "default")
        .with_mode(PartitionMode::Ask)
        .with_uses(AgentUses::default().with_web(WebAccess::Any));
    let proposal = kernel.agents().propose(definition, &proposer).await.unwrap();
    assert_eq!(proposal.risk, ApprovalRisk::Medium);
    (proposal.request_id, 1)
}

/// A held call of `tool` at `risk`, decided by the operators.
async fn held(world: &World, tool: &str, risk: ApprovalRisk) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new(tool, json!({ "to": "client@example.com" }));
    let request = ApprovalRequest::new(
        format!("held-{tool}-{risk:?}"),
        ApprovalScope::new(APP, PERSON, "s-1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("operator")]),
    )
    .with_risk(risk);
    world.approvals.submit(request).await.unwrap()
}

fn code(error: adk_kernel::KernelError) -> &'static str {
    AdkError::from(error).code
}

/// The `kernel.control` records of `approvals.decide`, as their extension fields.
fn decides(world: &World) -> Vec<serde_json::Value> {
    world
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_kernel::audit::CONTROL)
        .filter(|record| record.ext["action"] == "approvals.decide")
        .map(|record| serde_json::to_value(&record.ext).unwrap())
        .collect()
}

/// A tray answers Allow once and Deny only, and never adds an agent: its yes to a suggested
/// agent and its edit of a call are refused, its "Don't add" ends the suggestion (spec 3.3
/// §7.2, S-I11). Fails on `origin/main`, where the tray's yes adds the agent and its edit is
/// taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tray_neither_adds_an_agent_nor_changes_a_call() {
    let world = World::new();
    let (kernel, plane) = serve(&world, None).await;
    let (tray, _) = pair(&plane, DeviceKind::Tray).await;
    let (id, version) = suggest(&kernel).await;

    let error = tray.decide(&id, version, ApprovalAction::Approve, None).await.unwrap_err();
    assert_eq!(code(error), NOT_HERE);
    assert_eq!(world.approvals.get(&id).await.unwrap().unwrap().state, ApprovalState::Pending);
    tray.decide(&id, version, ApprovalAction::Reject, Some("not now")).await.unwrap();
    eventually("the suggestion to end", || async {
        world.store.get("finance").await.unwrap().is_none().then_some(())
    })
    .await;

    let call = held(&world, "send_email", ApprovalRisk::Medium).await;
    let error = tray.decide(&call.id, call.version, ApprovalAction::Edit, None).await.unwrap_err();
    assert_eq!(code(error), NOT_HERE);
    let allowed = tray.decide(&call.id, call.version, ApprovalAction::Approve, None).await.unwrap();
    assert_eq!(allowed.state, ApprovalState::Approved);
    plane.shutdown().await.unwrap();
}

/// A suggested agent approved on the phone is added at once, its `kernel.agent.created`
/// naming the phone, how, the device and the transport rather than the inbox (spec 3.3
/// S-I18). Fails on `origin/main`, where the tick adds it as surface `inbox`, how `approval`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_adds_a_suggested_agent_as_the_phone() {
    let world = World::new();
    let (kernel, plane) = serve(&world, None).await;
    let (phone, device) = pair(&plane, DeviceKind::Phone).await;
    let (id, version) = suggest(&kernel).await;
    phone.decide(&id, version, ApprovalAction::Approve, None).await.unwrap();
    eventually("Finance to be added", || async {
        world
            .store
            .get("finance")
            .await
            .ok()
            .flatten()
            .filter(|r| r.state == AgentRecordState::Active)
    })
    .await;
    let created = world
        .agent_records()
        .into_iter()
        .find(|record| record.action == adk_kernel::audit::AGENT_CREATED)
        .unwrap();
    for (key, value) in [
        ("surface", json!("phone")),
        ("how", json!("touch")),
        ("device", json!(device)),
        ("transport", json!("http")),
        ("approvalId", json!(id)),
    ] {
        assert_eq!(created.ext[key], value, "created {key}");
    }
    let recorded = decides(&world);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!((&recorded[0]["surface"], &recorded[0]["how"]), (&json!("phone"), &json!("touch")));
    plane.shutdown().await.unwrap();
}

/// An operator credential over HTTP is the `api` surface: it decides low and medium risk, its
/// decision recorded with that surface before it reaches the inbox, and approves no high-risk
/// call, which waits for "Confirm it's you" in the home or the kernel's own terminal (spec 3.3
/// §7.2). Fails on `origin/main`, which approves the high-risk call and records nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operator_client_approves_no_high_risk_call_over_http() {
    let world = World::new();
    let (_kernel, plane) = serve(&world, None).await;
    let operator = ControlClient::http(base(&plane));
    let high = held(&world, "transfer", ApprovalRisk::High).await;
    let error =
        operator.decide(&high.id, high.version, ApprovalAction::Approve, None).await.unwrap_err();
    assert_eq!(code(error), NOT_HERE);
    assert_eq!(world.approvals.get(&high.id).await.unwrap().unwrap().state, ApprovalState::Pending);
    assert!(decides(&world).is_empty(), "a refused decision records nothing");

    let medium = held(&world, "send_email", ApprovalRisk::Medium).await;
    operator.decide(&medium.id, medium.version, ApprovalAction::Approve, None).await.unwrap();
    operator.decide(&high.id, high.version, ApprovalAction::Reject, Some("no")).await.unwrap();
    let recorded = decides(&world);
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    for (record, (target, answer)) in
        recorded.iter().zip([(&medium.id, "approve"), (&high.id, "reject")])
    {
        assert_eq!(record["target"], json!(target));
        assert_eq!(record["answer"], answer);
        assert_eq!((&record["surface"], &record["how"]), (&json!("api"), &json!("api")));
        assert_eq!(record["transport"], "http");
        assert_eq!(record["subject"], json!(SubjectRef::human(adk_kernel_os_user())));
    }
    plane.shutdown().await.unwrap();
}

/// The kernel's own terminal, its socket, approves a high-risk call, and only it resumes after
/// a stop: `POST /api/kernel/unfreeze` over HTTP is refused, whoever holds the operator
/// credential (spec 3.3 §7.2, S-I10). Fails on `origin/main`, which resumes over HTTP.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_kernels_own_terminal_resumes_and_it_decides_high_risk() {
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("k.sock");
    let world = World::new();
    let (kernel, plane) = serve(&world, Some(&socket)).await;
    let local = ControlClient::socket(&socket);
    let operator = ControlClient::http(base(&plane));

    let high = held(&world, "transfer", ApprovalRisk::High).await;
    local.decide(&high.id, high.version, ApprovalAction::Approve, None).await.unwrap();
    assert_eq!(
        world.approvals.get(&high.id).await.unwrap().unwrap().state,
        ApprovalState::Approved
    );
    let recorded = decides(&world);
    assert_eq!(
        (&recorded[0]["surface"], &recorded[0]["how"], &recorded[0]["transport"]),
        (&json!("terminal"), &json!("keyboard"), &json!("socket"))
    );

    operator.freeze(Some("stop")).await.unwrap();
    assert!(kernel.health().frozen);
    let refused = operator.unfreeze().await.unwrap_err();
    assert_eq!(code(refused), adk_kernel::error::RESUME_NOT_HERE_CODE);
    assert!(kernel.health().frozen, "HTTP resumed nothing");
    local.unfreeze().await.unwrap();
    assert!(!kernel.health().frozen);
    plane.shutdown().await.unwrap();
}

/// The OS user the kernel admits loopback peers and its socket as.
fn adk_kernel_os_user() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}
