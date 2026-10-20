//! Enforcer conformance for a read-only path inside a read-write path (spec 3.1 §10 S3).
//!
//! A harness grants its workspace read-write and keeps `.git`, `AGENTS.md`, and similar
//! paths read-only inside it. Seatbelt applies the last rule that matches a path and
//! bubblewrap the last mount made over it, and before S3 neither made such an entry hold:
//! Seatbelt kept the parent's write grant, and bubblewrap's result depended on the order of
//! the policy's entries. The same script runs under both enforcers here and must reach the
//! same outcome — every write, creation, removal, rename, or link under the read-only entry
//! is refused, and ordinary work beside it succeeds — whatever order the policy lists the
//! entries in.
//!
//! Each case skips when the enforcer cannot run on the host (a sandboxed test runner, a
//! missing `bwrap`, user namespaces disabled).

#![cfg(any(
    all(feature = "sandbox-macos", target_os = "macos"),
    all(feature = "sandbox-linux", target_os = "linux")
))]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;

use adk_sandbox::sandbox::{
    AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder,
    sandboxed_command,
};

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

/// A workspace with the files a harness protects, plus ordinary sources beside them.
struct Workspace {
    _guard: tempfile::TempDir,
    root: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let guard = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(guard.path()).expect("canonicalize");
        for dir in [".git/hooks", "src", "sub/.git"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (file, content) in [
            (".git/config", "config\n"),
            ("AGENTS.md", "instructions\n"),
            ("sub/.git/config", "nested config\n"),
            ("src/lib.rs", "fn main() {}\n"),
        ] {
            std::fs::write(root.join(file), content).unwrap();
        }
        Self { _guard: guard, root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn read(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.path(relative)).ok()
    }

    /// The protected entries' contents and the names in their directories.
    fn protected_state(&self) -> Vec<String> {
        let mut state = Vec::new();
        for file in [".git/config", "AGENTS.md", "sub/.git/config"] {
            state.push(format!("{file}={:?}", self.read(file)));
        }
        for dir in [".git", ".git/hooks", "sub", "sub/.git"] {
            let mut names: Vec<String> = std::fs::read_dir(self.path(dir))
                .map(|entries| {
                    entries
                        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            state.push(format!("{dir}/={names:?}"));
        }
        state
    }
}

/// A policy granting the workspace read-write and `entries` inside it, with the read-only
/// entries first or last.
fn policy(
    workspace: &Workspace,
    entries: &[(&str, AccessMode)],
    nested_first: bool,
) -> SandboxPolicy {
    let mut builder =
        SandboxPolicyBuilder::new().allow_process_spawn().env("PATH", "/usr/bin:/bin");
    if cfg!(target_os = "linux") {
        // bubblewrap starts from an empty root, and on a merged-`/usr` host binding `/bin` and
        // `/lib` mounts `/usr/bin` and `/usr/lib`, leaving the dynamic loader's path missing.
        builder = builder.allow_read("/");
    }
    let mut policy = builder.build();
    let root = AllowedPath { path: workspace.root.clone(), mode: AccessMode::ReadWrite };
    let nested = entries
        .iter()
        .map(|(relative, mode)| AllowedPath { path: workspace.path(relative), mode: *mode });
    if nested_first {
        policy.allowed_paths.extend(nested);
        policy.allowed_paths.push(root);
    } else {
        policy.allowed_paths.push(root);
        policy.allowed_paths.extend(nested);
    }
    policy
}

const PROTECTED: &[(&str, AccessMode)] = &[
    (".git", AccessMode::ReadOnly),
    ("AGENTS.md", AccessMode::ReadOnly),
    ("sub/.git", AccessMode::ReadOnly),
];

async fn run(
    enforcer: &dyn SandboxEnforcer,
    policy: &SandboxPolicy,
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

/// Scripts that must fail without changing a protected entry.
const REFUSED: &[&str] = &[
    "echo tampered > .git/config",
    "echo '#!/bin/sh' > .git/hooks/pre-commit",
    "mv src/lib.rs .git/hooks/pre-commit",
    "chmod 777 .git/config",
    "echo tampered >> AGENTS.md",
    "rm AGENTS.md",
    "mv AGENTS.md AGENTS.old",
    "mv .git git.old",
    "rm -r .git",
    "echo tampered > sub/.git/config",
    // A directory above a read-only entry cannot carry it away from its path.
    "mv sub sub.old",
    // Neither a hard link nor a symbolic link opens a write path into the entry.
    "ln .git/config config-link && echo tampered >> config-link",
    "ln -s .git/config config-symlink && echo tampered > config-symlink",
];

/// Scripts beside the protected entries that must succeed, and a file each one leaves.
const ALLOWED: &[(&str, &str)] = &[
    ("echo 'fn changed() {}' > src/lib.rs", "src/lib.rs"),
    ("echo notes > NOTES.md", "NOTES.md"),
    ("mkdir -p target/debug && echo built > target/debug/out", "target/debug/out"),
    ("echo beside > sub/beside.txt && mv sub/beside.txt sub/moved.txt", "sub/moved.txt"),
    (
        "mkdir scratch && mv scratch scratch.renamed && echo ok > scratch.renamed/f",
        "scratch.renamed/f",
    ),
];

#[tokio::test]
async fn a_read_only_entry_inside_the_workspace_refuses_every_write() {
    let Some(enforcer) = enforcer() else { return };

    // A fresh workspace per script, so one script that gets through cannot hide another.
    let mut failures = Vec::new();
    for nested_first in [true, false] {
        for script in REFUSED {
            let workspace = Workspace::new();
            let before = workspace.protected_state();
            let policy = policy(&workspace, PROTECTED, nested_first);

            let output = run(enforcer.as_ref(), &policy, &workspace.root, script).await;

            if output.status.success() {
                failures.push(format!("nested_first={nested_first}: succeeded: {script}"));
            }
            if workspace.protected_state() != before {
                failures.push(format!(
                    "nested_first={nested_first}: changed protected state: {script}"
                ));
            }
        }
    }
    assert_eq!(failures, Vec::<String>::new());
}

#[tokio::test]
async fn work_beside_a_read_only_entry_still_succeeds() {
    let Some(enforcer) = enforcer() else { return };

    for nested_first in [true, false] {
        let workspace = Workspace::new();
        let policy = policy(&workspace, PROTECTED, nested_first);

        for (script, leaves) in ALLOWED {
            let output = run(enforcer.as_ref(), &policy, &workspace.root, script).await;
            assert!(output.status.success(), "nested_first={nested_first} {script}: {output:?}");
            assert!(workspace.path(leaves).exists(), "{script} did not leave {leaves}");
        }
    }
}

/// The most specific entry decides: a read-write entry inside the read-only one is
/// writable, and is itself fixed in place, since moving it would change `.git`.
#[tokio::test]
async fn a_read_write_entry_inside_a_read_only_entry_is_writable() {
    let Some(enforcer) = enforcer() else { return };
    let workspace = Workspace::new();
    std::fs::create_dir(workspace.path(".git/scratch")).unwrap();
    let policy = policy(
        &workspace,
        &[(".git", AccessMode::ReadOnly), (".git/scratch", AccessMode::ReadWrite)],
        true,
    );

    let inner = run(enforcer.as_ref(), &policy, &workspace.root, "echo ok > .git/scratch/f").await;
    assert!(inner.status.success(), "{inner:?}");
    assert_eq!(workspace.read(".git/scratch/f").as_deref(), Some("ok\n"));

    let outer =
        run(enforcer.as_ref(), &policy, &workspace.root, "echo tampered > .git/config").await;
    assert!(!outer.status.success(), "{outer:?}");
    assert_eq!(workspace.read(".git/config").as_deref(), Some("config\n"));

    // GNU `mv` falls back to copy-and-delete when the rename fails across bubblewrap's mount,
    // so the files inside, which are writable, may leave; the directory itself stays.
    let moved = run(enforcer.as_ref(), &policy, &workspace.root, "mv .git/scratch scratch").await;
    assert!(!moved.status.success(), "the entry moved out of .git: {moved:?}");
    assert!(workspace.path(".git/scratch").is_dir());
}
