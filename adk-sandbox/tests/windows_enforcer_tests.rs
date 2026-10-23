//! The Windows restricted-token enforcer confines a shell (spec 3.2 §10 W5).
//!
//! Each test runs `cmd.exe /c` through `WindowsEnforcer::run_confined` and checks the file
//! system afterwards. They run on the PR tier's `windows` job.
#![cfg(all(windows, feature = "sandbox-windows"))]

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use adk_sandbox::sandbox::SandboxPolicyBuilder;
use adk_sandbox::sandbox::windows::WindowsEnforcer;

fn run(workspace: &Path, script: &str) -> adk_sandbox::sandbox::windows::ConfinedOutcome {
    let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
    WindowsEnforcer::new()
        .run_confined(
            &policy,
            "cmd.exe".as_ref(),
            &[OsString::from("/c"), OsString::from(script)],
            workspace,
            Some(Duration::from_secs(60)),
        )
        .expect("the confined command runs")
}

/// A directory only the current user can open, so the restricted token is not granted it.
fn private_dir(parent: &Path, name: &str) -> std::path::PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create the directory");
    let status = std::process::Command::new("icacls")
        .arg(&dir)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{}:(OI)(CI)F", std::env::var("USERNAME").expect("USERNAME")))
        .status()
        .expect("icacls runs");
    assert!(status.success(), "icacls set the directory's ACL");
    dir
}

#[test]
fn the_shell_writes_inside_the_workspace() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = std::fs::canonicalize(root.path()).expect("canonicalize");
    let outcome = run(&workspace, "echo ok> inside.txt");
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    let written = std::fs::read_to_string(workspace.join("inside.txt")).expect("inside.txt");
    assert_eq!(written.trim(), "ok");
}

#[test]
fn the_shell_cannot_write_outside_the_workspace() {
    let root = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(root.path()).expect("canonicalize");
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let outside = private_dir(&base, "outside");
    let target = outside.join("outside.txt");

    run(&workspace, &format!("echo no> \"{}\"", target.display()));

    assert!(!target.exists(), "a file was written outside the workspace");
}

#[test]
fn the_shell_cannot_read_a_path_the_policy_does_not_grant() {
    let root = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(root.path()).expect("canonicalize");
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let outside = private_dir(&base, "ungranted");
    let file = outside.join("note.txt");
    std::fs::write(&file, "not-for-the-sandbox").expect("write the note");

    run(&workspace, &format!("type \"{}\" > read.txt 2>&1", file.display()));

    let seen = std::fs::read_to_string(workspace.join("read.txt")).unwrap_or_default();
    assert!(!seen.contains("not-for-the-sandbox"), "the ungranted file was read: {seen}");
}

#[test]
fn no_descendant_outlives_the_call() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = std::fs::canonicalize(root.path()).expect("canonicalize");
    // The background child writes its marker after ten seconds; the outer shell exits at once
    // and the job is terminated, so the marker must never appear.
    let outcome =
        run(&workspace, "start \"\" /b cmd /c \"ping -n 11 127.0.0.1 >nul & echo late> late.txt\"");
    assert!(!outcome.timed_out, "{outcome:?}");
    std::thread::sleep(Duration::from_secs(15));
    assert!(!workspace.join("late.txt").exists(), "a descendant kept running after the call");
}

#[test]
fn a_timeout_terminates_the_job() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = std::fs::canonicalize(root.path()).expect("canonicalize");
    let policy = SandboxPolicyBuilder::new().allow_read_write(&workspace).build();
    let outcome = WindowsEnforcer::new()
        .run_confined(
            &policy,
            "cmd.exe".as_ref(),
            &[OsString::from("/c"), OsString::from("ping -n 30 127.0.0.1 >nul")],
            &workspace,
            Some(Duration::from_secs(2)),
        )
        .expect("the confined command runs");
    assert!(outcome.timed_out, "{outcome:?}");
}
