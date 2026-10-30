//! The REPL's slash commands and "Always allow" on a real kernel's control plane over HTTP on
//! loopback (spec 3.3 §5.2, §7.2, §8.2):
//!
//! - the person runs the host's commands in their session, and each run is recorded;
//! - a paired device and a caller without the `operator` scope reach none of the routes;
//! - while everything is stopped, a command that lets the agents do more is refused and one
//!   that makes them do less still runs;
//! - "Always allow" is made while the decision waits and granted only after the caller's own
//!   yes, for the caller's own decisions only, and never for a high-risk call;
//! - a session's mode is read without a record;
//! - a surface engine attached to the kernel writes its records on the kernel's log, as the
//!   caller and over the transport it came by.

use crate::common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditRecordKind, AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApproverSelector, ErrorCategory, GovernanceControl, InMemoryApprovalStore,
    RequestContext, StoreApprovalService, ToolConfirmationRequest,
};
use adk_kernel::control::api::IssueRequest;
use adk_kernel::control::{
    CommandCall, CommandHost, CommandInfo, CommandReach, ControlClient, ControlPlane,
    ControlPlaneConfig, ControlTransport, SurfaceRecord,
};
use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing};
use adk_kernel::{Kernel, KernelConfig, KernelError};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use common::{APP, World};
use serde_json::json;

fn category(error: KernelError) -> ErrorCategory {
    AdkError::from(error).category
}

/// The REPL's `/todos` and `/rules` as a host offers them; "Always allow" for `write_file`.
#[derive(Default)]
struct Host {
    prepared: Mutex<Vec<String>>,
    granted: Mutex<Vec<String>>,
    reached: Mutex<Vec<CommandReach>>,
}

#[async_trait::async_trait]
impl CommandHost for Host {
    fn commands(&self) -> Vec<CommandInfo> {
        vec![
            CommandInfo::new("todos", "Show the session's todo list"),
            CommandInfo::new("rules", "List your standing rules; /rules rm N revokes one"),
            CommandInfo::new("web", "The web allowlist; /web allow HOST adds a standing rule"),
        ]
    }

    fn changes(&self, name: &str, args: &str) -> bool {
        (name == "rules" && args.starts_with("rm")) || (name == "web" && args.starts_with("allow"))
    }

    async fn tightens(&self, call: &CommandCall) -> adk_kernel::Result<bool> {
        Ok(call.name == "rules" && call.args.starts_with("rm"))
    }

    async fn session_mode(&self, user: &str, session: &str) -> adk_kernel::Result<Option<String>> {
        Ok((user == "ana" && session == "s1").then(|| "plan".to_string()))
    }

    async fn run(&self, call: &CommandCall) -> adk_kernel::Result<String> {
        self.reached.lock().unwrap().push(call.reach.clone());
        Ok(format!("/{} {} for {} in {}", call.name, call.args, call.user, call.session))
    }

    async fn always_allow_offer(
        &self,
        request: &ApprovalRequest,
    ) -> adk_kernel::Result<Option<String>> {
        Ok((request.tool_name == "write_file").then(|| "Always allow for this folder".into()))
    }

    async fn prepare_always_allow(
        &self,
        request: &ApprovalRequest,
        _: &SubjectRef,
    ) -> adk_kernel::Result<String> {
        self.prepared.lock().unwrap().push(request.id.clone());
        Ok("Always allow for this folder".into())
    }

    async fn grant_always_allow(&self, request: &ApprovalRequest) -> adk_kernel::Result<()> {
        assert!(self.prepared.lock().unwrap().contains(&request.id), "granted unprepared");
        self.granted.lock().unwrap().push(request.id.clone());
        Ok(())
    }
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

/// A kernel serving the command routes over `host`, with an approvals inbox, pairing and an
/// audit trail.
struct Plane {
    governance: GovernanceControl,
    _kernel: Kernel,
    plane: ControlPlane,
    host: Arc<Host>,
    approvals: Arc<dyn ApprovalService>,
    recorder: InMemoryAuditRecorder,
    _world: World,
}

impl Plane {
    async fn start() -> Self {
        let world = World::answer();
        let recorder = InMemoryAuditRecorder::new();
        let host = Arc::new(Host::default());
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
            ),
        );
        let control = ControlPlaneConfig::default()
            .without_socket()
            .with_http("127.0.0.1:0".parse().unwrap())
            .with_extractor(Arc::new(Tokens))
            .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new())))
            .with_commands(host.clone());
        let governance = GovernanceControl::new();
        let config = KernelConfig::default()
            .with_app_name(APP)
            .with_governance(governance.clone())
            .with_control(control);
        let kernel = Kernel::builder()
            .config(config)
            .run_store(world.runs.clone())
            .process_store(world.processes.clone())
            .session_service(world.sessions.clone())
            .approval_service(approvals.clone())
            .audit(AuditTrail::new(Arc::new(recorder.clone())))
            .build()
            .unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        Self { governance, _kernel: kernel, plane, host, approvals, recorder, _world: world }
    }

    fn client(&self, token: &str) -> ControlClient {
        let base = format!("http://{}", self.plane.http_addr().unwrap());
        ControlClient::http(base).with_bearer_token(token)
    }

    /// A paired device with every scope.
    async fn device(&self) -> ControlClient {
        let request = IssueRequest {
            name: Some("phone".into()),
            scopes: DeviceScope::ALL.to_vec(),
            expires_in_days: None,
            ..IssueRequest::default()
        };
        let issued = self.client("owner").issue_pairing_code(&request).await.unwrap();
        let base = format!("http://{}", self.plane.http_addr().unwrap());
        let claimed = ControlClient::http(&base).claim_pairing(&issued.code, None).await.unwrap();
        ControlClient::http(base).with_bearer_token(claimed.token)
    }

    /// The `kernel.control` records of `action`, as their extension fields.
    fn controls(&self, action: &str) -> Vec<serde_json::Value> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == adk_kernel::audit::CONTROL)
            .filter(|record| record.ext["action"] == action)
            .map(|record| serde_json::to_value(&record.ext).unwrap())
            .collect()
    }

    /// A call to `tool` held for `user` in session `s1`, waiting in the inbox.
    async fn held(&self, id: &str, user: &str, tool: &str) -> ApprovalRequest {
        self.held_at(id, user, tool, ApprovalRisk::Medium).await
    }

    /// A call to `tool` of `risk` held for `user` in session `s1`.
    async fn held_at(
        &self,
        id: &str,
        user: &str,
        tool: &str,
        risk: ApprovalRisk,
    ) -> ApprovalRequest {
        let call = ToolConfirmationRequest {
            tool_name: tool.into(),
            function_call_id: Some(format!("call-{id}")),
            args: json!({ "path": "invoice.txt" }),
            acting: None,
            origin: None,
        };
        let policy =
            ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
        let mut request =
            ApprovalRequest::new(id, ApprovalScope::new(APP, user, "s1"), &call, policy);
        request.risk = risk;
        self.approvals.submit(request).await.unwrap()
    }

    /// The records of `action` a surface wrote, whole.
    fn surface(&self, action: &str) -> Vec<adk_core::audit_log::AuditRecordDraft> {
        self.recorder.records().into_iter().filter(|record| record.action == action).collect()
    }
}

/// The person lists the commands and runs one in their session, by name and arguments; each
/// run is recorded with the session before it runs, and a command the host does not offer is
/// not found.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_person_runs_the_hosts_commands_and_each_run_is_recorded() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    let names: Vec<String> =
        owner.commands().await.unwrap().into_iter().map(|command| command.name).collect();
    assert_eq!(names, ["todos", "rules", "web"]);
    assert_eq!(owner.run_command("todos", "", "s1").await.unwrap(), "/todos  for ana in s1");
    let records = plane.controls("commands.run");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["target"], "todos");
    assert_eq!(records[0]["session"], "s1");
    assert_eq!(records[0]["changes"], false);
    assert_eq!(records[0]["reach"], json!({ "transport": "http" }), "how the caller reached it");
    assert_eq!(*plane.host.reached.lock().unwrap(), [CommandReach::new(ControlTransport::Http)]);
    let missing = owner.run_command("plan", "", "s1").await.unwrap_err();
    assert_eq!(category(missing), ErrorCategory::NotFound);
    assert_eq!(plane.controls("commands.run").len(), 1, "a missing command is not run");
}

/// A paired device, whatever its scopes, and a caller without the `operator` scope reach none
/// of the routes, and nothing is recorded (spec 3.3 S-I7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_or_a_viewer_reaches_no_command_and_no_rule() {
    let plane = Plane::start().await;
    plane.held("apr-1", "ana", "write_file").await;
    for client in [plane.device().await, plane.client("viewer")] {
        let refused = [
            client.commands().await.unwrap_err(),
            client.run_command("todos", "", "s1").await.unwrap_err(),
            client.prepare_always_allow("apr-1").await.unwrap_err(),
            client.grant_always_allow("apr-1").await.unwrap_err(),
        ];
        for error in refused {
            assert_eq!(category(error), ErrorCategory::Forbidden);
        }
        let offer = client.always_allow_offer("apr-1").await.unwrap_err();
        assert_eq!(category(offer), ErrorCategory::Forbidden);
    }
    assert!(plane.controls("commands.run").is_empty());
    assert!(plane.host.prepared.lock().unwrap().is_empty());
}

/// While everything is stopped, a command that lets the agents do more is refused and not
/// run; one that makes them do less, such as revoking a rule, runs, and so does one that
/// reads (wave 3 review: cleaning up is never refused while stopped, S-I10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopped_a_command_that_lets_agents_do_more_is_refused_and_a_tightening_one_runs() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    plane.governance.freeze("incident 12");
    let refused = owner.run_command("web", "allow docs.rs", "s1").await.unwrap_err();
    let refused = AdkError::from(refused);
    assert_eq!(refused.category, ErrorCategory::Conflict);
    assert!(refused.message.contains("everything is stopped (incident 12)"), "{refused}");
    assert!(refused.message.contains("cannot let the agents do more"), "{refused}");
    assert_eq!(owner.run_command("rules", "", "s1").await.unwrap(), "/rules  for ana in s1");
    assert_eq!(
        owner.run_command("rules", "rm 1", "s1").await.unwrap(),
        "/rules rm 1 for ana in s1"
    );
    let records = plane.controls("commands.run");
    assert_eq!(records.len(), 2, "the read and the revocation ran: {records:?}");
    assert_eq!(records[1]["changes"], true);
    plane.governance.unfreeze();
    owner.run_command("web", "allow docs.rs", "s1").await.unwrap();
    assert_eq!(plane.controls("commands.run")[2]["target"], "web");
}

/// "Always allow" is offered for a call the host can make a rule of, made while the decision
/// waits, and granted only once it was allowed; another person's decision is not found.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_is_made_while_waiting_and_granted_after_a_yes() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    let held = plane.held("apr-1", "ana", "write_file").await;
    plane.held("apr-2", "ana", "send_email").await;
    plane.held("apr-3", "bob", "write_file").await;

    assert_eq!(
        owner.always_allow_offer("apr-1").await.unwrap().as_deref(),
        Some("Always allow for this folder")
    );
    assert_eq!(owner.always_allow_offer("apr-2").await.unwrap(), None);
    let other = owner.prepare_always_allow("apr-3").await.unwrap_err();
    assert_eq!(category(other), ErrorCategory::NotFound, "never another person's decision");

    assert_eq!(owner.prepare_always_allow("apr-1").await.unwrap(), "Always allow for this folder");
    let early = owner.grant_always_allow("apr-1").await.unwrap_err();
    assert_eq!(category(early), ErrorCategory::Conflict, "no rule before the yes");
    assert!(plane.controls("rules.always_allow").is_empty());

    let yes = ApprovalDecision::approve(SubjectRef::human("ana")).with_groups(["operator"]);
    plane.approvals.decide("apr-1", held.version, yes).await.unwrap();
    owner.grant_always_allow("apr-1").await.unwrap();
    assert_eq!(*plane.host.granted.lock().unwrap(), ["apr-1"]);
    let records = plane.controls("rules.always_allow");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["target"], "apr-1");
    // Answered, it is no longer offered.
    let answered = owner.always_allow_offer("apr-1").await.unwrap_err();
    assert_eq!(category(answered), ErrorCategory::Conflict);
}

/// On the kernel's socket the host learns that the caller is local, and which process
/// called; the run's record names both.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_on_the_socket_names_the_calling_process() {
    let home = tempfile::tempdir().unwrap();
    let world = World::answer();
    let recorder = InMemoryAuditRecorder::new();
    let host = Arc::new(Host::default());
    let config = KernelConfig::default()
        .with_app_name(APP)
        .with_home(home.path())
        .with_control(ControlPlaneConfig::default().with_commands(host.clone()));
    let kernel = Kernel::builder()
        .config(config)
        .run_store(world.runs.clone())
        .process_store(world.processes.clone())
        .session_service(world.sessions.clone())
        .audit(AuditTrail::new(Arc::new(recorder.clone())))
        .build()
        .unwrap();
    let _plane = ControlPlane::start(&kernel).await.unwrap();
    let client = ControlClient::connect_local(home.path()).await.unwrap().expect("the socket");
    client.run_command("todos", "", "s1").await.unwrap();
    let reached = host.reached.lock().unwrap().clone();
    let pid = std::process::id();
    assert_eq!(reached, [CommandReach::new(ControlTransport::Socket).with_peer_pid(pid)]);
    assert!(reached[0].is_local());
    let record = recorder
        .records()
        .into_iter()
        .find(|record| record.ext.get("action") == Some(&json!("commands.run")))
        .expect("the run is recorded");
    assert_eq!(record.ext["reach"], json!({ "transport": "socket", "peerPid": pid }));
}

/// "Always allow" is never offered or made for a high-risk call, and a rule prepared for a
/// call is granted only after the caller's own "Allow once": an edit, or a yes given by
/// someone else, grants nothing (wave 3 review: spec 3.3 §7.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_needs_an_everyday_call_and_the_callers_own_yes() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    plane.held_at("apr-high", "ana", "write_file", ApprovalRisk::High).await;
    assert_eq!(owner.always_allow_offer("apr-high").await.unwrap(), None);
    let refused = owner.prepare_always_allow("apr-high").await.unwrap_err();
    assert_eq!(category(refused), ErrorCategory::Forbidden);

    let edited = plane.held("apr-edit", "ana", "write_file").await;
    owner.prepare_always_allow("apr-edit").await.unwrap();
    let edit = ApprovalDecision::edit(SubjectRef::human("ana"), json!({ "path": "other.txt" }))
        .with_groups(["operator"]);
    plane.approvals.decide("apr-edit", edited.version, edit).await.unwrap();
    let after_edit = owner.grant_always_allow("apr-edit").await.unwrap_err();
    assert_eq!(category(after_edit), ErrorCategory::Conflict, "an edit grants nothing");

    let others = plane.held("apr-bob", "ana", "write_file").await;
    owner.prepare_always_allow("apr-bob").await.unwrap();
    let bobs = ApprovalDecision::approve(SubjectRef::human("bob")).with_groups(["operator"]);
    plane.approvals.decide("apr-bob", others.version, bobs).await.unwrap();
    let after_bob = owner.grant_always_allow("apr-bob").await.unwrap_err();
    assert_eq!(category(after_bob), ErrorCategory::Conflict, "someone else's yes grants nothing");
    assert!(plane.host.granted.lock().unwrap().is_empty());
    assert!(plane.controls("rules.always_allow").is_empty());
}

/// A session's mode is read from the host for the caller, without a record; a device
/// reaches nothing, and a session the host keeps no mode for reads as none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sessions_mode_is_read_without_a_record() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    assert_eq!(owner.session_mode("s1").await.unwrap().as_deref(), Some("plan"));
    assert_eq!(owner.session_mode("elsewhere").await.unwrap(), None);
    assert!(plane.controls("commands.run").is_empty(), "a read is not a run");
    let device = plane.device().await.session_mode("s1").await.unwrap_err();
    assert_eq!(category(device), ErrorCategory::Forbidden);
}

/// A surface engine attached to the kernel writes its records on the kernel's log: the
/// caller is the actor and the transport the one it came over, whatever the record says; only
/// a surface's actions, an answer only to the caller's own decision, and never from a device
/// (wave 3 review: spec 3.3 §11).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_surface_attached_to_the_kernel_writes_its_records_there() {
    let plane = Plane::start().await;
    let owner = plane.client("owner");
    plane.held("apr-1", "ana", "write_file").await;
    plane.held("apr-3", "bob", "write_file").await;
    let answer = SurfaceRecord::new(AuditRecordKind::Approval, "surface.decide")
        .with_decision(adk_core::audit_log::AuditDecision::Approved)
        .with_ext("approvalId", json!("apr-1"))
        .with_ext("surface", json!("terminal"))
        .with_ext("origin", json!({ "kind": "keyboard" }))
        .with_ext("answer", json!("allowOnce"))
        .with_ext("outcome", json!("decided"))
        .with_ext("transport", json!("socket"))
        .with_ext("presence", json!("confirmed"));
    owner.record_surface(&answer).await.unwrap();
    let stop = SurfaceRecord::new(AuditRecordKind::Governance, "surface.stop_all")
        .with_ext("surface", json!("terminal"))
        .with_ext("origin", json!({ "kind": "keyboard" }));
    owner.record_surface(&stop).await.unwrap();

    let decided = plane.surface("surface.decide");
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0].actor.on_behalf_of, Some(SubjectRef::human("ana")));
    assert_eq!(decided[0].kind, AuditRecordKind::Approval);
    let ext = serde_json::to_value(&decided[0].ext).unwrap();
    assert_eq!(ext["surface"], "terminal");
    assert_eq!(ext["origin"], json!({ "kind": "keyboard" }));
    assert_eq!(ext["transport"], "http", "the transport the kernel saw");
    assert!(ext.get("presence").is_none(), "presence is never a surface's to claim");
    assert_eq!(plane.surface("surface.stop_all").len(), 1);

    let other = SurfaceRecord::new(AuditRecordKind::Approval, "surface.decide")
        .with_ext("approvalId", json!("apr-3"));
    assert_eq!(category(owner.record_surface(&other).await.unwrap_err()), ErrorCategory::NotFound);
    let foreign = SurfaceRecord::new(AuditRecordKind::Governance, "governance.freeze");
    assert_eq!(
        category(owner.record_surface(&foreign).await.unwrap_err()),
        ErrorCategory::InvalidInput
    );
    let device = plane.device().await.record_surface(&stop).await.unwrap_err();
    assert_eq!(category(device), ErrorCategory::Forbidden);
    assert_eq!(plane.surface("surface.stop_all").len(), 1, "nothing else was written");
}
