//! The Windows restricted-token enforcer confines a shell (spec 3.2 §10 W5).
//!
//! Each test runs `cmd.exe /c` through `WindowsEnforcer::run_confined`, or through
//! `WindowsEnforcer::spawn_contained` with its standard streams on pipes, and checks the file
//! system afterwards. Scripts name paths relative to the workspace, so they need no quoting,
//! and every refusal is paired with a control that the same command form succeeds where the
//! policy allows it. They run on the PR tier's `windows` job.
#![cfg(all(windows, feature = "sandbox-windows"))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::io::{Read as _, Write as _};

use adk_sandbox::process_tree::ContainedChild;
use adk_sandbox::sandbox::SandboxPolicyBuilder;
use adk_sandbox::sandbox::windows::{ConfinedOutcome, WindowsEnforcer};

fn run_for(workspace: &Path, script: &str, timeout: Duration) -> ConfinedOutcome {
    let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
    WindowsEnforcer::new()
        .run_confined(
            &policy,
            "cmd.exe".as_ref(),
            &[OsString::from("/c"), OsString::from(script)],
            workspace,
            Some(timeout),
        )
        .expect("the confined command runs")
}

fn run(workspace: &Path, script: &str) -> ConfinedOutcome {
    run_for(workspace, script, Duration::from_secs(60))
}

/// A temporary base directory holding the workspace `ws` and its siblings.
fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(root.path()).expect("canonicalize");
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    (root, base, workspace)
}

/// A sibling directory only the current user can open, so the restricted token is not
/// granted it; the user (outside the sandbox) can still write it.
fn private_dir(base: &Path, name: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).expect("create the directory");
    let user = std::env::var("USERNAME").expect("USERNAME");
    let status = std::process::Command::new("icacls")
        .arg(&dir)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(OI)(CI)F"))
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls runs");
    assert!(status.success(), "icacls set the directory's ACL");
    std::fs::write(dir.join("probe.txt"), "host").expect("the user can write the directory");
    dir
}

/// The workspace's files and their contents, for a failure message.
fn listing(workspace: &Path) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(workspace).into_iter().flatten().flatten() {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let content: String = content.chars().take(400).collect();
        out.push_str(&format!("{}: {content:?}\n", entry.file_name().to_string_lossy()));
    }
    out
}

#[test]
fn the_shell_writes_inside_the_workspace() {
    let (_root, _base, workspace) = layout();
    let outcome = run(&workspace, "echo ok> inside.txt");
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    let written = std::fs::read_to_string(workspace.join("inside.txt")).expect("inside.txt");
    assert_eq!(written.trim(), "ok");
}

#[test]
fn the_shell_cannot_write_outside_the_workspace() {
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "outside");

    run(&workspace, r"echo yes> control.txt & echo no> ..\outside\outside.txt");

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "yes", "the script ran");
    assert!(!outside.join("outside.txt").exists(), "a file was written outside the workspace");
}

#[test]
fn the_shell_cannot_read_a_path_the_policy_does_not_grant() {
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "ungranted");
    std::fs::write(outside.join("note.txt"), "ungranted-content").expect("write the note");
    std::fs::write(workspace.join("granted.txt"), "granted-content").expect("write granted");

    run(&workspace, r"type granted.txt > control.txt & type ..\ungranted\note.txt > read.txt 2>&1");

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "granted-content", "the same command reads a granted file");
    let seen = std::fs::read_to_string(workspace.join("read.txt")).expect("read.txt");
    assert!(!seen.contains("ungranted-content"), "the ungranted file was read: {seen}");
}

/// A sandboxed shell starts child processes, which inherit the restricted token: the token's
/// default DACL must let the shell open the processes it creates.
#[test]
fn the_shell_starts_a_child_process() {
    let (_root, _base, workspace) = layout();
    let outcome = run(&workspace, "cmd /c echo child^> child.txt 2> spawn-err.txt");
    let child = std::fs::read_to_string(workspace.join("child.txt")).unwrap_or_default();
    assert_eq!(child.trim(), "child", "{outcome:?}\n{}", listing(&workspace));
}

#[test]
fn no_descendant_outlives_the_call() {
    let (_root, _base, workspace) = layout();
    // A background shell appends to `tick.txt` in a long loop; the outer shell spins long
    // enough for it to start, then exits, and the call's job is terminated.
    let outcome = run(
        &workspace,
        "start /b cmd /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt 2> start-err.txt \
         & for /l %j in (1,1,1000000) do @rem",
    );
    assert!(!outcome.timed_out, "{outcome:?}");
    let tick = workspace.join("tick.txt");
    let size = std::fs::metadata(&tick)
        .unwrap_or_else(|e| panic!("the background shell ran: {e}\n{}", listing(&workspace)))
        .len();
    std::thread::sleep(Duration::from_secs(3));
    let later = std::fs::metadata(&tick).expect("tick.txt").len();
    assert_eq!(size, later, "a descendant kept running after the call");
}

#[test]
fn a_timeout_terminates_the_job() {
    let (_root, _base, workspace) = layout();
    let outcome =
        run_for(&workspace, "for /l %i in (1,1,2000000000) do @rem", Duration::from_secs(2));
    assert_eq!(outcome, ConfinedOutcome { exit_code: None, timed_out: true });
}

/// Starts `script` under `cmd.exe /d /c` with `spawn_contained`, the workspace read-write.
fn spawn(workspace: &Path, script: &str) -> ContainedChild {
    let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
    let system = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    WindowsEnforcer::new()
        .spawn_contained(
            &policy,
            system.join("System32").join("cmd.exe").as_os_str(),
            &[OsString::from("/d"), OsString::from("/c"), OsString::from(script)],
            workspace,
        )
        .expect("the contained command starts")
}

#[test]
fn the_probe_runs_a_command_under_the_restricted_token() {
    WindowsEnforcer::new().probe_confined().expect("the restricted-token path is available");
}

#[tokio::test]
async fn a_contained_command_streams_its_output_and_reports_its_exit_code() {
    let (_root, _base, workspace) = layout();
    let mut child = spawn(&workspace, "echo out& echo err 1>&2& exit /b 7");
    drop(child.stdin.take());
    let (mut stdout, mut stderr) = (String::new(), String::new());
    child.stdout.take().expect("piped").read_to_string(&mut stdout).expect("stdout");
    child.stderr.take().expect("piped").read_to_string(&mut stderr).expect("stderr");

    assert_eq!(child.process.wait().await.expect("waited"), 7);
    assert_eq!((stdout.trim(), stderr.trim()), ("out", "err"));
}

#[tokio::test]
async fn a_contained_command_reads_its_standard_input() {
    let (_root, _base, workspace) = layout();
    let mut child = spawn(&workspace, "findstr /r \".*\"");
    let mut stdin = child.stdin.take().expect("piped");
    stdin.write_all(b"hello\r\n").expect("write stdin");
    drop(stdin);
    let mut stdout = String::new();
    child.stdout.take().expect("piped").read_to_string(&mut stdout).expect("stdout");

    assert_eq!(child.process.wait().await.expect("waited"), 0);
    assert_eq!(stdout.trim(), "hello");
}

#[tokio::test]
async fn a_contained_command_is_confined_to_its_grants() {
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "outside");
    let mut child = spawn(&workspace, r"echo yes> control.txt & echo no> ..\outside\outside.txt");
    drop(child.stdin.take());

    child.process.wait().await.expect("waited");

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "yes", "the script ran");
    assert!(!outside.join("outside.txt").exists(), "a file was written outside the workspace");
}

#[tokio::test]
async fn dropping_a_contained_child_ends_every_process_it_started() {
    let (_root, _base, workspace) = layout();
    let mut child = spawn(
        &workspace,
        "start /b cmd /d /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt \
         & ping -n 300 127.0.0.1 >nul",
    );
    drop(child.stdin.take());
    let tick = workspace.join("tick.txt");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !tick.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(child.process.try_wait().expect("queried"), None, "the shell still runs");

    let process = child.process.clone();
    drop(child);

    tokio::time::timeout(Duration::from_secs(10), process.wait())
        .await
        .expect("the shell ended with its job")
        .expect("waited");
    let size = std::fs::metadata(&tick).expect("the background loop ran").len();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        std::fs::metadata(&tick).expect("tick.txt").len(),
        size,
        "a descendant kept running"
    );
}
