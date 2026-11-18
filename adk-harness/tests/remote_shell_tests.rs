//! The harness shell in a remote sandbox (`ShellIsolation::Remote`, feature `remote-shell`).
//!
//! Most tests run over a fake `SandboxClient` that reports itself off-host and runs each
//! environment in its own directory, apart from the workspace, as a remote microVM is. One
//! test runs the real `LocalUnixClient`, which reports itself on-host, so the harness governs
//! it as the host shell. The E2B, Daytona and Modal clients are covered by adk-sandbox's
//! contract tests; the live tests at the end run each of them.

#![cfg(all(feature = "remote-shell", feature = "runner", unix))]

use crate::common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::audit_log::AuditRecordOutcome;
use adk_core::{RunBudget, Tool, ToolOrigin};
use adk_harness::{
    AUTO_ALLOW_UNAVAILABLE_CODE, ContainerBashTool, HarnessMode, InMemoryShellLedger,
    IsolationStatus, RemoteShellConfig, ShellIsolation, ShellLedger,
};
use adk_model::ScriptedResponse;
use adk_sandbox::SandboxError;
use adk_sandbox::workspace::{
    ClientCapabilities, CommandRequest, DirEntry, ExecOutput, LocalUnixClient, Manifest,
    ManifestEntry, SandboxClient, SandboxSession, SessionHandle, SnapshotId,
};
use async_trait::async_trait;
use common::{APP, Outcome, fixture_in, outcome, response_of};
use serde_json::{Value, json};

/// What the fake's sessions do with a command.
#[derive(Clone, Copy)]
enum Behaviour {
    /// `sh -c` in the environment's own directory.
    Shell,
    /// Never finishes, until the call is dropped.
    Hang,
}

/// A remote provider stand-in: each environment is a directory of its own, never the
/// workspace, and it reports `remote` and `network_blocked` as configured.
struct FakeRemote {
    base: PathBuf,
    behaviour: Behaviour,
    network_blocked: bool,
    provision_delay: Duration,
    provisioned: AtomicUsize,
    stopped: AtomicUsize,
    live: Mutex<HashMap<String, PathBuf>>,
    manifests: Mutex<Vec<Manifest>>,
}

impl FakeRemote {
    fn new(base: &Path, behaviour: Behaviour) -> Arc<Self> {
        Self::with_network(base, behaviour, true)
    }

    fn with_network(base: &Path, behaviour: Behaviour, network_blocked: bool) -> Arc<Self> {
        Arc::new(Self {
            base: base.to_path_buf(),
            behaviour,
            network_blocked,
            provision_delay: Duration::ZERO,
            provisioned: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            live: Mutex::new(HashMap::new()),
            manifests: Mutex::new(Vec::new()),
        })
    }

    fn provisioned(&self) -> usize {
        self.provisioned.load(Ordering::SeqCst)
    }

    fn stopped(&self) -> usize {
        self.stopped.load(Ordering::SeqCst)
    }

    fn live(&self) -> usize {
        self.live.lock().unwrap().len()
    }
}

#[async_trait]
impl SandboxClient for FakeRemote {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        tokio::time::sleep(self.provision_delay).await;
        let n = self.provisioned.fetch_add(1, Ordering::SeqCst) + 1;
        let root = self.base.join(format!("vm-{n}"));
        std::fs::create_dir_all(&root).unwrap();
        for entry in &manifest.entries {
            if let ManifestEntry::File { path, content } = entry {
                std::fs::write(root.join(path), content).unwrap();
            }
        }
        self.manifests.lock().unwrap().push(manifest.clone());
        let handle = SessionHandle::new(format!("vm-{n}"));
        self.live.lock().unwrap().insert(handle.as_str().to_string(), root);
        Ok(handle)
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        let root =
            self.live.lock().unwrap().get(handle.as_str()).cloned().ok_or_else(|| {
                SandboxError::SessionNotFound { handle: handle.as_str().to_string() }
            })?;
        Ok(Box::new(FakeVm { root, behaviour: self.behaviour }))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        if self.live.lock().unwrap().remove(handle.as_str()).is_none() {
            return Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() });
        }
        self.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn snapshot(&self, _: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        Err(SandboxError::ExecutionFailed("the fake has no snapshots".into()))
    }

    async fn resume(&self, _: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        Err(SandboxError::ExecutionFailed("the fake has no snapshots".into()))
    }

    fn provider(&self) -> &str {
        "fakevm"
    }

    fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities::default()
            .with_remote(true)
            .with_network_blocked(self.network_blocked)
            .with_max_lifetime(Duration::from_secs(600))
    }
}

struct FakeVm {
    root: PathBuf,
    behaviour: Behaviour,
}

#[async_trait]
impl SandboxSession for FakeVm {
    async fn exec_command(
        &self,
        command: &str,
        _: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        self.exec(CommandRequest::new(command)).await
    }

    async fn exec(&self, request: CommandRequest) -> Result<ExecOutput, SandboxError> {
        let started = Instant::now();
        if let Behaviour::Hang = self.behaviour {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        let run = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&request.command)
            .current_dir(&self.root)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = match request.timeout {
            Some(timeout) => match tokio::time::timeout(timeout, run).await {
                Ok(output) => output,
                Err(_) => return Ok(ExecOutput::new("", "", -1, started.elapsed(), true)),
            },
            None => run.await,
        }
        .map_err(|error| SandboxError::ExecutionFailed(error.to_string()))?;
        Ok(ExecOutput::new(
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            output.status.code().unwrap_or(-1),
            started.elapsed(),
            false,
        ))
    }

    async fn read_file(&self, _: &str) -> Result<Vec<u8>, SandboxError> {
        Err(SandboxError::ExecutionFailed("not used".into()))
    }

    async fn write_file(&self, _: &str, _: &[u8]) -> Result<(), SandboxError> {
        Err(SandboxError::ExecutionFailed("not used".into()))
    }

    async fn list_dir(&self, _: &str) -> Result<Vec<DirEntry>, SandboxError> {
        Err(SandboxError::ExecutionFailed("not used".into()))
    }

    async fn apply_patch(&self, _: &str) -> Result<(), SandboxError> {
        Err(SandboxError::ExecutionFailed("not used".into()))
    }
}

fn remote(client: Arc<FakeRemote>) -> RemoteShellConfig {
    RemoteShellConfig::new(client).with_idle_ttl(Duration::from_secs(600))
}

fn call(args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call("bash", args)
}

fn call_with_id(id: &str, args: Value) -> ScriptedResponse {
    let mut response = adk_core::LlmResponse::new(adk_core::Content {
        role: "model".to_string(),
        parts: vec![adk_core::Part::FunctionCall {
            name: "bash".into(),
            args,
            id: Some(id.into()),
            thought_signature: None,
        }],
    });
    response.finish_reason = Some(adk_core::FinishReason::Stop);
    response.turn_complete = true;
    ScriptedResponse::response(response)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

#[tokio::test]
async fn the_remote_shell_declares_itself_and_binds_rules_to_its_provider() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = FakeRemote::new(vms.path(), Behaviour::Shell);
    let f =
        fixture_in(dir, vec![done()], |b| b.shell(ShellIsolation::Remote(remote(client.clone()))))
            .await;
    let status = f.harness.agent().info().isolation.clone();
    assert_eq!(status, IsolationStatus::Remote { provider: "fakevm".into(), off_host: true });
    assert!(!status.behaves_as_host() && status.confines_reads());
    assert_eq!(status.to_string(), "remote sandbox (fakevm)");
    assert!(f.harness.agent().info().degraded_reason.is_none());

    let ws = adk_devtools::Workspace::new(".").allow_bash(true);
    let ledger: Arc<dyn ShellLedger> = Arc::new(InMemoryShellLedger::new());
    let blocked = ContainerBashTool::remote(ws.clone(), remote(client), ledger.clone());
    let open = ContainerBashTool::remote(
        ws.clone(),
        remote(FakeRemote::with_network(vms.path(), Behaviour::Shell, false)),
        ledger.clone(),
    );
    let annotations = blocked.annotations().unwrap();
    assert_eq!(annotations["enforcer"], "remote");
    assert_eq!(annotations["provider"], "fakevm");
    assert_eq!(annotations["network"], false);
    assert_eq!(annotations["reads"], "confined");
    assert_eq!(open.annotations().unwrap()["network"], true);
    assert_ne!(
        ToolOrigin::of(&blocked).declaration_digest,
        ToolOrigin::of(&open).declaration_digest
    );
    assert!(blocked.description().contains("remote sandbox (fakevm)"), "{}", blocked.description());
    assert!(blocked.description().contains("not visible to read_file"));
    assert!(blocked.parameters_schema().unwrap()["properties"].get("unsandboxed").is_none());
}

#[tokio::test]
async fn one_environment_per_session_runs_apart_from_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = FakeRemote::new(vms.path(), Behaviour::Shell);
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "seed.txt".into(),
        content: b"seeded".to_vec(),
    }]);
    let config = remote(client.clone()).with_manifest(manifest.clone());
    let f = fixture_in(
        dir,
        vec![
            call_with_id("call-a", json!({ "command": "cat seed.txt; echo made > made.txt" })),
            done(),
            call(json!({ "command": "cat made.txt" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Remote(config)).mode(HarnessMode::Auto),
    )
    .await;
    let events = f.send("first").await;
    let response = response_of(&events, "bash");
    assert_eq!(outcome(&response), Outcome::Executed, "{response}");
    assert_eq!(response["stdout"], "seeded");
    assert!(!f.path("made.txt").exists(), "a remote command never writes the host workspace");
    let events = f.send("second").await;
    assert_eq!(
        response_of(&events, "bash")["stdout"],
        "made\n",
        "the environment is kept across runs"
    );
    assert_eq!((client.provisioned(), client.stopped()), (1, 0));
    assert_eq!(client.manifests.lock().unwrap().as_slice(), [manifest]);

    let begin = f.records("harness.bash.begin");
    assert_eq!(begin.len(), 2);
    assert_eq!(begin[0].ext["enforcer"], "remote");
    assert_eq!(begin[0].ext["provider"], "fakevm");
    assert_eq!(begin[0].ext["environment"], "vm-1");
    let end = f.records("harness.bash.end");
    assert_eq!(end[0].outcome, Some(AuditRecordOutcome::Succeeded));
    let entry = f
        .harness
        .agent()
        .runtime()
        .shell_ledger()
        .get(APP, "alice", "s1", "call-a")
        .await
        .unwrap()
        .expect("the call is in the shell ledger");
    assert!(entry.sandboxed);
    assert_eq!(entry.enforcer.as_deref(), Some("remote"));

    f.harness.close().await;
    assert_eq!((client.stopped(), client.live()), (1, 0), "close destroys the environment");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_freeze_stops_an_in_flight_remote_command_and_destroys_its_environment() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = FakeRemote::new(vms.path(), Behaviour::Hang);
    let f = fixture_in(dir, vec![call(json!({ "command": "sleep 100" })), done()], |b| {
        b.shell(ShellIsolation::Remote(remote(client.clone()))).mode(HarnessMode::Auto)
    })
    .await;
    let governance = f.harness.runner().governance().clone();
    let frozen_at = Arc::new(Mutex::new(None));
    let stamp = frozen_at.clone();
    let started = client.clone();
    tokio::spawn(async move {
        while started.provisioned() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        *stamp.lock().unwrap() = Some(Instant::now());
        governance.freeze("incident 88");
    });
    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        futures::StreamExt::collect::<Vec<_>>(stream),
    )
    .await
    .unwrap();
    let frozen_at = frozen_at.lock().unwrap().expect("the command started");
    assert!(
        frozen_at.elapsed() < Duration::from_secs(1),
        "stopped {:?} after the freeze",
        frozen_at.elapsed()
    );
    assert_eq!((client.stopped(), client.live()), (1, 0), "the environment was destroyed");
    let end = f.records("harness.bash.end");
    assert_eq!(end[0].ext["killedByFreeze"], true);
    assert_eq!(end[0].ext["enforcer"], "remote");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_freeze_during_provisioning_ends_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = Arc::new(FakeRemote {
        provision_delay: Duration::from_secs(60),
        ..Arc::into_inner(FakeRemote::new(vms.path(), Behaviour::Shell)).unwrap()
    });
    let f = fixture_in(dir, vec![call(json!({ "command": "true" })), done()], |b| {
        b.shell(ShellIsolation::Remote(remote(client.clone()))).mode(HarnessMode::Auto)
    })
    .await;
    let governance = f.harness.runner().governance().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        governance.freeze("incident 89");
    });
    let started = Instant::now();
    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(20),
        futures::StreamExt::collect::<Vec<_>>(stream),
    )
    .await
    .expect("the run must end");
    assert!(started.elapsed() < Duration::from_secs(10), "the freeze waited for provisioning");
    assert_eq!(client.provisioned(), 0);
}

#[tokio::test]
async fn a_timed_out_command_destroys_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = FakeRemote::new(vms.path(), Behaviour::Shell);
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "sleep 100", "timeout_secs": 1 })),
            done(),
            call(json!({ "command": "true" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Remote(remote(client.clone()))).mode(HarnessMode::Auto),
    )
    .await;
    let events = f.send("go").await;
    let response = response_of(&events, "bash");
    assert!(
        response["error"].as_str().unwrap_or_default().contains("harness.bash.timeout"),
        "{response}"
    );
    let end = f.records("harness.bash.end");
    assert_eq!(
        (end[0].ext["timedOut"].clone(), end[0].ext["enforcer"].clone()),
        (json!(true), json!("remote"))
    );
    assert_eq!((client.provisioned(), client.stopped(), client.live()), (1, 1, 0));
    f.send("again").await;
    assert_eq!(client.provisioned(), 2, "the next command got a fresh environment");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_run_destroys_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = FakeRemote::new(vms.path(), Behaviour::Hang);
    let f = fixture_in(dir, vec![call(json!({ "command": "sleep 100" })), done()], |b| {
        b.shell(ShellIsolation::Remote(remote(client.clone()))).mode(HarnessMode::Auto)
    })
    .await;
    let mut stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    // Read until the command is running, then drop the run.
    let reading = async {
        while futures::StreamExt::next(&mut stream).await.is_some() {
            if client.provisioned() > 0 {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), reading).await;
    while client.provisioned() == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(stream);
    for _ in 0..150 {
        if client.stopped() == 1 {
            assert_eq!(client.live(), 0);
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the cancelled run left its environment running");
}

#[tokio::test]
async fn the_run_wall_time_budget_caps_a_remote_command() {
    let dir = tempfile::tempdir().unwrap();
    let vms = tempfile::tempdir().unwrap();
    let client = FakeRemote::new(vms.path(), Behaviour::Shell);
    let f = fixture_in(
        dir,
        vec![call(json!({ "command": "sleep 30", "timeout_secs": 60 })), done()],
        |b| {
            b.shell(ShellIsolation::Remote(remote(client.clone())))
                .mode(HarnessMode::Auto)
                .budget(RunBudget::new().max_wall_time(Duration::from_secs(2)))
        },
    )
    .await;
    let started = Instant::now();
    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let results = tokio::time::timeout(
        Duration::from_secs(20),
        futures::StreamExt::collect::<Vec<_>>(stream),
    )
    .await
    .expect("the run must finish");
    assert!(started.elapsed() < Duration::from_secs(10), "the budget did not cap the command");
    // The command is cut at the budget, and the spent budget then ends the run.
    let events: Vec<_> =
        results.iter().filter_map(|result| result.as_ref().ok().cloned()).collect();
    let response = response_of(&events, "bash");
    assert!(
        response["error"].as_str().unwrap_or_default().contains("harness.bash.timeout"),
        "{response}"
    );
    assert_eq!(client.live(), 0);
}

#[test]
fn auto_allow_needs_an_off_host_environment_without_network() {
    let vms = tempfile::tempdir().unwrap();
    let build = |client: Arc<dyn SandboxClient>| {
        adk_harness::HarnessAgent::builder("harness")
            .model(Arc::new(adk_model::ScriptedLlm::new(
                "scripted",
                Vec::<ScriptedResponse>::new(),
            )))
            .workspace(adk_devtools::Workspace::new(vms.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(ShellIsolation::Remote(RemoteShellConfig::new(client)))
            .auto_allow_sandboxed()
            .build()
    };
    let open = build(FakeRemote::with_network(vms.path(), Behaviour::Shell, false)).unwrap_err();
    assert_eq!(open.code, AUTO_ALLOW_UNAVAILABLE_CODE);
    assert!(open.message.contains("can reach the network"), "{}", open.message);
    let local = build(Arc::new(LocalUnixClient::new(None, vms.path().join("snap")))).unwrap_err();
    assert!(local.message.contains("runs commands on this host"), "{}", local.message);
    assert!(build(FakeRemote::new(vms.path(), Behaviour::Shell)).is_ok());
}

/// The real process-backed client through the remote shell: it reports itself on-host, so
/// the harness governs it as the host shell, and commands run in its own workspace.
#[tokio::test]
async fn a_local_client_runs_through_the_remote_shell_and_is_governed_as_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let client =
        Arc::new(LocalUnixClient::new(Some(base.path().join("ws")), base.path().join("snap")));
    let config = RemoteShellConfig::new(client.clone()).with_manifest(Manifest::new(vec![
        ManifestEntry::File { path: "data.csv".into(), content: b"a,b\n1,2\n3,4\n".to_vec() },
    ]));
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "tail -n +2 data.csv | cut -d, -f2 | paste -sd+ - | bc" })),
            done(),
        ],
        |b| {
            b.shell(ShellIsolation::Remote(config))
                .mode(HarnessMode::Auto)
                .allow_host_shell_in_auto()
        },
    )
    .await;
    let status = f.harness.agent().info().isolation.clone();
    assert_eq!(status, IsolationStatus::Remote { provider: "local".into(), off_host: false });
    assert!(status.behaves_as_host());
    let events = f.send("sum column b").await;
    let response = response_of(&events, "bash");
    assert_eq!(response["stdout"], "6\n", "{response}");
    assert_eq!(f.records("harness.bash.begin")[0].ext["provider"], "local");
    f.harness.close().await;
    assert!(client.sessions.read().await.is_empty(), "close released the environment");
}

// ── Live ─────────────────────────────────────────────────────────────────────────────────
//
// Both runs read `ANTHROPIC_API_KEY` from the workspace `.env` and take the model from
// `ADK_LIVE_MODEL` (default `claude-haiku-5-5`):
//
//   ADK_LIVE_MODEL=claude-sonnet-5-5 cargo nextest run -p adk-harness --features remote-shell \
//     --run-ignored only -E 'test(/remote_shell_tests::live_/)'

fn live_model() -> (String, Arc<dyn adk_core::Llm>) {
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path_override(&env);
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let name = std::env::var("ADK_LIVE_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".to_string());
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new(key, &name),
    )
    .unwrap();
    (name, Arc::new(model))
}

fn live_e2b_client() -> adk_sandbox::workspace::remote::E2bClient {
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path_override(&env);
    let config = adk_sandbox::workspace::remote::E2bConfig::from_env()
        .expect("E2B_API_KEY")
        .with_timeout(Duration::from_secs(300));
    adk_sandbox::workspace::remote::E2bClient::new(config).unwrap()
}

fn live_daytona_client() -> adk_sandbox::workspace::remote::DaytonaClient {
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path_override(&env);
    let config = adk_sandbox::workspace::remote::DaytonaConfig::from_env()
        .expect("DAYTONA_API_KEY")
        .with_ttl(Duration::from_secs(600));
    adk_sandbox::workspace::remote::DaytonaClient::new(config).unwrap()
}

fn live_modal_client() -> adk_sandbox::workspace::remote::ModalClient {
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path_override(&env);
    let config = adk_sandbox::workspace::remote::ModalConfig::from_env()
        .expect("MODAL_TOKEN_ID and MODAL_TOKEN_SECRET")
        .with_timeout(Duration::from_secs(600));
    adk_sandbox::workspace::remote::ModalClient::new(config).unwrap()
}

const SALES: &[u8] = b"region,amount\nnorth,1200.50\nsouth,830.25\neast,410.00\nwest,1559.25\n";

/// The harness shell through `ShellIsolation::Remote` over the process-backed client: the
/// model runs bash in the client's environment, which the manifest seeded with a file the
/// host workspace does not have.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY in the workspace .env"]
async fn live_remote_shell_runs_the_models_commands_in_the_environment() {
    let base = tempfile::tempdir().unwrap();
    let client =
        Arc::new(LocalUnixClient::new(Some(base.path().join("ws")), base.path().join("snap")));
    live_remote_shell_sums_the_seeded_file(client.clone(), "local").await;
    assert!(client.sessions.read().await.is_empty(), "close released the environment");
}

/// The same run in an E2B microVM.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY and E2B_API_KEY in the workspace .env"]
async fn live_remote_shell_runs_the_models_commands_in_an_e2b_microvm() {
    let client = Arc::new(live_e2b_client());
    live_remote_shell_sums_the_seeded_file(client, "e2b").await;
}

/// The same run in a Daytona sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY and DAYTONA_API_KEY in the workspace .env"]
async fn live_remote_shell_runs_the_models_commands_in_a_daytona_sandbox() {
    let client = Arc::new(live_daytona_client());
    live_remote_shell_sums_the_seeded_file(client, "daytona").await;
}

/// The same run in a Modal Sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY, MODAL_TOKEN_ID and MODAL_TOKEN_SECRET in the workspace .env"]
async fn live_remote_shell_runs_the_models_commands_in_a_modal_sandbox() {
    let client = Arc::new(live_modal_client());
    live_remote_shell_sums_the_seeded_file(client, "modal").await;
}

async fn live_remote_shell_sums_the_seeded_file(client: Arc<dyn SandboxClient>, provider: &str) {
    let (name, model) = live_model();
    let dir = tempfile::tempdir().unwrap();
    let config =
        RemoteShellConfig::new(client).with_manifest(Manifest::new(vec![ManifestEntry::File {
            path: "sales.csv".into(),
            content: SALES.to_vec(),
        }]));
    let agent = Arc::new(
        adk_harness::HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .without_compaction()
            .shell(ShellIsolation::Remote(config))
            .mode(HarnessMode::Auto)
            .allow_host_shell_in_auto()
            .budget(RunBudget::new().max_wall_time(Duration::from_secs(180)).max_model_calls(12))
            .build()
            .unwrap(),
    );
    let audit = adk_core::audit_log::InMemoryAuditRecorder::new();
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            adk_runner::Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness = adk_harness::Harness::with_runner(agent, runner).unwrap();
    let alice = common::user("alice");
    let session = harness.open_session(&alice, None).await.unwrap();
    let events = common::collect(
        harness
            .send(
                &alice,
                &session,
                "There is a file sales.csv in your shell's working directory. Use bash to add up \
                 the amount column and tell me the total.",
            )
            .await
            .unwrap(),
    )
    .await;
    let reply: String = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let begins: Vec<_> =
        audit.records().into_iter().filter(|r| r.action == "harness.bash.begin").collect();
    println!("model: {name}");
    println!("bash calls: {}", begins.len());
    for record in &begins {
        println!(
            "  enforcer={} provider={} environment={}",
            record.ext["enforcer"], record.ext["provider"], record.ext["environment"]
        );
    }
    println!("reply: {reply}");
    assert!(!begins.is_empty(), "the model never ran bash");
    assert!(begins.iter().all(|r| r.ext["enforcer"] == "remote" && r.ext["provider"] == provider));
    assert!(reply.replace(',', "").contains("4000"), "the total is 4000.00: {reply}");
    assert!(!dir.path().join("sales.csv").exists(), "the file lives only in the environment");
    harness.close().await;
}

/// An `LlmAgent` with `SandboxTool` over `SessionBackend`: the call goes through the governed
/// tool path and the code runs in the client's environment.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY in the workspace .env"]
async fn live_sandbox_tool_runs_code_through_a_session_backend() {
    let base = tempfile::tempdir().unwrap();
    let client =
        Arc::new(LocalUnixClient::new(Some(base.path().join("ws")), base.path().join("snap")));
    live_sandbox_tool_sums_squares(client.clone()).await;
    assert!(client.sessions.read().await.is_empty());
}

/// The same run in an E2B microVM.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY and E2B_API_KEY in the workspace .env"]
async fn live_sandbox_tool_runs_code_in_an_e2b_microvm() {
    live_sandbox_tool_sums_squares(Arc::new(live_e2b_client())).await;
}

/// The same run in a Daytona sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY and DAYTONA_API_KEY in the workspace .env"]
async fn live_sandbox_tool_runs_code_in_a_daytona_sandbox() {
    live_sandbox_tool_sums_squares(Arc::new(live_daytona_client())).await;
}

/// The same run in a Modal Sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs ANTHROPIC_API_KEY, MODAL_TOKEN_ID and MODAL_TOKEN_SECRET in the workspace .env"]
async fn live_sandbox_tool_runs_code_in_a_modal_sandbox() {
    live_sandbox_tool_sums_squares(Arc::new(live_modal_client())).await;
}

async fn live_sandbox_tool_sums_squares(client: Arc<dyn SandboxClient>) {
    let (name, model) = live_model();
    let backend = Arc::new(adk_sandbox::workspace::SessionBackend::new(client));
    let agent = Arc::new(
        adk_agent::LlmAgentBuilder::new("coder")
            .model(model)
            .instruction("Run code with the sandbox_exec tool to answer; do not compute answers in your head.")
            .tool(Arc::new(adk_sandbox::SandboxTool::new(backend.clone())))
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let audit = adk_core::audit_log::InMemoryAuditRecorder::new();
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions.clone())
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let session = sessions
        .create(adk_session::CreateRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let events = common::collect(
        runner
            .run(
                common::user("alice"),
                adk_core::SessionId::new(session.id()).unwrap(),
                adk_core::Content::new("user").with_text(
                    "Using Python in the sandbox, what is the sum of the squares of the first 100 \
                     positive integers?",
                ),
            )
            .await
            .unwrap(),
    )
    .await;
    let responses = common::responses(&events);
    let reply: String = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    println!("model: {name}");
    println!("tool responses: {responses:?}");
    println!("reply: {reply}");
    assert!(responses.iter().any(|(tool, response)| tool == "sandbox_exec"
        && response["status"] == "success"
        && response["stdout"].as_str().unwrap_or_default().contains("338350")));
    assert!(reply.replace(',', "").contains("338350"), "{reply}");
    let handle = backend.handle().await.expect("the backend provisioned an environment");
    println!("environment: {}", handle.as_str());
    backend.close().await.unwrap();
}
