//! The Windows restricted-token enforcer confines a shell (spec 3.2 §10 W5).
//!
//! Each test runs `cmd.exe /c` through `WindowsEnforcer::run_confined` and checks the file
//! system afterwards. Scripts name paths relative to the workspace, so they need no quoting,
//! and every refusal is paired with a control that the same command form succeeds where the
//! policy allows it. They run on the PR tier's `windows` job.
#![cfg(all(windows, feature = "sandbox-windows"))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

#[test]
fn no_descendant_outlives_the_call() {
    let (_root, _base, workspace) = layout();
    // A background shell appends to `tick.txt` in a long loop; the outer shell spins long
    // enough for it to start, then exits, and the call's job is terminated.
    let outcome = run(
        &workspace,
        "start /b cmd /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt \
         & for /l %j in (1,1,1000000) do @rem",
    );
    assert!(!outcome.timed_out, "{outcome:?}");
    let tick = workspace.join("tick.txt");
    let size = std::fs::metadata(&tick).expect("the background shell ran").len();
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
