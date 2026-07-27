//! What the OS enforcers actually restrict, asserted rather than described.
//!
//! Two claims did not match behaviour:
//!
//! 1. `ProcessBackend::capabilities` reported one `filesystem_isolation` flag, set true
//!    whenever any enforcer was configured. The macOS Seatbelt profile contains both
//!    `(deny default)` and `(allow default)`, then denies network, fork, and *writes*
//!    before re-allowing writes to configured paths. It never denies reads, so sandboxed
//!    code could read host files outside the allowed paths while the capability said the
//!    filesystem was isolated.
//! 2. The Windows `probe` checked that `CreateAppContainerProfile` links, which proves the
//!    platform API exists — while `configure_command` still returns `EnforcerFailed`
//!    because nothing is implemented. A caller selecting an enforcer by probing would pick
//!    it and fail at run time.

use adk_sandbox::{ProcessBackend, SandboxBackend};

// ── Capability reporting is platform-accurate ─────────────────────────

#[test]
fn a_backend_without_an_enforcer_claims_no_filesystem_isolation() {
    let caps = ProcessBackend::default().capabilities();
    assert!(!caps.enforced_limits.filesystem_write_isolation);
    assert!(!caps.enforced_limits.filesystem_read_isolation);
    assert!(
        caps.enforced_limits.environment_isolation,
        "the environment is cleared even without an enforcer"
    );
    assert!(caps.enforced_limits.timeout);
}

#[cfg(all(feature = "sandbox-macos", target_os = "macos"))]
#[test]
fn the_macos_profile_denies_writes_but_not_reads() {
    use adk_sandbox::sandbox::macos::MacOsEnforcer;
    use adk_sandbox::{SandboxEnforcer, SandboxPolicyBuilder};

    let enforcer = MacOsEnforcer::new();
    if enforcer.probe().is_err() {
        // Seatbelt unavailable on this host; nothing to assert.
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let policy = SandboxPolicyBuilder::new().allow_read_write(dir.path()).build();
    let profile = enforcer
        .wrap_command(std::ffi::OsStr::new("/bin/echo"), &[], &policy)
        .expect("wrapping must succeed on macOS");

    // The generated profile is passed to sandbox-exec; find it in the arguments.
    let rendered = profile
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        rendered.contains("file-write") || rendered.contains("deny default"),
        "the profile must restrict writes: {rendered}"
    );
    // This documents the gap rather than asserting isolation the profile does not give:
    // there is no blanket read denial, which is why `filesystem_read_isolation` is false
    // on macOS.
    assert!(
        !rendered.contains("(deny file-read*)"),
        "if a blanket read denial is added, update the reported read-isolation capability"
    );
}

// The Windows enforcer type only exists when compiling for Windows with
// `sandbox-windows`, so its `probe` change cannot be exercised from this host. It now
// returns `EnforcerUnavailable` naming AppContainer, instead of succeeding on a link-time
// symbol check while `configure_command` still fails — a probe that passes where execution
// cannot is worse than one that says so.
