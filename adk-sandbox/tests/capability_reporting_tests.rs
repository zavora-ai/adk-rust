//! What the OS enforcers actually restrict, asserted rather than described.
//!
//! Two claims did not match behaviour:
//!
//! 1. `ProcessBackend::capabilities` reported one `filesystem_isolation` flag, set true
//!    whenever any enforcer was configured, while the macOS Seatbelt profile followed
//!    `(deny default)` with `(allow default)` and never denied reads. The flag was split
//!    into write and read isolation, and the profile is now genuinely deny-by-default, so
//!    macOS reports both.
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
fn the_macos_profile_is_deny_default_and_reports_read_isolation() {
    use adk_sandbox::sandbox::macos::MacOsEnforcer;
    use adk_sandbox::{SandboxEnforcer, SandboxPolicyBuilder};

    let enforcer = MacOsEnforcer::new();
    if enforcer.probe().is_err() {
        // Seatbelt unavailable on this host; nothing to assert.
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let policy = SandboxPolicyBuilder::new().allow_read_write(dir.path()).build();
    let wrapped = enforcer
        .wrap_command(std::ffi::OsStr::new("/bin/echo"), &[], &policy)
        .expect("wrapping must succeed on macOS");
    let profile = wrapped.args[1].to_string_lossy();

    assert!(profile.contains("(deny default)"), "{profile}");
    assert!(
        !profile.contains("(allow default)"),
        "an allow-default rule would reopen every read the capability claims is confined"
    );

    let caps =
        ProcessBackend::with_sandbox(Default::default(), Box::new(enforcer), policy).capabilities();
    assert!(caps.enforced_limits.filesystem_write_isolation);
    assert!(caps.enforced_limits.filesystem_read_isolation);
    assert!(caps.enforced_limits.network_isolation);
}

// The Windows enforcer type only exists when compiling for Windows with
// `sandbox-windows`, so its `probe` change cannot be exercised from this host. It now
// returns `EnforcerUnavailable` naming AppContainer, instead of succeeding on a link-time
// symbol check while `configure_command` still fails — a probe that passes where execution
// cannot is worse than one that says so.
