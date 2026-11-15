//! "Steer" on a real kernel (spec 3.3 §6, spec 3.2 §7.11): a message for the turn a replica
//! runs, through `Kernel::steer` and `POST /kernel/sessions/{session}/steer` over HTTP on
//! loopback:
//!
//! - a message a `ControlClient` steers reaches the running turn before its next model call
//!   and is in the session once, under the steer id the kernel answered;
//! - a session with no turn, and a turn another replica runs, hand the message back;
//! - the route is the person's: a caller without the `operator` scope and a paired device
//!   are refused;
//! - a blank message is refused, and nothing is steered while everything is stopped;
//! - each steer is recorded as `kernel.control` before the turn holds the message.

use crate::common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    AdkError, ErrorCategory, Event, GovernanceControl, LlmRequest, Part, RequestContext, RunState,
    SteerOutcome,
};
use adk_kernel::control::api::{AttachRequest, IssueRequest, SpawnRequest, SteerResponse};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing};
use adk_kernel::{Kernel, KernelConfig, KernelError, ProcessStore};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::DurableWorkerConfig;
use adk_session::SessionService;
use common::{APP, TARGET, World};
use serde_json::json;

const STEERED: &str = "use metric units";

fn error_of(error: KernelError) -> (ErrorCategory, &'static str) {
    let error = AdkError::from(error);
    (error.category, error.code)
}

/// `Bearer owner` is `ana` with the `operator` scope; `Bearer viewer` is `ana` without it.
struct Tokens;

#[async_trait::async_trait]
impl RequestContextExtractor for Tokens {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let scopes = match parts.headers.get("authorization").and_then(|value| value.to_str().ok())
        {
            Some("Bearer owner") => vec!["operator".to_string()],
            Some("Bearer viewer") => Vec::new(),
            _ => return Err(RequestContextError::MissingAuth),
        };
        Ok(RequestContext { user_id: "ana".into(), scopes, metadata: HashMap::new() })
    }
}

/// A started kernel whose agent calls the gated tool once, serving its control plane.
struct Plane {
    world: World,
    governance: GovernanceControl,
    kernel: Kernel,
    recorder: InMemoryAuditRecorder,
    base: String,
}

impl Plane {
    async fn start() -> Self {
        let world = World::one_step();
        let governance = GovernanceControl::new();
        let recorder = InMemoryAuditRecorder::new();
        let control = ControlPlaneConfig::default()
            .without_socket()
            .with_http("127.0.0.1:0".parse().unwrap())
            .with_extractor(Arc::new(Tokens))
            .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new())));
        let worker = DurableWorkerConfig::default()
            .with_worker_id("a")
            .with_lease_ttl(Duration::from_millis(600))
            .with_poll_interval(Duration::from_millis(20));
        let config = KernelConfig::default()
            .with_app_name(APP)
            .with_worker(worker)
            .with_scheduler_tick(Duration::from_millis(50))
            .with_governance(governance.clone())
            .with_control(control);
        let kernel = Kernel::builder()
            .config(config)
            .run_store(Arc::clone(&world.runs))
            .process_store(Arc::clone(&world.processes) as Arc<dyn ProcessStore>)
            .session_service(Arc::clone(&world.sessions) as Arc<dyn SessionService>)
            .agent(TARGET, world.runner(&governance))
            .audit(AuditTrail::new(Arc::new(recorder.clone())))
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        let addr = kernel.control_addr().await.expect("the control plane listens");
        Self { world, governance, kernel, recorder, base: format!("http://{addr}") }
    }

    fn client(&self, token: &str) -> ControlClient {
        ControlClient::http(&self.base).with_bearer_token(token)
    }

    /// A turn of `ana`'s session `session`, held at the gated tool; returns its process id.
    async fn turn(&self, session: &str) -> String {
        let request = SpawnRequest::new(TARGET, json!({ "text": "convert the report" }))
            .with_session_id(session);
        let process = self.client("owner").spawn(&request).await.unwrap();
        common::tool_called(&self.world, 1).await;
        process.meta.pid
    }

    /// The targets of the `kernel.control` records of a steer, in order.
    fn steers(&self) -> Vec<serde_json::Value> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == adk_kernel::audit::CONTROL)
            .filter(|record| record.ext["action"] == "steer")
            .map(|record| record.ext["target"].clone())
            .collect()
    }

    async fn stop(self) {
        self.world.release.add_permits(8);
        self.kernel.stop().await.unwrap();
    }
}

fn says(request: &LlmRequest, text: &str) -> bool {
    request
        .contents
        .iter()
        .any(|content| content.parts.iter().any(|part| part.text() == Some(text)))
}

/// The events of `transcript` that carry the steering message `steer_id`.
fn carrying<'a>(transcript: &'a [Event], steer_id: &str) -> Vec<&'a Event> {
    transcript
        .iter()
        .filter(|event| {
            event.actions.steering.as_ref().is_some_and(|marker| marker.steer_id == steer_id)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_steered_over_the_control_plane_reaches_the_running_turn() {
    let plane = Plane::start().await;
    let client = plane.client("owner");
    let pid = plane.turn("s1").await;

    // A session with no turn hands the message back.
    assert_eq!(client.steer("s2", STEERED).await.unwrap(), SteerResponse::Returned);
    let accepted = client.steer("s1", STEERED).await.unwrap();
    let SteerResponse::Accepted { steer_id, pid: held_by, lease_epoch } = accepted else {
        panic!("the running turn holds the message: {accepted:?}");
    };
    let claimed = plane.world.runs.get(&pid).await.unwrap().unwrap();
    assert_eq!((held_by.as_str(), lease_epoch), (pid.as_str(), claimed.lease_epoch));
    // The turn holds it, and has not taken it: the session does not carry it yet.
    let before = client.attach("s1", &AttachRequest::default()).await.unwrap();
    assert!(carrying(&before.transcript, &steer_id).is_empty());

    plane.world.release.add_permits(1);
    common::in_state(&plane.world.runs, &pid, RunState::Completed).await;

    let after = client.attach("s1", &AttachRequest::default()).await.unwrap();
    let taken = carrying(&after.transcript, &steer_id);
    assert_eq!(taken.len(), 1, "the message is in the session once");
    let content = taken[0].llm_response.content.as_ref().expect("the message");
    assert_eq!(content.parts.iter().filter_map(Part::text).collect::<String>(), STEERED);
    assert_eq!(
        (taken[0].author.as_str(), taken[0].actions.steering.as_ref().unwrap().sender.as_str()),
        ("user", "ana")
    );
    // One turn, two model calls: the message joins the second.
    let requests = plane.world.model.requests();
    assert_eq!(requests.len(), 2);
    assert!(!says(&requests[0], STEERED) && says(&requests[1], STEERED));
    assert_eq!(plane.steers(), [json!("s2"), json!("s1")]);
    plane.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_person_steers_and_nothing_is_steered_while_stopped() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    plane.turn("s1").await;

    // Changing what an agent reads needs the operator scope, and no device's token.
    let viewer = plane.client("viewer").steer("s1", STEERED).await.unwrap_err();
    assert_eq!(error_of(viewer).0, ErrorCategory::Forbidden);
    let request = IssueRequest {
        name: Some("phone".into()),
        scopes: DeviceScope::ALL.to_vec(),
        expires_in_days: None,
        ..IssueRequest::default()
    };
    let issued = owner.issue_pairing_code(&request).await.unwrap();
    let claimed = ControlClient::http(&plane.base).claim_pairing(&issued.code, None).await.unwrap();
    let device = ControlClient::http(&plane.base).with_bearer_token(claimed.token);
    assert_eq!(
        error_of(device.steer("s1", STEERED).await.unwrap_err()).0,
        ErrorCategory::Forbidden
    );
    assert!(plane.steers().is_empty(), "a refused caller leaves no steer on record");

    // A message with no words is no message.
    let blank = error_of(owner.steer("s1", "   ").await.unwrap_err());
    assert_eq!(blank.0, ErrorCategory::InvalidInput);

    // Everything stopped: nothing reaches an agent.
    plane.governance.freeze("incident 12");
    let frozen = error_of(owner.steer("s1", STEERED).await.unwrap_err());
    assert_eq!(frozen, (ErrorCategory::Unavailable, adk_kernel::error::FROZEN_CODE));

    let transcript = owner.attach("s1", &AttachRequest::default()).await.unwrap().transcript;
    assert!(transcript.iter().all(|event| event.actions.steering.is_none()));
    plane.stop().await;
}

/// A turn runs on one replica. Another replica over the same stores holds no run of it, so a
/// message steered there is handed back; the replica that runs the turn takes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_another_replica_runs_hands_the_message_back() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let running = world.kernel("a", &control);
    let other = world.kernel("b", &control);
    running.start().await.unwrap();
    let run = common::run_on(adk_kernel::QueueClass::Interactive, "convert the report")
        .with_session_id("s1");
    let process = running
        .processes()
        .spawn(run, adk_kernel::ProcessSpec::new(adk_kernel::ProcessKind::Interactive))
        .await
        .unwrap();
    common::tool_called(&world, 1).await;

    assert!(other.steer("alice", "s1", STEERED).await.unwrap().is_none());
    // Another person's session of the same id is not this turn either.
    assert!(running.steer("bob", "s1", STEERED).await.unwrap().is_none());
    let steering = running.steer("alice", "s1", STEERED).await.unwrap().expect("held here");
    assert_eq!(steering.pid, process.meta.pid);

    world.release.add_permits(1);
    let outcome = tokio::time::timeout(Duration::from_secs(5), steering.receipt.outcome())
        .await
        .expect("the turn takes the message")
        .unwrap();
    assert!(matches!(outcome, SteerOutcome::Delivered { .. }), "{outcome:?}");
    common::in_state(&world.runs, &process.meta.pid, RunState::Completed).await;
    assert!(says(&world.model.requests()[1], STEERED));
    running.stop().await.unwrap();
}
