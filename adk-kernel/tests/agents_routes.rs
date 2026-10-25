//! The agents' read routes (spec 3.3 §10 K7): `GET /api/kernel/agents` and
//! `GET /api/kernel/agents/{id}` answer the operator and a device with the `read` scope; a
//! device without it is refused; no route changes an agent.

mod agents_support;

use std::sync::Arc;

use adk_core::AdkError;
use adk_kernel::agents::{AgentDefinition, AgentRecordState};
use adk_kernel::control::api::IssueRequest;
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig};
use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing, device_scope_for};
use agents_support::{World, add};
use axum::http::Method;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agents_are_read_by_the_operator_and_a_reading_device_only() {
    let world = World::new();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http("127.0.0.1:0".parse().unwrap())
        .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new())));
    let kernel = world.kernel_with_control("k1", control, |builder| builder);
    add(&kernel, AgentDefinition::new("Finance", "default")).await;
    let plane = ControlPlane::start(&kernel).await.unwrap();
    let base = format!("http://{}", plane.http_addr().unwrap());

    // A loopback caller without a token is the operator.
    let operator = ControlClient::http(base.clone());
    let agents = operator.agents().await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(
        (agents[0].agent.id.as_str(), agents[0].agent.state),
        ("finance", AgentRecordState::Active)
    );
    assert_eq!(operator.agent("finance").await.unwrap().agent.display_name, "Finance");
    let missing = operator.agent("travel").await.unwrap_err();
    assert_eq!(AdkError::from(missing).code, "kernel.agent.not_found");

    let pair = |scopes: Vec<DeviceScope>| {
        let (operator, base) = (operator.clone(), base.clone());
        async move {
            let request =
                IssueRequest { name: Some("phone".into()), scopes, ..IssueRequest::default() };
            let issued = operator.issue_pairing_code(&request).await.unwrap();
            let claimed =
                ControlClient::http(base.clone()).claim_pairing(&issued.code, None).await.unwrap();
            ControlClient::http(base).with_bearer_token(claimed.token)
        }
    };
    let reader = pair(vec![DeviceScope::Read]).await;
    assert_eq!(reader.agents().await.unwrap().len(), 1);
    assert_eq!(reader.agent("finance").await.unwrap().agent.id, "finance");
    let stopper = pair(vec![DeviceScope::Stop]).await;
    assert!(stopper.agents().await.is_err(), "a device without `read` is refused");

    // No route creates, changes or removes an agent.
    let client = reqwest::Client::new();
    for (method, path) in [
        (reqwest::Method::POST, "/api/kernel/agents"),
        (reqwest::Method::PUT, "/api/kernel/agents/finance"),
        (reqwest::Method::DELETE, "/api/kernel/agents/finance"),
        (reqwest::Method::POST, "/api/kernel/agents/finance"),
    ] {
        let answer = client.request(method.clone(), format!("{base}{path}")).send().await.unwrap();
        assert_eq!(answer.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
    }
    assert_eq!(device_scope_for(&Method::GET, "/api/kernel/agents"), Some(DeviceScope::Read));
    assert_eq!(
        device_scope_for(&Method::GET, "/api/kernel/agents/finance"),
        Some(DeviceScope::Read)
    );
    assert_eq!(device_scope_for(&Method::POST, "/api/kernel/agents"), None);
    assert_eq!(kernel.agents().list().await.unwrap().len(), 1);

    // Wave 3 findings 28–30: the listing carries a removed agent with its state, so a surface
    // never looks agents up one by one.
    let finance = kernel.agents().get("finance").await.unwrap().unwrap().agent;
    agents_support::apply(
        &kernel,
        &agents_support::local(),
        adk_kernel::agents::AgentAction::Remove {
            id: finance.id.clone(),
            expected_version: finance.version,
        },
    )
    .await
    .unwrap();
    let listed = operator.agents().await.unwrap();
    assert_eq!(
        listed.iter().map(|view| (view.agent.id.as_str(), view.agent.state)).collect::<Vec<_>>(),
        [("finance", AgentRecordState::Removed)]
    );
    plane.shutdown().await.unwrap();
}
