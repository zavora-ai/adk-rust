//! The harness shell inside a container (spec 3.1 §7.9 "Container"; §9.2 I20, I24, I26, I29).
//!
//! Most tests run the container lifecycle over a fake `SandboxClient` that executes on the
//! host, so they need no container runtime. The tests that need a Docker daemon and the
//! `alpine:3.20` image are ignored by default:
//!
//! ```bash
//! cargo nextest run -p adk-harness --features docker --run-ignored only --test integration \
//!   -E 'test(/^container_shell_tests::/)'
//! ```

#![cfg(all(feature = "docker", feature = "runner", unix))]

use crate::common;

use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditRecordOutcome, InMemoryAuditRecorder};
use adk_core::{Content, FinishReason, LlmResponse, Part, Tool, ToolOrigin};
use adk_harness::{
    AUTO_ALLOW_UNAVAILABLE_CODE, ContainerBashTool, ContainerConfig, Harness, HarnessAgent,
    HarnessMode, InMemoryShellLedger, IsolationStatus, SANDBOX_PROTECTED_PATHS,
    SANDBOX_UNAVAILABLE_CODE, ShellIsolation, ShellLedger,
};
use adk_model::ScriptedResponse;
use adk_runner::Runner;
use adk_sandbox::SandboxError;
use adk_sandbox::workspace::{
    DirEntry, DockerClient, ExecOutput, Manifest, SandboxClient, SandboxSession, SessionHandle,
    SnapshotId,
};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use common::{APP, Outcome, fixture_in, outcome, response_of, responses, session_id};
use futures::FutureExt;
use serde_json::{Value, json};

// ----- A container runtime that runs on the host -----

/// What the fake's sessions do with a command.
#[derive(Clone, Copy)]
enum Behaviour {
    /// `sh -c` in the workspace root, as a container with the workspace mounted would.
    Shell,
    /// Never finishes.
    Hang,
    /// Succeeds with this many bytes of stdout.
    Output(usize),
}

/// A `SandboxClient` whose containers are sessions running on the host.
struct FakeContainers {
    root: PathBuf,
    behaviour: Behaviour,
    fail_provision: bool,
    provisioned: AtomicUsize,
    stopped: AtomicUsize,
    live: Mutex<HashSet<String>>,
}

impl FakeContainers {
    fn new(root: &Path, behaviour: Behaviour) -> Arc<Self> {
        Self::build(root, behaviour, false)
    }

    /// A runtime whose `provision` fails, as a daemon that went away does.
    fn failing(root: &Path) -> Arc<Self> {
        Self::build(root, Behaviour::Shell, true)
    }

    fn build(root: &Path, behaviour: Behaviour, fail_provision: bool) -> Arc<Self> {
        Arc::new(Self {
            root: root.to_path_buf(),
            behaviour,
            fail_provision,
            provisioned: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            live: Mutex::new(HashSet::new()),
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
impl SandboxClient for FakeContainers {
    async fn provision(&self, _: &Manifest) -> Result<SessionHandle, SandboxError> {
        if self.fail_provision {
            return Err(SandboxError::ProvisionFailed {
                resource: "fake:1".into(),
                reason: "the fake runtime is down".into(),
                suggestion: "none".into(),
            });
        }
        let n = self.provisioned.fetch_add(1, Ordering::SeqCst) + 1;
        let handle = SessionHandle::new(format!("fake-{n}"));
        self.live.lock().unwrap().insert(handle.as_str().to_string());
        Ok(handle)
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        if !self.live.lock().unwrap().contains(handle.as_str()) {
            return Err(SandboxError::SessionNotFound { handle: handle.as_str().to_string() });
        }
        Ok(Box::new(FakeSession { root: self.root.clone(), behaviour: self.behaviour }))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        if !self.live.lock().unwrap().remove(handle.as_str()) {
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
}

struct FakeSession {
    root: PathBuf,
    behaviour: Behaviour,
}

#[async_trait]
impl SandboxSession for FakeSession {
    async fn exec_command(
        &self,
        command: &str,
        _working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        let started = Instant::now();
        match self.behaviour {
            Behaviour::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(ExecOutput::new("", "", 0, started.elapsed(), false))
            }
            Behaviour::Output(bytes) => {
                Ok(ExecOutput::new("x".repeat(bytes), "", 0, started.elapsed(), false))
            }
            Behaviour::Shell => {
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(&self.root)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .await
                    .map_err(|error| SandboxError::ExecutionFailed(error.to_string()))?;
                Ok(ExecOutput::new(
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                    output.status.code().unwrap_or(-1),
                    started.elapsed(),
                    false,
                ))
            }
        }
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

fn container(client: Arc<FakeContainers>) -> ContainerConfig {
    ContainerConfig {
        image: "fake:1".into(),
        client: Some(client),
        idle_ttl: Duration::from_secs(600),
        ..ContainerConfig::default()
    }
}

fn call(args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call("bash", args)
}

fn call_with_id(id: &str, args: Value) -> ScriptedResponse {
    let mut response = LlmResponse::new(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "bash".into(),
            args,
            id: Some(id.into()),
            thought_signature: None,
        }],
    });
    response.finish_reason = Some(FinishReason::Stop);
    response.turn_complete = true;
    ScriptedResponse::response(response)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

/// Creates every root-level protected path and makes it read-only on the host, which is
/// what a read-only mount does inside a container.
fn protect_workspace(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for name in SANDBOX_PROTECTED_PATHS {
        let path = root.join(name);
        if [".git", ".adk", ".skills", ".claude", ".cargo"].contains(name) {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).unwrap();
        } else {
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
    }
}

fn unprotect_workspace(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for name in SANDBOX_PROTECTED_PATHS {
        let path = root.join(name);
        if path.is_dir() {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        } else if path.exists() {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
    }
}

// ----- Declaration and status (I9) -----

#[tokio::test]
async fn the_container_shell_declares_itself_and_binds_rules_to_its_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let f =
        fixture_in(dir, vec![done()], |b| b.shell(ShellIsolation::Container(container(client))))
            .await;
    assert_eq!(
        f.harness.agent().info().isolation,
        IsolationStatus::Container { image: "fake:1".into() }
    );
    assert!(f.harness.agent().info().degraded_reason.is_none());

    let ws = adk_devtools::Workspace::new(".").allow_bash(true);
    let ledger: Arc<dyn ShellLedger> = Arc::new(InMemoryShellLedger::new());
    let tool = |config: ContainerConfig| ContainerBashTool::new(ws.clone(), config, ledger.clone());
    let closed = tool(ContainerConfig::default());
    let open = tool(ContainerConfig { network: true, ..ContainerConfig::default() });
    let other_image =
        tool(ContainerConfig { image: "alpine:3.20".into(), ..ContainerConfig::default() });
    assert_eq!(
        closed.annotations().unwrap(),
        &json!({
            "enforcer": "container",
            "profileDigest": ContainerConfig::default().profile_digest(),
            "network": false,
            "reads": "confined",
            "msys2Isolated": true,
        })
    );
    assert_eq!(open.annotations().unwrap()["network"], true);
    let digests = [
        ToolOrigin::of(&closed).declaration_digest,
        ToolOrigin::of(&open).declaration_digest,
        ToolOrigin::of(&other_image).declaration_digest,
        ToolOrigin::of(&adk_harness::HarnessBashTool::new(
            ws.clone(),
            IsolationStatus::Host,
            false,
            ledger.clone(),
        ))
        .declaration_digest,
    ];
    for (i, a) in digests.iter().enumerate() {
        for b in &digests[i + 1..] {
            assert_ne!(a, b, "the declaration digest must differ per configuration");
        }
    }
    assert_eq!(
        ToolOrigin::of(&closed).declaration_digest,
        ToolOrigin::of(&tool(ContainerConfig::default())).declaration_digest
    );

    // The retry arguments are declared only when unsandboxed retries are on, which they are
    // not by default for a container.
    let schema = closed.parameters_schema().unwrap();
    for key in ["unsandboxed", "reason", "retry_of"] {
        assert!(schema["properties"].get(key).is_none(), "{key}");
    }
    let with_retry = tool(ContainerConfig::default()).allow_unsandboxed_retry(true);
    assert!(with_retry.parameters_schema().unwrap()["properties"].get("retry_of").is_some());
    assert!(with_retry.description().contains("unsandboxed: true"));
    assert!(!closed.description().contains("unsandboxed"));
}

// ----- Lifecycle: one container per (app, user, session), kept across runs -----

#[tokio::test]
async fn one_container_per_session_is_kept_across_runs() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let f = fixture_in(
        dir,
        vec![
            call_with_id("call-a", json!({ "command": "echo one > one.txt" })),
            done(),
            call(json!({ "command": "echo two >> one.txt" })),
            done(),
            call(json!({ "command": "echo other > other.txt" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(container(client.clone()))).mode(HarnessMode::Auto),
    )
    .await;
    let events = f.send("first").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed, "{events:#?}");
    f.send("second").await;
    assert_eq!(std::fs::read_to_string(f.path("one.txt")).unwrap(), "one\ntwo\n");
    assert_eq!(client.provisioned(), 1, "one container serves every run of the session");

    let other = f.harness.open_session(&f.user, Some(&session_id("s2"))).await.unwrap();
    f.send_as(&f.user, &other, "third").await;
    assert!(f.path("other.txt").exists());
    assert_eq!(client.provisioned(), 2, "another session gets its own container");
    assert_eq!(client.stopped(), 0);

    let begin = f.records("harness.bash.begin");
    assert_eq!(begin.len(), 3);
    assert!(begin.iter().all(|record| record.ext["enforcer"] == "container"));
    assert_eq!(begin[0].ext["workspaceAccess"], "read_write");
    assert_eq!(begin[0].ext["network"], false);
    assert!(begin[0].ext["profileDigest"].as_str().unwrap().starts_with("sha256:"));
    let end = f.records("harness.bash.end");
    assert_eq!(end[0].outcome, Some(AuditRecordOutcome::Succeeded));
    assert_eq!(end[0].ext["exitCode"], 0);
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
    assert_eq!(entry.enforcer.as_deref(), Some("container"));
    assert_eq!(entry.exit_code, Some(0));
}

#[tokio::test]
async fn an_idle_container_is_stopped_and_the_next_command_gets_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let config =
        ContainerConfig { idle_ttl: Duration::from_millis(200), ..container(client.clone()) };
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "true" })),
            done(),
            call(json!({ "command": "true" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(config)).mode(HarnessMode::Auto),
    )
    .await;
    f.send("first").await;
    assert_eq!((client.provisioned(), client.stopped()), (1, 0));
    let mut stopped = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if client.stopped() == 1 {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "the idle container was not stopped after its TTL");
    assert_eq!(client.live(), 0);
    f.send("second").await;
    assert_eq!(client.provisioned(), 2, "the next command provisioned a new container");
}

#[tokio::test]
async fn close_stops_every_session_container() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "true" })),
            done(),
            call(json!({ "command": "true" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(container(client.clone()))).mode(HarnessMode::Auto),
    )
    .await;
    f.send("one").await;
    let other = f.harness.open_session(&f.user, Some(&session_id("s2"))).await.unwrap();
    f.send_as(&f.user, &other, "two").await;
    assert_eq!((client.provisioned(), client.live()), (2, 2));
    f.harness.close().await;
    assert_eq!((client.stopped(), client.live()), (2, 0));
    // Closing twice is harmless.
    f.harness.close().await;
    assert_eq!(client.stopped(), 2);
}

// ----- The container row of the mode table (spec 3.1 §7.4) -----

#[tokio::test]
async fn the_container_row_denies_read_only_and_plan_holds_ask_and_allows_auto() {
    for (mode, expected) in [
        (HarnessMode::ReadOnly, Outcome::Denied),
        (HarnessMode::Plan, Outcome::Denied),
        (HarnessMode::Ask, Outcome::Held),
        (HarnessMode::Auto, Outcome::Executed),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let client = FakeContainers::new(dir.path(), Behaviour::Shell);
        let f = fixture_in(dir, vec![call(json!({ "command": "touch ran.txt" })), done()], |b| {
            b.shell(ShellIsolation::Container(container(client.clone()))).mode(mode)
        })
        .await;
        let events = f.send("go").await;
        assert_eq!(outcome(&response_of(&events, "bash")), expected, "{mode}");
        assert_eq!(f.path("ran.txt").exists(), expected == Outcome::Executed, "{mode}");
        assert_eq!(client.provisioned(), usize::from(expected == Outcome::Executed), "{mode}");
    }
}

// ----- auto_allow_sandboxed (I26) -----

#[tokio::test]
async fn auto_allow_sandboxed_runs_container_commands_in_ask_without_a_prompt() {
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let build = |dir: &Path, config: ContainerConfig| {
        HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(ShellIsolation::Container(config))
            .auto_allow_sandboxed()
            .build()
    };
    let empty = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(empty.path(), Behaviour::Shell);
    let error = build(empty.path(), container(client.clone())).unwrap_err();
    assert_eq!(error.code, AUTO_ALLOW_UNAVAILABLE_CODE);
    assert!(error.message.contains(".envrc (does not exist)"), "{}", error.message);

    let protected = tempfile::tempdir().unwrap();
    protect_workspace(protected.path());
    let client = FakeContainers::new(protected.path(), Behaviour::Shell);
    let error =
        build(protected.path(), ContainerConfig { network: true, ..container(client.clone()) })
            .unwrap_err();
    assert_eq!(error.code, AUTO_ALLOW_UNAVAILABLE_CODE);
    assert!(error.message.contains("allows the network"), "{}", error.message);

    let prompter = common::ScriptedPrompter::new();
    let f = fixture_in(
        protected,
        vec![
            call(json!({ "command": "echo one > one.txt" })),
            done(),
            call(json!({ "command": "echo two > two.txt" })),
            done(),
        ],
        |b| {
            b.shell(ShellIsolation::Container(container(client.clone())))
                .auto_allow_sandboxed()
                .prompter(prompter.clone())
        },
    )
    .await;
    f.send("first").await;
    assert!(f.path("one.txt").exists(), "the first command ran without a prompt");
    assert_eq!(prompter.count(), 0);
    assert_eq!(f.records("harness.bash.begin")[0].ext["enforcer"], "container");
    // With a protected path gone, the next command needs approval again.
    std::fs::remove_file(f.path(".envrc")).unwrap();
    let events = f.send("second").await;
    assert_eq!(prompter.count(), 1, "the second command was prompted once .envrc was gone");
    assert!(!f.path("two.txt").exists());
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    unprotect_workspace(f.dir.path());
}

/// While a container command runs unapproved, a host file tool in another session waits for
/// it rather than reading the workspace mid-command (I26).
#[tokio::test(flavor = "multi_thread")]
async fn no_host_file_tool_runs_while_an_auto_allowed_command_does() {
    let dir = tempfile::tempdir().unwrap();
    protect_workspace(dir.path());
    std::fs::write(dir.path().join("probe.txt"), "before\n").unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "touch started && sleep 0.8 && echo after > probe.txt" })),
            ScriptedResponse::function_call("read_file", json!({ "path": "probe.txt" })),
            done(),
            done(),
        ],
        |b| {
            b.shell(ShellIsolation::Container(container(client.clone())))
                .auto_allow_sandboxed()
                .prompter(common::ScriptedPrompter::new())
        },
    )
    .await;
    let other = f.harness.open_session(&f.user, Some(&session_id("s2"))).await.unwrap();
    let started = f.path("started");
    let command = common::collect(f.harness.send(&f.user, &f.session, "run it").await.unwrap());
    let read = async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        common::collect(f.harness.send(&f.user, &other, "read it").await.unwrap()).await
    };
    let (_, read) = tokio::join!(command, read);
    let response = response_of(&read, "read_file");
    assert!(response.to_string().contains("after"), "read mid-command: {response}");
    unprotect_workspace(f.dir.path());
}

/// A container whose protected mounts admit a write never runs a command (I24, I29).
#[tokio::test]
async fn a_container_that_lets_a_protected_path_be_written_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/config"), "config\n").unwrap();
    // The fake runs on the host, where nothing keeps `.git` read-only.
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let f = fixture_in(dir, vec![call(json!({ "command": "touch ran.txt" })), done()], |b| {
        b.shell(ShellIsolation::Container(container(client.clone()))).mode(HarnessMode::Auto)
    })
    .await;
    let events = f.send("go").await;
    let response = response_of(&events, "bash");
    let error = response["error"].as_str().unwrap_or_default();
    assert!(error.contains("harness.sandbox_unavailable"), "{response}");
    assert!(!f.path("ran.txt").exists(), "the command ran in an unprotected container");
    assert!(!f.path(".git/.adk-harness-probe").exists(), "the probe cleaned up after itself");
    assert_eq!((client.provisioned(), client.stopped(), client.live()), (1, 1, 0));
    let unavailable = f.records("harness.sandbox.unavailable");
    assert_eq!(unavailable.len(), 1);
    let reason = unavailable[0].ext["error"].as_str().unwrap_or_default();
    assert!(reason.contains("protected path .git"), "{reason}");
    assert!(f.records("harness.bash.begin").is_empty());
}

// ----- Availability (I24) -----

#[tokio::test]
async fn a_runtime_that_cannot_provision_refuses_the_call_rather_than_running_on_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::failing(dir.path());
    let f = fixture_in(dir, vec![call(json!({ "command": "touch ran.txt" })), done()], |b| {
        b.shell(ShellIsolation::Container(container(client.clone()))).mode(HarnessMode::Auto)
    })
    .await;
    let events = f.send("go").await;
    let response = response_of(&events, "bash");
    assert!(response["error"].as_str().unwrap_or_default().contains("unavailable"), "{response}");
    assert!(!f.path("ran.txt").exists());
    let unavailable = f.records("harness.sandbox.unavailable");
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0].ext["requested"], "container");
    assert!(f.records("harness.bash.begin").is_empty());
}

/// Without a client of its own, the harness probes the Docker daemon at build: degraded
/// where there is none, active where there is, and never the host shell.
#[tokio::test]
async fn an_unreachable_container_runtime_is_degraded_or_refused_at_build() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let build = |strict: bool| {
        let mut builder = HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(ShellIsolation::Container(ContainerConfig::default()));
        if strict {
            builder = builder.fail_if_unavailable();
        }
        builder.build()
    };
    match build(false).unwrap().info().isolation.clone() {
        IsolationStatus::Degraded { requested, reason } => {
            assert_eq!(requested, "container");
            assert!(reason.contains("Docker daemon"), "{reason}");
            let strict = build(true).unwrap_err();
            assert_eq!(strict.code, SANDBOX_UNAVAILABLE_CODE);
        }
        IsolationStatus::Container { image } => {
            assert_eq!(image, adk_harness::DEFAULT_CONTAINER_IMAGE);
            assert!(build(true).is_ok());
        }
        other => panic!("a requested container is never {other:?}"),
    }
}

// ----- Timeout, output cap and the kill switch (I20) -----

#[tokio::test]
async fn a_timed_out_command_stops_the_container() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Hang);
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "sleep 100", "timeout_secs": 1 })),
            done(),
            call(json!({ "command": "sleep 100", "timeout_secs": 1 })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(container(client.clone()))).mode(HarnessMode::Auto),
    )
    .await;
    let events = f.send("go").await;
    let response = response_of(&events, "bash");
    assert!(
        response["error"].as_str().unwrap_or_default().contains("harness.bash.timeout"),
        "{response}"
    );
    assert_eq!((client.provisioned(), client.stopped(), client.live()), (1, 1, 0));
    let end = f.records("harness.bash.end");
    assert_eq!(end[0].ext["timedOut"], true);
    assert_eq!(end[0].outcome, Some(AuditRecordOutcome::OutcomeUnknown));
    f.send("again").await;
    assert_eq!(client.provisioned(), 2, "the next command got a fresh container");
}

#[tokio::test]
/// A container command's output is cut as a host command's is once it finishes: its head
/// and tail inline, the whole of it kept for `shell_output` (spec 3.1 §7.33 fix 8).
async fn output_is_capped_after_the_command_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Output(100_000));
    let model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        vec![call(json!({ "command": "yes x | tr -d '\\n' | head -c 100000" })), done()],
    ));
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .without_compaction()
            .shell(ShellIsolation::Container(container(client)))
            .mode(HarnessMode::Auto)
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let audit = InMemoryAuditRecorder::new();
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let user = common::user("alice");
    let session = harness.open_session(&user, None).await.unwrap();
    let events = common::collect(harness.send(&user, &session, "go").await.unwrap()).await;
    let response = response_of(&events, "bash");
    let stdout = response["stdout"].as_str().unwrap();
    let id = response["output_id"].as_str().expect("the whole output is kept");
    let marker = format!(
        "\n…[70000 bytes left out; shell_output {{\"shell_id\": \"{id}\"}} reads the whole output]\n"
    );
    assert_eq!(stdout, format!("{}{marker}{}", "x".repeat(10_000), "x".repeat(20_000)));
    assert_eq!(response["truncated"], true);
    assert_eq!(response["exit_code"], 0);
    let end: Vec<_> =
        audit.records().into_iter().filter(|r| r.action == "harness.bash.end").collect();
    assert_eq!(end[0].ext["truncated"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_freeze_stops_the_container_within_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Hang);
    let f = fixture_in(dir, vec![call(json!({ "command": "sleep 100" })), done()], |b| {
        b.shell(ShellIsolation::Container(container(client.clone()))).mode(HarnessMode::Auto)
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
        governance.freeze("incident");
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
    assert_eq!((client.stopped(), client.live()), (1, 0), "the container was stopped");
    let end = f.records("harness.bash.end");
    assert_eq!(end[0].ext["killedByFreeze"], true);
    assert_eq!(end[0].outcome, Some(AuditRecordOutcome::OutcomeUnknown));
}

// ----- Unsandboxed retry (I25): off for containers unless turned on -----

#[tokio::test]
async fn unsandboxed_retries_are_off_for_containers_unless_turned_on() {
    let retry = |id: &str| {
        call_with_id(
            id,
            json!({ "command": "exit 3", "unsandboxed": true, "retry_of": "call-a", "reason": "needs the host" }),
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    prompter.approve_once();
    let f = fixture_in(
        dir,
        vec![call_with_id("call-a", json!({ "command": "exit 3" })), retry("call-b"), done()],
        |b| {
            b.shell(ShellIsolation::Container(container(client.clone()))).prompter(prompter.clone())
        },
    )
    .await;
    let events = f.send("go").await;
    let results: Vec<Value> =
        responses(&events).into_iter().filter(|(name, _)| name == "bash").map(|(_, r)| r).collect();
    assert_eq!(results.len(), 2, "{results:#?}");
    assert_eq!(results[0]["exit_code"], 3);
    assert_eq!(results[0]["sandbox"]["denialSuspected"], false);
    assert!(results[0]["sandbox"]["hint"].as_str().unwrap().contains("nothing in its output"));
    assert_eq!(outcome(&results[1]), Outcome::Denied);
    assert!(results[1]["error"].as_str().unwrap().contains("turned off"), "{}", results[1]);
    assert_eq!(prompter.count(), 1, "only the container command was put to the person");
    // The policy audits the refused request; nothing consumed the failed call.
    let retries = f.records("harness.bash.retry_unsandboxed");
    assert_eq!(retries.len(), 1, "{retries:?}");
    assert_eq!(
        (&retries[0].ext["source"], &retries[0].ext["decision"]),
        (&json!("policy"), &json!("deny"))
    );

    // Turned on, the retry is the OS sandbox's: held, approved once, run on the host.
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    prompter.approve_once();
    let f = fixture_in(
        dir,
        vec![
            call_with_id("call-a", json!({ "command": "exit 3" })),
            retry("call-b"),
            retry("call-c"),
            done(),
        ],
        |b| {
            b.shell(ShellIsolation::Container(container(client.clone())))
                .allow_unsandboxed_retry(true)
                .prompter(prompter.clone())
        },
    )
    .await;
    let events = f.send("go").await;
    let results: Vec<Value> =
        responses(&events).into_iter().filter(|(name, _)| name == "bash").map(|(_, r)| r).collect();
    assert_eq!(results.len(), 3, "{results:#?}");
    assert_eq!(results[1]["exit_code"], 3, "the retry ran on the host: {}", results[1]);
    assert_eq!(outcome(&results[2]), Outcome::Denied, "a retry is consumed by one call");
    assert_eq!(prompter.count(), 2);
    let begin = f.records("harness.bash.begin");
    assert_eq!(begin.len(), 2);
    assert_eq!(begin[0].ext["enforcer"], "container");
    assert_eq!(begin[1].ext["enforcer"], "host");
    assert_eq!(begin[1].ext["retryOf"], "call-a");
    let retries: Vec<(Value, Value)> = f
        .records("harness.bash.retry_unsandboxed")
        .into_iter()
        .map(|record| (record.ext["source"].clone(), record.ext["decision"].clone()))
        .collect();
    assert_eq!(
        retries,
        [
            (json!("policy"), json!("require_approval")),
            (json!("tool"), json!("consumed")),
            (json!("policy"), json!("deny")),
        ],
        "requested, consumed, and the replay refused"
    );
}

// ----- Goal checks (spec 3.1 §7.11, I24): never on the host in the container's place -----

#[tokio::test]
async fn a_goal_check_under_the_container_shell_never_runs_on_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let f = fixture_in(
        dir,
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "done.txt", "content": "x" }),
            ),
            done(),
        ],
        |b| {
            b.shell(ShellIsolation::Container(container(client)))
                .mode(HarnessMode::Auto)
                .without_default_members()
        },
    )
    .await;
    let spec = adk_harness::GoalSpec {
        goal: "create done.txt".into(),
        until: "touch ran-on-host.txt && test -f done.txt".into(),
        max_iterations: 1,
        check_timeout: Duration::from_secs(30),
    };
    common::collect(f.harness.start_goal(&f.user, &f.session, spec).await.unwrap()).await;
    assert!(f.path("done.txt").exists(), "the iteration ran");
    assert!(!f.path("ran-on-host.txt").exists(), "the goal check ran on the host");
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!(state.status, adk_harness::GoalStatus::Exhausted);
    assert_eq!(state.last_check, None);
    let begin = f.records("harness.bash.begin");
    assert_eq!(begin.len(), 1);
    assert_eq!(
        (&begin[0].ext["goalCheck"], &begin[0].ext["enforcer"]),
        (&json!(true), &json!("container"))
    );
}

/// Shell credentials (spec 3.1 §7.14) have no path into a container yet, so asking for them
/// fails the build instead of running commands without them.
#[cfg(feature = "auth")]
#[test]
fn shell_credentials_are_refused_under_the_container_shell() {
    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let error = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(common::no_user_file())
        .shell(ShellIsolation::Container(container(client)))
        .shell_credential(adk_harness::ShellCredential::new("https://github.com").unwrap())
        .build()
        .unwrap_err();
    assert!(error.message.contains("container shell"), "{}", error.message);
}

// ----- Hooks (I54): nothing runs on the host in the container's name -----

#[cfg(feature = "hooks")]
#[test]
fn inherit_hooks_are_refused_under_the_container_shell() {
    use adk_harness::hooks::{HOOK_CONFIG_CODE, HookEvent, HookIsolation, HookSet, HookSpec};

    let dir = tempfile::tempdir().unwrap();
    let client = FakeContainers::new(dir.path(), Behaviour::Shell);
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let build = |hooks: HookSet| {
        HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(ShellIsolation::Container(container(client.clone())))
            .hooks(hooks)
            .build()
    };
    let inherit = HookSet::new().with(HookSpec::new(HookEvent::PreTool, "true"));
    let error = build(inherit).unwrap_err();
    assert_eq!(error.code, HOOK_CONFIG_CODE);
    assert!(error.message.contains("container shell"), "{}", error.message);

    let host = HookSet::new()
        .with(HookSpec::new(HookEvent::PreTool, "true").isolation(HookIsolation::Host));
    assert!(build(host).is_ok(), "a hook that names the host runs there");
}

// ----- Under a Docker daemon -----

const IMAGE: &str = "alpine:3.20";

/// Runs `body`, then closes the harness even when `body` panicked, so a failing test never
/// leaves its session container on the daemon; the panic is raised again afterwards.
async fn closing(harness: &Harness, body: impl Future<Output = ()>) {
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    harness.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The host workspace, the container and the host file tools share one view (S4), the
/// protected paths are read-only inside, the network is off, and the container runs as
/// the host user.
#[tokio::test]
#[ignore = "needs a Docker daemon and the alpine:3.20 image"]
async fn the_container_shares_the_workspace_with_the_host_file_tools() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
    std::fs::write(root.join(".git/config"), "config\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "be careful\n").unwrap();
    std::fs::write(root.join("from-host.txt"), "written on the host\n").unwrap();
    let uid = std::fs::metadata(&root).unwrap().uid();
    let config = ContainerConfig { image: IMAGE.into(), ..ContainerConfig::default() };
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "cat from-host.txt; id -u; pwd" })),
            call(json!({ "command": "echo written in the container > from-container.txt" })),
            ScriptedResponse::function_call("read_file", json!({ "path": "from-container.txt" })),
            call(json!({ "command": "echo tampered > .git/config" })),
            call(json!({ "command": "echo '#!/bin/sh' > .git/hooks/pre-commit" })),
            call(json!({ "command": "wget -T 2 -q -O - http://1.1.1.1/" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(config)).mode(HarnessMode::Auto),
    )
    .await;
    closing(&f.harness, async {
        assert_eq!(
            f.harness.agent().info().isolation,
            IsolationStatus::Container { image: IMAGE.into() }
        );
        let events = f.send("go").await;
        let results: Vec<Value> = responses(&events)
            .into_iter()
            .filter(|(name, _)| name == "bash")
            .map(|(_, r)| r)
            .collect();
        assert_eq!(results.len(), 5, "{results:#?}");

        assert_eq!(results[0]["exit_code"], 0, "{}", results[0]);
        assert_eq!(results[0]["stdout"], format!("written on the host\n{uid}\n/workspace\n"));
        assert_eq!(results[1]["exit_code"], 0, "{}", results[1]);
        assert_eq!(
            std::fs::read_to_string(root.join("from-container.txt")).unwrap(),
            "written in the container\n",
            "the host sees the container's write"
        );
        let owned = std::fs::metadata(root.join("from-container.txt")).unwrap();
        assert_eq!(owned.uid(), uid, "the file belongs to the host user");
        let read = response_of(&events, "read_file");
        assert!(read.to_string().contains("written in the container"), "{read}");

        for result in [&results[2], &results[3]] {
            assert_ne!(result["exit_code"], 0, "{result}");
            assert_eq!(result["sandbox"]["enforcer"], "container");
            assert_eq!(result["sandbox"]["denialSuspected"], true, "{result}");
            assert!(
                result["stderr"].as_str().unwrap().contains("Read-only file system"),
                "{result}"
            );
            assert!(result["sandbox"]["hint"].as_str().unwrap().contains("protected paths"));
        }
        assert_eq!(std::fs::read_to_string(root.join(".git/config")).unwrap(), "config\n");
        assert!(
            !root.join(".git/hooks/pre-commit").exists(),
            "a git hook was planted from the container"
        );
        assert_ne!(results[4]["exit_code"], 0, "the network was reachable: {}", results[4]);

        let begin = f.records("harness.bash.begin");
        assert!(begin.iter().all(|record| {
            record.ext["enforcer"] == "container" && record.ext["network"] == false
        }));
    })
    .await;
}

/// A process a command leaves running in the session's container is ended when the command
/// exits, though the container stays up for the next command (I23).
#[tokio::test]
#[ignore = "needs a Docker daemon and the alpine:3.20 image"]
async fn no_process_a_container_command_starts_outlives_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = ContainerConfig { image: IMAGE.into(), ..ContainerConfig::default() };
    let count = "for p in /proc/[0-9]*/cmdline; do tr '\\000' ' ' < \"$p\"; echo; done | grep -c '^sleep 3199[12]' || true";
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "(sleep 31991 &) ; sleep 31992 & echo started" })),
            call(json!({ "command": count })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(config)).mode(HarnessMode::Auto),
    )
    .await;
    closing(&f.harness, async {
        let events = f.send("go").await;
        let results: Vec<Value> = responses(&events)
            .into_iter()
            .filter(|(name, _)| name == "bash")
            .map(|(_, r)| r)
            .collect();
        assert_eq!(results.len(), 2, "{results:#?}");
        assert_eq!(results[0]["stdout"], "started\n", "{}", results[0]);
        assert_eq!(
            results[1]["stdout"].as_str().unwrap().trim(),
            "0",
            "a background process survived: {}",
            results[1]
        );
    })
    .await;
}

/// A client given to the harness is used as it is: one container per session, kept across
/// runs, stopped by `Harness::close`. It runs its container as the workspace's owner, as the
/// harness's own client does: root without capabilities cannot write a host directory only
/// that owner may write.
struct Counting {
    inner: DockerClient,
    provisioned: AtomicUsize,
    stopped: AtomicUsize,
}

#[async_trait]
impl SandboxClient for Counting {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        self.provisioned.fetch_add(1, Ordering::SeqCst);
        self.inner.provision(manifest).await
    }
    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        self.inner.start(handle).await
    }
    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        self.stopped.fetch_add(1, Ordering::SeqCst);
        self.inner.stop(handle).await
    }
    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        self.inner.snapshot(handle).await
    }
    async fn resume(&self, snapshot: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        self.inner.resume(snapshot).await
    }
}

#[tokio::test]
#[ignore = "needs a Docker daemon and the alpine:3.20 image"]
async fn a_docker_container_is_kept_across_runs_and_stopped_on_close() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let owner = std::fs::metadata(&root).unwrap();
    let docker = DockerClient::with_image(IMAGE)
        .await
        .expect("a Docker daemon is running")
        .with_bind_mount(&root, "/workspace", false)
        .with_user(format!("{}:{}", owner.uid(), owner.gid()));
    let client = Arc::new(Counting {
        inner: docker,
        provisioned: AtomicUsize::new(0),
        stopped: AtomicUsize::new(0),
    });
    let config = ContainerConfig {
        image: IMAGE.into(),
        client: Some(client.clone()),
        ..ContainerConfig::default()
    };
    let f = fixture_in(
        dir,
        vec![
            call(json!({ "command": "echo $$ > pid-one.txt" })),
            done(),
            call(json!({ "command": "cat pid-one.txt > /dev/null && echo again > again.txt" })),
            done(),
        ],
        |b| b.shell(ShellIsolation::Container(config)).mode(HarnessMode::Auto),
    )
    .await;
    closing(&f.harness, async {
        let one = f.send("one").await;
        let two = f.send("two").await;
        let results: Vec<Value> = responses(&one)
            .into_iter()
            .chain(responses(&two))
            .filter(|(name, _)| name == "bash")
            .map(|(_, r)| r)
            .collect();
        assert_eq!(results.len(), 2, "{results:#?}");
        for result in &results {
            assert_eq!(result["exit_code"], 0, "{result}");
        }
        assert_eq!(std::fs::read_to_string(root.join("again.txt")).unwrap(), "again\n");
        assert_eq!(client.provisioned.load(Ordering::SeqCst), 1, "one container across two runs");
        assert_eq!(client.stopped.load(Ordering::SeqCst), 0);
    })
    .await;
    assert_eq!(client.stopped.load(Ordering::SeqCst), 1, "close stopped the container");
}
