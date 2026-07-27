//! The Linux enforcer must be selectable where bubblewrap works.
//!
//! `probe` ran `bwrap --unshare-user -- /bin/true` with no bind mounts. bwrap gives the new
//! namespace an empty root, so `/bin/true` did not exist inside it and execvp failed — which the
//! probe reported as "user namespaces are not available. Check that
//! `kernel.unprivileged_userns_clone` sysctl is set to 1". The check therefore failed on **every**
//! host, so `get_enforcer()` never returned the bubblewrap enforcer and Linux silently ran with no
//! OS-level sandbox while the docs advertised one. The diagnostic also pointed at a sysctl that
//! was never the cause.

#![cfg(all(target_os = "linux", feature = "sandbox-linux"))]

use adk_sandbox::sandbox::get_enforcer;
use adk_sandbox::sandbox::linux::LinuxEnforcer;
use adk_sandbox::{ProcessBackend, SandboxBackend, SandboxEnforcer, SandboxPolicyBuilder};

/// Whether this host can create user namespaces at all, checked independently of the probe.
fn user_namespaces_work() -> bool {
    std::process::Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--unshare-user", "--", "/bin/true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn the_probe_succeeds_where_user_namespaces_actually_work() {
    if !user_namespaces_work() {
        eprintln!("host cannot create user namespaces; nothing to assert");
        return;
    }

    LinuxEnforcer::new()
        .probe()
        .expect("bubblewrap works on this host, so the probe must not report it unavailable");
}

#[test]
fn a_working_host_reports_filesystem_isolation() {
    if !user_namespaces_work() {
        return;
    }

    // The consequence of the broken probe: `get_enforcer` never returned one, so a caller
    // following the documented path got a backend with no enforcer and both flags false on a
    // host that fully supports them.
    let enforcer = get_enforcer().expect("a working host must yield an enforcer");
    let policy = SandboxPolicyBuilder::new().build();
    let caps = ProcessBackend::with_sandbox(Default::default(), enforcer, policy).capabilities();
    assert!(
        caps.enforced_limits.filesystem_write_isolation,
        "bubblewrap is available, so writes must be reported as confined"
    );
    assert!(
        caps.enforced_limits.filesystem_read_isolation,
        "the Linux filesystem namespace confines reads, unlike the macOS profile"
    );
}
