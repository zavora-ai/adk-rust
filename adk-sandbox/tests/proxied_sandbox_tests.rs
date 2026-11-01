//! A sandboxed process under `Proxied` network access reaches the network only through the
//! call's egress proxy (#1281): Seatbelt on macOS, bubblewrap on Linux.
//!
//! The tests need no external network. A destination the policy refuses is answered by the
//! proxy itself with `403`, which proves the process reached the proxy and the proxy decided;
//! a direct connection to a listener on the host is refused by the sandbox.
//!
//! Each case skips when the enforcer cannot run on the host, and fails instead when
//! `ADK_SANDBOX_REQUIRE_ENFORCER` is set, as in the `Sandbox Linux` workflow.

#![cfg(all(
    feature = "egress-proxy",
    any(
        all(feature = "sandbox-macos", target_os = "macos"),
        all(feature = "sandbox-linux", target_os = "linux")
    )
))]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use adk_core::egress::EgressPolicy;
use adk_sandbox::egress_proxy::{EgressOutcome, EgressProxy, InMemoryEgressAudit};
use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicyBuilder, sandboxed_command};

fn enforcer() -> Option<Box<dyn SandboxEnforcer>> {
    #[cfg(target_os = "macos")]
    let enforcer = adk_sandbox::sandbox::macos::MacOsEnforcer::new();
    #[cfg(target_os = "linux")]
    let enforcer = adk_sandbox::sandbox::linux::LinuxEnforcer::new();
    match enforcer.probe() {
        Ok(()) => Some(Box::new(enforcer)),
        Err(error) => {
            assert!(
                std::env::var_os("ADK_SANDBOX_REQUIRE_ENFORCER").is_none(),
                "ADK_SANDBOX_REQUIRE_ENFORCER is set and the enforcer is unavailable: {error}"
            );
            eprintln!("the OS sandbox is unavailable on this host, skipping: {error}");
            None
        }
    }
}

/// A canonical temporary directory; both enforcers match resolved paths.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let canonical = std::fs::canonicalize(directory.path()).expect("canonicalize");
    (directory, canonical)
}

async fn run(
    enforcer: &dyn SandboxEnforcer,
    network: adk_sandbox::NetworkAccess,
    script: &str,
) -> Output {
    let (_guard, workspace) = canonical_tempdir();
    let builder = SandboxPolicyBuilder::new()
        .allow_process_spawn()
        .allow_read(&workspace)
        .network(network)
        .env("PATH", "/usr/bin:/bin");
    // bubblewrap starts from an empty root; on a merged-`/usr` host the root is bound
    // read-only, and the socket directories below it are covered.
    let builder = if cfg!(target_os = "linux") { builder.allow_read("/") } else { builder };
    let policy = builder.build();
    let command = sandboxed_command(
        enforcer,
        OsStr::new("sh"),
        &[OsString::from("-c"), OsString::from(script)],
        &policy,
        &workspace,
    );
    tokio::time::timeout(
        Duration::from_secs(60),
        command.expect("the sandboxed command builds").stdin(std::process::Stdio::null()).output(),
    )
    .await
    .expect("the sandboxed command ends")
    .expect("the sandboxed command runs")
}

fn curl_available() -> bool {
    let found = Path::new("/usr/bin/curl").exists();
    if !found {
        eprintln!("curl is not installed, skipping");
    }
    found
}

#[tokio::test]
async fn a_proxied_process_reaches_the_network_only_through_the_proxy() {
    let Some(enforcer) = enforcer() else { return };
    if !curl_available() {
        return;
    }
    let audit = Arc::new(InMemoryEgressAudit::new());
    let policy = EgressPolicy::allowlist(["allowed.example"]).unwrap().with_plain_http();
    let proxy = EgressProxy::builder(policy)
        .call_id("call-7")
        .audit(audit.clone())
        .start_for(enforcer.as_ref())
        .expect("the proxy starts");
    let host = std::net::TcpListener::bind("127.0.0.1:0").expect("a host listener");
    let host_port = host.local_addr().expect("address").port();
    let script = format!(
        "curl -s -o /dev/null -w '%{{http_code}}\\n' --max-time 10 http://blocked.example/; \
         curl -s -o /dev/null -w '%{{http_code}}\\n' --max-time 3 --noproxy '*' \
         http://127.0.0.1:{host_port}/; true"
    );

    let output = run(enforcer.as_ref(), proxy.network(), &script).await;

    let lines: Vec<String> =
        String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        ["403", "000"],
        "the proxy refuses the host and the sandbox refuses the direct connection: {output:?}"
    );
    assert!(proxy.is_listening());
    for _ in 0..100 {
        if !audit.records().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let records = audit.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].call.as_deref(), Some("call-7"));
    assert_eq!(records[0].host, "blocked.example");
    assert!(
        matches!(&records[0].outcome, EgressOutcome::Denied { code, .. } if code == "egress.host_not_allowed"),
        "{records:?}"
    );
}
