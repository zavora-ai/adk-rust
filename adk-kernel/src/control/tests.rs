//! The control plane over a test double of the kernel: the auth matrix (socket, loopback,
//! bearer, non-loopback refusal, no subject), the `kernel.control` records, and every
//! route through [`ControlClient`] (spec 3.2 §7.11, §9.2 K-I16–K-I18, §9.3).

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalService, DurableRunStore, Enqueued, Event, GovernanceControl, InMemoryRunStore, NewRun,
    RequestContext, RunRecord, RunState, ToolConfirmationRequest,
};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

use super::api::{AttachRequest, ProcessesQuery};
use super::routes::{ControlBackend, routes};
use super::server::os_user;
use super::{ControlClient, ControlPlane, ControlPlaneConfig, paths};
use crate::bus::{EventBus, EventSubscriber, KernelEvent};
use crate::error::{KernelError, Result};
use crate::kernel::KernelHealth;
use crate::process::{Process, ProcessKind, ProcessMeta, Suspension};
use crate::sched::QueueClass;
use crate::stores::ProcessFilter;

const APP: &str = "adk-rust";

/// A kernel double: processes in a map, a real governance control, bus, run store,
/// session service and audit trail.
struct Fake {
    governance: GovernanceControl,
    runs: Arc<dyn DurableRunStore>,
    sessions: Arc<dyn SessionService>,
    approvals: Arc<dyn ApprovalService>,
    recorder: InMemoryAuditRecorder,
    trail: AuditTrail,
    bus: EventBus,
    processes: Mutex<BTreeMap<String, Process>>,
    asked_after: Mutex<Vec<Option<u64>>>,
    #[cfg(feature = "drivers")]
    drivers: Option<crate::drivers::DriverSupervisor>,
    #[cfg(feature = "proc")]
    ledger: crate::proc::Ledger,
}

impl Fake {
    fn new() -> Arc<Self> {
        let recorder = InMemoryAuditRecorder::new();
        Arc::new(Self {
            governance: GovernanceControl::new(),
            runs: Arc::new(InMemoryRunStore::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            approvals: Arc::new(adk_core::StoreApprovalService::new(Arc::new(
                adk_core::InMemoryApprovalStore::new(),
            ))),
            trail: AuditTrail::new(Arc::new(recorder.clone())),
            recorder,
            bus: EventBus::new(16),
            processes: Mutex::new(BTreeMap::new()),
            asked_after: Mutex::new(Vec::new()),
            #[cfg(feature = "drivers")]
            drivers: None,
            #[cfg(feature = "proc")]
            ledger: crate::proc::Ledger::new(
                Arc::new(crate::proc::InMemoryLedgerStore::new()),
                crate::proc::LedgerKey::from_bytes([4; 32]),
            ),
        })
    }

    /// Enqueues a run of `session` for `user` and records it as a process in `state`.
    async fn spawn(&self, user: &str, session: &str, state: RunState) -> String {
        let run = NewRun::new("interactive", APP, user, APP, json!({})).with_session_id(session);
        let Enqueued::Created(record) = self.runs.enqueue(run).await.unwrap() else {
            panic!("the run is new");
        };
        let mut record: RunRecord = record;
        if state.is_terminal() {
            // A queued run is cancelled at once, so the store finishes it too.
            self.runs.request_cancel(&record.run_id).await.unwrap();
        }
        record.state = state;
        let meta =
            ProcessMeta::new(&record.run_id, QueueClass::Interactive, ProcessKind::Interactive);
        let pid = record.run_id.clone();
        self.processes.lock().unwrap().insert(pid.clone(), Process::new(record, meta));
        pid
    }

    fn with<T>(&self, pid: &str, change: impl FnOnce(&mut Process) -> Result<T>) -> Result<T> {
        let mut processes = self.processes.lock().unwrap();
        let process =
            processes.get_mut(pid).ok_or_else(|| KernelError::NotFound { pid: pid.into() })?;
        change(process)
    }

    fn control_records(&self) -> Vec<AuditRecordDraft> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == crate::audit::CONTROL)
            .collect()
    }
}

#[async_trait]
impl ControlBackend for Fake {
    fn app_name(&self) -> &str {
        APP
    }

    fn health(&self) -> KernelHealth {
        KernelHealth { frozen: self.governance.is_frozen(), ..KernelHealth::default() }
    }

    fn governance(&self) -> &GovernanceControl {
        &self.governance
    }

    fn run_store(&self) -> &Arc<dyn DurableRunStore> {
        &self.runs
    }

    fn sessions(&self) -> Option<&Arc<dyn SessionService>> {
        Some(&self.sessions)
    }

    fn approvals(&self) -> Option<&Arc<dyn ApprovalService>> {
        Some(&self.approvals)
    }

    fn targets(&self) -> Vec<String> {
        vec![APP.to_string(), "harness.goal".to_string()]
    }

    fn has_target(&self, target: &str) -> bool {
        self.targets().iter().any(|known| known == target)
    }

    async fn spawn(&self, run: NewRun, spec: crate::process::ProcessSpec) -> Result<Process> {
        let queue = QueueClass::from_name(&run.queue);
        let Enqueued::Created(record) = self.runs.enqueue(run).await? else {
            panic!("the run is new");
        };
        let mut meta = ProcessMeta::new(&record.run_id, queue, spec.kind);
        if let Some(wake) = spec.wake {
            meta = meta.with_wake(wake);
        }
        let process = Process::new(record, meta);
        self.processes.lock().unwrap().insert(process.meta.pid.clone(), process.clone());
        Ok(process)
    }

    fn audit_trail(&self) -> Option<&AuditTrail> {
        Some(&self.trail)
    }

    async fn ps(&self, filter: ProcessFilter) -> Result<Vec<Process>> {
        Ok(self
            .processes
            .lock()
            .unwrap()
            .values()
            .filter(|process| {
                filter.states.is_empty() || filter.states.contains(&process.run.state)
            })
            .cloned()
            .collect())
    }

    async fn process(&self, pid: &str) -> Result<Option<Process>> {
        Ok(self.processes.lock().unwrap().get(pid).cloned())
    }

    async fn kill(&self, pid: &str, reason: &str) -> Result<()> {
        self.with(pid, |process| {
            process.run.state = RunState::Cancelled;
            process.run.error = Some(reason.to_string());
            Ok(())
        })
    }

    async fn nice(&self, pid: &str, queue: QueueClass) -> Result<Process> {
        let moved = self.with(pid, |process| {
            process.run.state = RunState::Cancelled;
            let mut moved = process.clone();
            moved.run.run_id = format!("{pid}-niced");
            moved.run.state = RunState::Queued;
            moved.meta.pid = moved.run.run_id.clone();
            moved.meta.parent = Some(pid.to_string());
            moved.meta.queue = queue;
            Ok(moved)
        })?;
        self.processes.lock().unwrap().insert(moved.meta.pid.clone(), moved.clone());
        Ok(moved)
    }

    async fn suspend(&self, pid: &str) -> Result<()> {
        self.with(pid, |process| {
            process.run.state = RunState::Waiting;
            process.meta.suspended_by = Some(Suspension::Manual);
            Ok(())
        })
    }

    async fn resume(&self, pid: &str) -> Result<()> {
        self.with(pid, |process| {
            if process.meta.suspended_by == Some(Suspension::Freeze) {
                return Err(KernelError::InvalidState {
                    pid: pid.into(),
                    state: process.run.state,
                    action: "resumed",
                });
            }
            process.run.state = RunState::Queued;
            process.meta.suspended_by = None;
            Ok(())
        })
    }

    async fn freeze(&self, reason: &str, _by: AuditActor) -> Result<usize> {
        self.governance.freeze(reason);
        let mut parked = 0;
        for process in self.processes.lock().unwrap().values_mut() {
            if process.run.state == RunState::Running {
                process.run.state = RunState::Waiting;
                process.meta.suspended_by = Some(Suspension::Freeze);
                parked += 1;
            }
        }
        Ok(parked)
    }

    async fn unfreeze(&self, _by: AuditActor) -> Result<()> {
        self.governance.unfreeze();
        for process in self.processes.lock().unwrap().values_mut() {
            if process.meta.suspended_by == Some(Suspension::Freeze) {
                process.run.state = RunState::Queued;
                process.meta.suspended_by = None;
            }
        }
        Ok(())
    }

    async fn events(&self, after: Option<u64>) -> Result<EventSubscriber> {
        self.asked_after.lock().unwrap().push(after);
        Ok(self.bus.subscribe())
    }

    #[cfg(feature = "drivers")]
    fn drivers(&self) -> Option<crate::drivers::DriverSupervisor> {
        self.drivers.clone()
    }

    #[cfg(feature = "proc")]
    fn ledger(&self) -> Option<crate::proc::Ledger> {
        Some(self.ledger.clone())
    }

    async fn partitions(&self) -> Result<Vec<crate::partition::Partition>> {
        Ok(vec![
            crate::partition::Partition::new("home", "home"),
            crate::partition::Partition::new("work", "work").with_driver("calendar"),
        ])
    }

    async fn put_partition(
        &self,
        partition: crate::partition::Partition,
        _by: AuditActor,
    ) -> Result<crate::partition::Partition> {
        Ok(partition)
    }

    async fn set_partition_mode(
        &self,
        id: crate::partition::PartitionId,
        mode: crate::partition::PartitionMode,
        _by: AuditActor,
    ) -> Result<crate::partition::Partition> {
        Ok(crate::partition::Partition::new(id, "class").with_mode(mode))
    }
}

/// Admits `Bearer right` as `phone`, an operator, and `Bearer viewer` as `viewer`, who is
/// not.
struct TokenExtractor;

#[async_trait]
impl RequestContextExtractor for TokenExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> std::result::Result<RequestContext, RequestContextError> {
        match parts.headers.get("authorization").and_then(|value| value.to_str().ok()) {
            Some("Bearer right") => Ok(RequestContext {
                user_id: "phone".into(),
                scopes: vec!["operator".into()],
                metadata: HashMap::new(),
            }),
            Some("Bearer viewer") => Ok(RequestContext {
                user_id: "viewer".into(),
                scopes: Vec::new(),
                metadata: HashMap::new(),
            }),
            Some(_) => Err(RequestContextError::InvalidToken("the token does not match".into())),
            None => Err(RequestContextError::MissingAuth),
        }
    }
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn http_plane(fake: &Arc<Fake>, config: ControlPlaneConfig) -> (ControlPlane, ControlClient) {
    let home = tempfile::tempdir().unwrap();
    let plane =
        ControlPlane::serve(fake.clone(), &config.without_socket(), home.path()).await.unwrap();
    let client = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    (plane, client)
}

fn transport_of(record: &AuditRecordDraft) -> &str {
    record.ext["transport"].as_str().unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn the_socket_admits_the_os_user_and_records_every_action() {
    let fake = Fake::new();
    let pid = fake.spawn(&os_user(), "s-1", RunState::Running).await;
    let home = tempfile::tempdir().unwrap();
    let plane = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap();
    let socket = home.path().join(ControlPlaneConfig::DEFAULT_SOCKET);
    assert_eq!(plane.socket_path(), Some(socket.as_path()));
    assert_eq!(plane.http_addr(), None);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the socket is owner-only");
    }

    let client =
        ControlClient::connect_local(home.path()).await.unwrap().expect("a kernel answers");
    assert_eq!(client.endpoint(), format!("unix:{}", socket.display()));
    assert!(!client.health().await.unwrap().frozen);
    let killed = client.kill(&pid, Some("runaway")).await.unwrap();
    assert_eq!(
        (killed.run.state, killed.run.error.as_deref()),
        (RunState::Cancelled, Some("runaway"))
    );

    let records = fake.control_records();
    assert_eq!(records.len(), 1, "reads are not recorded, the kill is");
    let record = &records[0];
    assert_eq!(transport_of(record), "socket");
    assert_eq!(record.ext["action"], "kill");
    assert_eq!(record.ext["target"], json!(pid));
    assert_eq!(record.ext["subject"], json!({ "kind": "human", "id": os_user() }));
    assert_eq!(
        record.actor,
        AuditActor::subject(adk_core::principal::SubjectRef::human(os_user()))
    );

    // A second kernel on the same home is refused while the first answers.
    let error = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already listens"), "{error}");

    plane.shutdown().await.unwrap();
    assert!(!socket.exists(), "shutdown removes the socket");
    assert!(ControlClient::connect_local(home.path()).await.unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn a_stale_socket_is_replaced_and_reads_as_no_kernel() {
    let fake = Fake::new();
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join(ControlPlaneConfig::DEFAULT_SOCKET);
    // A kernel that died without removing its socket.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    assert!(ControlClient::connect_local(home.path()).await.unwrap().is_none());
    let plane = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap();
    assert!(ControlClient::connect_local(home.path()).await.unwrap().is_some());
    plane.shutdown().await.unwrap();

    // A path that is not a socket is never removed.
    std::fs::write(&socket, "keep").unwrap();
    let error =
        ControlPlane::serve(fake, &ControlPlaneConfig::default(), home.path()).await.unwrap_err();
    assert!(error.to_string().contains("is not a socket"), "{error}");
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "keep");
}

/// The longest path a Unix socket address holds here, its NUL aside.
#[cfg(unix)]
const SOCKET_MAX: usize =
    if cfg!(any(target_os = "linux", target_os = "android")) { 107 } else { 103 };

/// A kernel home under `base` whose `kernel.sock` overflows a socket address everywhere.
#[cfg(unix)]
fn long_home(base: &std::path::Path) -> std::path::PathBuf {
    let home = base.join("a-kernel-home-whose-path-is-too-long-for-a-unix-socket/".repeat(2));
    std::fs::create_dir_all(&home).unwrap();
    assert!(home.join(ControlPlaneConfig::DEFAULT_SOCKET).as_os_str().len() > SOCKET_MAX + 1);
    home
}

#[cfg(unix)]
#[tokio::test]
async fn a_home_too_long_for_a_socket_serves_a_short_one_its_clients_find() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fake = Fake::new();
    let pid = fake.spawn(&os_user(), "s-1", RunState::Running).await;
    let base = tempfile::tempdir().unwrap();
    let home = long_home(base.path());
    let plane =
        ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), &home).await.unwrap();
    let socket = plane.socket_path().expect("a socket is served").to_path_buf();
    assert!(socket.as_os_str().len() <= SOCKET_MAX, "{}", socket.display());
    assert_eq!(ControlPlaneConfig::default().socket_path(&home), Some(socket.clone()));
    let dir = socket.parent().unwrap();
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("adk-rust-"), "{name}");
    let (dir_meta, socket_meta) =
        (std::fs::symlink_metadata(dir).unwrap(), std::fs::metadata(&socket).unwrap());
    assert_eq!(
        (
            dir_meta.is_dir(),
            dir_meta.permissions().mode() & 0o777,
            dir_meta.uid(),
            socket_meta.permissions().mode() & 0o777,
        ),
        (true, 0o700, socket_meta.uid(), 0o600),
        "the socket and its own directory are the user's alone"
    );

    // Clients derive the same socket from the home, and from a short spelling of it through a
    // symlink, so the tag comes from the canonical home.
    let link = base.path().join("home");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    for spelling in [&home, &link] {
        let client =
            ControlClient::connect_local(spelling).await.unwrap().expect("a kernel answers");
        assert_eq!(client.endpoint(), format!("unix:{}", socket.display()));
    }
    let client = ControlClient::connect_local(&link).await.unwrap().unwrap();
    let killed = client.kill(&pid, Some("runaway")).await.unwrap();
    assert_eq!(killed.run.state, RunState::Cancelled);
    let records = fake.control_records();
    assert_eq!(
        (transport_of(&records[0]), &records[0].ext["subject"]),
        ("socket", &json!({ "kind": "human", "id": os_user() })),
        "the short socket admits the OS user, as the one in the home does"
    );

    plane.shutdown().await.unwrap();
    assert!(!dir.exists(), "shutdown removes the socket and its directory");
    assert!(ControlClient::connect_local(&home).await.unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn a_short_socket_directory_that_is_not_the_users_alone_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let fake = Fake::new();
    let base = tempfile::tempdir().unwrap();
    let home = long_home(base.path());
    let socket = ControlPlaneConfig::default().socket_path(&home).unwrap();
    assert!(socket.as_os_str().len() <= SOCKET_MAX, "{}", socket.display());
    let dir = socket.parent().unwrap();
    let elsewhere = base.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();

    // A symlink in its place, even to a directory of the user's, and a directory others may
    // write in: the kernel serves neither and no client connects through either.
    std::os::unix::fs::symlink(&elsewhere, dir).unwrap();
    let served = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), &home).await;
    let reached = ControlClient::connect_local(&home).await;
    std::fs::remove_file(dir).unwrap();
    for error in [served.unwrap_err(), reached.unwrap_err()] {
        let text = error.to_string();
        assert!(
            text.contains("is a symlink") && text.contains(&dir.display().to_string()),
            "{text}"
        );
    }
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing is made there");

    std::fs::create_dir(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let served = ControlPlane::serve(fake, &ControlPlaneConfig::default(), &home).await;
    let reached = ControlClient::connect_local(&home).await;
    std::fs::remove_dir(dir).unwrap();
    for error in [served.unwrap_err(), reached.unwrap_err()] {
        let text = error.to_string();
        assert!(
            text.contains("other users") && text.contains(&dir.display().to_string()),
            "{text}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_socket_too_long_even_in_the_temp_directory_is_a_complete_error() {
    let home = tempfile::tempdir().unwrap();
    let name = format!("{}.sock", "s".repeat(SOCKET_MAX));
    let config = ControlPlaneConfig::default().with_socket(&name);
    let error = ControlPlane::serve(Fake::new(), &config, home.path()).await.unwrap_err();
    let text = error.to_string();
    let short = config.socket_path(home.path()).unwrap();
    for part in [
        home.path().join(&name).display().to_string(),
        short.display().to_string(),
        format!("{SOCKET_MAX} bytes"),
        "--home".to_string(),
    ] {
        assert!(text.contains(&part), "{part:?} is missing from: {text}");
    }
    assert!(!short.parent().unwrap().exists(), "no directory is left behind");
}

#[cfg(unix)]
#[test]
fn the_socket_refuses_a_peer_of_another_uid() {
    use super::server::socket_subject;

    let context = socket_subject(Some(501), 501, "ana").unwrap();
    assert_eq!(
        (context.user_id.as_str(), context.scopes.as_slice()),
        ("ana", &["operator".to_string()][..])
    );
    for peer in [Some(0), Some(502), None] {
        let error = socket_subject(peer, 501, "ana").unwrap_err();
        assert!(matches!(error, KernelError::Unauthorized { .. }), "{peer:?}: {error}");
    }
}

#[cfg(windows)]
#[tokio::test]
async fn the_pipe_admits_the_os_user_and_records_every_action() {
    use std::os::windows::io::AsRawHandle as _;

    let fake = Fake::new();
    let pid = fake.spawn(&os_user(), "s-1", RunState::Running).await;
    let home = tempfile::tempdir().unwrap();
    let plane = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap();
    let name = ControlPlaneConfig::default().pipe_name(home.path()).unwrap();
    assert_eq!(plane.pipe_name(), Some(name.as_str()));
    assert_eq!((plane.socket_path(), plane.http_addr()), (None, None));
    let raw = super::pipe::open(&name).await.unwrap();
    assert!(
        super::pipe::testing::is_owner_only(raw.as_raw_handle()).unwrap(),
        "the pipe is owned by the kernel's user and admits that user alone"
    );
    drop(raw);

    let client =
        ControlClient::connect_local(home.path()).await.unwrap().expect("a kernel answers");
    assert_eq!(client.endpoint(), format!("pipe:{name}"));
    assert!(!client.health().await.unwrap().frozen);
    let killed = client.kill(&pid, Some("runaway")).await.unwrap();
    assert_eq!(
        (killed.run.state, killed.run.error.as_deref()),
        (RunState::Cancelled, Some("runaway"))
    );
    let records = fake.control_records();
    assert_eq!(records.len(), 1, "reads are not recorded, the kill is");
    assert_eq!(transport_of(&records[0]), "pipe");
    assert_eq!(records[0].ext["subject"], json!({ "kind": "human", "id": os_user() }));

    // A second kernel on the same home is refused while the first answers.
    let error = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already listens"), "{error}");

    plane.shutdown().await.unwrap();
    assert!(ControlClient::connect_local(home.path()).await.unwrap().is_none());
}

/// Opens the pipe `name` from a thread impersonating the anonymous logon, a token whose
/// SID is not the kernel's user's, and answers what `GET /api/kernel/health` gets.
#[cfg(windows)]
async fn health_as_anonymous(name: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use windows_sys::Win32::Security::{ImpersonateAnonymousToken, RevertToSelf};
    use windows_sys::Win32::System::Threading::GetCurrentThread;

    let name = name.to_string();
    let mut pipe = tokio::task::spawn_blocking(move || {
        // SAFETY: impersonates on this thread only, and reverts before it returns.
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        let opened = tokio::net::windows::named_pipe::ClientOptions::new().open(&name);
        // SAFETY: reverts the impersonation above.
        assert_ne!(unsafe { RevertToSelf() }, 0);
        opened
    })
    .await
    .unwrap()?;
    let request = format!(
        "GET {}{} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
        paths::API_PREFIX,
        paths::HEALTH
    );
    pipe.write_all(request.as_bytes()).await?;
    let mut answer = String::new();
    pipe.read_to_string(&mut answer).await?;
    Ok(answer)
}

#[cfg(windows)]
#[tokio::test]
async fn the_pipe_refuses_a_client_of_another_identity() {
    use super::pipe::Descriptor;
    use super::server::api;

    // The owner-only DACL refuses another identity at open.
    let fake = Fake::new();
    let home = tempfile::tempdir().unwrap();
    let plane = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap();
    let name = plane.pipe_name().unwrap().to_string();
    let error = health_as_anonymous(&name).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied, "{error}");
    plane.shutdown().await.unwrap();

    // Behind a DACL that lets anyone open it, the token check still refuses another
    // identity and admits the kernel's user.
    let name = format!(r"\\.\pipe\adk-rust-kernel-test-{}", uuid::Uuid::new_v4().simple());
    // Owned by the user, so the client's owner check passes; open to everyone, the anonymous
    // logon included, at any integrity level, the anonymous token's being untrusted.
    let sid = crate::win::UserSid::current().unwrap().to_sid_string().unwrap();
    let open =
        Descriptor::from_sddl(&format!("O:{sid}D:(A;;FA;;;WD)(A;;FA;;;AN)S:(ML;;NW;;;S-1-16-0)"))
            .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let backend: std::sync::Arc<dyn ControlBackend> = fake.clone();
    let task = super::pipe::serve_with(&name, api(&backend), stop.clone(), open).await.unwrap();
    let answer = health_as_anonymous(&name).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
    let owner = ControlClient::pipe(name.clone());
    assert!(!owner.health().await.unwrap().frozen);
    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn loopback_http_admits_loopback_peers_as_the_os_user() {
    let fake = Fake::new();
    let pid = fake.spawn(&os_user(), "s-1", RunState::Queued).await;
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;
    assert!(plane.http_addr().unwrap().port() > 0, "the bound port is reported");
    let process = client.suspend(&pid).await.unwrap();
    assert_eq!(process.meta.suspended_by, Some(Suspension::Manual));
    let record = &fake.control_records()[0];
    assert_eq!(
        (transport_of(record), record.ext["subject"]["id"].clone()),
        ("http", json!(os_user()))
    );
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn bearer_http_admits_only_the_token_holder() {
    let fake = Fake::new();
    let pid = fake.spawn("phone", "s-1", RunState::Waiting).await;
    let config = ControlPlaneConfig::default()
        .with_http(loopback())
        .with_extractor(Arc::new(TokenExtractor));
    let (plane, client) = http_plane(&fake, config).await;

    for client in [client.clone(), client.clone().with_bearer_token("wrong")] {
        let error = client.health().await.unwrap_err();
        assert_eq!(adk_core::AdkError::from(error).category, adk_core::ErrorCategory::Unauthorized);
        let error = client.resume(&pid).await.unwrap_err();
        assert_eq!(adk_core::AdkError::from(error).http_status_code(), 401);
    }
    assert!(
        fake.control_records().is_empty(),
        "a refused caller records nothing and changes nothing"
    );
    assert_eq!(fake.process(&pid).await.unwrap().unwrap().run.state, RunState::Waiting);

    let client = client.with_bearer_token("right");
    assert_eq!(client.resume(&pid).await.unwrap().run.state, RunState::Queued);
    let record = &fake.control_records()[0];
    assert_eq!(record.ext["subject"], json!({ "kind": "human", "id": "phone" }));
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_non_loopback_bind_without_authentication_is_refused() {
    let fake = Fake::new();
    let home = tempfile::tempdir().unwrap();
    let exposed =
        ControlPlaneConfig::default().without_socket().with_http("0.0.0.0:0".parse().unwrap());
    let error = ControlPlane::serve(fake, &exposed, home.path()).await.unwrap_err();
    assert!(matches!(error, KernelError::Config { .. }), "{error}");
    assert!(error.to_string().contains("needs authentication"), "{error}");
    assert!(exposed.with_extractor(Arc::new(TokenExtractor)).validate().is_ok());
}

#[tokio::test]
async fn routes_merged_without_an_authenticated_subject_answer_401() {
    // Merged into a server that authenticates no one: the auth middleware inserts `None`,
    // or a router mounted without it inserts nothing. Both are refused.
    let fake = Fake::new();
    let pid = fake.spawn(&os_user(), "s-1", RunState::Running).await;
    let none = routes(fake.clone()).layer(axum::Extension(None::<RequestContext>));
    for api in [routes(fake.clone()), none] {
        let app = axum::Router::new().nest(paths::API_PREFIX, api);
        let listener = tokio::net::TcpListener::bind(loopback()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ControlClient::http(format!("http://{addr}"));
        for error in [
            client.health().await.unwrap_err(),
            client.processes(&ProcessesQuery::default()).await.unwrap_err(),
            client.kill(&pid, None).await.unwrap_err(),
            client.freeze(None).await.unwrap_err(),
            client.events(None).await.err().unwrap(),
        ] {
            assert_eq!(adk_core::AdkError::from(error).http_status_code(), 401);
        }
        server.abort();
    }
    assert!(!fake.governance.is_frozen());
    assert_eq!(fake.process(&pid).await.unwrap().unwrap().run.state, RunState::Running);
}

#[tokio::test]
async fn ps_nice_and_errors_round_trip() {
    let fake = Fake::new();
    let waiting = fake.spawn(&os_user(), "s-1", RunState::Waiting).await;
    fake.spawn(&os_user(), "s-2", RunState::Running).await;
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;

    assert_eq!(client.processes(&ProcessesQuery::default()).await.unwrap().len(), 2);
    let query = ProcessesQuery { state: Some(RunState::Waiting), ..ProcessesQuery::default() };
    let listed = client.processes(&query).await.unwrap();
    assert_eq!(
        listed.iter().map(|process| process.meta.pid.as_str()).collect::<Vec<_>>(),
        [waiting.as_str()]
    );

    let niced = client.nice(&waiting, QueueClass::Background).await.unwrap();
    assert_eq!(niced.meta.queue, QueueClass::Background);
    assert_eq!(niced.meta.parent.as_deref(), Some(waiting.as_str()), "the moved process is new");
    assert_ne!(niced.meta.pid, waiting);

    let error = adk_core::AdkError::from(client.suspend("no-such-run").await.unwrap_err());
    assert_eq!((error.code, error.http_status_code()), (crate::error::NOT_FOUND_CODE, 404));
    assert!(error.message.contains("adk-rust ps"), "{}", error.message);
    plane.shutdown().await.unwrap();
}

/// Spec 3.2 §7.8, §7.11: the partition table is read by any authenticated caller, without a
/// control record.
#[tokio::test]
async fn partitions_are_listed_to_an_authenticated_caller() {
    let fake = Fake::new();
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;
    let partitions = client.partitions().await.unwrap();
    let ids: Vec<&str> = partitions.iter().map(|partition| partition.id.as_str()).collect();
    assert_eq!(ids, ["home", "work"]);
    assert_eq!(partitions[1].drivers, ["calendar"]);
    assert!(fake.recorder.records().is_empty(), "a read is not recorded");

    // The writes need the operator scope, which a loopback bind grants, and are recorded.
    let put =
        client.put_partition(&crate::partition::Partition::new("team", "team")).await.unwrap();
    assert_eq!(put.id.as_str(), "team");
    let moded =
        client.set_partition_mode("work", crate::partition::PartitionMode::Auto).await.unwrap();
    assert_eq!(moded.default_mode, crate::partition::PartitionMode::Auto);
    let actions: Vec<String> = fake
        .recorder
        .records()
        .iter()
        .map(|record| record.ext["action"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(actions, ["partition.put", "partition.mode"]);
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn freeze_parks_and_only_unfreeze_resumes_what_it_parked() {
    let fake = Fake::new();
    let running = fake.spawn(&os_user(), "s-1", RunState::Running).await;
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;

    let frozen = client.freeze(Some("incident 12")).await.unwrap();
    assert_eq!(
        (frozen.frozen, frozen.reason.as_deref(), frozen.parked_runs),
        (true, Some("incident 12"), Some(1))
    );
    assert!(client.health().await.unwrap().frozen);
    let error = adk_core::AdkError::from(client.resume(&running).await.unwrap_err());
    assert_eq!(error.http_status_code(), 409, "a freeze-parked process resumes only on unfreeze");

    // Resuming over HTTP is refused: it happens in the web home, in the kernel's process, or
    // on the kernel's own terminal (spec 3.3 S-I10).
    let refused = adk_core::AdkError::from(client.unfreeze().await.unwrap_err());
    assert_eq!(
        (refused.http_status_code(), refused.code),
        (403, crate::error::RESUME_NOT_HERE_CODE)
    );
    assert!(client.health().await.unwrap().frozen);
    fake.unfreeze(AuditActor::default()).await.unwrap();
    assert_eq!(fake.process(&running).await.unwrap().unwrap().run.state, RunState::Queued);
    let actions: Vec<_> =
        fake.control_records().iter().map(|record| record.ext["action"].clone()).collect();
    assert_eq!(actions, [json!("freeze"), json!("resume")]);
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn events_stream_as_server_sent_events_from_a_cursor() {
    let fake = Fake::new();
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;
    let mut events = client.events(Some(41)).await.unwrap();
    let published =
        fake.bus.publish(KernelEvent::Governance { frozen: true, reason: Some("drill".into()) });
    let received = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(received, published);
    let _ = client.events(None).await.unwrap();
    assert_eq!(*fake.asked_after.lock().unwrap(), [Some(41), None]);
    drop(events);
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn last_event_id_wins_over_the_query() {
    let fake = Fake::new();
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;
    let url = format!("{}/api{}?after=3", client.endpoint(), paths::EVENTS);
    let response =
        reqwest::Client::new().get(url).header("last-event-id", "7").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(*fake.asked_after.lock().unwrap(), [Some(7)]);
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn attach_returns_the_transcript_cursor_pending_holds_and_process() {
    let fake = Fake::new();
    let user = os_user();
    fake.sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: user.clone(),
            session_id: Some("s-1".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut asked = Event::new("inv-1");
    asked.author = "user".into();
    fake.sessions.append_event("s-1", asked.clone()).await.unwrap();
    let mut held = Event::new("inv-1");
    held.author = "adk-rust".into();
    held.actions.tool_confirmation = Some(ToolConfirmationRequest {
        tool_name: "bash".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "command": "cargo test" }),
        acting: None,
        origin: None,
    });
    fake.sessions.append_event("s-1", held.clone()).await.unwrap();
    let pid = fake.spawn(&user, "s-1", RunState::Waiting).await;
    fake.spawn(&user, "s-1", RunState::Completed).await;

    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;
    let attached = client.attach("s-1", &AttachRequest::default()).await.unwrap();
    assert_eq!(attached.session_id, "s-1");
    assert_eq!(attached.transcript.len(), 2);
    assert_eq!(attached.cursor, Some(attached.transcript[1].timestamp));
    assert_eq!(
        attached.pending.iter().map(|request| request.tool_name.as_str()).collect::<Vec<_>>(),
        ["bash"]
    );
    assert_eq!(
        attached.process.as_ref().map(|process| process.meta.pid.as_str()),
        Some(pid.as_str())
    );

    // From the cursor, a second surface sees only what is new, and the same holds.
    let again = AttachRequest { after: attached.cursor, ..AttachRequest::default() };
    let later = client.attach("s-1", &again).await.unwrap();
    assert!(later.transcript.is_empty());
    assert_eq!(later.cursor, attached.cursor);
    assert_eq!(later.pending.len(), 1);

    // Another user's session is not the caller's to attach to.
    let other = AttachRequest { user_id: Some("mallory".into()), ..AttachRequest::default() };
    let error = adk_core::AdkError::from(client.attach("s-1", &other).await.unwrap_err());
    assert_eq!(error.http_status_code(), 401);
    let error = adk_core::AdkError::from(
        client.attach("no-such", &AttachRequest::default()).await.unwrap_err(),
    );
    assert_eq!(error.http_status_code(), 404);

    let attaches =
        fake.control_records().iter().filter(|record| record.ext["action"] == "attach").count();
    assert_eq!(attaches, 3, "every attach by the caller is recorded; the refused one is not");
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_action_whose_record_fails_is_refused_except_freeze() {
    struct Failing;
    #[async_trait]
    impl adk_core::audit_log::AuditRecorder for Failing {
        fn digest(&self, value: &serde_json::Value) -> String {
            adk_core::json_digest(value)
        }
        async fn record(&self, _draft: AuditRecordDraft) -> adk_core::Result<()> {
            Err(adk_core::AdkError::internal(
                adk_core::ErrorComponent::Auth,
                "audit.disk_full",
                "disk full",
            ))
        }
    }
    let mut fake = Fake::new();
    Arc::get_mut(&mut fake).unwrap().trail = AuditTrail::new(Arc::new(Failing));
    let pid = fake.spawn(&os_user(), "s-1", RunState::Running).await;
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;

    let error = adk_core::AdkError::from(client.kill(&pid, None).await.unwrap_err());
    assert_eq!(error.http_status_code(), 503);
    assert_eq!(fake.process(&pid).await.unwrap().unwrap().run.state, RunState::Running);
    let frozen = client.freeze(None).await.unwrap();
    assert!(frozen.frozen, "the kill switch never waits on the log");
    assert!(
        frozen.audit_error.as_deref().is_some_and(|error| error.contains("disk full")),
        "the answer reports the missing record: {frozen:?}"
    );
    assert!(client.unfreeze().await.is_err());
    assert!(fake.governance.is_frozen());
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn changing_the_kernel_needs_the_operator_scope() {
    let fake = Fake::new();
    let pid = fake.spawn("viewer", "s-1", RunState::Running).await;
    let config = ControlPlaneConfig::default()
        .with_http(loopback())
        .with_extractor(Arc::new(TokenExtractor));
    let (plane, client) = http_plane(&fake, config).await;
    let viewer = client.with_bearer_token("viewer");

    // Reads need only an authenticated subject.
    assert!(!viewer.health().await.unwrap().frozen);
    assert_eq!(viewer.processes(&ProcessesQuery::default()).await.unwrap().len(), 1);
    // Every change needs the operator scope.
    let spawn = super::api::SpawnRequest::new(APP, json!({ "text": "hi" }));
    for error in [
        viewer.kill(&pid, None).await.unwrap_err(),
        viewer.nice(&pid, QueueClass::Background).await.unwrap_err(),
        viewer.suspend(&pid).await.unwrap_err(),
        viewer.resume(&pid).await.unwrap_err(),
        viewer.freeze(None).await.unwrap_err(),
        viewer.unfreeze().await.unwrap_err(),
        viewer.spawn(&spawn).await.unwrap_err(),
    ] {
        let error = adk_core::AdkError::from(error);
        assert_eq!((error.http_status_code(), error.code), (403, crate::error::UNAUTHORIZED_CODE));
        assert!(error.message.contains("operator"), "{}", error.message);
    }
    assert!(!fake.governance.is_frozen());
    assert_eq!(fake.processes.lock().unwrap().len(), 1);
    assert!(fake.control_records().is_empty());
    plane.shutdown().await.unwrap();
}

#[tokio::test]
async fn spawn_runs_a_registered_target_as_the_caller() {
    let fake = Fake::new();
    let (plane, client) =
        http_plane(&fake, ControlPlaneConfig::default().with_http(loopback())).await;
    let request = super::api::SpawnRequest::new(APP, json!({ "text": "run the tests" }))
        .with_session_id("s-9")
        .with_queue(QueueClass::Background)
        .with_kind(ProcessKind::Goal);
    let process = client.spawn(&request).await.unwrap();
    assert_eq!(
        (
            process.run.user_id.as_str(),
            process.run.session_id.as_deref(),
            process.run.queue.as_str()
        ),
        (os_user().as_str(), Some("s-9"), "background")
    );
    assert!(process.run.authenticated, "the caller authenticated, so the run is theirs");
    assert_eq!(
        (process.meta.kind.clone(), process.meta.queue.clone()),
        (ProcessKind::Goal, QueueClass::Background)
    );

    let records = fake.control_records();
    let record = records.last().unwrap();
    assert_eq!(record.ext["action"], "spawn");
    assert_eq!(record.ext["target"], APP);
    assert_eq!(record.ext["pid"], json!(process.meta.pid));

    // A sleeper: the operator may start a process on a wake set.
    let wake = crate::wake::WakeSet::any_of([crate::wake::Wake::Manual]);
    let sleeper = client
        .spawn(&super::api::SpawnRequest::new(APP, json!("later")).with_wake(wake.clone()))
        .await
        .unwrap();
    assert_eq!(sleeper.meta.wake, Some(wake));

    // An unknown target is refused with the registered ones named.
    let error = adk_core::AdkError::from(
        client.spawn(&super::api::SpawnRequest::new("rm-rf", json!({}))).await.unwrap_err(),
    );
    assert_eq!(error.http_status_code(), 404);
    assert!(error.message.contains("adk-rust, harness.goal"), "{}", error.message);

    // The run's user is never a body field: naming one, or any unknown field, is a 422.
    let url = format!("{}/api{}", client.endpoint(), paths::PROCESSES);
    for body in [
        json!({ "target": APP, "userId": "someone-else" }),
        json!({ "target": APP, "authenticated": false }),
    ] {
        let response = reqwest::Client::new().post(&url).json(&body).send().await.unwrap();
        assert_eq!(response.status(), 422, "{body}");
    }
    assert_eq!(fake.processes.lock().unwrap().len(), 2);
    plane.shutdown().await.unwrap();
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn a_held_call_is_decided_over_the_local_transport() {
    use adk_core::{
        ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector,
    };

    let fake = Fake::new();
    let call = ToolConfirmationRequest {
        tool_name: "bash".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "command": "cargo test" }),
        acting: None,
        origin: None,
    };
    let policy = ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
    let scope = ApprovalScope::new(APP, os_user(), "s-1");
    fake.approvals.submit(ApprovalRequest::new("req-1", scope, &call, policy)).await.unwrap();

    let home = tempfile::tempdir().unwrap();
    let plane = ControlPlane::serve(fake.clone(), &ControlPlaneConfig::default(), home.path())
        .await
        .unwrap();
    let client = ControlClient::connect_local(home.path()).await.unwrap().unwrap();
    let pending = client.approvals().await.unwrap();
    assert_eq!(
        pending.iter().map(|item| (item.id.as_str(), item.tool_name.as_str())).collect::<Vec<_>>(),
        [("req-1", "bash")]
    );
    let decided =
        client.decide("req-1", pending[0].version, ApprovalAction::Approve, None).await.unwrap();
    assert_eq!(decided.state, adk_core::ApprovalState::Approved);
    assert!(client.approvals().await.unwrap().is_empty());
    let error = adk_core::AdkError::from(
        client
            .decide("req-1", pending[0].version, ApprovalAction::Approve, None)
            .await
            .unwrap_err(),
    );
    assert_eq!(error.http_status_code(), 409, "a stale version is refused");
    plane.shutdown().await.unwrap();
}

/// The agenda lists newest first when asked and pages on from an item, so a surface reads
/// the newest items however many older ones the store keeps (spec 3.3 §5.1).
#[tokio::test]
async fn the_agenda_lists_newest_first_and_pages_on_from_an_item() {
    use crate::agenda::{AgendaItem, AgendaOrigin};
    use crate::control::api::AgendaQuery;

    let kernel = crate::Kernel::builder()
        .config(
            crate::KernelConfig::default()
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .build()
        .unwrap();
    let start = chrono::Utc::now() - chrono::Duration::hours(1);
    for index in 0..5 {
        let item = AgendaItem::new(
            format!("item-{index}"),
            adk_core::principal::SubjectRef::human("phone"),
            format!("Item {index}"),
            "because",
            AgendaOrigin::User,
            start + chrono::Duration::minutes(index),
        );
        kernel.agenda().propose(item, crate::PartitionMode::Plan).await.unwrap();
    }
    let config = ControlPlaneConfig::default().with_http(loopback()).without_socket();
    let home = tempfile::tempdir().unwrap();
    let plane = ControlPlane::serve(Arc::new(kernel.clone()), &config, home.path()).await.unwrap();
    let client = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    let ids = |items: Vec<AgendaItem>| items.into_iter().map(|item| item.id).collect::<Vec<_>>();

    let newest = AgendaQuery { newest_first: true, limit: Some(2), ..AgendaQuery::default() };
    assert_eq!(ids(client.agenda(&newest).await.unwrap()), ["item-4", "item-3"]);
    let next = AgendaQuery { after: Some("item-3".into()), ..newest.clone() };
    assert_eq!(ids(client.agenda(&next).await.unwrap()), ["item-2", "item-1"]);
    let oldest = AgendaQuery { after: Some("item-2".into()), ..AgendaQuery::default() };
    assert_eq!(ids(client.agenda(&oldest).await.unwrap()), ["item-3", "item-4"]);
    let unknown = AgendaQuery { after: Some("item-9".into()), ..AgendaQuery::default() };
    let error = client.agenda(&unknown).await.unwrap_err();
    assert_eq!(adk_core::AdkError::from(error).http_status_code(), 404);
    plane.shutdown().await.unwrap();
}

/// The agenda routes on a kernel (spec 3.2 §7.11, K-I16): any authenticated caller lists
/// the agenda; approving, rejecting and scheduling need the `operator` scope, act as the
/// caller, and write a `kernel.control` record before they act.
#[tokio::test]
async fn the_agenda_routes_change_items_only_for_an_operator_and_record_each_change() {
    use crate::agenda::{AgendaItem, AgendaOrigin, AgendaState, AgendaStateKind, ProposalOutcome};
    use crate::control::api::AgendaQuery;
    use crate::wake::{Wake, WakeSet};

    let recorder = InMemoryAuditRecorder::new();
    let kernel = crate::Kernel::builder()
        .config(
            crate::KernelConfig::default()
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .audit(AuditTrail::new(Arc::new(recorder.clone())))
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for title in ["Re-run the tests", "Tidy the changelog", "Review two contradicting facts"] {
        let origin = if ids.len() == 2 { AgendaOrigin::Consolidation } else { AgendaOrigin::User };
        let item = AgendaItem::new(
            format!("item-{}", ids.len()),
            adk_core::principal::SubjectRef::human("phone"),
            title,
            "because",
            origin,
            chrono::Utc::now(),
        );
        let outcome = kernel.agenda().propose(item, crate::PartitionMode::Plan).await.unwrap();
        assert!(matches!(outcome, ProposalOutcome::Held { .. }));
        ids.push(format!("item-{}", ids.len()));
    }
    let config = ControlPlaneConfig::default()
        .with_http(loopback())
        .with_extractor(Arc::new(TokenExtractor))
        .without_socket();
    let home = tempfile::tempdir().unwrap();
    let plane = ControlPlane::serve(Arc::new(kernel.clone()), &config, home.path()).await.unwrap();
    let client = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    let control = |action: &str| {
        recorder
            .records()
            .into_iter()
            .filter(|record| {
                record.action == crate::audit::CONTROL && record.ext["action"] == action
            })
            .count()
    };

    // A viewer reads the agenda and changes nothing.
    let viewer = client.clone().with_bearer_token("viewer");
    let listed = viewer
        .agenda(&AgendaQuery { state: Some(AgendaStateKind::Proposed), ..AgendaQuery::default() })
        .await
        .unwrap();
    assert_eq!(listed.len(), 3);
    let error = viewer.approve_item(&ids[0]).await.unwrap_err();
    assert_eq!(adk_core::AdkError::from(error).http_status_code(), 403);
    assert_eq!(control("agenda.approve"), 0, "a refused change records nothing");
    assert_eq!(kernel.agenda().get(&ids[0]).await.unwrap().unwrap().state, AgendaState::Proposed);

    // An operator approves, rejects and schedules, as itself.
    let operator = client.with_bearer_token("right");
    let approved = operator.approve_item(&ids[0]).await.unwrap();
    assert!(matches!(approved.state, AgendaState::Scheduled { .. }), "{:?}", approved.state);
    let rejected = operator.reject_item(&ids[1], Some("not this week")).await.unwrap();
    assert_eq!(rejected.state, AgendaState::Rejected);
    let at = chrono::Utc::now() + chrono::Duration::hours(2);
    let scheduled = operator.schedule_item(&ids[2], WakeSet::any_of([Wake::At(at)])).await.unwrap();
    assert_eq!(
        scheduled.state,
        AgendaState::Scheduled { wake: WakeSet::any_of([Wake::At(at)]) },
        "a person schedules the consolidation item"
    );
    let error = operator.approve_item(&ids[1]).await.unwrap_err();
    assert_eq!(adk_core::AdkError::from(error).http_status_code(), 409, "a rejected item stays so");
    for action in ["agenda.approve", "agenda.reject", "agenda.schedule"] {
        assert!(control(action) >= 1, "{action} is recorded");
    }
    let approvals: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|record| record.action == crate::audit::AGENDA_APPROVED)
        .collect();
    assert!(
        approvals.iter().any(|record| record.ext["by"] == "phone"),
        "the item records its approver"
    );
    plane.shutdown().await.unwrap();
}

#[cfg(feature = "drivers")]
#[tokio::test]
async fn a_driver_mounts_only_once_the_operator_confirms_the_proposal() {
    use crate::drivers::{DriverState, EffectClass};

    let mut fake = Fake::new();
    let drivers = crate::drivers::DriverSupervisor::builder(Arc::new(
        crate::stores::InMemoryDriverStore::new(),
    ))
    .build();
    Arc::get_mut(&mut fake).unwrap().drivers = Some(drivers.clone());
    let config = ControlPlaneConfig::default()
        .with_http(loopback())
        .with_extractor(Arc::new(TokenExtractor));
    let (plane, client) = http_plane(&fake, config).await;
    let operator = client.clone().with_bearer_token("right");
    let viewer = client.with_bearer_token("viewer");
    let manifest = "id = \"email\"\ncommand = \"mcp-email\"\nargs = [\"--imap\"]\n\
                    partitions = [\"work\"]\nsandbox = \"unsandboxed\"\n\
                    [tools]\nlist_inbox = \"read\"\nsend_draft = \"send_external\"\n";

    // A viewer may list but not mount.
    assert!(viewer.drivers().await.unwrap().is_empty());
    let error =
        adk_core::AdkError::from(viewer.mount_driver("email", manifest, true).await.unwrap_err());
    assert_eq!(error.http_status_code(), 403);

    // Without `confirm` the kernel only shows what mounting approves.
    let shown = operator.mount_driver("email", manifest, false).await.unwrap();
    assert!(shown.mounted.is_none());
    assert_eq!(shown.proposal.command_line, "mcp-email --imap");
    assert_eq!(
        shown.proposal.effect_classes,
        [
            ("list_inbox".to_string(), EffectClass::Read),
            ("send_draft".to_string(), EffectClass::SendExternal)
        ]
        .into_iter()
        .collect()
    );
    assert!(drivers.drivers().await.unwrap().is_empty(), "nothing is mounted before the confirm");
    assert!(fake.control_records().is_empty());

    // A manifest posted under another id is refused.
    assert!(operator.mount_driver("calendar", manifest, true).await.is_err());

    let mounted = operator.mount_driver("email", manifest, true).await.unwrap().mounted.unwrap();
    assert_eq!((mounted.record.id(), mounted.state), ("email", DriverState::Stopped));
    assert_eq!(mounted.record.approved_by, "phone");
    let records = fake.control_records();
    let record = records.last().unwrap();
    assert_eq!(record.ext["action"], "drivers.mount");
    assert_eq!(record.ext["commandDigest"], json!(shown.proposal.command_digest));
    assert_eq!(operator.drivers().await.unwrap().len(), 1);

    let error = adk_core::AdkError::from(operator.restart_driver("calendar").await.unwrap_err());
    assert_eq!(error.http_status_code(), 404);
    assert!(operator.unmount_driver("email").await.unwrap());
    assert!(!operator.unmount_driver("email").await.unwrap());
    assert!(viewer.drivers().await.unwrap().is_empty());
    plane.shutdown().await.unwrap();
}

#[cfg(feature = "proc")]
#[tokio::test]
async fn the_proc_routes_page_and_export_the_ledger_for_an_operator_only() {
    use crate::proc::{
        Activity, Direction, LedgerEntry, LedgerKind, LedgerSink, Observation, ObservedDimension,
        PassState, Phrasing,
    };

    let fake = Fake::new();
    for n in 0..5_u64 {
        let kind = if n % 2 == 0 { LedgerKind::ToolCall } else { LedgerKind::Intent };
        fake.ledger
            .record(
                Activity::new(kind, "harness")
                    .with_subject("cat ~/.ssh/id_ed25519")
                    .with_count("n", n),
            )
            .await
            .unwrap();
    }
    let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
    let observation = Observation {
        id: Observation::id_for(&"default".into(), day),
        partition: "default".into(),
        as_of: day,
        dimensions: vec![ObservedDimension::new(
            "activity.last_minute",
            Direction::Higher,
            day,
            10,
        )],
        windows: vec![7, 14],
        question: "Your routine has changed over the last two weeks. Would you like to review \
                   what changed?"
            .into(),
        phrasing: Phrasing::Template,
        created_at: chrono::Utc::now(),
    };
    fake.ledger.store().put_observation(observation.clone()).await.unwrap();
    let state = PassState::new("default", day, chrono::Utc::now());
    fake.ledger.store().put_pass_state(state.clone()).await.unwrap();

    let config = ControlPlaneConfig::default()
        .with_http(loopback())
        .with_extractor(Arc::new(TokenExtractor));
    let (plane, client) = http_plane(&fake, config).await;
    let operator = client.clone().with_bearer_token("right");
    let viewer = client.with_bearer_token("viewer");

    // A JSON page, then the next from its cursor.
    let params = super::api::LedgerParams { limit: Some(2), ..Default::default() };
    let first = operator.ledger(&params).await.unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.next, Some(first.entries[1].seq));
    let rest = operator
        .ledger(&super::api::LedgerParams { after: first.next, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(rest.entries.len(), 3);
    assert_eq!(rest.next, None);
    let tools = operator
        .ledger(&super::api::LedgerParams { kind: Some("tool_call".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(tools.entries.len(), 3);

    // The JSONL export: one row per line, the cursor in a header, no subject in the clear.
    let url = format!(
        "http://{}{}{}?format=jsonl&limit=3",
        plane.http_addr().unwrap(),
        paths::API_PREFIX,
        paths::PROC_LEDGER
    );
    let answer = reqwest::Client::new().get(&url).bearer_auth("right").send().await.unwrap();
    assert_eq!(answer.headers()["content-type"], "application/x-ndjson");
    let next: u64 = answer.headers()["x-ledger-next"].to_str().unwrap().parse().unwrap();
    let body = answer.text().await.unwrap();
    assert!(!body.contains("id_ed25519"), "{body}");
    let lines: Vec<LedgerEntry> =
        body.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(next, lines[2].seq);

    let records = fake.control_records();
    assert_eq!(records.len(), 4, "every ledger read is recorded");
    assert!(records.iter().all(|record| record.ext["action"] == "proc.ledger"));
    assert_eq!(records[3].ext["format"], "jsonl");

    assert_eq!(operator.observations(&Default::default()).await.unwrap(), [observation]);
    assert_eq!(operator.baselines().await.unwrap(), [state]);

    for error in [
        viewer.ledger(&Default::default()).await.unwrap_err(),
        viewer.observations(&Default::default()).await.unwrap_err(),
        viewer.baselines().await.map(|_| ()).unwrap_err(),
    ] {
        let error = adk_core::AdkError::from(error);
        assert_eq!(error.http_status_code(), 403, "{}", error.message);
    }
    assert_eq!(fake.control_records().len(), 4, "a refused read writes no record");
    plane.shutdown().await.unwrap();
}
