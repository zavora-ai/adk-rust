//! `sandboxed_command` (spec 3.1 §10 S1) under the platform's real enforcer.
//!
//! A harness runs a shell in the user's workspace, so the command starts in the workspace
//! it is given rather than a scratch directory, sees only the policy's environment, and is
//! confined to the policy's paths and network setting. Each case runs a real process under
//! Seatbelt on macOS and bubblewrap on Linux, and skips when the enforcer cannot run on the
//! host (a sandboxed test runner, a missing `bwrap`, user namespaces disabled).

#![cfg(any(
    all(feature = "sandbox-macos", target_os = "macos"),
    all(feature = "sandbox-linux", target_os = "linux")
))]

use std::ffi::{OsStr, OsString};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;

use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicyBuilder, sandboxed_command};

#[cfg(target_os = "macos")]
fn enforcer() -> Option<Box<dyn SandboxEnforcer>> {
    let enforcer = adk_sandbox::sandbox::macos::MacOsEnforcer::new();
    match enforcer.probe() {
        Ok(()) => Some(Box::new(enforcer)),
        Err(error) => {
            eprintln!("seatbelt unavailable on this host, skipping: {error}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn enforcer() -> Option<Box<dyn SandboxEnforcer>> {
    let enforcer = adk_sandbox::sandbox::linux::LinuxEnforcer::new();
    match enforcer.probe() {
        Ok(()) => Some(Box::new(enforcer)),
        Err(error) => {
            eprintln!("bubblewrap unavailable on this host, skipping: {error}");
            None
        }
    }
}

/// What a shell needs to start. Seatbelt grants its own system runtime. bubblewrap starts
/// from an empty root, and on a merged-`/usr` host `/bin` and `/lib` resolve to `/usr/bin` and
/// `/usr/lib`, so binding them leaves the dynamic loader's `/lib` path missing; the root is
/// bound read-only instead.
fn system_policy() -> SandboxPolicyBuilder {
    let builder = SandboxPolicyBuilder::new().allow_process_spawn().env("PATH", "/usr/bin:/bin");
    if cfg!(target_os = "linux") { builder.allow_read("/") } else { builder }
}

/// A canonical temporary directory; both enforcers match resolved paths.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let canonical = std::fs::canonicalize(directory.path()).expect("canonicalize");
    (directory, canonical)
}

async fn run(
    enforcer: &dyn SandboxEnforcer,
    policy: &adk_sandbox::SandboxPolicy,
    cwd: &Path,
    script: &str,
) -> Output {
    sandboxed_command(
        enforcer,
        OsStr::new("sh"),
        &[OsString::from("-c"), OsString::from(script)],
        policy,
        cwd,
    )
    .expect("the sandboxed command builds")
    .stdin(std::process::Stdio::null())
    .output()
    .await
    .expect("the sandboxed command runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test]
async fn the_command_starts_in_the_workspace_and_writes_there() {
    let Some(enforcer) = enforcer() else { return };
    let (_guard, workspace) = canonical_tempdir();
    let policy = system_policy().allow_read_write(&workspace).build();

    let output = run(enforcer.as_ref(), &policy, &workspace, "pwd; echo built > out.txt").await;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&output.stdout).trim(), workspace.display().to_string());
    assert_eq!(std::fs::read_to_string(workspace.join("out.txt")).unwrap(), "built\n");
}

#[tokio::test]
async fn a_write_outside_the_allowed_paths_is_refused() {
    let Some(enforcer) = enforcer() else { return };
    let (_guard, workspace) = canonical_tempdir();
    let (_outside_guard, outside) = canonical_tempdir();
    let target = outside.join("escaped.txt");
    let policy = system_policy().allow_read_write(&workspace).build();

    let output = run(
        enforcer.as_ref(),
        &policy,
        &workspace,
        &format!("echo escaped > '{}'", target.display()),
    )
    .await;

    assert!(!output.status.success(), "{output:?}");
    assert!(!target.exists(), "a write outside every allowed path succeeded");
    // Seatbelt refuses the open. bubblewrap never mounts the directory, so the path does not
    // exist inside the sandbox and the shell's wording varies.
    if cfg!(target_os = "macos") {
        assert!(text(&output.stderr).contains("Operation not permitted"), "{output:?}");
    }
}

#[tokio::test]
async fn home_is_the_scratch_directory_the_policy_names() {
    let Some(enforcer) = enforcer() else { return };
    let (_guard, workspace) = canonical_tempdir();
    let (_scratch_guard, scratch) = canonical_tempdir();
    let policy = system_policy()
        .allow_read_write(&workspace)
        .allow_read_write(&scratch)
        .env("HOME", scratch.display().to_string())
        .env("TMPDIR", scratch.display().to_string())
        .build();

    let output =
        run(enforcer.as_ref(), &policy, &workspace, r#"echo "$HOME"; echo cached > ~/.cache"#)
            .await;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&output.stdout).trim(), scratch.display().to_string());
    assert_eq!(std::fs::read_to_string(scratch.join(".cache")).unwrap(), "cached\n");
}

#[tokio::test]
async fn the_parent_environment_does_not_reach_the_command() {
    let Some(enforcer) = enforcer() else { return };
    let (_guard, workspace) = canonical_tempdir();
    let policy = system_policy().allow_read(&workspace).env("VISIBLE", "yes").build();

    // `HOME` and `USER` are set in every test runner's environment.
    let output = run(
        enforcer.as_ref(),
        &policy,
        &workspace,
        r#"echo "${VISIBLE}:${HOME-unset}:${USER-unset}""#,
    )
    .await;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&output.stdout).trim(), "yes:unset:unset");
}

/// Connects to a listener on the host's loopback interface from inside the sandbox.
async fn connect_to_host_loopback(allow_network: bool) -> Option<Output> {
    let enforcer = enforcer()?;
    if !Path::new("/usr/bin/nc").exists() && !Path::new("/bin/nc").exists() {
        eprintln!("nc is not installed, skipping");
        return None;
    }
    let (_guard, workspace) = canonical_tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
    let port = listener.local_addr().expect("local address").port();
    let builder = system_policy().allow_read(&workspace);
    let policy = if allow_network { builder.allow_network() } else { builder }.build();

    let output =
        run(enforcer.as_ref(), &policy, &workspace, &format!("nc -z -w 3 127.0.0.1 {port}")).await;
    drop(listener);
    Some(output)
}

#[tokio::test]
async fn the_network_is_blocked_when_the_policy_denies_it() {
    let Some(output) = connect_to_host_loopback(false).await else { return };
    assert!(!output.status.success(), "the sandbox reached the host network: {output:?}");
}

#[tokio::test]
async fn the_network_is_reachable_when_the_policy_allows_it() {
    let Some(output) = connect_to_host_loopback(true).await else { return };
    assert!(output.status.success(), "{output:?}");
}

/// The session id from a `/proc/<pid>/stat` line: the fourth field after the command name.
#[cfg(target_os = "linux")]
fn session_id(stat: &str) -> i64 {
    let after_command = &stat[stat.rfind(')').expect("a stat line") + 1..];
    after_command.split_whitespace().nth(3).expect("a session field").parse().expect("a number")
}

/// Spec 3.1 §10 S2: a shell, which needs process spawning, still runs in a new session, so
/// it has no controlling terminal to inject input into with `TIOCSTI`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_spawning_command_runs_in_a_new_session() {
    let Some(enforcer) = enforcer() else { return };
    let (_guard, workspace) = canonical_tempdir();
    let policy = system_policy().allow_read(&workspace).build();
    assert!(policy.allow_process_spawn);

    let output = run(enforcer.as_ref(), &policy, &workspace, "cat /proc/self/stat").await;

    assert!(output.status.success(), "{output:?}");
    let own = session_id(&std::fs::read_to_string("/proc/self/stat").expect("own stat"));
    assert_ne!(session_id(&text(&output.stdout)), own, "the command shares the caller's session");
}
