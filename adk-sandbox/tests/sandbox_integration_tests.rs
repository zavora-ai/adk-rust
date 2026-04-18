//! Integration tests for OS-level sandbox enforcement.
//!
//! All tests are marked `#[ignore]` because they require platform-specific
//! tools to be installed:
//! - macOS: `sandbox-exec` (built-in)
//! - Linux: `bwrap` (`apt install bubblewrap`)
//!
//! Run with: `cargo nextest run -p adk-sandbox --features sandbox-macos -- --ignored`

// ---------------------------------------------------------------------------
// macOS Seatbelt integration tests
// ---------------------------------------------------------------------------

/// macOS: Execute `cat` on a temp file under sandbox with read access to /tmp.
/// Verifies the sandboxed process can read allowed paths.
#[cfg(all(feature = "sandbox-macos", target_os = "macos"))]
#[tokio::test]
#[ignore]
async fn test_macos_sandbox_read_allowed_path() {
    use adk_sandbox::sandbox::SandboxEnforcer;
    use adk_sandbox::sandbox::SandboxPolicyBuilder;
    use adk_sandbox::sandbox::macos::MacOsEnforcer;
    use adk_sandbox::types::{ExecRequest, Language};
    use adk_sandbox::{ProcessBackend, ProcessConfig, SandboxBackend};
    use std::collections::HashMap;
    use std::io::Write;
    use std::time::Duration;

    // Create a temp file with known content
    let mut tmp = tempfile::NamedTempFile::new_in("/tmp").expect("create temp file");
    writeln!(tmp, "sandbox-test-content").expect("write temp file");
    let tmp_path = tmp.path().to_path_buf();

    // Build policy: allow read to /tmp, deny network, allow process spawn (for cat)
    let policy = SandboxPolicyBuilder::new().allow_read("/tmp").allow_process_spawn().build();

    let enforcer = MacOsEnforcer::new();
    enforcer.probe().expect("seatbelt probe should succeed on macOS");

    // Use ProcessBackend::with_sandbox for end-to-end test
    let backend = ProcessBackend::with_sandbox(
        ProcessConfig::default(),
        Box::new(MacOsEnforcer::new()),
        policy,
    );

    let mut env = HashMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }

    let request = ExecRequest {
        language: Language::Command,
        code: format!("cat {}", tmp_path.display()),
        stdin: None,
        timeout: Duration::from_secs(10),
        memory_limit_mb: None,
        env,
    };

    let result = backend.execute(request).await.expect("execution should succeed");
    assert_eq!(result.exit_code, 0, "exit code should be 0, stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("sandbox-test-content"),
        "stdout should contain file content, got: {}",
        result.stdout
    );
}

/// macOS: Execute a network request under sandbox with network denied.
/// Verifies the sandboxed process cannot access the network.
#[cfg(all(feature = "sandbox-macos", target_os = "macos"))]
#[tokio::test]
#[ignore]
async fn test_macos_sandbox_network_blocked() {
    use adk_sandbox::sandbox::SandboxPolicyBuilder;
    use adk_sandbox::sandbox::macos::MacOsEnforcer;
    use adk_sandbox::types::{ExecRequest, Language};
    use adk_sandbox::{ProcessBackend, ProcessConfig, SandboxBackend};
    use std::collections::HashMap;
    use std::time::Duration;

    // Build policy: deny network, allow process spawn, allow read to system paths
    let policy = SandboxPolicyBuilder::new()
        .allow_read("/usr")
        .allow_read("/System")
        .allow_read("/Library")
        .allow_read("/private")
        .allow_process_spawn()
        .build();

    let backend = ProcessBackend::with_sandbox(
        ProcessConfig::default(),
        Box::new(MacOsEnforcer::new()),
        policy,
    );

    let mut env = HashMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }

    let request = ExecRequest {
        language: Language::Command,
        code:
            r#"python3 -c "import urllib.request; urllib.request.urlopen('https://example.com')""#
                .to_string(),
        stdin: None,
        timeout: Duration::from_secs(15),
        memory_limit_mb: None,
        env,
    };

    let result = backend.execute(request).await.expect("execution should complete");
    assert_ne!(
        result.exit_code, 0,
        "exit code should be non-zero (network blocked), stdout: {}, stderr: {}",
        result.stdout, result.stderr
    );
    // stderr should contain some error about the network being blocked
    assert!(
        !result.stderr.is_empty(),
        "stderr should contain an error message about network failure"
    );
}

// ---------------------------------------------------------------------------
// get_enforcer tests (platform-agnostic)
// ---------------------------------------------------------------------------

/// On macOS with sandbox-macos feature: get_enforcer returns Ok with name "seatbelt".
#[cfg(all(feature = "sandbox-macos", target_os = "macos"))]
#[test]
#[ignore]
fn test_get_enforcer_macos() {
    use adk_sandbox::sandbox::get_enforcer;
    let enforcer =
        get_enforcer().expect("get_enforcer should succeed on macOS with sandbox-macos feature");
    assert_eq!(enforcer.name(), "seatbelt");
}

/// On Linux with sandbox-linux feature: get_enforcer returns Ok with name "bubblewrap"
/// (if bwrap is installed).
#[cfg(all(feature = "sandbox-linux", target_os = "linux"))]
#[test]
#[ignore]
fn test_get_enforcer_linux() {
    use adk_sandbox::sandbox::get_enforcer;
    let result = get_enforcer();
    match result {
        Ok(enforcer) => {
            assert_eq!(enforcer.name(), "bubblewrap");
        }
        Err(e) => {
            // bwrap might not be installed — that's acceptable for this test
            eprintln!("get_enforcer failed (bwrap may not be installed): {e}");
        }
    }
}
