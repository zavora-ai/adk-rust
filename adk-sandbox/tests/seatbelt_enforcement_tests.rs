//! What the Seatbelt profile enforces, observed from inside `sandbox-exec`.
//!
//! Two defects motivated these tests:
//!
//! 1. The profile emitted `(allow default)` after `(deny default)`, so every read and every
//!    mach lookup was allowed while the module documented a deny-default whitelist. Host
//!    secrets such as `~/.ssh` were readable from inside the sandbox.
//! 2. Allowed paths were interpolated into the profile without escaping. A directory whose
//!    name closed the string literal could append its own directives — the reported payload
//!    `w"))(allow network*)(allow file-write* (subpath "/` granted network and writes to `/`.
//!
//! Each test skips when `sandbox-exec` cannot apply a profile on this host, for example when
//! the test runner is itself sandboxed.

#![cfg(all(feature = "sandbox-macos", target_os = "macos"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use adk_sandbox::sandbox::macos::MacOsEnforcer;
use adk_sandbox::{
    ExecRequest, ExecResult, Language, ProcessBackend, ProcessConfig, SandboxBackend,
    SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder,
};

fn seatbelt_available() -> bool {
    match MacOsEnforcer::new().probe() {
        Ok(()) => true,
        Err(error) => {
            eprintln!("seatbelt unavailable on this host, skipping: {error}");
            false
        }
    }
}

/// Quotes `path` for a POSIX shell command line.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// A canonical temporary directory; Seatbelt matches resolved paths.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let canonical = std::fs::canonicalize(directory.path()).expect("canonicalize");
    (directory, canonical)
}

async fn run_sandboxed(policy: SandboxPolicy, script: &str, env: &[(&str, &Path)]) -> ExecResult {
    let backend = ProcessBackend::with_sandbox(
        ProcessConfig::default(),
        Box::new(MacOsEnforcer::new()),
        policy,
    );
    let mut request_env = HashMap::new();
    request_env.insert("PATH".to_string(), "/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    for (key, value) in env {
        request_env.insert((*key).to_string(), value.display().to_string());
    }
    backend
        .execute(ExecRequest {
            language: Language::Command,
            code: script.to_string(),
            stdin: None,
            timeout: Duration::from_secs(30),
            memory_limit_mb: None,
            env: request_env,
        })
        .await
        .expect("the sandboxed command runs to completion")
}

#[tokio::test]
async fn ssh_keys_under_home_are_unreadable() {
    if !seatbelt_available() {
        return;
    }
    let (_guard, home) = canonical_tempdir();
    std::fs::create_dir(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_ed25519"), "PRIVATE-KEY-MATERIAL").unwrap();
    std::fs::create_dir(home.join("project")).unwrap();
    std::fs::write(home.join("project/notes.txt"), "project-notes").unwrap();

    // The policy grants a project directory under the same home: the key beside it must
    // stay out of reach.
    let policy =
        SandboxPolicyBuilder::new().allow_read(home.join("project")).allow_process_spawn().build();
    let result = run_sandboxed(
        policy,
        r#"cat "$HOME/project/notes.txt"; echo; cat "$HOME/.ssh/id_ed25519"; ls "$HOME/.ssh""#,
        &[("HOME", &home)],
    )
    .await;

    assert!(
        result.stdout.contains("project-notes"),
        "the allowed path must be readable: {result:?}"
    );
    assert!(!result.stdout.contains("PRIVATE-KEY-MATERIAL"), "the key was read: {result:?}");
    assert!(!result.stdout.contains("id_ed25519"), "the key directory was listed: {result:?}");
    assert!(result.stderr.contains("Operation not permitted"), "{result:?}");
}

#[tokio::test]
async fn the_temp_directory_and_home_are_not_writable_by_default() {
    if !seatbelt_available() {
        return;
    }
    let (_guard, home) = canonical_tempdir();
    let target = home.join("written-by-sandbox");

    let result = run_sandboxed(
        SandboxPolicyBuilder::new().build(),
        &format!("echo pwned > {}", shell_quote(&target)),
        &[],
    )
    .await;

    assert_ne!(result.exit_code, 0, "{result:?}");
    assert!(!target.exists(), "a write outside every allowed path succeeded");
}

/// The payload from the finding, made real: a directory chain whose first component closes
/// the literal and whose remainder spells the parent directory, so the unescaped profile
/// would grant writes to the parent.
#[tokio::test]
async fn a_hostile_directory_name_cannot_grant_writes_outside_it() {
    if !seatbelt_available() {
        return;
    }
    let (_guard, root) = canonical_tempdir();
    let parent_literal = root.display().to_string();
    let hostile = PathBuf::from(format!(
        "{parent_literal}/w\"))(allow network*)(allow file-write* (subpath \"{parent_literal}"
    ));
    std::fs::create_dir_all(&hostile).expect("the hostile directory chain is creatable");
    let outside = root.join("outside");

    let policy = SandboxPolicyBuilder::new().allow_read_write(&hostile).build();
    let profile =
        MacOsEnforcer::new().wrap_command("/bin/sh".as_ref(), &[], &policy).expect("wraps").args[1]
            .to_string_lossy()
            .into_owned();
    assert!(!profile.contains("\n(allow network*)"), "network was injected:\n{profile}");

    let result = run_sandboxed(
        policy,
        &format!(
            "echo inside > {inside}/file && echo outside > {outside}",
            inside = shell_quote(&hostile),
            outside = shell_quote(&outside)
        ),
        &[],
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(hostile.join("file")).unwrap_or_default(),
        "inside\n",
        "the hostile directory itself must stay writable: {result:?}"
    );
    assert!(!outside.exists(), "the directory name granted writes to its parent: {result:?}");
}

/// A path with a quote and a backslash is granted exactly, no more and no less.
#[tokio::test]
async fn a_quoted_path_still_sandboxes_correctly() {
    if !seatbelt_available() {
        return;
    }
    let (_guard, root) = canonical_tempdir();
    let quoted = root.join("it's \"quoted\" \\ dir");
    std::fs::create_dir(&quoted).unwrap();
    let sibling = root.join("sibling");

    let result = run_sandboxed(
        SandboxPolicyBuilder::new().allow_read_write(&quoted).build(),
        &format!(
            "echo ok > {inside}/file; echo no > {sibling}",
            inside = shell_quote(&quoted),
            sibling = shell_quote(&sibling)
        ),
        &[],
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(quoted.join("file")).unwrap_or_default(),
        "ok\n",
        "{result:?}"
    );
    assert!(!sibling.exists(), "a write next to the quoted path succeeded: {result:?}");
}

/// The deny-default profile still runs a shell, the system `echo`, and process spawning
/// when the policy allows it.
#[tokio::test]
async fn shell_programs_run_under_the_deny_default_profile() {
    if !seatbelt_available() {
        return;
    }
    let result = run_sandboxed(
        SandboxPolicyBuilder::new().allow_process_spawn().build(),
        "echo builtin; /bin/echo external; /usr/bin/env true && echo spawned",
        &[],
    )
    .await;

    assert_eq!(result.exit_code, 0, "{result:?}");
    assert_eq!(result.stdout, "builtin\nexternal\nspawned\n");
}

#[tokio::test]
async fn process_spawning_is_denied_unless_allowed() {
    if !seatbelt_available() {
        return;
    }
    let result =
        run_sandboxed(SandboxPolicyBuilder::new().build(), "/bin/echo a; /bin/echo b", &[]).await;

    assert_ne!(result.exit_code, 0, "{result:?}");
    assert!(!result.stdout.contains('b'), "a second process was spawned: {result:?}");
}

/// `python3` with only the policy's defaults, provided it lives in the system runtime.
#[tokio::test]
#[ignore = "depends on a python3 under /usr, /opt/homebrew, or /Library/Frameworks"]
async fn python_runs_under_the_deny_default_profile() {
    if !seatbelt_available() {
        return;
    }
    let backend = ProcessBackend::with_sandbox(
        ProcessConfig::default(),
        Box::new(MacOsEnforcer::new()),
        SandboxPolicyBuilder::new().build(),
    );
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
    let result = backend
        .execute(ExecRequest {
            language: Language::Python,
            code: "import json, os\nprint(json.dumps({'cwd_listable': bool(os.listdir('.')) or True}))"
                .to_string(),
            stdin: None,
            timeout: Duration::from_secs(30),
            memory_limit_mb: None,
            env,
        })
        .await
        .expect("python runs");

    assert_eq!(result.exit_code, 0, "{result:?}");
    assert!(result.stdout.contains("cwd_listable"), "{result:?}");
}

/// Compiling and running Rust needs the toolchain granted explicitly, as any program
/// installed outside the system runtime does.
#[tokio::test]
#[ignore = "depends on a rustup toolchain and Xcode or the Command Line Tools"]
async fn a_compiled_rust_binary_runs_under_the_deny_default_profile() {
    if !seatbelt_available() {
        return;
    }
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let rustup = std::env::var("RUSTUP_HOME").map_or_else(|_| home.join(".rustup"), PathBuf::from);
    let cargo = std::env::var("CARGO_HOME").map_or_else(|_| home.join(".cargo"), PathBuf::from);
    let developer = std::process::Command::new("xcode-select").arg("-p").output().expect("xcode");
    let developer = PathBuf::from(String::from_utf8_lossy(&developer.stdout).trim());
    // `/Applications/Xcode.app/Contents/Developer` -> `/Applications/Xcode.app`.
    let developer_root = developer
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(&developer)
        .to_path_buf();

    let policy = SandboxPolicyBuilder::new()
        .allow_read(rustup)
        .allow_read(cargo)
        .allow_read(developer_root)
        .allow_read("/Library/Developer")
        .allow_read("/Library/Preferences")
        .allow_process_spawn()
        .build();
    let backend = ProcessBackend::with_sandbox(
        ProcessConfig::default(),
        Box::new(MacOsEnforcer::new()),
        policy,
    );
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
    let result = backend
        .execute(ExecRequest {
            language: Language::Rust,
            code: "fn main() { println!(\"hello from seatbelt\"); }".to_string(),
            stdin: None,
            timeout: Duration::from_secs(120),
            memory_limit_mb: None,
            env,
        })
        .await
        .expect("compiles and runs");

    assert_eq!(result.exit_code, 0, "{result:?}");
    assert_eq!(result.stdout.trim(), "hello from seatbelt");
}
