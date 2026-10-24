//! The harness shell under the Windows restricted-token sandbox (spec 3.2 §10 W1, W5; spec
//! 3.1 §7.9, §9.2 I20, I24).
//!
//! The sandbox is active when its probe runs a command under the restricted token, and the
//! shell then runs `sh -c` from the sandbox's `PATH` (Git for Windows on the CI runner) in a
//! Job Object. Toolchain reads are off, so no grant rewrites the runner's toolchain ACLs. The
//! tests run on the PR tier's `windows` job.

#![cfg(all(feature = "sandbox", feature = "runner", windows))]

mod common;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::Tool;
use adk_harness::{
    AUTO_ALLOW_UNAVAILABLE_CODE, HarnessBashTool, HarnessMode, InMemoryShellLedger,
    IsolationStatus, OsSandboxConfig, SANDBOX_UNAVAILABLE_CODE, SandboxLauncher, ShellIsolation,
};
use adk_model::ScriptedResponse;
use adk_sandbox::SandboxError;
use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicy, WrappedCommand};
use common::{Outcome, fixture_in, outcome, response_of};
use serde_json::json;

/// The restricted-token sandbox with toolchain reads off.
fn sandbox() -> OsSandboxConfig {
    OsSandboxConfig { read_toolchains: false, ..OsSandboxConfig::default() }
}

/// An enforcer whose probe fails.
struct Failing;

impl SandboxEnforcer for Failing {
    fn name(&self) -> &str {
        "failing"
    }
    fn probe(&self) -> Result<(), SandboxError> {
        Err(SandboxError::EnforcerUnavailable {
            enforcer: "failing".into(),
            message: "probe refused for the test".into(),
        })
    }
    fn wrap_command(
        &self,
        _program: &OsStr,
        _args: &[OsString],
        _policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        Err(SandboxError::PolicyViolation("unavailable".into()))
    }
}

/// Approves every held call once.
#[derive(Debug)]
struct ApproveAll;

#[async_trait::async_trait]
impl adk_core::ToolConfirmationHandler for ApproveAll {
    async fn decide(
        &self,
        _request: &adk_core::ToolConfirmationRequest,
    ) -> adk_core::Result<adk_core::ToolConfirmationDecision> {
        Ok(adk_core::ToolConfirmationDecision::Approve)
    }
}

fn call(command: &str) -> ScriptedResponse {
    ScriptedResponse::function_call("bash", json!({ "command": command }))
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

/// `path` as `sh` writes it, with forward slashes.
fn sh_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// A directory only the current user can open, which the restricted token is not granted.
fn private_dir(base: &Path) -> PathBuf {
    let dir = base.join("outside");
    std::fs::create_dir_all(&dir).unwrap();
    let user = std::env::var("USERNAME").expect("USERNAME");
    let status = std::process::Command::new("icacls")
        .arg(&dir)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(OI)(CI)F"))
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls runs");
    assert!(status.success(), "icacls set the directory's ACL");
    dir
}

#[tokio::test]
async fn the_sandbox_is_active_and_declares_the_open_network() {
    let launcher = SandboxLauncher::new(sandbox());
    let status = launcher.status();
    assert_eq!(status, IsolationStatus::OsSandbox { enforcer: "restricted-token".into() });
    assert!(status.to_string().contains("network not restricted"), "{status}");

    let ws = adk_devtools::Workspace::new(".").allow_bash(true);
    let bash = HarnessBashTool::new(ws, status, true, Arc::new(InMemoryShellLedger::new()))
        .with_sandbox(launcher);
    let annotations = bash.annotations().unwrap();
    assert_eq!(annotations["enforcer"], "restricted-token");
    assert_eq!(annotations["network"], true, "the restricted token leaves the network open");
    assert!(bash.description().contains("network access"), "{}", bash.description());

    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let build = |config: OsSandboxConfig, auto: bool| {
        let mut builder = adk_harness::HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(ShellIsolation::OsSandbox(config))
            .fail_if_unavailable();
        if auto {
            builder = builder.auto_allow_sandboxed();
        }
        builder.build()
    };
    let active = build(sandbox(), false).expect("fail_if_unavailable accepts an active sandbox");
    assert_eq!(
        active.info().isolation,
        IsolationStatus::OsSandbox { enforcer: "restricted-token".into() }
    );
    let refused = build(sandbox(), true).unwrap_err();
    assert_eq!(refused.code, AUTO_ALLOW_UNAVAILABLE_CODE);
    assert!(refused.message.contains("network"), "{}", refused.message);
}

#[tokio::test]
async fn an_unavailable_sandbox_is_degraded_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let failing =
        OsSandboxConfig { enforcer: Some(Arc::new(Failing)), ..OsSandboxConfig::default() };
    let build = |strict: bool| {
        let mut builder = adk_harness::HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(ShellIsolation::OsSandbox(failing.clone()));
        if strict {
            builder = builder.fail_if_unavailable();
        }
        builder.build()
    };
    let degraded = build(false).unwrap();
    assert!(
        matches!(degraded.info().isolation, IsolationStatus::Degraded { .. }),
        "{:?}",
        degraded.info().isolation
    );
    assert_eq!(build(true).unwrap_err().code, SANDBOX_UNAVAILABLE_CODE);
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_bash_writes_the_workspace_but_not_outside_it() {
    let outside_base = tempfile::tempdir().unwrap();
    let outside = private_dir(outside_base.path());
    let target = outside.join("escaped.txt");
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let command =
        format!("echo inside > inside.txt; echo out > '{}'; echo after", sh_path(&target));
    let f = fixture_in(tempfile::tempdir().unwrap(), vec![call(&command), done()], |b| {
        b.shell(ShellIsolation::OsSandbox(sandbox())).prompter(prompter.clone())
    })
    .await;

    let events = f.send("go").await;

    let response = response_of(&events, "bash");
    assert_eq!(
        std::fs::read_to_string(f.path("inside.txt")).unwrap_or_default().trim(),
        "inside",
        "{response}"
    );
    assert!(!target.exists(), "the sandbox let a write out of the workspace: {response}");
    assert!(response["stdout"].as_str().unwrap_or_default().contains("after"), "{response}");
    assert_eq!(prompter.count(), 1, "a sandboxed command is asked in ask mode");
    assert_eq!(prompter.prompts.lock().unwrap()[0].isolation.enforcer(), "restricted-token");
    let begin = f.records("harness.bash.begin");
    assert_eq!(begin.len(), 1);
    assert_eq!(begin[0].ext["enforcer"], "restricted-token");
    assert_eq!(begin[0].ext["network"], true);
    assert_eq!(begin[0].ext["workspaceAccess"], "read_write");
}

#[tokio::test]
async fn plan_mode_does_not_run_sandboxed_bash_unprompted() {
    let prompter = common::ScriptedPrompter::new();
    let f =
        fixture_in(tempfile::tempdir().unwrap(), vec![call("echo hi > ran.txt"), done()], |b| {
            b.shell(ShellIsolation::OsSandbox(sandbox()))
                .mode(HarnessMode::Plan)
                .prompter(prompter.clone())
        })
        .await;

    let events = f.send("go").await;

    // The restricted token's grants stay in the workspace's ACL, so it cannot promise a
    // read-only workspace: Plan mode denies the shell instead.
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert_eq!(prompter.count(), 0);
    assert!(!f.path("ran.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn no_process_started_by_a_call_outlives_it() {
    let f = fixture_in(
        tempfile::tempdir().unwrap(),
        vec![call("(sleep 3; echo late > late.txt) & echo started"), done()],
        |b| {
            b.shell(ShellIsolation::OsSandbox(sandbox()))
                .mode(HarnessMode::Auto)
                .confirmation_handler(Arc::new(ApproveAll))
        },
    )
    .await;

    let started = Instant::now();
    let events = f.send("go").await;

    let response = response_of(&events, "bash");
    assert!(response["stdout"].as_str().unwrap_or_default().contains("started"), "{response}");
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!f.path("late.txt").exists(), "a background process outlived the call");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_freeze_kills_a_sandboxed_command() {
    let f = fixture_in(
        tempfile::tempdir().unwrap(),
        vec![call("echo started > started.txt; sleep 31877; echo late > late.txt"), done()],
        |b| {
            b.shell(ShellIsolation::OsSandbox(sandbox()))
                .mode(HarnessMode::Auto)
                .confirmation_handler(Arc::new(ApproveAll))
        },
    )
    .await;
    let governance = f.harness.runner().governance().clone();
    let marker = f.path("started.txt");
    let frozen_at = Arc::new(std::sync::Mutex::new(None));
    let stamp = frozen_at.clone();
    tokio::spawn(async move {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        *stamp.lock().unwrap() = Some(Instant::now());
        governance.freeze("incident");
    });

    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(60),
        futures::StreamExt::collect::<Vec<_>>(stream),
    )
    .await
    .expect("the frozen run ends");

    let frozen_at = frozen_at.lock().unwrap().expect("the command started");
    assert!(frozen_at.elapsed() < Duration::from_secs(10), "{:?}", frozen_at.elapsed());
    let end = f.records("harness.bash.end");
    assert_eq!(end.len(), 1);
    assert_eq!(end[0].ext["killedByFreeze"], true);
    assert_eq!(end[0].ext["enforcer"], "restricted-token");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!f.path("late.txt").exists());
}
